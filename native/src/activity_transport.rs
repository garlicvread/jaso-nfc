//! Same-user, bounded local activity transport. The responder performs no DB,
//! control-file, provider, or cloud I/O: every response comes from memory.
use crate::{
    activity::{Activity, MAX_RESPONSE_BYTES},
    config::Config,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const IO_TIMEOUT: Duration = Duration::from_millis(250);

pub fn socket_path(config: &Config) -> PathBuf {
    // sockaddr_un is short on macOS. Hash the full state identity into a short,
    // private per-user directory instead of truncating distinct state paths.
    let digest = Sha256::digest(crate::policy::absolute(&config.state_dir).as_bytes());
    let key: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    PathBuf::from(format!("/tmp/jaso-nfc-{}", unsafe { libc::geteuid() }))
        .join(format!("activity-{key}.sock"))
}
fn owned_socket(path: &std::path::Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(info) => Ok(info.file_type().is_socket() && info.uid() == unsafe { libc::geteuid() }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn same_user(stream: &UnixStream) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let uid = {
        let (mut uid, mut gid) = (0, 0);
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        uid
    };
    #[cfg(not(target_os = "macos"))]
    let uid = {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        credentials.uid
    };
    if uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "activity peer belongs to another user",
        ));
    }
    Ok(())
}
fn envelope(activity: &Activity) -> Vec<u8> {
    let mut value = json!({"schema_version":1,"available":true,"activity":activity.snapshot()});
    loop {
        let bytes = serde_json::to_vec(&value).unwrap();
        if bytes.len() < MAX_RESPONSE_BYTES {
            return bytes;
        }
        let events = value["activity"]["events"].as_array_mut().unwrap();
        if events.is_empty() {
            return serde_json::to_vec(&json!({"schema_version":1,"available":false,"activity":null,"error":"snapshot exceeds response bound"})).unwrap();
        }
        events.remove(0);
        let count = value["activity"]["dropped_events"].as_u64().unwrap_or(0);
        value["activity"]["dropped_events"] = json!(count + 1);
    }
}
pub struct ActivityResponder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
    identity: (u64, u64),
}
impl ActivityResponder {
    /// Caller holds RuntimeLock for this Config until this responder is dropped.
    pub fn start(config: &Config, activity: Activity) -> Result<Self> {
        let path = socket_path(config);
        let parent = path.parent().unwrap();
        match fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let info = fs::symlink_metadata(parent)?;
        ensure!(
            info.is_dir()
                && !info.file_type().is_symlink()
                && info.uid() == unsafe { libc::geteuid() }
                && info.mode() & 0o077 == 0,
            "activity directory must be private and owned by this user"
        );
        if fs::symlink_metadata(&path).is_ok() {
            ensure!(
                owned_socket(&path)?,
                "refusing to replace a non-socket activity path"
            );
            // A live endpoint means a competing runtime; never unlink it.
            match connect(&path) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) => {}
                _ => anyhow::bail!("activity responder is already running or unavailable"),
            }
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let info = fs::symlink_metadata(&path)?;
        let identity = (info.dev(), info.ino());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("activity".into())
            .spawn(move || {
                // Exactly one client at a time; no client threads or unbounded queue.
                while !stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut client, _)) => {
                            let result = (|| -> io::Result<()> {
                                same_user(&client)?;
                                client.set_read_timeout(Some(IO_TIMEOUT))?;
                                client.set_write_timeout(Some(IO_TIMEOUT))?;
                                write_response(&mut client, &envelope(&activity))?;
                                client.shutdown(std::net::Shutdown::Write)
                            })();
                            let _ = result;
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::park_timeout(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                }
            });
        match thread {
            Ok(thread) => Ok(Self {
                stop,
                thread: Some(thread),
                path,
                identity,
            }),
            Err(error) => {
                let _ = fs::remove_file(path);
                Err(error.into())
            }
        }
    }
}
impl Drop for ActivityResponder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && (m.dev(), m.ino()) == self.identity)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn connect(path: &std::path::Path) -> io::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as _;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "activity socket path is too long",
        ));
    }
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as _;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    stream.set_nonblocking(true)?;
    let connected = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as _,
        )
    };
    if connected < 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS) | Some(libc::EWOULDBLOCK)
        ) {
            return Err(error);
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, IO_TIMEOUT.as_millis() as i32) } <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "activity connection timed out",
            ));
        }
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
    }
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    same_user(&stream)?;
    Ok(stream)
}
fn wait_io(stream: &UnixStream, event: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "activity client deadline expired",
            ));
        }
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: event,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "activity client deadline expired",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
fn write_response(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut offset = 0;
    while offset < bytes.len() {
        wait_io(stream, libc::POLLOUT, deadline)?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "activity peer closed",
                ));
            }
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn read_response(mut stream: UnixStream) -> io::Result<Vec<u8>> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        wait_io(&stream, libc::POLLIN, deadline)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                if bytes.len() + count > MAX_RESPONSE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "activity response exceeds byte limit",
                    ));
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

pub fn snapshot_for(config: &Config) -> Result<Value> {
    let unavailable = || json!({"schema_version":1,"available":false,"activity":null});
    let path = socket_path(config);
    if !owned_socket(&path)? {
        return Ok(unavailable());
    }
    let stream = match connect(&path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(unavailable());
        }
        Err(error) => return Err(error.into()),
    };
    let bytes = match read_response(stream) {
        Ok(bytes) => bytes,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(unavailable());
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        bytes.len() <= MAX_RESPONSE_BYTES,
        "activity response exceeds byte limit"
    );
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        value["schema_version"] == 1 && value["available"].is_boolean(),
        "unsupported activity response"
    );
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    #[test]
    fn refuses_to_replace_non_socket_or_live_endpoint() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            state_dir: directory.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let server = ActivityResponder::start(&config, Activity::new())?;
        assert!(ActivityResponder::start(&config, Activity::new()).is_err());
        assert_eq!(snapshot_for(&config)?["available"], true);
        drop(server);
        fs::write(socket_path(&config), b"owned fixture")?;
        assert!(ActivityResponder::start(&config, Activity::new()).is_err());
        assert_eq!(fs::read(socket_path(&config))?, b"owned fixture");
        fs::remove_file(socket_path(&config))?;
        Ok(())
    }
    #[test]
    fn silent_client_and_slow_peer_have_bounded_waits() -> Result<()> {
        let (mut writer, _silent) = UnixStream::pair()?;
        let started = Instant::now();
        assert!(write_response(&mut writer, &vec![0; 4 * MAX_RESPONSE_BYTES]).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        let (reader, _silent) = UnixStream::pair()?;
        let started = Instant::now();
        assert_eq!(
            read_response(reader).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        Ok(())
    }
    #[test]
    fn snapshot_responds_while_worker_is_blocked_and_socket_is_cleaned_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            state_dir: directory.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let activity = Activity::new();
        let responder = ActivityResponder::start(&config, activity.clone())?;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            activity.select_scope("blocked", "/fixture", 0, 0, None);
            activity.begin("reading_metadata", Some("/fixture/file"));
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            activity.finish("reading_metadata", Some("/fixture/file"));
        });
        entered_rx.recv()?;
        let started = Instant::now();
        let first = snapshot_for(&config)?;
        let second = snapshot_for(&config)?;
        release_tx.send(())?;
        worker.join().unwrap();
        assert_eq!(first["available"], true);
        for key in [
            "scope",
            "phase_started_at",
            "last_progress_at",
            "counters",
            "events",
        ] {
            assert_eq!(first["activity"][key], second["activity"][key]);
        }
        assert_eq!(first["activity"]["item_path"], "/fixture/file");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(socket_path(&config).exists());
        drop(responder);
        assert!(!socket_path(&config).exists());
        assert_eq!(snapshot_for(&config)?["available"], false);
        Ok(())
    }
}
