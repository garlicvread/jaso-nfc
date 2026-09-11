//! Descriptor-based stored spelling and private operation-marker ownership.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use unicode_normalization::UnicodeNormalization;

pub use crate::directory_io::DirectoryMaterialization;
#[cfg(all(test, target_os = "macos"))]
use crate::directory_io::materialization_policy;

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    static TEST_OPEN_MATERIALIZATION_POLICY: std::cell::Cell<i32> = const { std::cell::Cell::new(-1) };
    static TEST_METADATA_STALL: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    static TEST_METADATA_FD: std::cell::Cell<RawFd> = const { std::cell::Cell::new(-1) };
    static TEST_METADATA_INTERRUPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, target_os = "macos"))]
fn test_metadata_checkpoint(stage: &'static str, fd: RawFd) {
    if TEST_METADATA_STALL.get() == Some(stage) {
        TEST_METADATA_STALL.set(None);
        TEST_METADATA_FD.set(fd);
        TEST_METADATA_INTERRUPTED
            .set(crate::directory_io::tests::blocking_read() == (-1, libc::EINTR));
    }
}

pub fn cstring(value: &str) -> io::Result<CString> {
    CString::new(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in filesystem name"))
}
pub fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a single directory entry name",
        ));
    }
    cstring(name).map(|_| ())
}
pub fn open_at(parent: RawFd, name: &str, flags: i32) -> io::Result<File> {
    let name = cstring(name)?;
    let deadline = if flags & libc::O_DIRECTORY != 0 {
        Some(crate::directory_io::DirectoryIo::begin()?)
    } else {
        None
    };
    let _materialization = if flags & libc::O_DIRECTORY != 0 {
        Some(DirectoryMaterialization::begin()?)
    } else {
        None
    };
    #[cfg(all(test, target_os = "macos"))]
    TEST_OPEN_MATERIALIZATION_POLICY.set(unsafe {
        materialization_policy::getiopolicy_np(
            materialization_policy::TYPE,
            materialization_policy::THREAD,
        )
    });
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags | libc::O_CLOEXEC) };
    let result = if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    };
    #[cfg(all(test, target_os = "macos"))]
    test_metadata_checkpoint("open", fd);
    if let Some(deadline) = deadline {
        deadline.progress()?;
    }
    result
}
pub fn open_entry(parent: RawFd, name: &str) -> io::Result<File> {
    validate_name(name)?;
    // O_EVTONLY entry opens read metadata without requesting file contents.
    // Keep the deadline local so later marker/rename/fsync mutations are outside it.
    let deadline = crate::directory_io::DirectoryIo::begin()?;
    #[cfg(target_os = "macos")]
    let flags = libc::O_EVTONLY | libc::O_SYMLINK;
    #[cfg(not(target_os = "macos"))]
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW;
    let first = open_at(parent, name, flags);
    deadline.progress()?;
    #[cfg(target_os = "macos")]
    if first
        .as_ref()
        .is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
    {
        let normalized: String = name.nfc().collect();
        if normalized != name {
            let fallback = open_at(parent, &normalized, flags);
            deadline.progress()?;
            return fallback;
        }
    }
    first
}
pub fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    let mut info = std::mem::MaybeUninit::uninit();
    if unsafe { libc::fstat(fd, info.as_mut_ptr()) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { info.assume_init() })
    }
}
pub fn stat_at(parent: RawFd, name: &str) -> io::Result<Option<libc::stat>> {
    let name = cstring(name)?;
    let mut info = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            info.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } < 0
    {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        }
    } else {
        Ok(Some(unsafe { info.assume_init() }))
    }
}
// Fault-injection state is thread local and exists only in fixture test builds.
#[cfg(test)]
thread_local! {static TEST_IDENTITY_SHIFTS:std::cell::RefCell<std::collections::HashMap<[u64;2],u64>>=std::cell::RefCell::new(std::collections::HashMap::new());}
#[cfg(test)]
pub(crate) fn test_adjust_identity(id: [u64; 2]) -> [u64; 2] {
    TEST_IDENTITY_SHIFTS.with(|shifts| {
        [
            id[0],
            id[1] + shifts.borrow().get(&id).copied().unwrap_or(0),
        ]
    })
}
#[cfg(test)]
pub(crate) fn test_shift_identity(id: [u64; 2]) {
    TEST_IDENTITY_SHIFTS
        .with(|shifts| *shifts.borrow_mut().entry(id).or_insert(0) += 1_000_000_000_000);
}
#[cfg(test)]
pub(crate) fn test_clear_identities() {
    TEST_IDENTITY_SHIFTS.with(|shifts| shifts.borrow_mut().clear());
}
pub fn identity(info: &libc::stat) -> [u64; 2] {
    let id = [info.st_dev as u64, info.st_ino];
    #[cfg(test)]
    {
        test_adjust_identity(id)
    }
    #[cfg(not(test))]
    {
        id
    }
}
pub fn actual_stored_name(parent: RawFd, name: &str) -> io::Result<String> {
    validate_name(name)?;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = parent;
        Ok(name.to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let deadline = crate::directory_io::DirectoryIo::begin()?;
        let held = open_entry(parent, name)?;
        let mut buffer = [0i8; 1024];
        let resolved =
            if unsafe { libc::fcntl(held.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
        #[cfg(test)]
        test_metadata_checkpoint("getpath", held.as_raw_fd());
        deadline.progress()?;
        resolved?;
        // A zero-filled fixed buffer and a successful F_GETPATH ensure termination.
        let bytes = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes();
        let path =
            std::str::from_utf8(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
        let actual = path.rsplit('/').next().unwrap_or_default();
        validate_name(actual)?;
        let mapped = stat_at(parent, actual);
        #[cfg(test)]
        test_metadata_checkpoint("stat_at", held.as_raw_fd());
        deadline.progress()?;
        let mapped = mapped?.ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        let info = fstat(held.as_raw_fd());
        #[cfg(test)]
        test_metadata_checkpoint("fstat", held.as_raw_fd());
        deadline.progress()?;
        if identity(&info?) != identity(&mapped) {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        Ok(actual.to_owned())
    }
}
/// Resolve one candidate spelling and bind it to the expected inode without
/// enumerating siblings. Unrelated hard-link aliases are never accepted.
pub fn candidate_stored_name(
    parent: RawFd,
    name: &str,
    expected: [u64; 2],
) -> io::Result<Option<String>> {
    let deadline = crate::directory_io::DirectoryIo::begin()?;
    let actual = match actual_stored_name(parent, name) {
        Ok(actual) => actual,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    deadline.progress()?;
    if actual.nfc().ne(name.nfc()) {
        return Ok(None);
    }
    let info = stat_at(parent, &actual);
    deadline.progress()?;
    Ok(info?
        .filter(|info| identity(info) == expected)
        .map(|_| actual))
}
pub fn marker_get(fd: RawFd, key: &str) -> io::Result<Option<Vec<u8>>> {
    let key = cstring(key)?;
    #[cfg(target_os = "macos")]
    let size = unsafe { libc::fgetxattr(fd, key.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
    #[cfg(not(target_os = "macos"))]
    let size = unsafe { libc::fgetxattr(fd, key.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        let error = io::Error::last_os_error();
        #[cfg(target_os = "macos")]
        let absent = error.raw_os_error() == Some(libc::ENOATTR);
        #[cfg(not(target_os = "macos"))]
        let absent = error.raw_os_error() == Some(libc::ENODATA);
        return if absent { Ok(None) } else { Err(error) };
    }
    if size > 4096 {
        return Err(io::Error::from_raw_os_error(libc::E2BIG));
    }
    let mut buffer = vec![0u8; usize::max(size as usize, 1)];
    #[cfg(target_os = "macos")]
    let count = unsafe {
        libc::fgetxattr(
            fd,
            key.as_ptr(),
            buffer.as_mut_ptr().cast(),
            size as usize,
            0,
            0,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let count =
        unsafe { libc::fgetxattr(fd, key.as_ptr(), buffer.as_mut_ptr().cast(), size as usize) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(count as usize);
    Ok(Some(buffer))
}
pub fn marker_create(fd: RawFd, key: &str, token: &[u8]) -> io::Result<libc::stat> {
    if token.is_empty() || token.len() > 4096 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Operation marker must contain 1..4096 bytes",
        ));
    }
    let key = cstring(key)?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::fsetxattr(
            fd,
            key.as_ptr(),
            token.as_ptr().cast(),
            token.len(),
            0,
            libc::XATTR_CREATE,
        )
    };
    #[cfg(not(target_os = "macos"))]
    let result = unsafe {
        libc::fsetxattr(
            fd,
            key.as_ptr(),
            token.as_ptr().cast(),
            token.len(),
            libc::XATTR_CREATE,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    fstat(fd)
}
pub fn marker_remove(fd: RawFd, key: &str, token: &[u8]) -> io::Result<libc::stat> {
    if let Some(current) = marker_get(fd, key)? {
        if current != token {
            return Err(io::Error::from_raw_os_error(libc::ESTALE));
        }
        let key = cstring(key)?;
        #[cfg(target_os = "macos")]
        let result = unsafe { libc::fremovexattr(fd, key.as_ptr(), 0) };
        #[cfg(not(target_os = "macos"))]
        let result = unsafe { libc::fremovexattr(fd, key.as_ptr()) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    fstat(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    fn metadata_timeout(stage: &'static str, stored_name: bool) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        std::fs::write(&path, b"fixture").unwrap();
        let parent = File::open(temp.path()).unwrap();
        let original = stat_at(parent.as_raw_fd(), "file").unwrap().unwrap();
        TEST_METADATA_STALL.set(Some(stage));
        TEST_METADATA_INTERRUPTED.set(false);
        let result =
            crate::directory_io::test_timeout(std::time::Duration::from_millis(30), || {
                if stored_name {
                    actual_stored_name(parent.as_raw_fd(), "file").map(drop)
                } else {
                    open_entry(parent.as_raw_fd(), "file").map(drop)
                }
            });
        assert_eq!(
            result.unwrap_err().raw_os_error(),
            Some(libc::ETIMEDOUT),
            "{stage}"
        );
        assert!(
            TEST_METADATA_INTERRUPTED.get(),
            "{stage} was outside the deadline"
        );
        assert_eq!(TEST_METADATA_STALL.get(), None);
        // Parallel tests may reuse a closed descriptor. It must no longer hold
        // this fixture's otherwise unopened file, even if its number was reused.
        if let Ok(info) = fstat(TEST_METADATA_FD.get()) {
            assert_ne!(
                identity(&info),
                identity(&original),
                "expired operation leaked its held descriptor"
            );
        }
        assert_eq!(
            actual_stored_name(parent.as_raw_fd(), "file").unwrap(),
            "file"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn entry_metadata_open_times_out_and_closes_an_apparently_successful_descriptor() {
        metadata_timeout("open", false);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stored_spelling_discards_partial_success_after_each_metadata_deadline() {
        for stage in ["open", "getpath", "stat_at", "fstat"] {
            metadata_timeout(stage, true);
        }
    }

    #[cfg(target_os = "macos")]
    mod materialization {
        use super::*;
        use materialization_policy::{ON, THREAD, TYPE, getiopolicy_np, setiopolicy_np};

        const PROCESS: i32 = 0;
        const OFF: i32 = 1;

        fn policy(scope: i32) -> i32 {
            let value = unsafe { getiopolicy_np(TYPE, scope) };
            assert!(value >= 0, "{}", io::Error::last_os_error());
            value
        }

        struct RestoreThreadPolicy(i32);
        impl Drop for RestoreThreadPolicy {
            fn drop(&mut self) {
                unsafe { setiopolicy_np(TYPE, THREAD, self.0) };
            }
        }

        fn with_materialization_off(body: impl FnOnce()) {
            let _restore = RestoreThreadPolicy(policy(THREAD));
            assert_eq!(unsafe { setiopolicy_np(TYPE, THREAD, OFF) }, 0);
            body();
        }

        #[test]
        fn directory_open_enables_materialization_only_during_open() {
            let temp = tempfile::tempdir().unwrap();
            with_materialization_off(|| {
                let process = policy(PROCESS);
                let directory = open_at(
                    libc::AT_FDCWD,
                    temp.path().to_str().unwrap(),
                    libc::O_RDONLY | libc::O_DIRECTORY,
                )
                .unwrap();
                assert!(directory.metadata().unwrap().is_dir());
                assert_eq!(TEST_OPEN_MATERIALIZATION_POLICY.get(), ON);
                assert_eq!(policy(THREAD), OFF);
                assert_eq!(policy(PROCESS), process);

                let missing = temp.path().join("missing");
                assert_eq!(
                    open_at(
                        libc::AT_FDCWD,
                        missing.to_str().unwrap(),
                        libc::O_RDONLY | libc::O_DIRECTORY
                    )
                    .unwrap_err()
                    .kind(),
                    io::ErrorKind::NotFound
                );
                assert_eq!(TEST_OPEN_MATERIALIZATION_POLICY.get(), ON);
                assert_eq!(policy(THREAD), OFF);
                assert_eq!(policy(PROCESS), process);
            });
        }

        #[test]
        fn regular_file_open_preserves_materialization_policy() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("file");
            std::fs::write(&path, b"fixture").unwrap();
            with_materialization_off(|| {
                let process = policy(PROCESS);
                let file = open_at(libc::AT_FDCWD, path.to_str().unwrap(), libc::O_RDONLY).unwrap();
                assert!(file.metadata().unwrap().is_file());
                assert_eq!(TEST_OPEN_MATERIALIZATION_POLICY.get(), OFF);
                assert_eq!(policy(THREAD), OFF);
                assert_eq!(policy(PROCESS), process);

                let parent = File::open(temp.path()).unwrap();
                let held = open_entry(parent.as_raw_fd(), "file").unwrap();
                assert!(held.metadata().unwrap().is_file());
                assert_eq!(TEST_OPEN_MATERIALIZATION_POLICY.get(), OFF);
                assert_eq!(
                    actual_stored_name(parent.as_raw_fd(), "file").unwrap(),
                    "file"
                );
                assert_eq!(policy(THREAD), OFF);
                assert_eq!(policy(PROCESS), process);
            });
        }

        #[test]
        fn materialization_guard_restores_policy_after_success_and_nesting() {
            with_materialization_off(|| {
                let process = policy(PROCESS);
                {
                    let _outer = DirectoryMaterialization::begin().unwrap();
                    assert_eq!(policy(THREAD), ON);
                    {
                        let _inner = DirectoryMaterialization::begin().unwrap();
                        assert_eq!(policy(THREAD), ON);
                    }
                    assert_eq!(policy(THREAD), ON);
                    assert_eq!(policy(PROCESS), process);
                }
                assert_eq!(policy(THREAD), OFF);
                assert_eq!(policy(PROCESS), process);
            });
        }

        #[test]
        fn materialization_guard_restores_policy_after_error_and_unwind() {
            with_materialization_off(|| {
                let process = policy(PROCESS);
                let failed = (|| -> io::Result<()> {
                    let _guard = DirectoryMaterialization::begin()?;
                    assert_eq!(policy(THREAD), ON);
                    Err(io::Error::from_raw_os_error(libc::EIO))
                })();
                assert_eq!(failed.unwrap_err().raw_os_error(), Some(libc::EIO));
                assert_eq!(policy(THREAD), OFF);
                let unwound = std::panic::catch_unwind(|| {
                    let _guard = DirectoryMaterialization::begin().unwrap();
                    assert_eq!(policy(THREAD), ON);
                    panic!("fixture unwind");
                });
                assert!(unwound.is_err());
                assert_eq!(policy(THREAD), OFF);
                assert_eq!(policy(PROCESS), process);
            });
        }
    }
    #[test]
    fn entry_names_never_traverse_parent() {
        for name in ["", ".", "..", "/absolute", "child/file"] {
            assert!(actual_stored_name(-1, name).is_err());
        }
    }
    #[test]
    fn marker_is_owned_and_cleanup_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        std::fs::write(&path, b"").unwrap();
        let file = std::fs::File::open(path).unwrap();
        let fd = file.as_raw_fd();
        let key = "user.jaso_nfc.fixture";
        assert!(marker_get(fd, key).unwrap().is_none());
        marker_create(fd, key, b"owner").unwrap();
        assert!(marker_create(fd, key, b"other").is_err());
        assert!(marker_remove(fd, key, b"other").is_err());
        assert_eq!(marker_get(fd, key).unwrap().unwrap(), b"owner");
        marker_remove(fd, key, b"owner").unwrap();
        marker_remove(fd, key, b"owner").unwrap();
    }
}
