//! Native per-root FSEvents streams.
use crate::model::{Event, Volume};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString, c_void};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub const MUST_SCAN_SUBDIRS: u32 = 0x1;
pub const USER_DROPPED: u32 = 0x2;
pub const KERNEL_DROPPED: u32 = 0x4;
pub const EVENT_IDS_WRAPPED: u32 = 0x8;
pub const HISTORY_DONE: u32 = 0x10;
pub const ROOT_CHANGED: u32 = 0x20;
pub const MOUNT: u32 = 0x40;
pub const UNMOUNT: u32 = 0x80;
pub const ITEM_CREATED: u32 = 0x100;
pub const ITEM_REMOVED: u32 = 0x200;
pub const ITEM_RENAMED: u32 = 0x800;
pub const ITEM_IS_DIR: u32 = 0x20000;
pub type Callback = Arc<dyn Fn(Vec<Event>) -> Result<()> + Send + Sync>;
pub type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub struct CursorInvalidError(pub String);
impl std::fmt::Display for CursorInvalidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CursorInvalidError {}

pub fn translate(
    volume: &Volume,
    watched: &str,
    path: &str,
    flags: u32,
    id: u64,
) -> Result<Vec<Event>> {
    let event = |path: String, flags| Event { path, flags, id };
    if flags & (USER_DROPPED | KERNEL_DROPPED | EVENT_IDS_WRAPPED | HISTORY_DONE) != 0 {
        return Ok(vec![event(String::new(), flags)]);
    }
    if path.split('/').any(|p| p == "..") {
        bail!("FSEvents supplied a path outside its volume");
    }
    let physical = crate::policy::absolute(
        &Path::new(&volume.mount)
            .join(path.trim_start_matches('/'))
            .to_string_lossy(),
    );
    if crate::policy::within(&physical, watched) {
        let suffix: std::path::PathBuf = Path::new(&physical)
            .components()
            .skip(Path::new(watched).components().count())
            .collect();
        let mapped = Path::new(&volume.roots[0])
            .join(suffix)
            .to_string_lossy()
            .into_owned();
        return Ok(vec![event(mapped, flags)]);
    }
    if crate::policy::within(watched, &physical) {
        return Ok(vec![event(
            volume.roots[0].clone(),
            flags | MUST_SCAN_SUBDIRS,
        )]);
    }
    if flags & (ROOT_CHANGED | MUST_SCAN_SUBDIRS | MOUNT | UNMOUNT) != 0 {
        return Ok(vec![event(String::new(), flags)]);
    }
    Ok(vec![])
}

fn invalid(message: impl Into<String>) -> anyhow::Error {
    CursorInvalidError(message.into()).into()
}
fn failure(message: &str) -> anyhow::Error {
    io::Error::other(message).into()
}

struct CallbackState {
    volume: Volume,
    watched: String,
    callback: Callback,
    wake: Wake,
    error: Mutex<Option<String>>,
    thread: Mutex<Option<std::thread::ThreadId>>,
}
impl CallbackState {
    fn fail(&self, error: String) {
        if let Ok(mut slot) = self.error.lock()
            && slot.is_none()
        {
            *slot = Some(error);
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.wake)()));
    }
    fn error(&self) -> Option<String> {
        self.error
            .lock()
            .map(|v| v.clone())
            .unwrap_or_else(|_| Some("native callback state poisoned".into()))
    }
    fn check_thread(&self) -> Result<()> {
        if self
            .thread
            .lock()
            .map_err(|_| failure("callback thread state poisoned"))?
            .is_some_and(|id| id == std::thread::current().id())
        {
            bail!("Stream lifecycle cannot run inside its callback");
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod ffi {
    use super::*;
    pub type Pointer = *mut c_void;
    pub type CallbackFn =
        unsafe extern "C" fn(Pointer, Pointer, usize, Pointer, *const u32, *const u64);
    #[repr(C)]
    pub struct Context {
        pub version: isize,
        pub info: Pointer,
        pub retain: Option<unsafe extern "C" fn(Pointer) -> Pointer>,
        pub release: Option<unsafe extern "C" fn(Pointer)>,
        pub describe: Option<unsafe extern "C" fn(Pointer) -> Pointer>,
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFStringCreateWithFileSystemRepresentation(
            allocator: Pointer,
            bytes: *const libc::c_char,
        ) -> Pointer;
        pub fn CFStringGetCString(
            value: Pointer,
            buffer: *mut libc::c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        pub fn CFUUIDCreateString(allocator: Pointer, value: Pointer) -> Pointer;
        pub fn CFArrayCreate(
            allocator: Pointer,
            values: *const Pointer,
            count: isize,
            callbacks: Pointer,
        ) -> Pointer;
        pub fn CFRelease(value: Pointer);
    }
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        pub fn FSEventsCopyUUIDForDevice(device: libc::dev_t) -> Pointer;
        pub fn FSEventsGetCurrentEventId() -> u64;
        pub fn FSEventsGetLastEventIdForDeviceBeforeTime(device: libc::dev_t, time: f64) -> u64;
        pub fn FSEventStreamCreateRelativeToDevice(
            allocator: Pointer,
            callback: CallbackFn,
            context: *const Context,
            device: libc::dev_t,
            paths: Pointer,
            since: u64,
            latency: f64,
            flags: u32,
        ) -> Pointer;
        pub fn FSEventStreamSetDispatchQueue(stream: Pointer, queue: Pointer);
        pub fn FSEventStreamStart(stream: Pointer) -> u8;
        pub fn FSEventStreamFlushSync(stream: Pointer);
        pub fn FSEventStreamStop(stream: Pointer);
        pub fn FSEventStreamInvalidate(stream: Pointer);
        pub fn FSEventStreamRelease(stream: Pointer);
    }
    unsafe extern "C" {
        pub fn dispatch_queue_create(label: *const libc::c_char, attr: Pointer) -> Pointer;
        pub fn dispatch_sync_f(
            queue: Pointer,
            context: Pointer,
            work: unsafe extern "C" fn(Pointer),
        );
        #[cfg(test)]
        pub fn dispatch_async_f(
            queue: Pointer,
            context: Pointer,
            work: unsafe extern "C" fn(Pointer),
        );
        pub fn dispatch_release(queue: Pointer);
    }
}

#[cfg(target_os = "macos")]
fn device_uuid(device: u64) -> Result<String> {
    unsafe {
        let uuid = ffi::FSEventsCopyUUIDForDevice(device as libc::dev_t);
        if uuid.is_null() {
            return Err(failure("FSEvents UUID unavailable for volume"));
        }
        let string = ffi::CFUUIDCreateString(std::ptr::null_mut(), uuid);
        let mut buffer = [0i8; 64];
        let good = !string.is_null()
            && ffi::CFStringGetCString(string, buffer.as_mut_ptr(), 64, 0x08000100) != 0;
        let result = if good {
            CStr::from_ptr(buffer.as_ptr())
                .to_str()
                .map(str::to_owned)
                .context("invalid volume UUID")
        } else {
            Err(failure("Cannot decode FSEvents volume UUID"))
        };
        if !string.is_null() {
            ffi::CFRelease(string);
        }
        ffi::CFRelease(uuid);
        result
    }
}

#[cfg(target_os = "macos")]
fn root_info(root: &str) -> Result<(u64, String, String)> {
    use crate::directory_io::{DirectoryIo, DirectoryMaterialization};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    fn completed(deadline: &DirectoryIo, status: i32, operation: &str) -> Result<()> {
        let error = (status < 0).then(io::Error::last_os_error);
        #[cfg(test)]
        root_io_hook(operation);
        // Capture errno before checking the guard. Timeout wins over EINTR and
        // apparent success; these raw calls must not retry EINTR indefinitely.
        deadline
            .progress()
            .and_then(|()| error.map_or(Ok(()), Err))
            .with_context(|| format!("watch root {operation}"))
    }
    fn path_string(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in watch path"))
    }
    fn directory(path: &str, deadline: &DirectoryIo) -> Result<std::fs::File> {
        let path = path_string(Path::new(path))?;
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        // Own a successful descriptor before a timeout check can return early.
        let file = (fd >= 0).then(|| unsafe { std::fs::File::from_raw_fd(fd) });
        completed(deadline, fd, "open")?;
        Ok(file.expect("successful directory open owns its descriptor"))
    }
    fn descriptor_metadata(fd: libc::c_int, deadline: &DirectoryIo) -> Result<libc::stat> {
        let mut metadata = std::mem::MaybeUninit::uninit();
        let status = unsafe { libc::fstat(fd, metadata.as_mut_ptr()) };
        completed(deadline, status, "fstat")?;
        Ok(unsafe { metadata.assume_init() })
    }
    fn path_metadata(path: &Path, deadline: &DirectoryIo) -> Result<libc::stat> {
        let path = path_string(path)?;
        let mut metadata = std::mem::MaybeUninit::uninit();
        let status = unsafe { libc::stat(path.as_ptr(), metadata.as_mut_ptr()) };
        completed(deadline, status, "stat")?;
        Ok(unsafe { metadata.assume_init() })
    }
    fn descriptor_path(fd: libc::c_int, deadline: &DirectoryIo) -> Result<String> {
        let mut buffer = [0i8; 1024];
        let status = unsafe { libc::fcntl(fd, libc::F_GETPATH_NOFIRMLINK, buffer.as_mut_ptr()) };
        completed(deadline, status, "fcntl")?;
        Ok(unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_str()
            .context("watched path is not UTF-8")?
            .to_owned())
    }

    let deadline = DirectoryIo::begin().context("watch root I/O deadline")?;
    let _materialization =
        DirectoryMaterialization::begin().context("watch root materialization policy")?;
    let file = directory(root, &deadline)?;
    let metadata = descriptor_metadata(file.as_raw_fd(), &deadline)?;
    let physical = descriptor_path(file.as_raw_fd(), &deadline)?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    let status = unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) };
    completed(&deadline, status, "fstatfs")?;
    let filesystem = unsafe { filesystem.assume_init() };
    let mount = unsafe { CStr::from_ptr(filesystem.f_mntonname.as_ptr()) }
        .to_str()
        .context("mount path is not UTF-8")?;
    if mount.is_empty() {
        return Err(failure("Cannot map watched root to its physical volume"));
    }
    let mounted = directory(mount, &deadline)?;
    let mounted_info = descriptor_metadata(mounted.as_raw_fd(), &deadline)?;
    if metadata.st_dev != mounted_info.st_dev {
        return Err(failure("Volume mount changed during discovery"));
    }
    let mount = descriptor_path(mounted.as_raw_fd(), &deadline)?;
    let resolved = path_metadata(Path::new(&mount), &deadline)?;
    if (resolved.st_dev, resolved.st_ino) != (mounted_info.st_dev, mounted_info.st_ino) {
        return Err(failure("Volume mount changed during discovery"));
    }
    let relative = Path::new(&physical)
        .strip_prefix(&mount)
        .map_err(|_| failure("Cannot map root to physical volume"))?
        .to_str()
        .context("relative root is not UTF-8")?
        .to_owned();
    let checked = path_metadata(&Path::new(&mount).join(&relative), &deadline)?;
    if (checked.st_dev, checked.st_ino) != (metadata.st_dev, metadata.st_ino) {
        return Err(failure("Watched root changed during discovery"));
    }
    Ok((metadata.st_dev as u64, mount, relative))
}

#[cfg(all(test, target_os = "macos"))]
type RootIoHook = Box<dyn FnMut(&str)>;
#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static TEST_ROOT_IO: std::cell::RefCell<Option<RootIoHook>> = const {std::cell::RefCell::new(None)};
}
#[cfg(all(test, target_os = "macos"))]
fn root_io_hook(operation: &str) {
    TEST_ROOT_IO.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(operation);
        }
    });
}

pub fn discover_volumes(roots: &[String]) -> Result<Vec<Volume>> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = roots;
        Err(io::Error::new(io::ErrorKind::Unsupported, "FSEvents requires macOS").into())
    }
    #[cfg(target_os = "macos")]
    {
        let mut seen = std::collections::HashSet::new();
        let mut identities = std::collections::HashMap::new();
        let mut result = vec![];
        for supplied in roots {
            let root = crate::policy::absolute(supplied);
            if !seen.insert(root.clone()) {
                continue;
            }
            let (device, mount, _) = root_info(&root)?;
            let uuid = device_uuid(device)?;
            if identities
                .insert(uuid.clone(), (device, mount.clone()))
                .is_some_and(|old| old != (device, mount.clone()))
            {
                return Err(failure("Volume identity changed during discovery"));
            }
            let key = format!("{uuid}:{:x}", Sha256::digest(root.as_bytes()));
            result.push(Volume {
                key,
                uuid,
                device,
                mount,
                roots: vec![root],
            });
        }
        Ok(result)
    }
}

pub struct Stream {
    state: Option<Box<CallbackState>>,
    since: Option<u64>,
    start_id: Option<u64>,
    native: *mut c_void,
    queue: *mut c_void,
    array: *mut c_void,
    string: *mut c_void,
    started: bool,
    scheduled: bool,
    closed: bool,
}
// Ownership is exclusive; callbacks access only the stable boxed synchronized
// context. Lifecycle calls are rejected from the callback queue itself.
unsafe impl Send for Stream {}
impl Stream {
    pub fn new(volume: Volume, since: Option<u64>, callback: Callback, wake: Wake) -> Result<Self> {
        if volume.roots.len() != 1 || since == Some(u64::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A stream requires one root and an unsigned saved cursor below SinceNow",
            )
            .into());
        }
        Ok(Self {
            state: Some(Box::new(CallbackState {
                volume,
                watched: String::new(),
                callback,
                wake,
                error: Mutex::new(None),
                thread: Mutex::new(None),
            })),
            since,
            start_id: None,
            native: std::ptr::null_mut(),
            queue: std::ptr::null_mut(),
            array: std::ptr::null_mut(),
            string: std::ptr::null_mut(),
            started: false,
            scheduled: false,
            closed: false,
        })
    }
    pub fn start_id(&self) -> Option<u64> {
        self.start_id
    }
    pub fn error(&self) -> Option<String> {
        self.state.as_ref().and_then(|s| s.error())
    }
    pub fn start(&mut self) -> Result<()> {
        self.state.as_ref().unwrap().check_thread()?;
        if self.started {
            return Ok(());
        }
        if self.closed {
            bail!("Create a new stream after stopping");
        }
        let result = self.start_native();
        // Startup errors are returned synchronously, retaining their IO or
        // cursor-invalid type. Only asynchronous ingestion errors latch here.
        if result.is_err() {
            self.cleanup();
            self.closed = true;
        }
        result
    }
    #[cfg(not(target_os = "macos"))]
    fn start_native(&mut self) -> Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "FSEvents requires macOS").into())
    }
    #[cfg(target_os = "macos")]
    fn start_native(&mut self) -> Result<()> {
        let state = self.state.as_mut().unwrap();
        if device_uuid(state.volume.device)? != state.volume.uuid {
            return Err(invalid("FSEvents volume UUID changed"));
        }
        if self
            .since
            .is_some_and(|id| id > unsafe { ffi::FSEventsGetCurrentEventId() })
        {
            return Err(invalid("Saved cursor exceeds current history"));
        }
        let (device, mount, relative) = root_info(&state.volume.roots[0])?;
        if (device, &mount) != (state.volume.device, &state.volume.mount) {
            return Err(invalid("Watched root moved to another volume"));
        }
        state.watched = Path::new(&mount)
            .join(&relative)
            .to_string_lossy()
            .trim_end_matches('/')
            .into();
        if state.watched.is_empty() {
            state.watched = "/".into();
        }
        self.start_id = Some(self.since.unwrap_or_else(|| unsafe {
            ffi::FSEventsGetLastEventIdForDeviceBeforeTime(
                device as libc::dev_t,
                crate::model::now(),
            )
        }));
        let relative = CString::new(relative)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in watch path"))?;
        unsafe {
            self.string = ffi::CFStringCreateWithFileSystemRepresentation(
                std::ptr::null_mut(),
                relative.as_ptr(),
            );
            if self.string.is_null() {
                return Err(failure("Cannot encode FSEvents watch path"));
            }
            self.array =
                ffi::CFArrayCreate(std::ptr::null_mut(), &self.string, 1, std::ptr::null_mut());
            if self.array.is_null() {
                return Err(failure("Cannot allocate FSEvents path array"));
            }
            let context = ffi::Context {
                version: 0,
                info: (&mut **state as *mut CallbackState).cast(),
                retain: None,
                release: None,
                describe: None,
            };
            self.native = ffi::FSEventStreamCreateRelativeToDevice(
                std::ptr::null_mut(),
                receive,
                &context,
                device as libc::dev_t,
                self.array,
                self.start_id.unwrap(),
                0.5,
                0x04 | 0x08 | 0x10,
            );
            if self.native.is_null() {
                return Err(failure("Cannot create FSEvents stream"));
            }
            self.queue =
                ffi::dispatch_queue_create(c"jaso-nfc.events".as_ptr(), std::ptr::null_mut());
            if self.queue.is_null() {
                return Err(failure("Cannot create FSEvents dispatch queue"));
            }
            ffi::FSEventStreamSetDispatchQueue(self.native, self.queue);
            self.scheduled = true;
            if ffi::FSEventStreamStart(self.native) == 0 {
                return Err(failure("Cannot start FSEvents stream"));
            }
            self.started = true;
        }
        Ok(())
    }
    pub fn flush(&mut self) -> Result<()> {
        self.state.as_ref().unwrap().check_thread()?;
        #[cfg(target_os = "macos")]
        if self.started {
            unsafe {
                ffi::FSEventStreamFlushSync(self.native);
            }
        }
        Ok(())
    }
    pub fn stop(&mut self) -> Result<()> {
        self.state.as_ref().unwrap().check_thread()?;
        if !self.closed {
            self.cleanup();
            self.closed = true;
        }
        Ok(())
    }
    fn cleanup(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            if self.started {
                ffi::FSEventStreamStop(self.native);
                self.started = false;
            }
            if self.scheduled {
                ffi::FSEventStreamInvalidate(self.native);
                self.scheduled = false;
            }
            if !self.queue.is_null() {
                ffi::dispatch_sync_f(self.queue, std::ptr::null_mut(), barrier);
            }
            if !self.native.is_null() {
                ffi::FSEventStreamRelease(self.native);
                self.native = std::ptr::null_mut();
            }
            if !self.queue.is_null() {
                ffi::dispatch_release(self.queue);
                self.queue = std::ptr::null_mut();
            }
            if !self.array.is_null() {
                ffi::CFRelease(self.array);
                self.array = std::ptr::null_mut();
            }
            if !self.string.is_null() {
                ffi::CFRelease(self.string);
                self.string = std::ptr::null_mut();
            }
        }
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // An invalid callback-side destruction must leak rather than free
            // context which the native queue is still using.
            if let Some(state) = self.state.take() {
                Box::leak(state);
            }
        }
    }
}
#[cfg(target_os = "macos")]
unsafe extern "C" fn barrier(_: *mut c_void) {}
#[cfg(target_os = "macos")]
unsafe extern "C" fn receive(
    _: *mut c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    if info.is_null() {
        return;
    }
    let state = unsafe { &*info.cast::<CallbackState>() };
    if state.error().is_some() {
        return;
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        *state
            .thread
            .lock()
            .map_err(|_| failure("callback thread state poisoned"))? =
            Some(std::thread::current().id());
        if count > 0 && (paths.is_null() || flags.is_null() || ids.is_null()) {
            bail!("Null native event buffer");
        }
        let mut batch = vec![];
        for i in 0..count {
            let pointer = unsafe { *paths.cast::<*const libc::c_char>().add(i) };
            let path = if pointer.is_null() {
                ""
            } else {
                unsafe { CStr::from_ptr(pointer) }
                    .to_str()
                    .context("FSEvent path is not UTF-8")?
            };
            batch.extend(translate(
                &state.volume,
                &state.watched,
                path,
                unsafe { *flags.add(i) },
                unsafe { *ids.add(i) },
            )?);
        }
        if !batch.is_empty() {
            (state.callback)(batch)?;
        }
        Ok(())
    }));
    match outcome {
        Ok(Ok(())) => (),
        Ok(Err(error)) => state.fail(format!("{error:#}")),
        Err(_) => state.fail("native event callback panicked".into()),
    }
    if let Ok(mut thread) = state.thread.lock() {
        *thread = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    fn with_root_io_hook<T>(hook: impl FnMut(&str) + 'static, run: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_ROOT_IO.set(None);
            }
        }
        TEST_ROOT_IO.set(Some(Box::new(hook)));
        let _reset = Reset;
        run()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn root_discovery_rejects_stalled_metadata_at_every_io_boundary() {
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_str().unwrap().to_owned();
        for operation in ["open", "fstat", "fcntl", "fstatfs", "stat"] {
            let mut first = true;
            let result = with_root_io_hook(
                move |current| {
                    if first && current == operation {
                        first = false;
                        crate::directory_io::tests::blocking_read();
                    }
                },
                || {
                    crate::directory_io::test_timeout(Duration::from_millis(30), || {
                        discover_volumes(std::slice::from_ref(&root))
                    })
                },
            );
            let error = result.expect_err(operation);
            assert!(format!("{error:#}").contains(operation), "{error:#}");
            assert_eq!(
                error
                    .downcast_ref::<io::Error>()
                    .and_then(io::Error::raw_os_error),
                Some(libc::ETIMEDOUT),
                "{operation}: {error:#}"
            );
        }
        assert_eq!(discover_volumes(&[root]).unwrap().len(), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn root_discovery_preserves_alias_physical_mapping() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("폴더 with space");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &alias).unwrap();
        let expected = std::fs::metadata(&target).unwrap();
        let (device, mount, relative) = root_info(alias.to_str().unwrap()).unwrap();
        assert_eq!(device, expected.dev());
        let mapped = std::fs::metadata(Path::new(&mount).join(&relative)).unwrap();
        assert_eq!(
            (mapped.dev(), mapped.ino()),
            (expected.dev(), expected.ino())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn root_discovery_rejects_a_replaced_physical_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let moved = temp.path().join("moved");
        std::fs::create_dir(&root).unwrap();
        let path = root.to_str().unwrap().to_owned();
        let mut first = true;
        let error = with_root_io_hook(
            move |operation| {
                if first && operation == "stat" {
                    first = false;
                    std::fs::rename(&root, &moved).unwrap();
                    std::fs::create_dir(&root).unwrap();
                }
            },
            || root_info(&path),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Watched root changed during discovery"),
            "{error:#}"
        );
    }

    fn volume() -> Volume {
        Volume {
            key: "k".into(),
            uuid: "u".into(),
            device: 1,
            mount: "/System/Volumes/Data".into(),
            roots: vec!["/Users/example".into()],
        }
    }
    #[test]
    fn translates_physical_firmlink_paths_and_ancestor_controls() {
        let v = volume();
        let physical = "/System/Volumes/Data/Users/example";
        let e = translate(&v, physical, "Users/example/파일", ITEM_CREATED, 44).unwrap();
        assert_eq!(e[0].path, "/Users/example/파일");
        assert_eq!(e[0].id, 44);
        let e = translate(&v, physical, "Users", MUST_SCAN_SUBDIRS, 45).unwrap();
        assert_eq!(e[0].path, v.roots[0]);
        assert_ne!(e[0].flags & MUST_SCAN_SUBDIRS, 0);
        assert!(
            translate(&v, physical, "Users/example2/file", ITEM_CREATED, 46)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn control_history_paths_are_not_filename_jobs_and_escape_is_rejected() {
        let v = volume();
        for flag in [
            HISTORY_DONE,
            USER_DROPPED,
            KERNEL_DROPPED,
            EVENT_IDS_WRAPPED,
        ] {
            let e = translate(&v, "/watched", "untrusted", flag, 9).unwrap();
            assert_eq!(e.len(), 1);
            assert_eq!(e[0].path, "");
            assert_eq!(e[0].flags, flag);
        }
        assert!(translate(&v, "/watched", "../elsewhere", ITEM_CREATED, 1).is_err());
        assert_eq!(
            translate(&v, "/watched", "unrelated", ROOT_CHANGED, 2).unwrap()[0].path,
            ""
        );
    }
    #[test]
    fn refuses_shared_cursor_roots_and_invalid_saved_cursor() {
        let mut v = volume();
        v.roots.push("/other".into());
        assert!(Stream::new(v, None, Arc::new(|_| Ok(())), Arc::new(|| {})).is_err());
        assert!(
            Stream::new(
                volume(),
                Some(u64::MAX),
                Arc::new(|_| Ok(())),
                Arc::new(|| {})
            )
            .is_err()
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn callback_failure_latches_and_withholds_later_batches() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let wakes = Arc::new(AtomicUsize::new(0));
        let notified = wakes.clone();
        let mut state = CallbackState {
            volume: volume(),
            watched: "/System/Volumes/Data/Users/example".into(),
            callback: Arc::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                bail!("fixture durable ingestion failed")
            }),
            wake: Arc::new(move || {
                notified.fetch_add(1, Ordering::SeqCst);
            }),
            error: Mutex::new(None),
            thread: Mutex::new(None),
        };
        let path = CString::new("Users/example/file").unwrap();
        let mut paths = [path.as_ptr()];
        for id in [4u64, 5] {
            unsafe {
                receive(
                    std::ptr::null_mut(),
                    (&mut state as *mut CallbackState).cast(),
                    1,
                    paths.as_mut_ptr().cast(),
                    &ITEM_CREATED,
                    &id,
                );
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(state.error().unwrap().contains("durable ingestion failed"));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn failure_wake_still_rejects_callback_thread_lifecycle() {
        use std::sync::{
            OnceLock, Weak,
            atomic::{AtomicBool, Ordering},
        };
        let context: Arc<OnceLock<Weak<CallbackState>>> = Arc::new(OnceLock::new());
        let observed = Arc::new(AtomicBool::new(false));
        let output = observed.clone();
        let weak = context.clone();
        let state = Arc::new(CallbackState {
            volume: volume(),
            watched: "/System/Volumes/Data/Users/example".into(),
            callback: Arc::new(|_| bail!("failure")),
            wake: Arc::new(move || {
                output.store(
                    weak.get()
                        .unwrap()
                        .upgrade()
                        .unwrap()
                        .check_thread()
                        .is_err(),
                    Ordering::SeqCst,
                );
            }),
            error: Mutex::new(None),
            thread: Mutex::new(None),
        });
        context.set(Arc::downgrade(&state)).unwrap();
        let path = CString::new("Users/example/file").unwrap();
        let mut paths = [path.as_ptr()];
        unsafe {
            receive(
                std::ptr::null_mut(),
                Arc::as_ptr(&state).cast_mut().cast(),
                1,
                paths.as_mut_ptr().cast(),
                &ITEM_CREATED,
                &1,
            );
        }
        assert!(
            observed.load(Ordering::SeqCst),
            "error wake runs on the native callback queue too"
        );
        assert!(state.check_thread().is_ok());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn startup_revalidation_failure_is_not_a_callback_failure() {
        let temp = tempfile::tempdir().unwrap();
        let volume = discover_volumes(&[temp.path().to_str().unwrap().into()])
            .unwrap()
            .remove(0);
        let mut stream = Stream::new(
            volume,
            Some(u64::MAX - 1),
            Arc::new(|_| Ok(())),
            Arc::new(|| {}),
        )
        .unwrap();
        assert!(stream.start().unwrap_err().is::<CursorInvalidError>());
        stream.stop().unwrap();
        assert!(
            stream.error().is_none(),
            "synchronous startup failure must remain recoverable"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn stop_waits_for_an_in_flight_callback_before_releasing_context() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let volume = discover_volumes(&[temp.path().to_str().unwrap().into()])
            .unwrap()
            .remove(0);
        let (entered, observed) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let wait = Mutex::new(wait);
        let once = AtomicBool::new(false);
        let mut stream = Stream::new(
            volume,
            Some(unsafe { ffi::FSEventsGetCurrentEventId() }),
            Arc::new(move |_| {
                if !once.swap(true, Ordering::SeqCst) {
                    entered.send(())?;
                    wait.lock().unwrap().recv_timeout(Duration::from_secs(8))?;
                }
                Ok(())
            }),
            Arc::new(|| {}),
        )
        .unwrap();
        stream.start().unwrap();
        unsafe extern "C" fn queued(context: *mut c_void) {
            let mut path = [c"".as_ptr()];
            unsafe {
                receive(
                    std::ptr::null_mut(),
                    context,
                    1,
                    path.as_mut_ptr().cast(),
                    &HISTORY_DONE,
                    &1,
                );
            }
        }
        unsafe {
            ffi::dispatch_async_f(
                stream.queue,
                (&**stream.state.as_ref().unwrap() as *const CallbackState)
                    .cast_mut()
                    .cast(),
                queued,
            );
        }
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let (stopped, done) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = stream.stop();
            stopped.send(()).unwrap();
            result
        });
        let premature = done.recv_timeout(Duration::from_millis(50));
        release.send(()).unwrap();
        if premature.is_err() {
            done.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        worker.join().unwrap().unwrap();
        assert!(
            matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)),
            "stop returned while the callback still held context"
        );
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn real_native_stream_delivers_external_changes_and_drains_on_stop() {
        let temp = tempfile::tempdir().unwrap();
        let one = temp.path().join("one");
        let two = temp.path().join("two");
        std::fs::create_dir(&one).unwrap();
        std::fs::create_dir(&two).unwrap();
        let roots = vec![one.to_str().unwrap().into(), two.to_str().unwrap().into()];
        let volumes = discover_volumes(&roots).unwrap();
        assert_eq!(volumes.len(), 2);
        assert_eq!(volumes[0].uuid, volumes[1].uuid);
        assert_ne!(volumes[0].key, volumes[1].key);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut stream = Stream::new(
            volumes[0].clone(),
            Some(unsafe { ffi::FSEventsGetCurrentEventId() }),
            Arc::new(move |events| {
                sender.send(events)?;
                Ok(())
            }),
            Arc::new(|| {}),
        )
        .unwrap();
        stream.start().unwrap();
        assert!(stream.start_id().is_some());
        let path = one.join("delivered");
        assert!(
            std::process::Command::new("/usr/bin/touch")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        stream.flush().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut found = false;
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            let batch = receiver
                .recv_timeout(remaining)
                .expect("native event did not arrive");
            if batch
                .iter()
                .any(|event| event.path == path.to_str().unwrap())
            {
                found = true;
                break;
            }
        }
        stream.stop().unwrap();
        stream.stop().unwrap();
        assert!(found);
        assert!(stream.error().is_none());
        assert!(stream.start().is_err());
    }
}
