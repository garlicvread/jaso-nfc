//! Policy-aware scans and durable, identity-checked filename operations.
//! Pending uncertainty always stops mutation and index advancement.
use crate::journal::{
    Journal, JournalLocks, RetryState, atomic_json, journal_records, path_signature, suffix,
    sync_directory, sync_file, visit_records,
};
use crate::model::{Entry, PendingRecoveryError, Reconciler, ScanError, ScanResult};
use crate::native_names::{
    DirectoryMaterialization, actual_stored_name, cstring, fstat, identity, marker_create,
    marker_get, marker_remove, open_at, open_entry, stat_at, validate_name,
};
use crate::policy::{Policy, TMP_SUFFIX, absolute, nfc};
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::CStr;
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const STEP_ENTRIES: usize = 128;
const STEP_TIME: Duration = Duration::from_millis(75);
const RETAINED_SCANS: usize = 8;

/// An independently positioned read-only directory stream, owned by one worker.
struct DirectoryStream(*mut libc::DIR);
impl Drop for DirectoryStream {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.0) };
    }
}
impl DirectoryStream {
    fn open(fd: RawFd) -> Result<Self> {
        let deadline = crate::directory_io::DirectoryIo::begin()?;
        let _materialization = DirectoryMaterialization::begin()?;
        let raw = open_at(fd, ".", libc::O_RDONLY | libc::O_DIRECTORY)?.into_raw_fd();
        let pointer = unsafe { libc::fdopendir(raw) };
        if pointer.is_null() {
            let error = io::Error::last_os_error();
            unsafe { libc::close(raw) };
            deadline.check()?;
            return Err(error.into());
        }
        let stream = Self(pointer);
        deadline.progress()?;
        Ok(stream)
    }

    fn batch(&mut self, fd: RawFd) -> Result<(Vec<(String, libc::stat)>, bool)> {
        let started = Instant::now();
        let deadline = crate::directory_io::DirectoryIo::begin()?;
        let _materialization = DirectoryMaterialization::begin()?;
        let mut children = Vec::new();
        let mut visited = 0;
        loop {
            if visited >= STEP_ENTRIES || (visited > 0 && started.elapsed() >= STEP_TIME) {
                return Ok((children, false));
            }
            deadline.check()?;
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0
            };
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0
            };
            let next = unsafe { libc::readdir(self.0) };
            let error = io::Error::last_os_error();
            deadline.progress()?;
            if next.is_null() {
                if error.raw_os_error() != Some(0) {
                    return Err(error.into());
                }
                return Ok((children, true));
            }
            let bytes = unsafe { CStr::from_ptr((*next).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            visited += 1;
            let name = std::str::from_utf8(bytes)
                .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
            let actual = if nfc(name) != name {
                match actual_stored_name(fd, name) {
                    Ok(name) => name,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
            } else {
                name.to_owned()
            };
            deadline.progress()?;
            let info = stat_at(fd, &actual);
            deadline.progress()?;
            if let Some(info) = info? {
                children.push((actual, info));
                #[cfg(test)]
                TEST_LISTING_PROGRESS.with(|hook| {
                    if let Some(hook) = hook.borrow_mut().as_mut() {
                        hook();
                    }
                });
                deadline.check()?;
            }
        }
    }
}

/// SQLite's private temporary database is deleted when the snapshot closes.
/// Only a small row batch and a bounded page cache remain resident in memory.
struct DirectoryScan {
    id: String,
    directory: File,
    listing: Option<DirectoryStream>,
    spool: rusqlite::Connection,
    position: i64,
}
impl DirectoryScan {
    fn new(directory: File) -> Result<Self> {
        let listing = DirectoryStream::open(directory.as_raw_fd())?;
        let spool = rusqlite::Connection::open("")?;
        spool.execute_batch(
            "PRAGMA journal_mode=OFF; PRAGMA cache_size=-64; PRAGMA temp_store=FILE;
            CREATE TABLE children(position INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
            dev TEXT NOT NULL, ino TEXT NOT NULL)",
        )?;
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            directory,
            listing: Some(listing),
            spool,
            position: 0,
        })
    }
}

fn stale() -> anyhow::Error {
    io::Error::from_raw_os_error(libc::ESTALE).into()
}
fn field<'a>(operation: &'a Value, key: &str) -> Result<&'a str> {
    operation[key]
        .as_str()
        .ok_or_else(|| anyhow!("invalid pending operation field {key}"))
}
fn op_identity(operation: &Value) -> Result<[u64; 2]> {
    let array = operation["identity"]
        .as_array()
        .ok_or_else(|| anyhow!("invalid identity"))?;
    if array.len() != 2 {
        bail!("invalid identity");
    }
    Ok([
        array[0].as_u64().ok_or_else(|| anyhow!("invalid device"))?,
        array[1].as_u64().ok_or_else(|| anyhow!("invalid inode"))?,
    ])
}
fn temporary_name(operation: &Value) -> Result<String> {
    Ok(Path::new(field(operation, "temporary_path")?)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("invalid temporary path"))?
        .into())
}
fn remove_field(operation: &mut Value, key: &str) -> Value {
    operation
        .as_object_mut()
        .and_then(|o| o.remove(key))
        .unwrap_or(Value::Null)
}
fn sync_fd(fd: RawFd) -> io::Result<()> {
    if unsafe { libc::fsync(fd) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn timestamp() -> String {
    let time = unsafe { libc::time(std::ptr::null_mut()) };
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    let mut output = [0i8; 64];
    if unsafe { libc::localtime_r(&time, local.as_mut_ptr()) }.is_null() {
        return String::new();
    }
    unsafe {
        libc::strftime(
            output.as_mut_ptr(),
            output.len(),
            c"%Y-%m-%dT%H:%M:%S".as_ptr(),
            local.as_ptr(),
        );
        CStr::from_ptr(output.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}
fn entry(path: String, info: &libc::stat) -> Entry {
    let mode = info.st_mode as u32;
    let kind = match mode & libc::S_IFMT as u32 {
        x if x == libc::S_IFLNK as u32 => "symlink",
        x if x == libc::S_IFDIR as u32 => "dir",
        _ => "file",
    };
    Entry {
        path,
        kind: kind.into(),
        dev: info.st_dev as u64,
        ino: info.st_ino,
        mtime_ns: info
            .st_mtime
            .saturating_mul(1_000_000_000)
            .saturating_add(info.st_mtime_nsec),
        ctime_ns: info
            .st_ctime
            .saturating_mul(1_000_000_000)
            .saturating_add(info.st_ctime_nsec),
        size: info.st_size as u64,
        mode,
    }
}
fn list_directory(fd: RawFd) -> Result<Vec<(String, libc::stat)>> {
    list_directory_filtered(fd, None)
}
fn list_directory_filtered(
    fd: RawFd,
    candidates: Option<&[&str]>,
) -> Result<Vec<(String, libc::stat)>> {
    let deadline = crate::directory_io::DirectoryIo::begin()?;
    // readdir may need the provider to fetch a folder's child metadata. This
    // scope never reads regular-file contents and restores the caller's policy.
    let _materialization = DirectoryMaterialization::begin()?;
    // An independent open file description avoids changing the caller's offset.
    let listing = open_at(fd, ".", libc::O_RDONLY | libc::O_DIRECTORY)?;
    let raw = listing.into_raw_fd();
    let pointer = unsafe { libc::fdopendir(raw) };
    if pointer.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(raw);
        };
        deadline.check()?;
        return Err(error.into());
    }
    struct Dir(*mut libc::DIR);
    impl Drop for Dir {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let directory = Dir(pointer);
    // fdopendir may return a non-null stream even when its eager first read
    // was interrupted. Expiration must win over success and apparent EOF.
    deadline.progress()?;
    let mut values = Vec::new();
    loop {
        deadline.check()?;
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let next = unsafe { libc::readdir(directory.0) };
        let error = io::Error::last_os_error();
        deadline.progress()?;
        if next.is_null() {
            if error.raw_os_error() != Some(0) {
                return Err(error.into());
            }
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*next).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name =
            std::str::from_utf8(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))?;
        // Verification must not stat every unrelated sibling for each rename.
        if candidates
            .is_some_and(|names| !names.iter().any(|candidate| nfc(candidate) == nfc(name)))
        {
            continue;
        }
        let actual = if nfc(name) != name {
            let stored = actual_stored_name(fd, name);
            deadline.progress()?;
            match stored {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            }
        } else {
            name.into()
        };
        let info = stat_at(fd, &actual);
        deadline.progress()?;
        if let Some(info) = info? {
            values.push((actual, info));
            #[cfg(test)]
            TEST_LISTING_PROGRESS.with(|hook| {
                if let Some(hook) = hook.borrow_mut().as_mut() {
                    hook();
                }
            });
            deadline.check()?;
        }
    }
    Ok(values)
}
#[cfg(test)]
thread_local! {static TEST_DECOMPOSE_STORED:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
#[cfg(test)]
thread_local! {
    static TEST_LISTING_PROGRESS: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const {std::cell::RefCell::new(None)};
}
fn stored_name(fd: RawFd, id: [u64; 2], candidates: &[&str]) -> Result<Option<String>> {
    let adjust = |name: String| {
        #[cfg(test)]
        let name = TEST_DECOMPOSE_STORED.with(|decompose| {
            use unicode_normalization::UnicodeNormalization;
            if decompose.get() {
                name.nfd().collect()
            } else {
                name
            }
        });
        name
    };
    for candidate in candidates {
        if let Some(name) = crate::native_names::candidate_stored_name(fd, candidate, id)? {
            return Ok(Some(adjust(name)));
        }
    }
    // Rare ambiguous/vanished candidate paths retain the filtered, identity-
    // checked fallback used by recovery. The common rename path stays O(1).
    for (name, info) in list_directory_filtered(fd, Some(candidates))? {
        if identity(&info) == id {
            return Ok(Some(adjust(name)));
        }
    }
    Ok(None)
}
pub fn rename_exclusive(source: &str, destination: &str, fd: RawFd) -> io::Result<()> {
    let source = cstring(source)?;
    let destination = cstring(destination)?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            fd,
            source.as_ptr(),
            fd,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            fd,
            source.as_ptr(),
            fd,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
type RecoveryHook = Box<dyn FnMut(&str, &Value) -> Result<()>>;

pub struct Normalizer {
    pub policy: Policy,
    pub log_path: Option<PathBuf>,
    pub retry_path: Option<PathBuf>,
    pub pending_path: Option<PathBuf>,
    pub apply: bool,
    pub retry: RetryState,
    journal: Option<Journal>,
    scans: HashMap<String, DirectoryScan>,
    #[cfg(test)]
    exclusive_error: Option<i32>,
    #[cfg(test)]
    hook: Option<RecoveryHook>,
}
impl Normalizer {
    pub const RETRY_BASE: f64 = 900.;
    pub const RETRY_MAX: f64 = 86400.;
    pub fn new(
        policy: Policy,
        log: Option<PathBuf>,
        retry: Option<PathBuf>,
        pending: Option<PathBuf>,
        apply: bool,
    ) -> Result<Self> {
        if apply && (log.is_none() || pending.is_none()) {
            bail!("applying normalization requires journal and pending paths");
        }
        let retry_state = RetryState::new(retry.clone(), Self::RETRY_BASE, Self::RETRY_MAX)?;
        Ok(Self {
            policy,
            log_path: log,
            retry_path: retry,
            pending_path: pending,
            apply,
            retry: retry_state,
            journal: None,
            scans: HashMap::new(),
            #[cfg(test)]
            exclusive_error: None,
            #[cfg(test)]
            hook: None,
        })
    }
    pub fn close(&mut self) {
        self.journal = None;
    }
    fn checkpoint(&mut self, _point: &str, _operation: &Value) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = &mut self.hook {
            hook(_point, _operation)?;
        }
        Ok(())
    }
    fn prepare_state(&self) -> Result<()> {
        for path in [&self.log_path, &self.retry_path, &self.pending_path]
            .into_iter()
            .flatten()
        {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        Ok(())
    }
    fn open_directory(&self, path: &str) -> Result<File> {
        let root = self
            .policy
            .root_for(path)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EACCES))?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let mut descriptor = open_at(libc::AT_FDCWD, &root, flags)?;
        for component in Path::new(path)
            .components()
            .skip(Path::new(&root).components().count())
        {
            let part = component
                .as_os_str()
                .to_str()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EILSEQ))?;
            validate_name(part)?;
            descriptor = open_at(descriptor.as_raw_fd(), part, flags)?;
        }
        Ok(descriptor)
    }
    fn actual_scope(&self, path: &str) -> Result<String> {
        let Some(root) = self.policy.root_for(path) else {
            return Ok(path.into());
        };
        if root == path {
            return Ok(path.into());
        }
        let mut descriptor = self.open_directory(&root)?;
        let mut actual = PathBuf::from(&root);
        for component in Path::new(path)
            .components()
            .skip(Path::new(&root).components().count())
        {
            let name = component
                .as_os_str()
                .to_str()
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EILSEQ))?;
            let spelling = match actual_stored_name(descriptor.as_raw_fd(), name) {
                Ok(name) => name,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(path.into()),
                Err(e) => return Err(e.into()),
            };
            actual.push(&spelling);
            descriptor = open_at(
                descriptor.as_raw_fd(),
                &spelling,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )?;
        }
        Ok(actual.to_string_lossy().into_owned())
    }
    fn pending_write(&mut self, operation: &Value) -> Result<()> {
        let path = self
            .pending_path
            .as_ref()
            .ok_or_else(|| anyhow!("pending path required"))?;
        atomic_json(path, operation)?;
        sync_directory(path.parent().unwrap_or(Path::new(".")))?;
        self.checkpoint("pending-written", operation)
    }
    fn pending_clear(&self) -> Result<()> {
        let path = self
            .pending_path
            .as_ref()
            .ok_or_else(|| anyhow!("pending path required"))?;
        fs::remove_file(path)?;
        sync_directory(path.parent().unwrap_or(Path::new(".")))?;
        Ok(())
    }
    fn load_pending(&self) -> Result<Option<Value>> {
        let Some(path) = self.pending_path.as_ref() else {
            return Ok(None);
        };
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let operation: Value = serde_json::from_reader(file)?;
        if !operation.is_object() || operation["version"] != 1 {
            bail!("invalid pending operation; preserve state");
        }
        for key in ["dir", "old", "new", "temporary_path", "operation_id"] {
            field(&operation, key)?;
        }
        op_identity(&operation)?;
        validate_name(field(&operation, "old")?)?;
        validate_name(field(&operation, "new")?)?;
        let parent = field(&operation, "dir")?;
        let temporary = field(&operation, "temporary_path")?;
        if Path::new(temporary).parent() != Some(Path::new(parent)) {
            bail!("invalid pending temporary path; preserve state");
        }
        validate_name(&temporary_name(&operation)?)?;
        if let Some(marker) = operation.get("marker") {
            let name = marker["name"]
                .as_str()
                .ok_or_else(|| anyhow!("invalid pending marker"))?;
            let token = marker["token"]
                .as_str()
                .ok_or_else(|| anyhow!("invalid pending marker"))?;
            if !name.starts_with("user.jaso_nfc.")
                || name.len() > 255
                || token != field(&operation, "operation_id")?
                || !token.is_ascii()
            {
                bail!("invalid pending marker; preserve state");
            }
            cstring(name)?;
        }
        Ok(Some(operation))
    }
    fn emit(&mut self, record: &Value) -> Result<()> {
        self.journal
            .as_mut()
            .ok_or_else(|| anyhow!("journal must be locked and opened before mutation"))?
            .emit(record)
    }
    fn emit_success(&mut self, operation: &Value, recovered: bool) -> Result<Value> {
        let mut record = operation.clone();
        record["status"] = operation
            .get("operation_status")
            .cloned()
            .unwrap_or(json!("renamed"));
        if recovered {
            record["recovered"] = json!(true);
        }
        self.emit(&record)?;
        self.checkpoint("journal-written", operation)?;
        Ok(record)
    }
    fn find_marker(
        &self,
        operation: &Value,
        fd: RawFd,
        names: &[&str],
    ) -> Result<Option<(String, libc::stat)>> {
        let Some(marker) = operation.get("marker") else {
            return Ok(None);
        };
        let key = field(marker, "name")?;
        let token = field(marker, "token")?.as_bytes();
        for name in names {
            let held = match open_entry(fd, name) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if marker_get(held.as_raw_fd(), key)?.as_deref() == Some(token) {
                return Ok(Some((
                    actual_stored_name(fd, name)?,
                    fstat(held.as_raw_fd())?,
                )));
            }
        }
        Ok(None)
    }
    fn before_fallback(
        &mut self,
        operation: &mut Value,
        source: &str,
        fd: RawFd,
    ) -> Result<[u64; 2]> {
        operation["rename_mode"] = json!("guarded");
        if operation.get("marker").is_none() {
            operation["marker"] = json!({"name":format!("user.jaso_nfc.{}",uuid::Uuid::new_v4().simple()),"token":operation["operation_id"]});
            operation["phase"] = json!("marker-intent");
        }
        self.pending_write(operation)?;
        let held = open_entry(fd, source)?;
        let marker = operation["marker"].clone();
        let key = field(&marker, "name")?;
        let token = field(&marker, "token")?.as_bytes();
        let current = marker_get(held.as_raw_fd(), key)?;
        let mut info = fstat(held.as_raw_fd())?;
        if current.as_deref() != Some(token) {
            if current.is_some()
                || operation["phase"] != "marker-intent"
                || identity(&info) != op_identity(operation)?
            {
                return Err(stale());
            }
            info = marker_create(held.as_raw_fd(), key, token)?;
            self.checkpoint("marker-created", operation)?;
            sync_file(&held)?;
        }
        operation["identity"] = json!(identity(&info));
        operation["phase"] = json!("moving");
        self.pending_write(operation)?;
        op_identity(operation)
    }
    fn move_entry(
        &mut self,
        operation: &mut Value,
        source: &str,
        destination: &str,
        fd: RawFd,
    ) -> Result<()> {
        self.checkpoint("before-exclusive", operation)?;
        #[cfg(test)]
        let result = if let Some(code) = self.exclusive_error {
            Err(io::Error::from_raw_os_error(code))
        } else {
            rename_exclusive(source, destination, fd)
        };
        #[cfg(not(test))]
        let result = rename_exclusive(source, destination, fd);
        if let Err(error) = result {
            let unsupported = error.raw_os_error().is_some_and(|code| {
                [libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS].contains(&code)
            });
            if !cfg!(target_os = "macos") || !unsupported {
                return Err(error.into());
            }
            let expected = self.before_fallback(operation, source, fd)?;
            if stat_at(fd, source)?.as_ref().map(identity) != Some(expected) {
                return Err(stale());
            }
            if stat_at(fd, destination)?.is_some() {
                return Err(io::Error::from_raw_os_error(libc::EEXIST).into());
            }
            let source = cstring(source)?;
            let destination = cstring(destination)?;
            if unsafe { libc::renameat(fd, source.as_ptr(), fd, destination.as_ptr()) } < 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        self.checkpoint("moved", operation)?;
        if operation.get("marker").is_some()
            && let Some((_, info)) = self.find_marker(operation, fd, &[destination])?
        {
            operation["identity"] = json!(identity(&info));
            self.pending_write(operation)?;
        }
        Ok(())
    }
    fn remove_marker(&mut self, operation: &mut Value, fd: RawFd, name: &str) -> Result<()> {
        let Some(marker) = operation.get("marker").cloned() else {
            return Ok(());
        };
        let held = open_entry(fd, name)?;
        if identity(&fstat(held.as_raw_fd())?) != op_identity(operation)? {
            return Err(stale());
        }
        let info = marker_remove(
            held.as_raw_fd(),
            field(&marker, "name")?,
            field(&marker, "token")?.as_bytes(),
        )?;
        self.checkpoint("marker-removed", operation)?;
        sync_file(&held)?;
        operation["identity"] = json!(identity(&info));
        operation["identity_finalized"] = json!(true);
        self.pending_write(operation)
    }
    fn cancel(&mut self, operation: &mut Value, fd: RawFd, name: &str) -> Result<()> {
        if operation.get("marker").is_some() {
            operation["phase"] = json!("not-started");
            self.pending_write(operation)?;
            self.remove_marker(operation, fd, name)?;
        }
        self.pending_clear()
    }
    fn recorded_success(&self, operation: &Value) -> Result<Option<Value>> {
        let Some(path) = self.log_path.as_ref() else {
            return Ok(None);
        };
        if !path.exists() && !suffix(path, ".history").is_dir() {
            return Ok(None);
        }
        let status = operation
            .get("operation_status")
            .and_then(Value::as_str)
            .unwrap_or("renamed");
        let mut latest = None;
        visit_records(path, |record| {
            if record["operation_id"] == operation["operation_id"] && record["status"] == status {
                latest = Some(record);
            }
            Ok(())
        })?;
        Ok(latest)
    }
    fn complete(
        &mut self,
        operation: &mut Value,
        fd: RawFd,
        recovered: bool,
        recorded: Option<Value>,
    ) -> Result<Value> {
        if operation.get("marker").is_some() {
            operation["phase"] = json!("committed");
            self.pending_write(operation)?;
        }
        let mut record = match recorded {
            Some(record) => record,
            None => self.emit_success(operation, recovered)?,
        };
        if operation.get("marker").is_some() {
            let name = field(operation, "new")?.to_owned();
            self.remove_marker(operation, fd, &name)?;
            if record["identity"] != operation["identity"] {
                record = self.emit_success(operation, recovered)?;
            }
        }
        self.pending_clear()?;
        Ok(record)
    }
    fn recover_inner(&mut self) -> Result<Option<Value>> {
        let Some(mut operation) = self.load_pending()? else {
            return Ok(None);
        };
        let parent = field(&operation, "dir")?.to_owned();
        let directory = self.open_directory(&parent)?;
        let fd = directory.as_raw_fd();
        let mut expected = op_identity(&operation)?;
        let old = field(&operation, "old")?.to_owned();
        let new = field(&operation, "new")?.to_owned();
        let temporary = temporary_name(&operation)?;
        let marked = self.find_marker(&operation, fd, &[&temporary, &new, &old])?;
        if operation["phase"] == "not-started" {
            if let Some((name, info)) = marked {
                operation["identity"] = json!(identity(&info));
                self.remove_marker(&mut operation, fd, &name)?;
            }
            self.pending_clear()?;
            operation["status"] = json!("not-started");
            operation["recovered"] = json!(true);
            return Ok(Some(operation));
        }
        let recorded = self.recorded_success(&operation)?;
        if operation["phase"] == "committed"
            && let Some(mut record) = recorded
        {
            if let Some((_, info)) = marked {
                operation["identity"] = json!(identity(&info));
                self.pending_write(&operation)?;
                return Ok(Some(self.complete(
                    &mut operation,
                    fd,
                    true,
                    Some(record),
                )?));
            }
            if operation["identity_finalized"] == true {
                if record["identity"] != operation["identity"] {
                    record = self.emit_success(&operation, true)?;
                }
                self.pending_clear()?;
                record["recovered"] = json!(true);
                return Ok(Some(record));
            }
            record["recovered"] = json!(true);
            record["identity_finalization_unavailable"] = json!(true);
            let mut diagnostic = record.clone();
            diagnostic["status"] = json!("identity-finalization-unavailable");
            self.emit(&diagnostic)?;
            self.pending_clear()?;
            return Ok(Some(record));
        }
        if let Some((_, info)) = marked {
            expected = identity(&info);
            operation["identity"] = json!(expected);
            self.pending_write(&operation)?;
        } else if operation["phase"] == "moving" {
            if let Some(info) = stat_at(fd, &temporary)? {
                operation["expected_identity"] = json!(expected);
                operation["identity"] = json!(identity(&info));
                operation["recovery_action"] = json!("rollback");
                let marker = remove_field(&mut operation, "marker");
                operation["expected_marker"] = marker;
                remove_field(&mut operation, "phase");
                self.pending_write(&operation)?;
                return self.recover_inner();
            }
            bail!("pending operation marker is missing: {parent}");
        }
        let located_old = stat_at(fd, &old)?;
        let located_new = stat_at(fd, &new)?;
        let located_temporary = stat_at(fd, &temporary)?;
        let names: HashSet<String> = list_directory(fd)?
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        if operation["recovery_action"] == "rollback" {
            if located_old
                .as_ref()
                .is_some_and(|s| identity(s) != expected)
            {
                bail!("rollback destination was replaced: {parent}/{old}");
            }
            if !names.contains(&old) {
                if located_temporary.as_ref().map(identity) != Some(expected) {
                    return Err(stale());
                }
                self.move_entry(&mut operation, &temporary, &old, fd)?;
                sync_file(&directory)?;
            }
            if operation.get("marker").is_some() {
                self.remove_marker(&mut operation, fd, &old)?;
            }
            operation["status"] = json!("rolled-back");
            operation["recovered"] = json!(true);
            self.emit(&operation)?;
            self.pending_clear()?;
            return Ok(Some(operation));
        }
        let matches = [
            (&temporary, &located_temporary),
            (&new, &located_new),
            (&old, &located_old),
        ]
        .into_iter()
        .find(|(name, info)| names.contains(*name) && info.as_ref().map(identity) == Some(expected))
        .map(|(name, _)| name.clone());
        let Some(actual) = matches else {
            if let Some(staged) = located_temporary {
                operation["expected_identity"] = json!(expected);
                operation["identity"] = json!(identity(&staged));
                operation["recovery_action"] = json!("rollback");
                self.pending_write(&operation)?;
                return self.recover_inner();
            }
            return Err(stale());
        };
        if located_new
            .as_ref()
            .is_some_and(|s| identity(s) != expected)
        {
            bail!("pending destination was replaced: {parent}/{new}");
        }
        if actual == old {
            self.cancel(&mut operation, fd, &old)?;
            operation["status"] = json!("not-started");
            operation["recovered"] = json!(true);
            return Ok(Some(operation));
        }
        if actual == temporary {
            self.move_entry(&mut operation, &temporary, &new, fd)?;
            sync_file(&directory)?;
            expected = op_identity(&operation)?;
        }
        if stored_name(fd, expected, &[&new, &old, &temporary])?.as_deref() != Some(&new) {
            bail!("recovered name is not stored as requested: {parent}");
        }
        Ok(Some(self.complete(&mut operation, fd, true, recorded)?))
    }
    fn recover_locked(&mut self) -> Result<Option<Value>> {
        self.recover_inner().map_err(|error| {
            PendingRecoveryError(format!("pending operation requires recovery: {error:#}")).into()
        })
    }
    pub fn recover(&mut self) -> Result<Option<Value>> {
        if !self.apply {
            return Ok(None);
        }
        self.prepare_state()?;
        let log = self.log_path.clone().unwrap();
        let pending = self.pending_path.clone().unwrap();
        let _locks = JournalLocks::acquire(&[&log, &pending])?;
        self.journal = Some(Journal::standard(&log)?);
        let result = self.recover_locked();
        self.close();
        result
    }
    fn rename_entry(
        &mut self,
        parent: &str,
        name: &str,
        info: &libc::stat,
        fd: RawFd,
        target: Option<&str>,
        status: &str,
    ) -> Result<(String, Option<String>, bool)> {
        let target = target.map(str::to_owned).unwrap_or_else(|| nfc(name));
        let source = Path::new(parent).join(name).to_string_lossy().into_owned();
        let destination = Path::new(parent)
            .join(&target)
            .to_string_lossy()
            .into_owned();
        if !self.apply
            || target == name
            || (status == "renamed"
                && self
                    .policy
                    .roots
                    .iter()
                    .any(|root| nfc(root) == nfc(&source)))
            || self.retry.deferred(&source, &destination)
        {
            return Ok((name.into(), None, false));
        }
        validate_name(&target)?;
        let expected = identity(info);
        let temporary = format!(".jaso-{}{}", uuid::Uuid::new_v4().simple(), TMP_SUFFIX);
        let mut operation = json!({"version":1,"operation_id":uuid::Uuid::new_v4().simple().to_string(),"dir":parent,"old":name,"new":target,"operation_status":status,"rename_mode":"exclusive","type":entry(source.clone(),info).kind,"identity":expected,"temporary_path":Path::new(parent).join(&temporary),"ts":timestamp()});
        let mut pending = false;
        let attempt = (|| -> Result<()> {
            if stat_at(fd, name)?.as_ref().map(identity) != Some(expected) {
                return Err(stale());
            }
            if stat_at(fd, &target)?
                .as_ref()
                .is_some_and(|s| identity(s) != expected)
            {
                return Err(io::Error::from_raw_os_error(libc::EEXIST).into());
            }
            self.pending_write(&operation)?;
            pending = true;
            self.move_entry(&mut operation, name, &temporary, fd)?;
            sync_fd(fd)?;
            let expected = op_identity(&operation)?;
            let staged = stat_at(fd, &temporary)?;
            if let Some(staged) = staged.as_ref().filter(|s| identity(s) != expected) {
                operation["expected_identity"] = json!(expected);
                operation["identity"] = json!(identity(staged));
                operation["recovery_action"] = json!("rollback");
                if operation.get("marker").is_some() {
                    let marker = remove_field(&mut operation, "marker");
                    operation["expected_marker"] = marker;
                    remove_field(&mut operation, "phase");
                }
                self.pending_write(&operation)?;
                self.move_entry(&mut operation, &temporary, name, fd)?;
                sync_fd(fd)?;
                self.cancel(&mut operation, fd, name)?;
                pending = false;
                return Err(stale());
            }
            if staged.is_none() {
                return Err(stale());
            }
            self.move_entry(&mut operation, &temporary, &target, fd)?;
            sync_fd(fd)?;
            let expected = op_identity(&operation)?;
            if stored_name(fd, expected, &[&target, name, &temporary])?.as_deref() != Some(&target)
            {
                if stat_at(fd, &target)?.as_ref().map(identity) != Some(expected) {
                    return Err(stale());
                }
                self.move_entry(&mut operation, &target, &temporary, fd)?;
                self.move_entry(&mut operation, &temporary, name, fd)?;
                sync_fd(fd)?;
                self.cancel(&mut operation, fd, name)?;
                pending = false;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "filesystem does not preserve requested spelling",
                )
                .into());
            }
            self.complete(&mut operation, fd, false, None)?;
            pending = false;
            self.retry.entries.remove(&source);
            Ok(())
        })();
        match attempt {
            Ok(()) => Ok((target, None, true)),
            Err(error) => {
                // Reload durable existence after errors occurring during persistence.
                pending |= self.pending_path.as_ref().is_some_and(|p| p.exists());
                let expected = op_identity(&operation)?;
                let actual = stored_name(fd, expected, &[&target, name, &temporary])?;
                if pending
                    && actual.as_deref() == Some(name)
                    && stat_at(fd, &temporary)?.is_none()
                    && stat_at(fd, &target)?
                        .as_ref()
                        .is_none_or(|s| identity(s) == expected)
                    && self.cancel(&mut operation, fd, name).is_ok()
                {
                    pending = false;
                }
                let reason = error
                    .downcast_ref::<io::Error>()
                    .and_then(|e| e.raw_os_error())
                    .map_or_else(|| "None".into(), |n| n.to_string());
                self.retry.failure(&source, &destination, &reason);
                operation["status"] = json!("error");
                operation["error"] = json!(format!("{error:#}"));
                if pending {
                    operation["recovery_required"] = json!(true);
                    if let Some(actual) = &actual {
                        operation["recovery_path"] = json!(Path::new(parent).join(actual));
                    }
                }
                self.emit(&operation)?;
                Ok((
                    actual.unwrap_or_else(|| name.into()),
                    Some(format!("{error:#}")),
                    false,
                ))
            }
        }
    }
    fn rewrite(result: &mut ScanResult, old: &str, new: &str) {
        let prefix = format!("{old}/");
        for item in &mut result.entries {
            if item.path.starts_with(&prefix) {
                item.path = format!("{}{}", new, &item.path[old.len()..]);
            }
        }
        for path in &mut result.directories {
            if path == old || path.starts_with(&prefix) {
                *path = format!("{}{}", new, &path[old.len()..]);
            }
        }
        for error in &mut result.errors {
            if error.path == old || error.path.starts_with(&prefix) {
                error.path = format!("{}{}", new, &error.path[old.len()..]);
            }
        }
    }
    fn add_error(result: &mut ScanResult, path: &str, error: &anyhow::Error) {
        result.errors.push(ScanError {
            path: path.into(),
            error: format!("{error:#}"),
            errno: error
                .downcast_ref::<io::Error>()
                .and_then(io::Error::raw_os_error),
        });
    }
    fn rebase_retries(&mut self, old: &str, new: &str) {
        if old == new {
            return;
        }
        let prefix = format!("{old}/");
        let changed: Vec<String> = self
            .retry
            .entries
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        for key in changed {
            if let Some(value) = self.retry.entries.remove(&key) {
                self.retry
                    .entries
                    .insert(format!("{}{}", new, &key[old.len()..]), value);
            }
        }
    }
    fn walk_step(&mut self, request: &str, path: &str, result: &mut ScanResult) -> Result<()> {
        // The durable job owns the snapshot by its requested spelling. The
        // resolved filesystem spelling can differ without starting a new scan,
        // and independent requests must not consume each other's observations.
        let saved = self.scans.remove(request);
        result.scan_id = saved.as_ref().map(|scan| scan.id.clone());
        let observed = (|| -> Result<File> {
            if !self.policy.descend(path) {
                if saved.is_some() {
                    return Err(stale());
                }
                return Err(io::Error::from_raw_os_error(libc::ENOENT).into());
            }
            let directory = self.open_directory(path)?;
            if let Some(scan) = &saved {
                let deadline = crate::directory_io::DirectoryIo::begin()?;
                let before = fstat(scan.directory.as_raw_fd())?;
                let current = fstat(directory.as_raw_fd())?;
                deadline.progress()?;
                if identity(&before) != identity(&current) {
                    return Err(stale());
                }
            }
            Ok(directory)
        })();
        let directory = match observed {
            Ok(directory) => directory,
            Err(error) => {
                if saved.is_some()
                    || !error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
                {
                    Self::add_error(result, path, &error);
                }
                return Ok(());
            }
        };
        let mut scan = match saved {
            Some(scan) => scan,
            None => match DirectoryScan::new(directory) {
                Ok(scan) => scan,
                Err(error) => {
                    Self::add_error(result, path, &error);
                    return Ok(());
                }
            },
        };
        result.scan_id = Some(scan.id.clone());
        let fd = scan.directory.as_raw_fd();
        let work = (|| -> Result<bool> {
            if let Some(listing) = &mut scan.listing {
                // No database writes or filename mutations run inside the
                // metadata interruption scope owned by batch().
                let (children, complete) = listing.batch(fd)?;
                let transaction = scan.spool.transaction()?;
                for (name, info) in children {
                    let [dev, ino] = identity(&info);
                    transaction.execute(
                        "INSERT OR IGNORE INTO children(name,dev,ino) VALUES(?,?,?)",
                        rusqlite::params![name, dev.to_string(), ino.to_string()],
                    )?;
                }
                transaction.commit()?;
                if !complete {
                    return Ok(false);
                }
                scan.listing = None;
                // Only successful EOF can retire retry names absent from the
                // snapshot. A partial stream never reaches this branch.
                use rusqlite::OptionalExtension;
                let mut retired = Vec::new();
                for source in self.retry.entries.keys() {
                    let source_path = Path::new(source);
                    if source_path.parent() == Some(Path::new(path)) {
                        let name = source_path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default();
                        let exists = scan
                            .spool
                            .query_row("SELECT 1 FROM children WHERE name=?", [name], |_| Ok(()))
                            .optional()?
                            .is_some();
                        if !exists || nfc(name) == name {
                            retired.push(source.clone());
                        }
                    }
                }
                for source in retired {
                    self.retry.entries.remove(&source);
                }
            }
            let children: Vec<(i64, String, String, String)> = scan.spool
                .prepare("SELECT position,name,dev,ino FROM children WHERE position>? ORDER BY position LIMIT ?")?
                .query_map(rusqlite::params![scan.position, STEP_ENTRIES as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?.collect::<rusqlite::Result<_>>()?;
            let started = Instant::now();
            for (position, name, dev, ino) in children {
                if started.elapsed() >= STEP_TIME && scan.position > 0 {
                    return Ok(false);
                }
                scan.position = position;
                let source = Path::new(path).join(&name).to_string_lossy().into_owned();
                if !self.policy.accepts(&source) {
                    continue;
                }
                let info = {
                    let deadline = crate::directory_io::DirectoryIo::begin()?;
                    let info = stat_at(fd, &name);
                    deadline.progress()?;
                    info?
                };
                let Some(info) = info else { continue };
                if identity(&info) != [dev.parse::<u64>()?, ino.parse::<u64>()?] {
                    return Err(stale());
                }
                if self.apply && self.pending_path.as_ref().is_some_and(|p| p.exists()) {
                    return Err(PendingRecoveryError(
                        "unresolved pending operation blocks mutations".into(),
                    )
                    .into());
                }
                let (actual, error, renamed) =
                    self.rename_entry(path, &name, &info, fd, None, "renamed")?;
                if self.apply && self.pending_path.as_ref().is_some_and(|p| p.exists()) {
                    return Err(PendingRecoveryError(
                        "unresolved pending operation blocks mutations".into(),
                    )
                    .into());
                }
                let final_path = Path::new(path).join(&actual).to_string_lossy().into_owned();
                if actual != name
                    && info.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFDIR as u32
                {
                    // Invalidate queued descendants after an actual rename,
                    // rather than after merely resolving an equivalent spelling.
                    self.scans
                        .retain(|path, _| !Path::new(path).starts_with(&source));
                    self.rebase_retries(&source, &final_path);
                }
                result.renamed += u64::from(renamed);
                if let Some(error) = error {
                    result.errors.push(ScanError {
                        path: source,
                        error,
                        errno: None,
                    });
                    return Ok(true);
                }
                let current = {
                    let deadline = crate::directory_io::DirectoryIo::begin()?;
                    let info = stat_at(fd, &actual);
                    deadline.progress()?;
                    info?
                };
                if let Some(info) = current {
                    result.entries.push(entry(final_path, &info));
                }
                if self.apply && self.pending_path.as_ref().is_some_and(|p| p.exists()) {
                    return Err(PendingRecoveryError(
                        "unresolved pending operation blocks mutations".into(),
                    )
                    .into());
                }
            }
            let remaining: bool = scan.spool.query_row(
                "SELECT EXISTS(SELECT 1 FROM children WHERE position>?)",
                [scan.position],
                |row| row.get(0),
            )?;
            Ok(!remaining)
        })();
        match work {
            Ok(true) if result.errors.is_empty() => result.directories.push(path.into()),
            Ok(true) => (),
            Ok(false) => {
                result.complete = false;
                if self.scans.len() < RETAINED_SCANS {
                    self.scans.insert(request.into(), scan);
                }
                // If all lanes are occupied, this ephemeral observation may
                // finish a small fresh directory. Larger overflow work stays
                // queued and obtains a lane after an existing snapshot ends.
            }
            Err(error) if error.downcast_ref::<PendingRecoveryError>().is_some() => {
                return Err(error);
            }
            Err(error) => Self::add_error(result, path, &error),
        }
        Ok(())
    }
    fn walk(&mut self, path: &str, recursive: bool, result: &mut ScanResult) -> Result<()> {
        if !self.policy.descend(path) {
            // A replaced parent cannot still own these child names. Retire
            // only proven stale descendants; unreadable metadata is not proof.
            let stale_parent = match fs::symlink_metadata(path) {
                Ok(info) => !info.is_dir() || info.file_type().is_symlink(),
                Err(error) => error.kind() == io::ErrorKind::NotFound,
            };
            if stale_parent {
                self.retry.entries.retain(|source, _| {
                    Path::new(source) == Path::new(path) || !Path::new(source).starts_with(path)
                });
            }
            return Ok(());
        }
        let directory = match self.open_directory(path) {
            Ok(file) => file,
            Err(e)
                if e.downcast_ref::<io::Error>()
                    .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
            {
                self.retry
                    .entries
                    .retain(|source, _| !Path::new(source).starts_with(path));
                return Ok(());
            }
            Err(e) => {
                Self::add_error(result, path, &e);
                return Ok(());
            }
        };
        let fd = directory.as_raw_fd();
        let children = match list_directory(fd) {
            Ok(list) => list,
            Err(error) => {
                Self::add_error(result, path, &error);
                return Ok(());
            }
        };
        // Only a completed listing can prove that a saved source disappeared
        // or is already normalized. Failed enumeration keeps retry evidence.
        let names: HashSet<&str> = children.iter().map(|(name, _)| name.as_str()).collect();
        self.retry.entries.retain(|source, _| {
            let source = Path::new(source);
            source.parent() != Some(Path::new(path))
                || source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| names.contains(name) && nfc(name) != name)
        });
        result.directories.push(path.into());
        for (name, info) in children {
            let source = Path::new(path).join(&name).to_string_lossy().into_owned();
            if !self.policy.accepts(&source) {
                continue;
            }
            let is_directory = (info.st_mode as u32 & libc::S_IFMT as u32) == libc::S_IFDIR as u32;
            if recursive && is_directory && self.policy.descend(&source) {
                self.walk(&source, true, result)?;
            }
            if self.apply && self.pending_path.as_ref().is_some_and(|p| p.exists()) {
                return Err(PendingRecoveryError(
                    "unresolved pending operation blocks mutations".into(),
                )
                .into());
            }
            let (actual, error, renamed) =
                self.rename_entry(path, &name, &info, fd, None, "renamed")?;
            let final_path = Path::new(path).join(&actual).to_string_lossy().into_owned();
            if actual != name && is_directory {
                Self::rewrite(result, &source, &final_path);
                self.scans
                    .retain(|path, _| !Path::new(path).starts_with(&source));
                self.rebase_retries(&source, &final_path);
            }
            result.renamed += u64::from(renamed);
            if let Some(error) = error {
                result.errors.push(ScanError {
                    path: source,
                    error,
                    errno: None,
                });
            }
            match stat_at(fd, &actual) {
                Ok(Some(info)) => result.entries.push(entry(final_path, &info)),
                Ok(None) => (),
                Err(e) => Self::add_error(result, &final_path, &e.into()),
            }
        }
        if self.apply && self.pending_path.as_ref().is_some_and(|p| p.exists()) {
            return Err(PendingRecoveryError(
                "unresolved pending operation blocks mutations".into(),
            )
            .into());
        }
        Ok(())
    }
    pub fn next_retry_time(&self, checked_after: Option<f64>) -> Option<f64> {
        self.retry
            .entries
            .iter()
            .filter(|(path, record)| {
                self.policy.accepts_lexically(path)
                    && checked_after.is_none_or(|time| record.next_retry > time)
            })
            .map(|(_, record)| record.next_retry)
            .min_by(f64::total_cmp)
    }
    pub fn retry_paths(&mut self, time: f64) -> Result<Vec<String>> {
        if self.apply {
            let count = self.retry.entries.len();
            self.retry
                .entries
                .retain(|path, _| !crate::policy::is_managed_cloud_path(path));
            if self.retry.entries.len() != count {
                self.retry.save()?;
            }
        }
        let mut due = BTreeSet::new();
        for (path, record) in &mut self.retry.entries {
            record.next_retry = record.next_retry.min(time + Self::RETRY_MAX);
            // Discovery runs before every job. Keep it lexical and in memory;
            // enqueue/reconcile enforce the full filesystem-aware policy.
            if record.next_retry > time || !self.policy.accepts_lexically(path) {
                continue;
            }
            let parent = Path::new(&path)
                .parent()
                .unwrap_or(Path::new("."))
                .to_string_lossy()
                .into_owned();
            due.insert(parent);
        }
        Ok(due.into_iter().collect())
    }
    pub fn reconcile(&mut self, path: &str, recursive: bool) -> Result<ScanResult> {
        self.scans.remove(&absolute(path));
        self.reconcile_inner(path, recursive, false)
    }
    pub fn reconcile_step(&mut self, path: &str, recursive: bool) -> Result<ScanResult> {
        if recursive {
            return self.reconcile(path, true);
        }
        self.reconcile_inner(path, false, true)
    }
    fn reconcile_inner(
        &mut self,
        path: &str,
        recursive: bool,
        bounded: bool,
    ) -> Result<ScanResult> {
        let mut path = absolute(path);
        let request = path.clone();
        let mut result = ScanResult {
            scope: path.clone(),
            ..Default::default()
        };
        // Recovery is performed even when the requested nested scope disappeared.
        if self.apply {
            self.prepare_state()?;
            let log = self.log_path.clone().unwrap();
            let pending = self.pending_path.clone().unwrap();
            let _locks = JournalLocks::acquire(&[&log, &pending])?;
            self.journal = Some(Journal::standard(&log)?);
            let work = (|| -> Result<()> {
                self.recover_locked()?;
                if self.policy.descend(&path) {
                    match self.actual_scope(&path) {
                        Ok(actual) => {
                            self.rebase_retries(&path, &actual);
                            path = actual;
                            result.scope = path.clone();
                        }
                        Err(error) => {
                            Self::add_error(&mut result, &path, &error);
                            return Ok(());
                        }
                    }
                }
                if bounded {
                    self.walk_step(&request, &path, &mut result)
                } else {
                    self.walk(&path, recursive, &mut result)
                }
            })();
            let save = self.retry.save();
            self.close();
            work?;
            save?;
        } else {
            if self.policy.descend(&path) {
                match self.actual_scope(&path) {
                    Ok(actual) => {
                        self.rebase_retries(&path, &actual);
                        path = actual;
                        result.scope = path.clone();
                    }
                    Err(error) => {
                        Self::add_error(&mut result, &path, &error);
                        return Ok(result);
                    }
                }
            }
            if bounded {
                self.walk_step(&request, &path, &mut result)?;
            } else {
                self.walk(&path, recursive, &mut result)?;
            }
        }
        Ok(result)
    }
}
impl Reconciler for Normalizer {
    fn policy(&self) -> &Policy {
        &self.policy
    }
    fn set_policy(&mut self, policy: Policy) {
        if self.policy.roots != policy.roots
            || self.policy.excludes != policy.excludes
            || self.policy.exclude_names != policy.exclude_names
            || self.policy.skip_hidden_tops != policy.skip_hidden_tops
            || self.policy.root_excludes != policy.root_excludes
        {
            self.scans.clear();
        }
        self.policy = policy;
    }
    fn active_scans(&self) -> Vec<String> {
        self.scans.keys().cloned().collect()
    }
    fn retain_scans(&mut self, paths: &[String]) {
        self.scans.retain(|path, _| paths.contains(path));
    }
    fn reconcile(&mut self, path: &str, recursive: bool) -> Result<ScanResult> {
        Normalizer::reconcile(self, path, recursive)
    }
    fn reconcile_step(&mut self, path: &str, recursive: bool) -> Result<ScanResult> {
        Normalizer::reconcile_step(self, path, recursive)
    }
    fn retry_paths(&mut self, time: f64) -> Result<Vec<String>> {
        Normalizer::retry_paths(self, time)
    }
}

pub fn revert(log: &Path, output: Option<&Path>) -> Result<(u64, u64)> {
    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| suffix(log, ".revert.jsonl"));
    let pending = suffix(&output, ".pending.json");
    fs::create_dir_all(output.parent().unwrap_or(Path::new(".")))?;
    let _locks = JournalLocks::acquire(&[log, &output, &pending])?;
    let records = journal_records(log)?;
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for record in records
        .into_iter()
        .rev()
        .filter(|r| r["status"] == "renamed" || r["recovery_required"] == true)
    {
        if let Some(operation) = record.get("operation_id").filter(|v| !v.is_null())
            && !seen.insert(operation.to_string())
        {
            continue;
        }
        // Validate untrusted history before its paths can participate in moves.
        for key in ["dir", "old", "new"] {
            field(&record, key)?;
        }
        validate_name(field(&record, "old")?)?;
        validate_name(field(&record, "new")?)?;
        if record.get("identity").is_some() {
            op_identity(&record)?;
        }
        unique.push(record);
    }
    let roots = unique
        .iter()
        .map(|row| field(row, "dir").unwrap().to_owned())
        .collect();
    let policy = Policy::new(roots, vec![], vec![".git".into()], vec![], HashMap::new());
    let mut engine = Normalizer::new(
        policy,
        Some(output.clone()),
        None,
        Some(pending.clone()),
        true,
    )?;
    engine.journal = Some(Journal::standard(&output)?);
    engine.recover_locked()?;
    let mut done = 0;
    let mut failed = 0;
    for operation in &unique {
        let parent = field(operation, "dir")?;
        let old = field(operation, "old")?;
        let new = field(operation, "new")?;
        let destination = Path::new(parent).join(old);
        let initial_source = operation
            .get("recovery_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(parent).join(new));
        let attempt = (|| -> Result<bool> {
            let source = if operation.get("identity").is_some() {
                let expected = op_identity(operation)?;
                [
                    operation
                        .get("recovery_path")
                        .and_then(Value::as_str)
                        .map(PathBuf::from),
                    operation
                        .get("temporary_path")
                        .and_then(Value::as_str)
                        .map(PathBuf::from),
                    Some(initial_source.clone()),
                    Some(destination.clone()),
                ]
                .into_iter()
                .flatten()
                .find(|path| {
                    let signature = path_signature(path, false);
                    signature.first().and_then(Value::as_u64) == Some(expected[0])
                        && signature.get(1).and_then(Value::as_u64) == Some(expected[1])
                })
                .ok_or_else(stale)?
            } else {
                initial_source.clone()
            };
            if source.parent() != Some(Path::new(parent)) {
                return Err(io::Error::from_raw_os_error(libc::EXDEV).into());
            }
            let directory = engine.open_directory(parent)?;
            let fd = directory.as_raw_fd();
            let basename = source
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(stale)?;
            let info = stat_at(fd, basename)?.ok_or_else(stale)?;
            if operation.get("identity").is_some() && identity(&info) != op_identity(operation)? {
                return Err(stale());
            }
            let name = stored_name(fd, identity(&info), &[basename])?.ok_or_else(stale)?;
            let (_, error, _) =
                engine.rename_entry(parent, &name, &info, fd, Some(old), "reverted")?;
            Ok(error.is_none())
        })();
        match attempt {
            Ok(true) => done += 1,
            Ok(false) => failed += 1,
            Err(error) => {
                failed += 1;
                engine.emit(&json!({"revert":true,"dir":parent,"old":new,"new":old,"ts":timestamp(),"status":"error","error":format!("{error:#}")}))?;
            }
        }
        if pending.exists() {
            failed = unique.len() as u64 - done;
            break;
        }
    }
    engine.close();
    Ok((done, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;
    use unicode_normalization::UnicodeNormalization;
    fn fixture() -> (TempDir, Normalizer, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("files");
        std::fs::create_dir(&root).unwrap();
        let engine = Normalizer::new(
            Policy::new(
                vec![root.to_string_lossy().into_owned()],
                vec![],
                vec![".git".into()],
                vec![],
                HashMap::new(),
            ),
            Some(temp.path().join("renames.jsonl")),
            Some(temp.path().join("retry.json")),
            Some(temp.path().join("pending.json")),
            true,
        )
        .unwrap();
        (temp, engine, root)
    }
    #[test]
    fn worker_step_yields_before_a_wide_listing_and_keeps_unseen_retries() {
        let (_temp, mut engine, root) = fixture();
        engine.apply = false;
        for index in 0..600 {
            fs::write(root.join(format!("file-{index}")), b"fixture").unwrap();
        }
        let unseen = root
            .join("아직없음".nfd().collect::<String>())
            .to_string_lossy()
            .into_owned();
        engine.retry.failure(&unseen, &unseen, "1");
        let first = Reconciler::reconcile_step(&mut engine, root.to_str().unwrap(), false).unwrap();
        assert!(
            !first.complete,
            "a wide directory must yield its worker turn"
        );
        assert!(
            first.directories.is_empty(),
            "partial listing cannot prove completeness"
        );
        assert!(engine.retry.entries.contains_key(&unseen));
        assert!(first.entries.len() < 600);

        let mut entries = first.entries;
        let id = first.scan_id;
        let mut finished = false;
        for _ in 0..30 {
            let step =
                Reconciler::reconcile_step(&mut engine, root.to_str().unwrap(), false).unwrap();
            assert_eq!(step.scan_id, id);
            entries.extend(step.entries);
            if step.complete {
                assert_eq!(step.directories, vec![root.to_string_lossy().into_owned()]);
                finished = true;
                break;
            }
        }
        assert!(
            finished,
            "bounded turns must eventually finish the snapshot"
        );
        assert_eq!(entries.len(), 600);
        assert!(engine.retry.entries.is_empty());
    }

    #[test]
    fn fresh_unrelated_directory_runs_between_wide_directory_steps() {
        let (_temp, mut engine, root) = fixture();
        let wide = root.join("wide");
        let fresh = root.join("fresh");
        fs::create_dir(&wide).unwrap();
        fs::create_dir(&fresh).unwrap();
        for index in 0..600 {
            fs::write(wide.join(format!("file-{index}")), b"wide fixture").unwrap();
        }
        let first = Reconciler::reconcile_step(&mut engine, wide.to_str().unwrap(), false).unwrap();
        assert!(!first.complete);
        fs::write(
            fresh.join("새파일".nfd().collect::<String>()),
            b"fresh fixture",
        )
        .unwrap();
        let event =
            Reconciler::reconcile_step(&mut engine, fresh.to_str().unwrap(), false).unwrap();
        assert!(event.complete);
        assert_eq!(event.renamed, 1);
        assert_eq!(fs::read(fresh.join("새파일")).unwrap(), b"fresh fixture");
        let next = Reconciler::reconcile_step(&mut engine, wide.to_str().unwrap(), false).unwrap();
        assert_eq!(next.scan_id, first.scan_id);
        assert!(!next.complete);
    }
    #[test]
    fn stored_name_verification_does_not_enumerate_unrelated_siblings() {
        let (_temp, _engine, root) = fixture();
        for index in 0..600 {
            fs::write(root.join(format!("unrelated-{index}")), b"fixture").unwrap();
        }
        fs::write(root.join("wanted"), b"owned").unwrap();
        let directory = File::open(&root).unwrap();
        let fd = directory.as_raw_fd();
        let expected = identity(&stat_at(fd, "wanted").unwrap().unwrap());
        let visits = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = visits.clone();
        TEST_LISTING_PROGRESS.set(Some(Box::new(move || count.set(count.get() + 1))));
        let actual = stored_name(fd, expected, &["wanted"]).unwrap();
        TEST_LISTING_PROGRESS.set(None);
        assert_eq!(actual.as_deref(), Some("wanted"));
        assert_eq!(
            visits.get(),
            0,
            "candidate metadata verification must not enumerate siblings"
        );
    }

    #[test]
    fn retry_wakeup_selection_never_reads_filesystem_metadata() {
        let (_temp, mut engine, root) = fixture();
        let source = root
            .join("대기".nfd().collect::<String>())
            .to_string_lossy()
            .into_owned();
        engine.retry.failure(&source, &source, "1");
        crate::policy::FORBID_FILESYSTEM.set(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.next_retry_time(None)
        }));
        crate::policy::FORBID_FILESYSTEM.set(false);
        assert!(result.is_ok(), "scheduling cannot probe a stalled volume");
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn resumed_step_aborts_when_its_directory_has_been_replaced() {
        let (_temp, mut engine, root) = fixture();
        let scope = root.join("directory");
        fs::create_dir(&scope).unwrap();
        for index in 0..600 {
            fs::write(
                scope.join(format!("한글{index}").nfd().collect::<String>()),
                b"original",
            )
            .unwrap();
        }
        let first = engine
            .reconcile_step(scope.to_str().unwrap(), false)
            .unwrap();
        assert!(!first.complete);
        let saved = root.join("saved");
        fs::rename(&scope, &saved).unwrap();
        fs::create_dir(&scope).unwrap();
        let replacement = scope.join("대체".nfd().collect::<String>());
        fs::write(&replacement, b"replacement").unwrap();
        let next = engine
            .reconcile_step(scope.to_str().unwrap(), false)
            .unwrap();
        assert!(next.complete);
        assert_eq!(next.scan_id, first.scan_id);
        assert!(next.directories.is_empty());
        assert_eq!(next.errors[0].errno, Some(libc::ESTALE));
        assert_eq!(next.renamed, 0);
        assert_eq!(fs::read(&replacement).unwrap(), b"replacement");
        assert_eq!(fs::read_dir(&saved).unwrap().count(), 600);
    }

    #[test]
    fn worker_step_pending_race_blocks_index_advancement() {
        let (temp, mut engine, root) = fixture();
        contents(&root, "한글");
        let target = root.join("한글");
        let raced_target = target.clone();
        let mut count = 0;
        engine.hook = Some(Box::new(move |point, _| {
            if point == "before-exclusive" {
                count += 1;
                if count == 2 {
                    fs::write(&raced_target, b"concurrent").unwrap();
                }
            }
            Ok(())
        }));
        let error = engine
            .reconcile_step(root.to_str().unwrap(), false)
            .unwrap_err();
        assert!(error.downcast_ref::<PendingRecoveryError>().is_some());
        let pending: Value =
            serde_json::from_reader(File::open(temp.path().join("pending.json")).unwrap()).unwrap();
        assert_eq!(
            fs::read(pending["temporary_path"].as_str().unwrap()).unwrap(),
            b"owned contents"
        );
        assert_eq!(fs::read(target).unwrap(), b"concurrent");
    }

    #[test]
    fn withdrawn_jobs_release_snapshot_lanes_for_other_wide_directories() {
        let (_temp, mut engine, root) = fixture();
        engine.apply = false;
        for index in 0..RETAINED_SCANS + 1 {
            let directory = root.join(format!("directory-{index}"));
            fs::create_dir(&directory).unwrap();
            for child in 0..STEP_ENTRIES + 1 {
                fs::write(directory.join(format!("file-{child}")), b"fixture").unwrap();
            }
            let step = engine
                .reconcile_step(directory.to_str().unwrap(), false)
                .unwrap();
            assert!(!step.complete);
        }
        assert_eq!(engine.scans.len(), RETAINED_SCANS);
        Reconciler::retain_scans(&mut engine, &[]);
        assert!(
            engine.scans.is_empty(),
            "withdrawn jobs must release scan lanes"
        );
        let scope = root.join(format!("directory-{RETAINED_SCANS}"));
        let first = engine
            .reconcile_step(scope.to_str().unwrap(), false)
            .unwrap();
        let next = engine
            .reconcile_step(scope.to_str().unwrap(), false)
            .unwrap();
        assert_eq!(first.scan_id, next.scan_id);
        assert!(!first.complete);
        assert_eq!(engine.scans.len(), 1);
    }
    #[test]
    fn policy_change_restarts_a_snapshot_before_a_new_index_baseline() {
        let (_temp, mut engine, root) = fixture();
        engine.apply = false;
        for child in 0..STEP_ENTRIES + 1 {
            fs::write(root.join(format!("file-{child}")), b"fixture").unwrap();
        }
        let first = engine
            .reconcile_step(root.to_str().unwrap(), false)
            .unwrap();
        assert!(!first.complete);
        let mut policy = engine.policy.clone();
        policy.excludes.push(
            root.join("unrelated-exclusion")
                .to_string_lossy()
                .into_owned(),
        );
        Reconciler::set_policy(&mut engine, policy);
        let next = engine
            .reconcile_step(root.to_str().unwrap(), false)
            .unwrap();
        assert_ne!(
            first.scan_id, next.scan_id,
            "policy changes must restart complete observation after an index reset"
        );
    }
    #[test]
    fn timed_out_partial_listing_keeps_retry_evidence_and_never_marks_directory_complete() {
        let (temp, mut engine, root) = fixture();
        std::fs::write(root.join("first"), b"owned first").unwrap();
        std::fs::write(root.join("second"), b"owned second").unwrap();
        let missing = root
            .join("한글".nfd().collect::<String>())
            .to_string_lossy()
            .into_owned();
        engine.retry.failure(&missing, &missing, "1");
        engine.retry.save().unwrap();
        let mut first = true;
        TEST_LISTING_PROGRESS.set(Some(Box::new(move || {
            if first {
                first = false;
                crate::directory_io::tests::blocking_read();
            }
        })));
        let result =
            crate::directory_io::test_timeout(std::time::Duration::from_millis(30), || {
                engine.reconcile(root.to_str().unwrap(), false).unwrap()
            });
        TEST_LISTING_PROGRESS.set(None);
        assert_eq!(
            result.errors.len(),
            1,
            "partial directory must fail: {result:?}"
        );
        assert_eq!(result.errors[0].errno, Some(libc::ETIMEDOUT));
        assert!(result.directories.is_empty());
        assert!(result.entries.is_empty());
        assert!(engine.retry.entries.contains_key(&missing));
        assert!(!temp.path().join("pending.json").exists());
        assert_eq!(std::fs::read(root.join("first")).unwrap(), b"owned first");
        let complete = engine.reconcile(root.to_str().unwrap(), false).unwrap();
        assert!(complete.errors.is_empty());
        assert_eq!(complete.entries.len(), 2);
        assert!(engine.retry.entries.is_empty());
    }
    #[test]
    fn recursive_normalization_and_revert_preserve_contents_and_paths() {
        let (temp, mut engine, root) = fixture();
        let folder = root.join("폴더".nfd().collect::<String>());
        std::fs::create_dir(&folder).unwrap();
        let original = folder.join("한글.txt".nfd().collect::<String>());
        std::fs::write(&original, b"owned contents").unwrap();
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 2);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let final_path = root.join("폴더/한글.txt");
        assert_eq!(std::fs::read(&final_path).unwrap(), b"owned contents");
        assert!(
            result
                .entries
                .iter()
                .any(|e| e.path == final_path.to_string_lossy())
        );
        assert!(!temp.path().join("pending.json").exists());
        assert_eq!(
            crate::journal::revert(&temp.path().join("renames.jsonl"), None).unwrap(),
            (2, 0)
        );
        assert_eq!(std::fs::read(&original).unwrap(), b"owned contents");
    }
    #[test]
    fn unresolved_pending_prevents_every_new_mutation() {
        let (temp, mut engine, root) = fixture();
        let source = root.join("한글".nfd().collect::<String>());
        std::fs::write(&source, b"owned").unwrap();
        std::fs::write(temp.path().join("pending.json"), b"{bad").unwrap();
        let error = engine.reconcile(root.to_str().unwrap(), true).unwrap_err();
        assert!(error.downcast_ref::<PendingRecoveryError>().is_some());
        assert_eq!(std::fs::read(&source).unwrap(), b"owned");
    }
    #[test]
    fn recovery_completes_durable_temporary_hop_without_duplicate_journal() {
        let (temp, mut engine, root) = fixture();
        let staged = root.join(".jaso-owned.__jaso_nfc_tmp__");
        std::fs::write(&staged, b"owned").unwrap();
        let metadata = std::fs::symlink_metadata(&staged).unwrap();
        let operation = serde_json::json!({"version":1,"operation_id":"owned-operation","dir":root,"old":"한글".nfd().collect::<String>(),"new":"한글","temporary_path":staged,"identity":[metadata.dev(),metadata.ino()],"operation_status":"renamed"});
        crate::journal::atomic_json(&temp.path().join("pending.json"), &operation).unwrap();
        let record = engine.recover().unwrap().unwrap();
        assert_eq!(record["status"], "renamed");
        assert_eq!(record["recovered"], true);
        assert_eq!(std::fs::read(root.join("한글")).unwrap(), b"owned");
        assert!(engine.recover().unwrap().is_none());
        assert_eq!(
            crate::journal::journal_records(&temp.path().join("renames.jsonl"))
                .unwrap()
                .len(),
            1
        );
    }

    fn contents(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name.nfd().collect::<String>());
        std::fs::write(&path, b"owned contents").unwrap();
        path
    }
    fn catch_crash(engine: &mut Normalizer, root: &Path) {
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || engine.reconcile(root.to_str().unwrap(), true)
            ))
            .is_err()
        );
        engine.hook = None;
        engine.close();
    }
    #[test]
    fn intent_precedes_both_hops_and_avoids_appledouble_namespace() {
        let (temp, mut engine, root) = fixture();
        contents(&root, "한글");
        let pending = temp.path().join("pending.json");
        engine.hook = Some(Box::new(move |point, operation| {
            if point == "before-exclusive" {
                assert!(pending.exists());
                let disk: Value = serde_json::from_reader(File::open(&pending).unwrap()).unwrap();
                assert_eq!(disk["operation_id"], operation["operation_id"]);
                assert!(!temporary_name(&disk).unwrap().starts_with("._"));
            }
            Ok(())
        }));
        assert_eq!(
            engine
                .reconcile(root.to_str().unwrap(), true)
                .unwrap()
                .renamed,
            1
        );
    }
    #[test]
    fn crash_after_either_hop_and_journal_is_recoverable_and_deduplicated() {
        for (crash_point, hop) in [("moved", 1), ("moved", 2), ("journal-written", 1)] {
            let (temp, mut engine, root) = fixture();
            contents(&root, "한글");
            let mut count = 0;
            engine.hook = Some(Box::new(move |point, _| {
                if point == crash_point {
                    count += 1;
                    if count == hop {
                        panic!("owned fixture crash");
                    }
                }
                Ok(())
            }));
            catch_crash(&mut engine, &root);
            let row = engine.recover().unwrap().unwrap();
            assert_eq!(row["status"], "renamed");
            assert_eq!(std::fs::read(root.join("한글")).unwrap(), b"owned contents");
            assert!(!temp.path().join("pending.json").exists());
            assert_eq!(
                journal_records(&temp.path().join("renames.jsonl"))
                    .unwrap()
                    .len(),
                1
            );
        }
    }
    #[test]
    fn destination_race_preserves_both_objects_and_blocks_later_mutation() {
        let (temp, mut engine, root) = fixture();
        contents(&root, "한글");
        let target = root.join("한글");
        let raced_target = target.clone();
        let mut count = 0;
        engine.hook = Some(Box::new(move |point, _| {
            if point == "before-exclusive" {
                count += 1;
                if count == 2 {
                    std::fs::write(&raced_target, b"concurrent").unwrap();
                }
            }
            Ok(())
        }));
        let error = engine.reconcile(root.to_str().unwrap(), true).unwrap_err();
        assert!(error.downcast_ref::<PendingRecoveryError>().is_some());
        engine.hook = None;
        let pending: Value =
            serde_json::from_reader(File::open(temp.path().join("pending.json")).unwrap()).unwrap();
        assert_eq!(
            std::fs::read(pending["temporary_path"].as_str().unwrap()).unwrap(),
            b"owned contents"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"concurrent");
        assert!(engine.recover().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"concurrent");
    }
    #[test]
    fn source_replacement_is_rolled_back_instead_of_normalized() {
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        let saved = temp.path().join("saved-original");
        let moved_source = source.clone();
        let saved2 = saved.clone();
        let mut first = true;
        engine.hook = Some(Box::new(move |point, _| {
            if point == "before-exclusive" && first {
                first = false;
                std::fs::rename(&moved_source, &saved2).unwrap();
                std::fs::write(&moved_source, b"replacement").unwrap();
            }
            Ok(())
        }));
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 0);
        assert!(!result.errors.is_empty());
        assert_eq!(std::fs::read(&source).unwrap(), b"replacement");
        assert_eq!(std::fs::read(saved).unwrap(), b"owned contents");
        assert!(!temp.path().join("pending.json").exists());
    }
    #[test]
    fn dry_run_never_creates_journal_retry_or_pending() {
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        engine.apply = false;
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 0);
        assert_eq!(result.entries[0].path, source.to_string_lossy());
        for name in ["renames.jsonl", "retry.json", "pending.json"] {
            assert!(!temp.path().join(name).exists());
        }
    }
    #[test]
    fn permission_denial_backs_off_and_retry_resolution_survives_restart() {
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        engine.hook = Some(Box::new(|point, _| {
            if point == "before-exclusive" {
                return Err(io::Error::from_raw_os_error(libc::EACCES).into());
            }
            Ok(())
        }));
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 0);
        assert!(!result.errors.is_empty());
        assert!(!temp.path().join("pending.json").exists());
        engine.hook = None;
        assert!(engine.retry_paths(crate::model::now()).unwrap().is_empty());
        assert!(engine.next_retry_time(None).is_some());
        assert_eq!(
            engine.retry_paths(crate::model::now() + 100000.).unwrap(),
            vec![root.to_string_lossy().into_owned()]
        );
        std::fs::rename(&source, root.join("한글")).unwrap();
        // Retry discovery must remain cheap; successful reconciliation owns
        // cleanup of names removed or normalized by another application.
        engine.reconcile(root.to_str().unwrap(), false).unwrap();
        assert!(engine.retry.entries.is_empty());
        assert!(
            engine
                .retry_paths(crate::model::now() + 100000.)
                .unwrap()
                .is_empty()
        );
        assert!(
            RetryState::new(engine.retry_path.clone(), 900., 86400.)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn retry_discovery_keeps_missing_paths_and_deadlines_until_parent_reconciles() {
        let (temp, mut engine, root) = fixture();
        let source = root.join("missing").to_string_lossy().into_owned();
        engine.retry.failure(&source, &source, "1");
        engine.retry.entries.get_mut(&source).unwrap().next_retry = 1.0;
        engine.retry.save().unwrap();
        let before = std::fs::read(temp.path().join("retry.json")).unwrap();
        for _ in 0..3 {
            assert_eq!(
                engine.retry_paths(crate::model::now()).unwrap(),
                [root.to_string_lossy().into_owned()]
            );
            assert_eq!(engine.retry.entries[&source].next_retry, 1.0);
        }
        assert_eq!(
            std::fs::read(temp.path().join("retry.json")).unwrap(),
            before
        );
        engine.reconcile(root.to_str().unwrap(), false).unwrap();
        assert!(engine.retry.entries.is_empty());
        assert!(
            RetryState::new(engine.retry_path.clone(), 900., 86400.)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn retry_discovery_prunes_only_the_google_drive_internal_store_and_persists() {
        let (temp, mut engine, _root) = fixture();
        engine.policy.roots = vec!["/Users/jaso-retry-fixture".into()];
        let root = Path::new("/Users/jaso-retry-fixture");
        let paths = [
            root.join("Library/CloudStorage/GoogleDrive-person/.tmp/internal"),
            root.join("Library/CloudStorage/GoogleDrive-person/My Drive/.tmp/user-file"),
            root.join("Library/CloudStorage/OneDrive-person/.tmp/user-file"),
        ];
        for path in &paths {
            engine.retry.entries.insert(
                path.to_string_lossy().into_owned(),
                crate::journal::RetryRecord {
                    signature: vec![],
                    reason: "1".into(),
                    count: 1,
                    last_failure: 0.0,
                    next_retry: 1.0,
                },
            );
        }
        engine.retry.save().unwrap();
        let before = std::fs::read(temp.path().join("retry.json")).unwrap();
        engine.apply = false;
        engine.retry_paths(crate::model::now()).unwrap();
        assert_eq!(
            std::fs::read(temp.path().join("retry.json")).unwrap(),
            before
        );
        engine.apply = true;
        let due = engine.retry_paths(crate::model::now()).unwrap();
        assert_eq!(due.len(), 2);
        let saved = RetryState::new(engine.retry_path.clone(), 900., 86400.).unwrap();
        assert_eq!(saved.entries.len(), 2);
        for path in &paths[1..] {
            assert_eq!(saved.entries[path.to_str().unwrap()].next_retry, 1.0);
        }
        assert!(!temp.path().join("renames.jsonl").exists());
        assert!(!temp.path().join("pending.json").exists());
    }

    #[test]
    fn retry_cleanup_waits_for_successful_parent_enumeration() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, mut engine, root) = fixture();
        let parent = root.join("blocked");
        std::fs::create_dir(&parent).unwrap();
        let source = parent.join("missing").to_string_lossy().into_owned();
        engine.retry.failure(&source, &source, "1");
        engine.retry.entries.get_mut(&source).unwrap().next_retry = 1.0;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o0)).unwrap();
        let result = engine.reconcile(parent.to_str().unwrap(), false).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!result.errors.is_empty());
        assert!(engine.retry.entries.contains_key(&source));
        engine.reconcile(parent.to_str().unwrap(), false).unwrap();
        assert!(engine.retry.entries.is_empty());
    }

    #[test]
    fn retry_cleanup_follows_the_actual_parent_spelling() {
        let (_temp, mut engine, root) = fixture();
        let parent = root.join("폴더".nfd().collect::<String>());
        std::fs::create_dir(&parent).unwrap();
        let source = contents(&parent, "한글");
        engine.retry.failure(
            source.to_str().unwrap(),
            parent.join("한글").to_str().unwrap(),
            "1",
        );
        engine
            .retry
            .entries
            .get_mut(source.to_str().unwrap())
            .unwrap()
            .next_retry = 1.0;
        std::fs::rename(&parent, root.join("폴더")).unwrap();
        let result = engine.reconcile(parent.to_str().unwrap(), false).unwrap();
        assert!(result.errors.is_empty());
        assert!(engine.retry.entries.is_empty());
    }

    #[test]
    fn retry_cleanup_handles_a_removed_parent_without_requeueing_forever() {
        let (_temp, mut engine, root) = fixture();
        let parent = root.join("removed");
        let source = parent.join("missing").to_string_lossy().into_owned();
        engine.retry.failure(&source, &source, "1");
        engine.retry.entries.get_mut(&source).unwrap().next_retry = 1.0;
        let result = engine.reconcile(parent.to_str().unwrap(), false).unwrap();
        assert!(result.errors.is_empty());
        assert!(engine.retry.entries.is_empty());
    }

    #[test]
    fn retry_discovery_does_not_requeue_an_individually_excluded_source() {
        let (_temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        engine.retry.failure(
            source.to_str().unwrap(),
            root.join("한글").to_str().unwrap(),
            "1",
        );
        engine
            .retry
            .entries
            .get_mut(source.to_str().unwrap())
            .unwrap()
            .next_retry = 1.0;
        engine
            .policy
            .excludes
            .push(source.to_string_lossy().into_owned());
        assert!(engine.policy.accepts(root.to_str().unwrap()));
        assert!(engine.retry_paths(crate::model::now()).unwrap().is_empty());
        engine.reconcile(root.to_str().unwrap(), false).unwrap();
        assert!(engine.retry_paths(crate::model::now()).unwrap().is_empty());
        assert_eq!(
            engine.retry.entries[source.to_str().unwrap()].next_retry,
            1.0
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"owned contents");
    }

    #[test]
    fn retry_cleanup_retires_children_when_parent_is_replaced_by_file_or_symlink() {
        for symlink in [false, true] {
            let (temp, mut engine, root) = fixture();
            let parent = root.join("replaced");
            std::fs::create_dir(&parent).unwrap();
            let source = contents(&parent, "한글");
            engine.retry.failure(
                source.to_str().unwrap(),
                parent.join("한글").to_str().unwrap(),
                "1",
            );
            engine
                .retry
                .entries
                .get_mut(source.to_str().unwrap())
                .unwrap()
                .next_retry = 1.0;
            let unrelated = root.join("unrelated");
            engine.retry.failure(
                unrelated.to_str().unwrap(),
                unrelated.to_str().unwrap(),
                "1",
            );
            std::fs::remove_file(&source).unwrap();
            std::fs::remove_dir(&parent).unwrap();
            let outside = temp.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            let protected = contents(&outside, "한글");
            if symlink {
                std::os::unix::fs::symlink(&outside, &parent).unwrap();
            } else {
                std::fs::write(&parent, b"replacement").unwrap();
            }
            let result = engine.reconcile(parent.to_str().unwrap(), false).unwrap();
            assert!(result.errors.is_empty());
            assert!(
                !engine.retry.entries.contains_key(source.to_str().unwrap()),
                "symlink={symlink}"
            );
            assert!(
                engine
                    .retry
                    .entries
                    .contains_key(unrelated.to_str().unwrap())
            );
            assert!(
                RetryState::new(engine.retry_path.clone(), 900., 86400.)
                    .unwrap()
                    .entries
                    .contains_key(unrelated.to_str().unwrap())
            );
            assert_eq!(std::fs::read(&protected).unwrap(), b"owned contents");
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn guarded_fallback_records_marker_ownership_and_normalizes_symlink() {
        let (temp, mut engine, root) = fixture();
        contents(&root, "한글");
        std::os::unix::fs::symlink(
            "missing-owned-target",
            root.join("링크".nfd().collect::<String>()),
        )
        .unwrap();
        engine.exclusive_error = Some(libc::ENOTSUP);
        let pending = temp.path().join("pending.json");
        engine.hook = Some(Box::new(move |point, operation| {
            if point == "marker-created" {
                let disk: Value = serde_json::from_reader(File::open(&pending).unwrap()).unwrap();
                assert_eq!(disk["phase"], "marker-intent");
                assert_eq!(disk["marker"], operation["marker"]);
            }
            Ok(())
        }));
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 2);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            std::fs::read_link(root.join("링크")).unwrap(),
            PathBuf::from("missing-owned-target")
        );
        for record in journal_records(&temp.path().join("renames.jsonl")).unwrap() {
            assert_eq!(record["rename_mode"], "guarded");
            let directory = File::open(&root).unwrap();
            let held = open_entry(directory.as_raw_fd(), record["new"].as_str().unwrap()).unwrap();
            assert!(
                marker_get(held.as_raw_fd(), record["marker"]["name"].as_str().unwrap())
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn guarded_crashes_at_marker_attachment_each_hop_and_cleanup_recover() {
        for (crash_point, hop) in [
            ("marker-created", 1),
            ("moved", 1),
            ("moved", 2),
            ("marker-removed", 1),
        ] {
            let (temp, mut engine, root) = fixture();
            let source = contents(&root, "한글");
            engine.exclusive_error = Some(libc::ENOTSUP);
            let mut count = 0;
            engine.hook = Some(Box::new(move |point, _| {
                if point == crash_point {
                    count += 1;
                    if count == hop {
                        panic!("owned fixture crash");
                    }
                }
                Ok(())
            }));
            catch_crash(&mut engine, &root);
            let record = engine.recover().unwrap().unwrap();
            if crash_point == "marker-created" {
                assert_eq!(record["status"], "not-started");
                assert_eq!(std::fs::read(&source).unwrap(), b"owned contents");
            } else {
                assert_eq!(record["status"], "renamed");
                assert_eq!(std::fs::read(root.join("한글")).unwrap(), b"owned contents");
            }
            assert!(!temp.path().join("pending.json").exists());
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn guarded_cleanup_crash_does_not_adopt_replaced_target() {
        let (temp, mut engine, root) = fixture();
        contents(&root, "한글");
        engine.exclusive_error = Some(libc::ENOTSUP);
        engine.hook = Some(Box::new(|point, _| {
            if point == "marker-removed" {
                panic!("crash before finalized identity");
            }
            Ok(())
        }));
        catch_crash(&mut engine, &root);
        let target = root.join("한글");
        std::fs::rename(&target, temp.path().join("saved-owned")).unwrap();
        std::fs::write(&target, b"replacement").unwrap();
        let record = engine.recover().unwrap().unwrap();
        assert_eq!(record["identity_finalization_unavailable"], true);
        assert_eq!(std::fs::read(&target).unwrap(), b"replacement");
        assert_eq!(
            crate::journal::revert(&temp.path().join("renames.jsonl"), None).unwrap(),
            (0, 1)
        );
    }
    #[test]
    fn malformed_marker_is_rejected_before_any_attribute_access() {
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        let info = std::fs::metadata(&source).unwrap();
        let operation = json!({"version":1,"operation_id":"owned","dir":root,"old":source.file_name().unwrap().to_str().unwrap(),"new":"한글","temporary_path":root.join(".jaso-owned.__jaso_nfc_tmp__"),"identity":[info.dev(),info.ino()],"marker":{"name":"com.someone.else","token":"owned"}});
        atomic_json(&temp.path().join("pending.json"), &operation).unwrap();
        assert!(
            engine
                .recover()
                .unwrap_err()
                .downcast_ref::<PendingRecoveryError>()
                .is_some()
        );
        assert_eq!(std::fs::read(source).unwrap(), b"owned contents");
    }

    #[cfg(target_os = "macos")]
    fn identity_churn_hook(crash: Option<(&'static str, u32)>) -> RecoveryHook {
        let mut count = 0;
        Box::new(move |point, operation| {
            if matches!(point, "marker-created" | "moved" | "marker-removed") {
                let parent = Path::new(operation["dir"].as_str().unwrap());
                let paths = [
                    PathBuf::from(operation["temporary_path"].as_str().unwrap()),
                    parent.join(operation["new"].as_str().unwrap()),
                    parent.join(operation["old"].as_str().unwrap()),
                ];
                let metadata = paths
                    .iter()
                    .find_map(|path| std::fs::symlink_metadata(path).ok())
                    .unwrap();
                crate::native_names::test_shift_identity([metadata.dev(), metadata.ino()]);
                if let Some((crash_point, hop)) = crash
                    && point == crash_point
                {
                    count += 1;
                    if count == hop {
                        panic!("crash during synthetic exFAT identity churn");
                    }
                }
            }
            Ok(())
        })
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn empty_file_identity_changes_at_every_marker_and_rename_are_durably_finalized() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::native_names::test_clear_identities();
            }
        }
        let _reset = Reset;
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "빈파일");
        std::fs::write(&source, b"").unwrap();
        engine.exclusive_error = Some(libc::ENOTSUP);
        engine.hook = Some(identity_churn_hook(None));
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 1);
        assert!(result.errors.is_empty());
        let rows = journal_records(&temp.path().join("renames.jsonl")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["operation_id"], rows[1]["operation_id"]);
        assert_eq!(rows[1]["identity_finalized"], true);
        let metadata = std::fs::metadata(root.join("빈파일")).unwrap();
        let final_identity =
            crate::native_names::test_adjust_identity([metadata.dev(), metadata.ino()]);
        assert_eq!(rows.last().unwrap()["identity"], json!(final_identity));
        assert_eq!(
            crate::journal::revert(&temp.path().join("renames.jsonl"), None).unwrap(),
            (1, 0)
        );
        assert!(source.exists());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn changed_identity_is_recovered_after_marker_attachment_and_each_guarded_hop() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::native_names::test_clear_identities();
            }
        }
        let _reset = Reset;
        for (point, hop) in [("marker-created", 1), ("moved", 1), ("moved", 2)] {
            let (temp, mut engine, root) = fixture();
            let source = contents(&root, "빈파일");
            std::fs::write(&source, b"").unwrap();
            engine.exclusive_error = Some(libc::ENOTSUP);
            engine.hook = Some(identity_churn_hook(Some((point, hop))));
            catch_crash(&mut engine, &root);
            let record = engine.recover().unwrap().unwrap();
            assert_eq!(
                record["status"],
                if point == "marker-created" {
                    "not-started"
                } else {
                    "renamed"
                }
            );
            assert!(!temp.path().join("pending.json").exists());
            let final_path = if point == "marker-created" {
                source
            } else {
                root.join("빈파일")
            };
            let metadata = std::fs::metadata(&final_path).unwrap();
            assert_eq!(
                record["identity"],
                json!(crate::native_names::test_adjust_identity([
                    metadata.dev(),
                    metadata.ino()
                ]))
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn guarded_fallback_is_limited_to_unsupported_exclusive_rename_errors() {
        for code in [
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::ENOSYS,
            libc::EACCES,
            libc::EEXIST,
        ] {
            let (temp, mut engine, root) = fixture();
            let source = contents(&root, "한글");
            engine.exclusive_error = Some(code);
            let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
            if [libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS].contains(&code) {
                assert_eq!(result.renamed, 1, "unsupported errno {code}");
            } else {
                assert_eq!(result.renamed, 0);
                assert!(!result.errors.is_empty());
                assert_eq!(std::fs::read(&source).unwrap(), b"owned contents");
            }
            assert!(!temp.path().join("pending.json").exists());
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn lost_marker_staged_replacement_is_restored_without_touching_true_owner() {
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        let saved = temp.path().join("saved-true-owner");
        let original_source = source.clone();
        let original_saved = saved.clone();
        engine.exclusive_error = Some(libc::ENOTSUP);
        engine.hook = Some(Box::new(move |point, operation| {
            if point == "pending-written" && operation["phase"] == "moving" {
                std::fs::rename(&original_source, &original_saved).unwrap();
                std::fs::write(&original_source, b"replacement").unwrap();
                std::fs::rename(
                    &original_source,
                    operation["temporary_path"].as_str().unwrap(),
                )
                .unwrap();
                panic!("crash after another writer swapped source before first hop");
            }
            Ok(())
        }));
        catch_crash(&mut engine, &root);
        let record = engine.recover().unwrap().unwrap();
        assert_eq!(record["status"], "rolled-back");
        assert!(record.get("expected_marker").is_some());
        assert_eq!(std::fs::read(&source).unwrap(), b"replacement");
        assert_eq!(std::fs::read(&saved).unwrap(), b"owned contents");
        assert!(!temp.path().join("pending.json").exists());
    }
    #[test]
    fn pending_persistence_failure_prevents_the_first_mutation() {
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        // A directory at the pending file path makes the atomic rename fail.
        let pending = temp.path().join("intent-as-directory");
        std::fs::create_dir(&pending).unwrap();
        engine.pending_path = Some(pending.clone());
        let error = engine.reconcile(root.to_str().unwrap(), true).unwrap_err();
        assert!(error.downcast_ref::<PendingRecoveryError>().is_some());
        assert_eq!(std::fs::read(&source).unwrap(), b"owned contents");
        assert!(pending.is_dir());
    }
    #[test]
    fn filesystem_that_restores_decomposition_is_rolled_back_and_deferred() {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_DECOMPOSE_STORED.with(|value| value.set(false));
            }
        }
        let _reset = Reset;
        let (temp, mut engine, root) = fixture();
        let source = contents(&root, "한글");
        TEST_DECOMPOSE_STORED.with(|value| value.set(true));
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 0);
        assert!(!result.errors.is_empty());
        assert_eq!(std::fs::read(source).unwrap(), b"owned contents");
        assert!(!temp.path().join("pending.json").exists());
        assert_eq!(engine.retry.entries.len(), 1);
    }
    #[test]
    fn hardlink_alias_does_not_confuse_stored_name_verification() {
        let (_temp, mut engine, root) = fixture();
        let original = root.join("plain");
        std::fs::write(&original, b"owned contents").unwrap();
        std::fs::hard_link(&original, root.join("한글".nfd().collect::<String>())).unwrap();
        let result = engine.reconcile(root.to_str().unwrap(), true).unwrap();
        assert_eq!(result.renamed, 1);
        assert!(result.errors.is_empty());
        assert_eq!(
            std::fs::metadata(original).unwrap().ino(),
            std::fs::metadata(root.join("한글")).unwrap().ino()
        );
    }
}
