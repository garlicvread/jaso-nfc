//! Bounded, thread-directed interruption of read-only directory metadata I/O.
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Read a path's metadata with the same bounded read-only deadline as
/// descriptor operations. This is never called while an index transaction is held.
pub(crate) fn metadata(path: &std::path::Path, follow: bool) -> io::Result<libc::stat> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in metadata path"))?;
    let deadline = DirectoryIo::begin()?;
    let _materialization = DirectoryMaterialization::deny()?;
    let display = path.to_string_lossy();
    crate::activity::current(|a| a.begin("reading_metadata", Some(&display)));
    let mut info = std::mem::MaybeUninit::uninit();
    let code = unsafe {
        if follow {
            libc::stat(path.as_ptr(), info.as_mut_ptr())
        } else {
            libc::lstat(path.as_ptr(), info.as_mut_ptr())
        }
    };
    let error = (code < 0).then(io::Error::last_os_error);
    let error = deadline.progress().err().or(error);
    match error {
        Some(error) if error.kind() == io::ErrorKind::NotFound => {
            // Files can disappear between an event and its metadata probe.
            // Preserve absence for callers without presenting a failed check.
            crate::activity::current(|a| a.finish("reading_metadata", Some(&display)));
            Err(error)
        }
        Some(error) => {
            crate::activity::current(|a| {
                a.failed_with_reason(
                    "reading_metadata",
                    Some(&display),
                    Some(&error.to_string()),
                    error.raw_os_error(),
                )
            });
            Err(error)
        }
        None => {
            crate::activity::current(|a| {
                a.finish("reading_metadata", Some(&display));
                a.resolve("reading_metadata", Some(&display), "checked");
            });
            Ok(unsafe { info.assume_init() })
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod materialization_policy {
    // Public sys/resource.h constants; these bindings are absent from libc.
    pub const TYPE: i32 = 3; // IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES
    pub const THREAD: i32 = 1; // IOPOL_SCOPE_THREAD
    pub const ON: i32 = 2; // IOPOL_MATERIALIZE_DATALESS_FILES_ON
    unsafe extern "C" {
        pub fn getiopolicy_np(iotype: i32, scope: i32) -> i32;
        pub fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
    }
}

/// Temporarily allows directory metadata materialization on the calling thread.
/// Keep this scope limited to directory opens/enumeration and metadata lookups;
/// it must never enclose a file-content read. Other platforms leave policy alone.
///
/// The guard must stay on its originating thread:
/// ```compile_fail
/// use jaso_nfc::directory_io::DirectoryMaterialization;
/// fn require_send<T: Send>() {}
/// require_send::<DirectoryMaterialization>();
/// ```
/// ```compile_fail
/// use jaso_nfc::directory_io::DirectoryMaterialization;
/// fn require_sync<T: Sync>() {}
/// require_sync::<DirectoryMaterialization>();
/// ```
#[must_use = "keep the guard alive for the directory operation"]
pub struct DirectoryMaterialization {
    #[cfg(target_os = "macos")]
    previous: i32,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl DirectoryMaterialization {
    pub fn begin() -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let requested = materialization_policy::ON;
        #[cfg(not(target_os = "macos"))]
        let requested = 2;
        Self::set(requested)
    }
    /// Deny implicit dataless-file materialization around entry access/mutation.
    /// Narrow directory syscalls may temporarily override this and must restore it.
    pub(crate) fn deny() -> io::Result<Self> {
        Self::set(1)
    }
    fn set(_requested: i32) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let previous = {
            use materialization_policy::{THREAD, TYPE, getiopolicy_np, setiopolicy_np};
            let previous = unsafe { getiopolicy_np(TYPE, THREAD) };
            if previous < 0 {
                return Err(io::Error::last_os_error());
            }
            if previous != _requested && unsafe { setiopolicy_np(TYPE, THREAD, _requested) } < 0 {
                return Err(io::Error::last_os_error());
            }
            previous
        };
        Ok(Self {
            #[cfg(target_os = "macos")]
            previous,
            _thread_bound: std::marker::PhantomData,
        })
    }
}
impl Drop for DirectoryMaterialization {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            use materialization_policy::{THREAD, TYPE, setiopolicy_np};
            {
                // Restoring a value just read on this same thread uses the
                // matching public API. Drop must also run during unwinding.
                unsafe { setiopolicy_np(TYPE, THREAD, self.previous) };
            }
        }
    }
}

const SIGNAL: i32 = libc::SIGUSR2;
const TIMEOUT: Duration = Duration::from_secs(15);
const INTERRUPT_INTERVAL: Duration = Duration::from_millis(50);

thread_local! {
    static CANCELLATION: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

/// Associate cancellation with this thread's future read-only I/O scopes.
/// This is not an interruption guard: database writes and stream shutdown may
/// run while the token is installed without becoming signal targets.
pub(crate) struct CancellationScope {
    previous: Option<Arc<AtomicBool>>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl CancellationScope {
    pub(crate) fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            previous: CANCELLATION.with(|current| current.replace(Some(cancelled))),
            _thread_bound: std::marker::PhantomData,
        }
    }
}
impl Drop for CancellationScope {
    fn drop(&mut self) {
        CANCELLATION.with(|current| current.replace(self.previous.take()));
    }
}

static RUNTIME: OnceLock<Result<Arc<Runtime>, i32>> = OnceLock::new();

/// Wake the existing watchdog after setting a cancellation token. Do not
/// reserve a signal merely because an idle source thread is shutting down.
pub(crate) fn notify_cancellation() {
    if let Some(Ok(runtime)) = RUNTIME.get() {
        // Pair with the watchdog's token check and atomic unlock-and-wait.
        // Otherwise cancellation can notify just before the watcher sleeps.
        let _state = runtime.lock();
        runtime.changed.notify_all();
    }
}

struct Active {
    thread: usize,
    generation: u64,
    depth: usize,
    original_mask: libc::sigset_t,
    timeout: Duration,
    deadline: Instant,
    next_signal: Instant,
    expired: bool,
    cancelled: Option<Arc<AtomicBool>>,
}
#[derive(Default)]
struct State {
    generation: u64,
    active: std::collections::BTreeMap<usize, Active>,
}
struct Runtime {
    state: Mutex<State>,
    changed: Condvar,
}
impl Runtime {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn watch(&self) {
        let mut state = self.lock();
        loop {
            if state.active.is_empty() {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
                continue;
            }
            let now = Instant::now();
            for active in state.active.values_mut() {
                if now >= active.next_signal
                    || (!active.expired
                        && active
                            .cancelled
                            .as_ref()
                            .is_some_and(|token| token.load(Ordering::Acquire)))
                {
                    active.expired = true;
                    // Drop and TLS cleanup use this same mutex: a late signal
                    // cannot target a disarmed or reused pthread.
                    unsafe { libc::pthread_kill(active.thread as libc::pthread_t, SIGNAL) };
                    active.next_signal = now + INTERRUPT_INTERVAL;
                }
            }
            let next = state
                .active
                .values()
                .map(|active| active.next_signal)
                .min()
                .unwrap();
            let wait = next.saturating_duration_since(Instant::now());
            (state, _) = self
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

extern "C" fn interrupt_directory_io(_: i32) {
    // Deliberately empty: no errno changes, allocation, locks, or unwinding.
}

fn runtime() -> io::Result<&'static Arc<Runtime>> {
    RUNTIME
        .get_or_init(|| {
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigaction(SIGNAL, std::ptr::null(), &mut previous) } != 0 {
                return Err(io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            if previous.sa_sigaction != libc::SIG_DFL && previous.sa_sigaction != libc::SIG_IGN {
                return Err(libc::EBUSY);
            }
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = interrupt_directory_io as *const () as usize;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            // SA_RESTART must remain unset so interrupted provider calls can return.
            if unsafe { libc::sigaction(SIGNAL, &action, std::ptr::null_mut()) } != 0 {
                return Err(io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            let runtime = Arc::new(Runtime {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            });
            let watcher = Arc::clone(&runtime);
            if let Err(error) = std::thread::Builder::new()
                .name("directory-io".into())
                .stack_size(128 * 1024)
                .spawn(move || watcher.watch())
            {
                unsafe { libc::sigaction(SIGNAL, &previous, std::ptr::null_mut()) };
                return Err(error.raw_os_error().unwrap_or(libc::EAGAIN));
            }
            Ok(runtime)
        })
        .as_ref()
        .map_err(|error| {
            if *error == libc::EBUSY {
                io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    "Cannot reserve SIGUSR2 for directory I/O: another signal handler is installed",
                )
            } else {
                io::Error::from_raw_os_error(*error)
            }
        })
}

fn signal_set() -> libc::sigset_t {
    let mut set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, SIGNAL);
    }
    set
}

struct ThreadOwner(std::cell::Cell<Option<(&'static Arc<Runtime>, u64)>>);
impl Drop for ThreadOwner {
    fn drop(&mut self) {
        if let Some((runtime, generation)) = self.0.take() {
            // Safe Rust can forget a guard. TLS destruction still runs before
            // this pthread exits, so the watchdog never retains a dead target.
            finish(runtime, generation, true);
        }
    }
}
thread_local! {
    static OWNER: ThreadOwner = const {ThreadOwner(std::cell::Cell::new(None))};
}

/// Interrupts read-only directory I/O after a period without progress.
/// Never enclose file-content reads, mutations, or journal/index transactions.
/// Nested guards share the outer deadline; only completed I/O renews it.
///
/// ```compile_fail
/// use jaso_nfc::directory_io::DirectoryIo;
/// fn require_send<T: Send>() {}
/// require_send::<DirectoryIo>();
/// ```
/// ```compile_fail
/// use jaso_nfc::directory_io::DirectoryIo;
/// fn require_sync<T: Sync>() {}
/// require_sync::<DirectoryIo>();
/// ```
#[must_use = "keep the guard alive for the read-only directory operation"]
pub struct DirectoryIo {
    runtime: &'static Arc<Runtime>,
    generation: u64,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl DirectoryIo {
    pub fn begin() -> io::Result<Self> {
        #[cfg(test)]
        let timeout = TEST_TIMEOUT.get().unwrap_or(TIMEOUT);
        #[cfg(not(test))]
        let timeout = TIMEOUT;
        Self::with_timeout(timeout)
    }
    fn with_timeout(timeout: Duration) -> io::Result<Self> {
        let cancelled = CANCELLATION.with(|current| current.borrow().clone());
        if cancelled
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
        {
            return Err(io::Error::from_raw_os_error(libc::ECANCELED));
        }
        let set = signal_set();
        let mut original_mask = unsafe { std::mem::zeroed() };
        let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut original_mask) };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        let runtime = match runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &original_mask, std::ptr::null_mut())
                };
                return Err(error);
            }
        };
        let thread = unsafe { libc::pthread_self() } as usize;
        let mut state = runtime.lock();
        let generation = if let Some(active) = state.active.get_mut(&thread) {
            active.depth += 1;
            active.generation
        } else {
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            let deadline = Instant::now() + timeout;
            state.active.insert(
                thread,
                Active {
                    thread,
                    generation,
                    depth: 1,
                    original_mask,
                    timeout,
                    deadline,
                    next_signal: deadline,
                    expired: false,
                    cancelled,
                },
            );
            generation
        };
        let guard = Self {
            runtime,
            generation,
            _thread_bound: std::marker::PhantomData,
        };
        drop(state);
        if OWNER
            .try_with(|owner| owner.0.set(Some((runtime, generation))))
            .is_err()
        {
            return Err(io::Error::other(
                "Cannot begin directory I/O during thread shutdown",
            ));
        }
        runtime.changed.notify_all();
        let error = unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        guard.check()?;
        Ok(guard)
    }
    pub fn check(&self) -> io::Result<()> {
        self.update(false)
    }
    pub fn progress(&self) -> io::Result<()> {
        self.update(true)
    }
    fn update(&self, progress: bool) -> io::Result<()> {
        let mut state = self.runtime.lock();
        let active = state
            .active
            .get_mut(&(unsafe { libc::pthread_self() } as usize))
            .expect("live thread-bound directory guard");
        let now = Instant::now();
        if active
            .cancelled
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
        {
            active.expired = true;
            self.runtime.changed.notify_all();
            return Err(io::Error::from_raw_os_error(libc::ECANCELED));
        }
        if active.expired || now >= active.deadline {
            active.expired = true;
            self.runtime.changed.notify_all();
            return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
        }
        if progress {
            active.deadline = now + active.timeout;
            active.next_signal = active.deadline;
            // A later deadline needs no wakeup. The watchdog will observe it
            // at its earlier wake time, avoiding a thread wake per entry.
        }
        Ok(())
    }
}
impl Drop for DirectoryIo {
    fn drop(&mut self) {
        if finish(self.runtime, self.generation, false) {
            let _ = OWNER.try_with(|owner| owner.0.set(None));
        }
    }
}
fn finish(runtime: &Runtime, generation: u64, all: bool) -> bool {
    let set = signal_set();
    let mut previous = unsafe { std::mem::zeroed() };
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous) };
    let mut state = runtime.lock();
    let mut removed = false;
    let thread = unsafe { libc::pthread_self() } as usize;
    if let Some(active) = state.active.get_mut(&thread)
        && active.generation == generation
    {
        active.depth = if all { 0 } else { active.depth - 1 };
        if active.depth == 0 {
            previous = active.original_mask;
            state.active.remove(&thread);
            removed = true;
            // The sender holds this mutex during pthread_kill. With
            // this generation removed, all its pending signals are now
            // drainable and no later send can race mask restoration.
            loop {
                let mut pending = unsafe { std::mem::zeroed() };
                if unsafe { libc::sigpending(&mut pending) } != 0
                    || unsafe { libc::sigismember(&pending, SIGNAL) } != 1
                {
                    break;
                }
                let mut signal = 0;
                if unsafe { libc::sigwait(&set, &mut signal) } != 0 {
                    break;
                }
            }
        }
    }
    drop(state);
    runtime.changed.notify_all();
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
    removed
}

#[cfg(test)]
thread_local! { static TEST_TIMEOUT: std::cell::Cell<Option<Duration>> = const {std::cell::Cell::new(None)}; }
#[cfg(test)]
pub(crate) fn test_timeout<T>(timeout: Duration, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_TIMEOUT.set(self.0);
        }
    }
    let _restore = Restore(TEST_TIMEOUT.replace(Some(timeout)));
    run()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::mpsc;
    use std::time::Instant;

    #[test]
    fn metadata_activity_captures_errno_and_resolves_only_a_successful_read() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("loop");
        symlink("loop", &path).unwrap();
        let activity = crate::activity::Activity::new();
        let _binding = activity.bind();
        let error = metadata(&path, true).unwrap_err();
        let failed = activity.snapshot();
        assert_eq!(failed["events"][0]["errno"], libc::ELOOP);
        assert_eq!(failed["events"][0]["reason"], error.to_string());
        activity.failed("observation", Some(path.to_str().unwrap()));
        fs_remove_and_write(&path);
        metadata(&path, true).unwrap();
        let recovered = activity.snapshot();
        assert_eq!(recovered["events"][0]["resolution"], "checked");
        assert!(recovered["events"][1]["resolved_at"].is_null());
    }

    fn fs_remove_and_write(path: &std::path::Path) {
        std::fs::remove_file(path).unwrap();
        std::fs::write(path, b"owned fixture").unwrap();
    }

    #[test]
    fn deleted_file_metadata_is_quiet_completed_absence() {
        let root = tempfile::tempdir().unwrap();
        let activity = crate::activity::Activity::new();
        let _binding = activity.bind();
        for index in 0..32 {
            let path = root.path().join(format!("rustc-{index}.tmp"));
            std::fs::write(&path, b"temporary fixture").unwrap();
            std::fs::remove_file(&path).unwrap();
            for follow in [false, true] {
                assert_eq!(
                    metadata(&path, follow).unwrap_err().kind(),
                    io::ErrorKind::NotFound
                );
            }
        }
        let snapshot = activity.snapshot();
        assert_eq!(snapshot["counters"]["errors"], 0);
        assert!(snapshot["events"].as_array().unwrap().is_empty());
        assert_eq!(snapshot["dropped_events"], 0);
        assert_eq!(snapshot["counters"]["io_completed"], 64);
        assert!(snapshot["last_progress_at"].is_number());
        assert_eq!(snapshot["state"], "processing");
        assert_eq!(
            snapshot["item_path"],
            root.path().join("rustc-31.tmp").to_str().unwrap()
        );
    }

    #[test]
    fn metadata_keeps_permission_and_other_io_failures_visible() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let activity = crate::activity::Activity::new();
        let _binding = activity.bind();
        let looping = root.path().join("loop");
        symlink("loop", &looping).unwrap();
        assert_eq!(
            metadata(&looping, true).unwrap_err().raw_os_error(),
            Some(libc::ELOOP)
        );
        let mut expected = 1;
        // An effective root account bypasses ordinary directory permissions.
        // The real I/O error above remains covered on privileged test hosts.
        if unsafe { libc::geteuid() } != 0 {
            let denied = root.path().join("denied");
            std::fs::create_dir(&denied).unwrap();
            let child = denied.join("file");
            std::fs::write(&child, b"permission fixture").unwrap();
            std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
            let result = metadata(&child, false);
            std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            expected += 1;
        }
        let snapshot = activity.snapshot();
        assert_eq!(snapshot["counters"]["errors"], expected);
        let events = snapshot["events"].as_array().unwrap();
        assert_eq!(events.len(), expected as usize);
        assert!(
            events
                .iter()
                .all(|event| event["kind"] == "error" && event["phase"] == "reading_metadata")
        );
        assert_eq!(snapshot["counters"]["io_completed"], 0);
    }

    /// Model elapsed I/O time without making test success depend on scheduling.
    /// Only the calling thread's active scope is advanced; other tests keep
    /// their own deadlines and signal targets.
    #[cfg(feature = "normalizer")]
    pub(crate) fn elapse_inactivity(elapsed: Duration) {
        let runtime = runtime().unwrap();
        let mut state = runtime.lock();
        let thread = unsafe { libc::pthread_self() } as usize;
        let active = state.active.get_mut(&thread).expect("active fixture I/O");
        active.deadline -= elapsed;
        active.next_signal -= elapsed;
        runtime.changed.notify_all();
    }

    // A fallback writer bounds the fixture even when interruption is broken.
    // The assertion, rather than a hung test process, then reports the failure.
    pub(crate) fn blocking_read() -> (isize, i32) {
        let mut pair = [0; 2];
        assert_eq!(unsafe { libc::pipe(pair.as_mut_ptr()) }, 0);
        let reader = unsafe { OwnedFd::from_raw_fd(pair[0]) };
        let writer = unsafe { OwnedFd::from_raw_fd(pair[1]) };
        let (finished, wait) = mpsc::channel();
        let escape = std::thread::spawn(move || {
            if wait.recv_timeout(Duration::from_millis(400)).is_err() {
                assert_eq!(
                    unsafe { libc::write(writer.as_raw_fd(), b"x".as_ptr().cast(), 1) },
                    1
                );
            }
        });
        let mut byte = 0u8;
        let result = unsafe { libc::read(reader.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        let error = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let _ = finished.send(());
        escape.join().unwrap();
        (result, error)
    }

    #[test]
    fn cancellation_wakes_the_watchdog_between_its_token_check_and_wait() {
        const CHILD: &str = "JASO_TEST_CANCELLATION_WAKE_RACE";
        if std::env::var_os(CHILD).is_none() {
            // Isolate the global watchdog from unrelated test notifications.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "directory_io::tests::cancellation_wakes_the_watchdog_between_its_token_check_and_wait"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        let runtime = runtime().unwrap();
        // Stand at the watchdog's check-to-wait boundary with its real mutex.
        let state = runtime.lock();
        let (started, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let notifier = std::thread::spawn(move || {
            started.send(()).unwrap();
            notify_cancellation();
            finished.send(()).unwrap();
        });
        ready.recv().unwrap();
        let _ = done.recv_timeout(Duration::from_millis(100));
        let (state, waited) = runtime
            .changed
            .wait_timeout(state, Duration::from_millis(200))
            .unwrap();
        drop(state);
        notifier.join().unwrap();
        assert!(
            !waited.timed_out(),
            "cancellation notification was lost before the watchdog began waiting"
        );
    }

    #[test]
    fn cancellation_prevents_later_probes_and_restores_the_callers_scope() {
        let cancelled = Arc::new(AtomicBool::new(true));
        {
            let _scope = CancellationScope::new(cancelled);
            let error = match DirectoryIo::begin() {
                Ok(_) => panic!("cancelled source must not start another metadata probe"),
                Err(error) => error,
            };
            assert_eq!(error.raw_os_error(), Some(libc::ECANCELED));
        }
        DirectoryIo::begin().unwrap().check().unwrap();
    }

    #[test]
    fn separate_threads_have_independent_directory_io_deadlines() {
        let first = DirectoryIo::with_timeout(Duration::from_secs(2)).unwrap();
        let (sent, received) = mpsc::channel();
        let other = std::thread::spawn(move || {
            let guard = DirectoryIo::with_timeout(Duration::from_millis(40)).unwrap();
            sent.send(()).unwrap();
            assert_eq!(blocking_read(), (-1, libc::EINTR));
            assert_eq!(
                guard.check().unwrap_err().raw_os_error(),
                Some(libc::ETIMEDOUT)
            );
        });
        let independent = received.recv_timeout(Duration::from_millis(150)).is_ok();
        first.check().unwrap();
        drop(first);
        other.join().unwrap();
        assert!(
            independent,
            "a discovery deadline must not occupy the foreground I/O slot"
        );
    }

    #[test]
    fn stalled_read_is_interrupted_and_reports_a_timeout() {
        let guard = DirectoryIo::with_timeout(Duration::from_millis(30)).unwrap();
        assert_eq!(blocking_read(), (-1, libc::EINTR));
        assert_eq!(
            guard.progress().unwrap_err().raw_os_error(),
            Some(libc::ETIMEDOUT)
        );
    }

    #[test]
    fn nested_scope_cannot_extend_or_clear_an_expired_deadline() {
        let outer = DirectoryIo::with_timeout(Duration::from_millis(30)).unwrap();
        {
            let inner = DirectoryIo::with_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(blocking_read(), (-1, libc::EINTR));
            assert_eq!(
                inner.progress().unwrap_err().raw_os_error(),
                Some(libc::ETIMEDOUT)
            );
        }
        assert_eq!(
            outer.progress().unwrap_err().raw_os_error(),
            Some(libc::ETIMEDOUT)
        );
        drop(outer);
        DirectoryIo::with_timeout(Duration::from_secs(1))
            .unwrap()
            .progress()
            .unwrap();
    }

    #[test]
    fn progress_renews_the_inactivity_deadline_after_each_operation() {
        // Inspect renewal directly: sleeping under a short deadline assumes
        // the test thread will be scheduled again before that deadline.
        let guard = DirectoryIo::begin().unwrap();
        let thread = unsafe { libc::pthread_self() } as usize;
        for _ in 0..5 {
            let before = Instant::now();
            guard.progress().unwrap();
            let after = Instant::now();
            let deadline = {
                let state = guard.runtime.lock();
                let active = &state.active[&thread];
                assert!(active.deadline >= before + active.timeout);
                assert!(active.deadline <= after + active.timeout);
                assert_eq!(active.next_signal, active.deadline);
                active.deadline
            };
            guard.check().unwrap();
            assert_eq!(guard.runtime.lock().active[&thread].deadline, deadline);
        }
    }

    #[test]
    fn dropping_a_scope_does_not_interrupt_later_unrelated_io() {
        drop(DirectoryIo::with_timeout(Duration::from_millis(20)).unwrap());
        assert_eq!(blocking_read().0, 1);
    }

    #[test]
    fn final_nested_drop_restores_the_mask_and_drains_pending_signals_even_on_unwind() {
        let set = signal_set();
        let mut original = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut original) },
            0
        );
        struct Restore(libc::sigset_t);
        impl Drop for Restore {
            fn drop(&mut self) {
                unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
            }
        }
        let _restore = Restore(original);
        let panic = std::panic::catch_unwind(|| {
            let outer = DirectoryIo::with_timeout(Duration::from_secs(2)).unwrap();
            let inner = DirectoryIo::with_timeout(Duration::from_secs(2)).unwrap();
            // Even explicitly dropping the outer object first keeps the shared
            // scope armed until the last guard leaves its originating thread.
            drop(outer);
            inner.progress().unwrap();
            assert_eq!(
                unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) },
                0
            );
            assert_eq!(
                unsafe { libc::pthread_kill(libc::pthread_self(), SIGNAL) },
                0
            );
            panic!("fixture unwind with a pending directory signal");
        });
        assert!(panic.is_err());
        let mut current = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut current) },
            0
        );
        assert_eq!(unsafe { libc::sigismember(&current, SIGNAL) }, 1);
        let mut pending = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::sigpending(&mut pending) }, 0);
        assert_eq!(unsafe { libc::sigismember(&pending, SIGNAL) }, 0);
        DirectoryIo::begin().unwrap().progress().unwrap();
    }

    #[test]
    fn existing_signal_handler_is_not_replaced() {
        const CHILD: &str = "JASO_TEST_DIRECTORY_SIGNAL_CONFLICT";
        if std::env::var_os(CHILD).is_some() {
            extern "C" fn existing(_: i32) {}
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = existing as *const () as usize;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            assert_eq!(
                unsafe { libc::sigaction(SIGNAL, &action, std::ptr::null_mut()) },
                0
            );
            let error = DirectoryIo::begin()
                .err()
                .expect("custom handler must be protected");
            assert!(error.to_string().contains("another signal handler"));
            let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::sigaction(SIGNAL, std::ptr::null(), &mut current) },
                0
            );
            assert_eq!(current.sa_sigaction, action.sa_sigaction);
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "directory_io::tests::existing_signal_handler_is_not_replaced",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn thread_exit_disarms_a_forgotten_guard_before_its_pthread_can_be_reused() {
        const CHILD: &str = "JASO_TEST_DIRECTORY_LEAKED_GUARD";
        if std::env::var_os(CHILD).is_some() {
            std::thread::spawn(|| {
                let guard = DirectoryIo::with_timeout(Duration::from_secs(60)).unwrap();
                std::mem::forget(guard);
            })
            .join()
            .unwrap();
            assert!(
                runtime().unwrap().lock().active.is_empty(),
                "exited pthread remains registered with the watchdog"
            );
            let (ready, wait) = mpsc::channel();
            std::thread::spawn(move || {
                let guard = DirectoryIo::begin().unwrap();
                guard.progress().unwrap();
                ready.send(()).unwrap();
            });
            assert!(
                wait.recv_timeout(Duration::from_secs(1)).is_ok(),
                "exited thread retained the directory I/O slot"
            );
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "directory_io::tests::thread_exit_disarms_a_forgotten_guard_before_its_pthread_can_be_reused"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
