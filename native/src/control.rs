use crate::config::Config;
use anyhow::{Result, ensure};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::unix::{
    fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    io::{AsRawFd, FromRawFd},
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

pub struct RuntimeLock(File);
impl RuntimeLock {
    pub fn acquire(config: &Config, timeout: Duration) -> Result<Self> {
        prepare_directories(config)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(Path::new(&config.state_dir).join(".lock"))?;
        ensure!(
            file.metadata()?.is_file(),
            "runtime lock is not a regular file"
        );
        let deadline = Instant::now() + timeout;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.into());
            }
            ensure!(
                Instant::now() < deadline,
                "jaso-nfc is already running; stop it before this operation"
            );
            std::thread::sleep(
                Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
impl Drop for RuntimeLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub fn prepare_directories(config: &Config) -> Result<()> {
    for path in [
        PathBuf::from(&config.state_dir),
        PathBuf::from(config.logs()),
        config.state_path("index.sqlite3").parent().unwrap().into(),
    ] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    Ok(())
}
pub fn running(config: &Config) -> Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(Path::new(&config.state_dir).join(".lock"))
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error.into())
    }
}
pub fn paused(config: &Config) -> Result<bool> {
    let path = config.state_path("control.json");
    match std::fs::read(path) {
        Ok(bytes) => {
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            value
                .get("paused")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| anyhow::anyhow!("invalid pause state"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
pub fn set_paused(config: &Config, paused: bool) -> Result<bool> {
    crate::config::atomic_json(
        config.state_path("control.json"),
        &serde_json::json!({"version":1,"paused":paused}),
    )?;
    Ok(signal_wakeup(config.state_path("wake.fifo")))
}
fn open_fifo(path: &Path, flags: i32) -> Result<File> {
    let name = CString::new(path.as_os_str().as_encoded_bytes())?;
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            flags | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    ensure!(
        file.metadata()?.mode() & u32::from(libc::S_IFMT) == u32::from(libc::S_IFIFO),
        "worker wakeup path is not a FIFO"
    );
    Ok(file)
}
pub fn signal_wakeup(path: impl AsRef<Path>) -> bool {
    let Ok(file) = open_fifo(path.as_ref(), libc::O_WRONLY) else {
        return false;
    };
    let result = unsafe { libc::write(file.as_raw_fd(), [0_u8].as_ptr().cast(), 1) };
    result == 1 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
}

pub struct Wakeup {
    file: File,
    path: PathBuf,
    identity: (u64, u64),
}
impl Wakeup {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let name = CString::new(path.as_os_str().as_encoded_bytes())?;
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        let file = open_fifo(path, libc::O_RDWR)?;
        let info = file.metadata()?;
        Ok(Self {
            file,
            path: path.into(),
            identity: (info.dev(), info.ino()),
        })
    }
    pub fn set(&self) {
        unsafe {
            libc::write(self.file.as_raw_fd(), [0_u8].as_ptr().cast(), 1);
        }
    }
    pub fn clear(&self) -> Result<()> {
        let mut buf = [0u8; 4096];
        loop {
            let n =
                unsafe { libc::read(self.file.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n == 0 {
                return Ok(());
            }
            if n > 0 {
                continue;
            }
            let e = std::io::Error::last_os_error();
            match e.kind() {
                std::io::ErrorKind::WouldBlock => return Ok(()),
                std::io::ErrorKind::Interrupted => continue,
                _ => return Err(e.into()),
            }
        }
    }
    pub fn wait(&self, timeout: Option<Duration>) -> Result<bool> {
        let mut fd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout
            .map(|d| {
                d.as_millis()
                    .saturating_add(u128::from(d.subsec_nanos() % 1_000_000 != 0))
                    .min(i32::MAX as u128) as i32
            })
            .unwrap_or(-1);
        let n = unsafe { libc::poll(&mut fd, 1, millis) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Ok(true);
            }
            return Err(e.into());
        }
        ensure!(
            fd.revents & (libc::POLLERR | libc::POLLNVAL) == 0,
            "worker wakeup descriptor failed"
        );
        Ok(n > 0)
    }
}
impl Drop for Wakeup {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| {
            (m.dev(), m.ino()) == self.identity
                && m.mode() & u32::from(libc::S_IFMT) == u32::from(libc::S_IFIFO)
        }) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

static STOP: AtomicBool = AtomicBool::new(false);
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);
extern "C" fn stop_signal(_: i32) {
    STOP.store(true, Ordering::Relaxed);
    let fd = SIGNAL_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe {
            libc::write(fd, [0_u8].as_ptr().cast(), 1);
        }
    }
}
pub struct StopSignals(Vec<(i32, libc::sigaction)>);
impl StopSignals {
    pub fn install(wake: &Wakeup) -> Result<Self> {
        STOP.store(false, Ordering::Relaxed);
        SIGNAL_FD.store(wake.file.as_raw_fd(), Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGTERM, libc::SIGINT] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = stop_signal as *const () as usize;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            let mut old = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(signal, &action, &mut old) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            guard.0.push((signal, old));
        }
        Ok(guard)
    }
    pub fn requested(&self) -> bool {
        STOP.load(Ordering::Relaxed)
    }
}
impl Drop for StopSignals {
    fn drop(&mut self) {
        for (signal, action) in &self.0 {
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
        SIGNAL_FD.store(-1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(d: &Path) -> Config {
        let mut c = Config {
            state_dir: d.to_str().unwrap().into(),
            ..Config::default()
        };
        c.validate().unwrap();
        c
    }
    #[test]
    fn lock_prevents_two_workers_and_releases_on_drop() {
        let d = tempfile::tempdir().unwrap();
        let c = config(d.path());
        let guard = RuntimeLock::acquire(&c, std::time::Duration::ZERO).unwrap();
        assert!(RuntimeLock::acquire(&c, std::time::Duration::ZERO).is_err());
        drop(guard);
        assert!(RuntimeLock::acquire(&c, std::time::Duration::ZERO).is_ok());
    }
    #[test]
    fn pause_persists_even_when_worker_is_stopped() {
        let d = tempfile::tempdir().unwrap();
        let c = config(d.path());
        assert!(!paused(&c).unwrap());
        set_paused(&c, true).unwrap();
        assert!(paused(&c).unwrap());
        set_paused(&c, false).unwrap();
        assert!(!paused(&c).unwrap());
    }
    #[test]
    fn control_does_not_write_into_regular_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("wake.fifo");
        std::fs::write(&p, b"keep").unwrap();
        assert!(!signal_wakeup(&p));
        assert_eq!(std::fs::read(p).unwrap(), b"keep");
    }
    #[test]
    fn fifo_wakes_without_polling_and_preserves_replacement_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("wake.fifo");
        let wake = Wakeup::new(&p).unwrap();
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(signal_wakeup(&p));
        assert!(wake.wait(None).unwrap());
        wake.clear().unwrap();
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        std::fs::remove_file(&p).unwrap();
        std::fs::write(&p, b"replacement").unwrap();
        drop(wake);
        assert_eq!(std::fs::read(&p).unwrap(), b"replacement");
    }
    #[test]
    fn lock_status_is_read_only_and_detects_live_owner() {
        let d = tempfile::tempdir().unwrap();
        let c = config(d.path());
        assert!(!running(&c).unwrap());
        assert!(!d.path().join(".lock").exists());
        let _guard = RuntimeLock::acquire(&c, Duration::ZERO).unwrap();
        assert!(running(&c).unwrap());
    }
}
