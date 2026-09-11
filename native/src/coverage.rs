//! Discover user-data roots through fixed catalogs, without walking documents.
use crate::directory_io::{DirectoryIo, DirectoryMaterialization};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Coverage {
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub catalog_roots: Vec<String>,
    pub unavailable: Vec<String>,
    #[serde(default)]
    pub unavailable_reasons: BTreeMap<String, String>,
    pub root_excludes: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct Account {
    pub uid: u32,
    pub name: String,
    pub home: String,
    pub shell: String,
}
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub directory: bool,
    pub regular: bool,
    pub device: u64,
}

pub trait CatalogReader {
    fn metadata(&self, path: &Path) -> io::Result<Node>;
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn readable(&self, path: &Path) -> io::Result<()>;
    fn is_mount(&self, path: &Path) -> io::Result<bool>;
    fn accounts(&self) -> io::Result<Vec<Account>>;
    fn startup_devices(&self) -> BTreeSet<u64>;
}

pub struct NativeCatalog;

fn catalog_io<T>(run: impl FnOnce(&DirectoryIo) -> io::Result<T>) -> io::Result<T> {
    let deadline = DirectoryIo::begin()?;
    let _materialization = DirectoryMaterialization::begin()?;
    let result = run(&deadline);
    deadline.check()?;
    result
}
fn path_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in catalog path"))
}
fn catalog_stat(path: &Path, follow: bool, deadline: &DirectoryIo) -> io::Result<libc::stat> {
    deadline.check()?;
    let path = path_cstring(path)?;
    let mut value = std::mem::MaybeUninit::uninit();
    let code = unsafe {
        if follow {
            libc::stat(path.as_ptr(), value.as_mut_ptr())
        } else {
            libc::lstat(path.as_ptr(), value.as_mut_ptr())
        }
    };
    let result = if code == 0 {
        Ok(unsafe { value.assume_init() })
    } else {
        Err(io::Error::last_os_error())
    };
    deadline.progress()?;
    result
}
fn open_catalog_directory(path: &Path, deadline: &DirectoryIo) -> io::Result<File> {
    deadline.check()?;
    let path = path_cstring(path)?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    let result = if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    };
    deadline.progress()?;
    result
}
#[cfg(test)]
type CatalogIoHook = Box<dyn FnMut(&str, &Path)>;
#[cfg(test)]
thread_local! {
    static TEST_CATALOG_IO: std::cell::RefCell<Option<CatalogIoHook>> = const {std::cell::RefCell::new(None)};
}
#[cfg(test)]
fn test_catalog_io(stage: &str, path: &Path) {
    TEST_CATALOG_IO.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(stage, path);
        }
    });
}
impl CatalogReader for NativeCatalog {
    fn metadata(&self, path: &Path) -> io::Result<Node> {
        catalog_io(|deadline| {
            #[cfg(test)]
            test_catalog_io("metadata", path);
            let value = catalog_stat(path, false, deadline)?;
            Ok(Node {
                directory: value.st_mode & libc::S_IFMT == libc::S_IFDIR,
                regular: value.st_mode & libc::S_IFMT == libc::S_IFREG,
                device: value.st_dev as u64,
            })
        })
    }
    fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        catalog_io(|deadline| {
            let raw = open_catalog_directory(path, deadline)?.into_raw_fd();
            #[cfg(test)]
            test_catalog_io("enumerate", path);
            let pointer = unsafe { libc::fdopendir(raw) };
            if pointer.is_null() {
                let error = io::Error::last_os_error();
                unsafe { libc::close(raw) };
                deadline.check()?;
                return Err(error);
            }
            struct Directory(*mut libc::DIR);
            impl Drop for Directory {
                fn drop(&mut self) {
                    unsafe { libc::closedir(self.0) };
                }
            }
            let directory = Directory(pointer);
            deadline.progress()?;
            let mut entries = Vec::new();
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
                let entry = unsafe { libc::readdir(directory.0) };
                let error = io::Error::last_os_error();
                deadline.progress()?;
                if entry.is_null() {
                    if error.raw_os_error().unwrap_or(0) != 0 {
                        return Err(error);
                    }
                    break;
                }
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                entries.push(path.join(OsStr::from_bytes(name)));
                #[cfg(test)]
                test_catalog_io("list-entry", path);
                deadline.check()?;
            }
            Ok(entries)
        })
    }
    fn readable(&self, path: &Path) -> io::Result<()> {
        catalog_io(|deadline| {
            #[cfg(test)]
            test_catalog_io("readable", path);
            // Permission to open the directory is sufficient here. fdopendir
            // may eagerly enumerate children and fetch provider metadata.
            open_catalog_directory(path, deadline).map(drop)
        })
    }
    fn is_mount(&self, path: &Path) -> io::Result<bool> {
        catalog_io(|deadline| {
            #[cfg(test)]
            test_catalog_io("mount", path);
            let current = catalog_stat(path, true, deadline)?;
            let parent = catalog_stat(&path.join(".."), true, deadline)?;
            Ok(current.st_dev != parent.st_dev || current.st_ino == parent.st_ino)
        })
    }
    fn accounts(&self) -> io::Result<Vec<Account>> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK
            .lock()
            .map_err(|_| io::Error::other("account catalog lock poisoned"))?;
        let deadline = DirectoryIo::begin()?;
        struct Accounts;
        impl Drop for Accounts {
            fn drop(&mut self) {
                unsafe { libc::endpwent() };
            }
        }
        let _accounts = Accounts;
        let mut accounts = vec![];
        unsafe {
            libc::setpwent();
            deadline.progress()?;
            loop {
                #[cfg(target_os = "macos")]
                {
                    *libc::__error() = 0;
                }
                #[cfg(target_os = "linux")]
                {
                    *libc::__errno_location() = 0;
                }
                let entry = libc::getpwent();
                let error = io::Error::last_os_error();
                deadline.progress()?;
                if entry.is_null() {
                    if error.raw_os_error().unwrap_or(0) != 0 {
                        return Err(error);
                    }
                    break;
                }
                let text = |ptr: *const libc::c_char| {
                    if ptr.is_null() {
                        String::new()
                    } else {
                        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
                    }
                };
                accounts.push(Account {
                    uid: (*entry).pw_uid,
                    name: text((*entry).pw_name),
                    home: text((*entry).pw_dir),
                    shell: text((*entry).pw_shell),
                });
            }
        }
        drop(_accounts);
        deadline.check()?;
        Ok(accounts)
    }
    fn startup_devices(&self) -> BTreeSet<u64> {
        ["/", "/System/Volumes/Data"]
            .iter()
            .filter_map(|p| {
                catalog_io(|deadline| catalog_stat(Path::new(p), true, deadline))
                    .ok()
                    .map(|m| m.st_dev as u64)
            })
            .collect()
    }
}

fn absolute(path: &Path) -> PathBuf {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| "/".into())
            .join(path)
    };
    let mut result = PathBuf::new();
    for component in full.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            _ => result.push(component.as_os_str()),
        }
    }
    result
}
fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

struct Discovery<'a, R: CatalogReader> {
    reader: &'a R,
    roots: BTreeSet<String>,
    catalogs: BTreeSet<String>,
    unavailable: BTreeSet<String>,
    unavailable_reasons: BTreeMap<String, String>,
    scoped: HashMap<String, BTreeSet<String>>,
    homes: BTreeSet<String>,
}
impl<R: CatalogReader> Discovery<'_, R> {
    fn failed(&mut self, path: &Path, operation: &str, error: io::Error) {
        let path = text(path);
        self.unavailable.insert(path.clone());
        self.unavailable_reasons
            .entry(path)
            .or_insert_with(|| format!("{operation}: {error}"));
    }
    fn info(&mut self, path: &Path) -> Option<Node> {
        match self.reader.metadata(path) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                self.failed(path, "metadata", error);
                None
            }
        }
    }
    fn directory(&mut self, path: &Path) -> bool {
        self.info(path).is_some_and(|v| v.directory)
    }
    fn readable(&mut self, path: &Path) {
        if let Err(error) = self.reader.readable(path) {
            self.failed(path, "readable", error);
        }
    }
    fn catalog(&mut self, path: &Path) -> Vec<(PathBuf, Node)> {
        if !self.directory(path) {
            return vec![];
        }
        self.catalogs.insert(text(path));
        let mut found = vec![];
        match self.reader.list(path) {
            Ok(entries) => {
                for entry in entries {
                    let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                        self.failed(
                            &entry,
                            "list",
                            io::Error::new(io::ErrorKind::InvalidData, "catalog name is not UTF-8"),
                        );
                        continue;
                    };
                    if name.starts_with('.') {
                        continue;
                    }
                    if let Some(info) = self.info(&entry)
                        && info.directory
                    {
                        found.push((entry, info));
                    }
                }
            }
            Err(error) => {
                self.failed(path, "list", error);
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }
    fn home(&mut self, path: &Path, shared: bool) {
        let name = text(path);
        if self.homes.contains(&name) || !self.directory(path) {
            return;
        }
        self.homes.insert(name.clone());
        self.roots.insert(name.clone());
        self.readable(path);
        if shared {
            return;
        }
        self.catalogs.insert(name.clone());
        self.scoped
            .entry(name)
            .or_default()
            .extend([text(&path.join("Library")), text(&path.join(".Trash"))]);
        let library = path.join("Library");
        if !self.directory(&library) {
            return;
        }
        self.catalogs.insert(text(&library));
        self.readable(&library);
        for relative in [
            "Library/CloudStorage",
            "Library/Mobile Documents/com~apple~CloudDocs",
        ] {
            let cloud = path.join(relative);
            if relative.ends_with("com~apple~CloudDocs") {
                let parent = cloud.parent().unwrap();
                if !self.directory(parent) {
                    continue;
                }
                self.catalogs.insert(text(parent));
                self.readable(parent);
            }
            if self.directory(&cloud) {
                self.roots.insert(text(&cloud));
                self.readable(&cloud);
            }
        }
    }
}

pub fn discover_user_coverage() -> Coverage {
    discover_with(Path::new("/Users"), Path::new("/Volumes"), &NativeCatalog)
}
pub fn discover_with(users: &Path, volumes: &Path, reader: &impl CatalogReader) -> Coverage {
    let users = absolute(users);
    let volumes = absolute(volumes);
    let mut d = Discovery {
        reader,
        roots: BTreeSet::new(),
        catalogs: BTreeSet::new(),
        unavailable: BTreeSet::new(),
        unavailable_reasons: BTreeMap::new(),
        scoped: HashMap::new(),
        homes: BTreeSet::new(),
    };
    for (path, _) in d.catalog(&users) {
        d.home(&path, path.file_name().is_some_and(|n| n == "Shared"));
    }
    match reader.accounts() {
        Ok(accounts) => {
            for account in accounts {
                let home = absolute(Path::new(&account.home));
                if account.uid < 500
                    || account.name.starts_with('_')
                    || !Path::new(&account.home).is_absolute()
                    || [
                        "/usr/bin/false",
                        "/bin/false",
                        "/sbin/nologin",
                        "/usr/sbin/nologin",
                    ]
                    .contains(&account.shell.as_str())
                    || [
                        "/",
                        "/var/empty",
                        "/private/var/empty",
                        "/dev/null",
                        "/nonexistent",
                    ]
                    .contains(&home.to_str().unwrap_or(""))
                {
                    continue;
                }
                d.home(&home, false);
            }
        }
        Err(error) => {
            d.failed(&users, "accounts", error);
        }
    }
    let devices = reader.startup_devices();
    for (path, node) in d.catalog(&volumes) {
        if devices.contains(&node.device) {
            continue;
        }
        match reader.is_mount(&path) {
            Ok(true) => (),
            Ok(false) => continue,
            Err(error) => {
                d.failed(&path, "is_mount", error);
                continue;
            }
        }
        d.roots.insert(text(&path));
        d.readable(&path);
        d.scoped.entry(text(&path)).or_default().extend(
            [
                ".DocumentRevisions-V100",
                ".HFS+ Private Directory Data\r",
                ".Spotlight-V100",
                ".TemporaryItems",
                ".Trashes",
                ".fseventsd",
                ".vol",
            ]
            .iter()
            .map(|n| text(&path.join(n))),
        );
        if d.info(&path.join("System/Library/CoreServices/SystemVersion.plist"))
            .is_some_and(|n| n.regular)
        {
            d.scoped.entry(text(&path)).or_default().extend(
                [
                    "Applications",
                    "Library",
                    "System",
                    "bin",
                    "dev",
                    "private",
                    "sbin",
                    "usr",
                ]
                .iter()
                .map(|n| text(&path.join(n))),
            );
            for (home, _) in d.catalog(&path.join("Users")) {
                d.home(&home, home.file_name().is_some_and(|n| n == "Shared"));
            }
        }
    }
    let excludes: BTreeSet<_> = d.scoped.values().flatten().cloned().collect();
    Coverage {
        roots: d.roots.into_iter().collect(),
        excludes: excludes.into_iter().collect(),
        catalog_roots: d.catalogs.into_iter().collect(),
        unavailable: d.unavailable.into_iter().collect(),
        unavailable_reasons: d.unavailable_reasons,
        root_excludes: d
            .scoped
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};

    struct Fixture {
        base: tempfile::TempDir,
        mounts: BTreeSet<PathBuf>,
        denied: BTreeSet<PathBuf>,
        accounts: Vec<Account>,
        account_error: bool,
        devices: BTreeSet<u64>,
        listings: RefCell<Vec<PathBuf>>,
        failures: HashMap<(PathBuf, &'static str), i32>,
    }
    impl Fixture {
        fn new() -> Self {
            let value = Self {
                base: tempfile::tempdir().unwrap(),
                mounts: BTreeSet::new(),
                denied: BTreeSet::new(),
                accounts: vec![],
                account_error: false,
                devices: BTreeSet::new(),
                listings: RefCell::new(vec![]),
                failures: HashMap::new(),
            };
            value.mkdir("Users");
            value.mkdir("Volumes");
            value
        }
        fn path(&self, name: &str) -> PathBuf {
            self.base.path().join(name)
        }
        fn mkdir(&self, name: &str) -> String {
            let path = self.path(name);
            fs::create_dir_all(&path).unwrap();
            path.to_str().unwrap().into()
        }
        fn discover(&self) -> Coverage {
            discover_with(&self.path("Users"), &self.path("Volumes"), self)
        }
        fn check(&self, path: &Path, operation: &'static str) -> io::Result<()> {
            match self.failures.get(&(path.to_owned(), operation)) {
                Some(code) => Err(io::Error::from_raw_os_error(*code)),
                None => Ok(()),
            }
        }
    }
    impl CatalogReader for Fixture {
        fn metadata(&self, path: &Path) -> io::Result<Node> {
            self.check(path, "metadata")?;
            let value = fs::symlink_metadata(path)?;
            Ok(Node {
                directory: value.is_dir(),
                regular: value.is_file(),
                device: value.dev(),
            })
        }
        fn list(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            self.check(path, "list")?;
            self.readable(path)?;
            NativeCatalog.list(path)
        }
        fn readable(&self, path: &Path) -> io::Result<()> {
            self.check(path, "readable")?;
            self.listings.borrow_mut().push(path.into());
            if self.denied.contains(path) {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                fs::read_dir(path).map(drop)
            }
        }
        fn is_mount(&self, path: &Path) -> io::Result<bool> {
            self.check(path, "is_mount")?;
            Ok(self.mounts.contains(path))
        }
        fn accounts(&self) -> io::Result<Vec<Account>> {
            if self.account_error {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(self.accounts.clone())
            }
        }
        fn startup_devices(&self) -> BTreeSet<u64> {
            self.devices.clone()
        }
    }
    #[test]
    fn discovery_preserves_the_operation_and_original_error_for_unavailable_paths() {
        for operation in ["metadata", "list", "readable", "is_mount"] {
            for code in [libc::EACCES, libc::ETIMEDOUT, libc::EINTR] {
                let mut f = Fixture::new();
                let home = f.mkdir("Users/person");
                let mounted = f.mkdir("Volumes/External");
                f.mounts.insert(PathBuf::from(&mounted));
                let path = match operation {
                    "list" => f.path("Users"),
                    "is_mount" => PathBuf::from(mounted),
                    _ => PathBuf::from(home),
                };
                f.failures.insert((path.clone(), operation), code);
                let coverage = f.discover();
                assert!(coverage.unavailable.contains(&text(&path)));
                let encoded = serde_json::to_value(&coverage).unwrap();
                let expected = format!("{operation}: {}", io::Error::from_raw_os_error(code));
                assert_eq!(encoded["unavailable_reasons"][text(&path)], expected);
            }
        }
    }
    #[test]
    fn native_catalog_metadata_and_access_checks_stop_after_a_deadline() {
        let f = Fixture::new();
        for stage in ["metadata", "readable", "mount"] {
            TEST_CATALOG_IO.set(Some(Box::new(move |point, _| {
                if point == stage {
                    crate::directory_io::tests::blocking_read();
                }
            })));
            let result =
                crate::directory_io::test_timeout(std::time::Duration::from_millis(30), || {
                    match stage {
                        "metadata" => NativeCatalog.metadata(&f.path("Users")).map(|_| ()),
                        "readable" => NativeCatalog.readable(&f.path("Users")),
                        _ => NativeCatalog.is_mount(&f.path("Users")).map(|_| ()),
                    }
                });
            TEST_CATALOG_IO.set(None);
            assert_eq!(
                result.unwrap_err().raw_os_error(),
                Some(libc::ETIMEDOUT),
                "{stage}"
            );
        }
    }
    #[test]
    fn readable_only_checks_directory_open_without_enumerating_children() {
        let f = Fixture::new();
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = calls.clone();
        TEST_CATALOG_IO.set(Some(Box::new(move |stage, _| {
            seen.borrow_mut().push(stage.to_owned())
        })));
        let result = NativeCatalog.readable(&f.path("Users"));
        TEST_CATALOG_IO.set(None);
        result.unwrap();
        assert!(
            !calls
                .borrow()
                .iter()
                .any(|stage| stage == "enumerate" || stage == "list-entry")
        );
        std::fs::write(f.path("file"), b"owned").unwrap();
        assert!(NativeCatalog.readable(&f.path("file")).is_err());
    }
    #[test]
    fn timed_out_catalog_discards_partial_homes_and_continues_external_discovery() {
        let mut f = Fixture::new();
        let hidden_by_timeout = f.mkdir("Users/person");
        let external = f.mkdir("Volumes/Books");
        f.mounts.insert(f.path("Volumes/Books"));
        let users = f.path("Users");
        TEST_CATALOG_IO.set(Some(Box::new(move |stage, path| {
            if stage == "list-entry" && path == users {
                crate::directory_io::tests::blocking_read();
            }
        })));
        let coverage =
            crate::directory_io::test_timeout(std::time::Duration::from_millis(30), || {
                f.discover()
            });
        TEST_CATALOG_IO.set(None);
        assert!(coverage.unavailable.contains(&text(&f.path("Users"))));
        assert!(!coverage.roots.contains(&hidden_by_timeout));
        assert!(coverage.roots.contains(&external));
    }
    #[test]
    fn visible_accounts_shared_and_catalog_ancestors() {
        let f = Fixture::new();
        let home = f.mkdir("Users/person");
        let shared = f.mkdir("Users/Shared/Library");
        f.mkdir("Users/.hidden");
        symlink(&home, f.path("Users/alias")).unwrap();
        let c = f.discover();
        assert_eq!(c.roots.len(), 2);
        assert!(c.roots.contains(&home));
        assert!(c.catalog_roots.contains(&home));
        assert!(!c.excludes.contains(&shared));
        assert_eq!(
            c.root_excludes[&home],
            vec![format!("{home}/.Trash"), format!("{home}/Library")]
        );
    }
    #[test]
    fn cloud_roots_and_creation_catalogs_override_only_home_exclusions() {
        let f = Fixture::new();
        let home = f.mkdir("Users/person");
        let library = f.mkdir("Users/person/Library");
        let mobile = f.mkdir("Users/person/Library/Mobile Documents");
        let cloud = f.mkdir("Users/person/Library/CloudStorage");
        let icloud = f.mkdir("Users/person/Library/Mobile Documents/com~apple~CloudDocs");
        let c = f.discover();
        for root in [&home, &cloud, &icloud] {
            assert!(c.roots.contains(root));
        }
        for catalog in [&home, &library, &mobile] {
            assert!(c.catalog_roots.contains(catalog));
        }
        assert!(!c.root_excludes.contains_key(&cloud));
        let policy = crate::policy::Policy::new(c.roots, vec![], vec![], vec![], c.root_excludes);
        assert!(policy.accepts(&format!("{cloud}/Provider/file")));
        assert!(!policy.accepts(&format!("{library}/Caches/file")));
    }
    #[test]
    fn only_real_external_mounts_and_bootable_internal_exclusions() {
        let mut f = Fixture::new();
        let ordinary = f.mkdir("Volumes/Books/Library");
        let boot = f.mkdir("Volumes/Boot/System/Library/CoreServices");
        fs::write(Path::new(&boot).join("SystemVersion.plist"), b"not read").unwrap();
        let cloud = f.mkdir("Volumes/Boot/Users/person/Library/CloudStorage");
        f.mkdir("Volumes/Unmounted");
        f.mkdir("Volumes/.Hidden");
        f.mounts.extend([
            f.path("Volumes/Books"),
            f.path("Volumes/Boot"),
            f.path("Volumes/.Hidden"),
        ]);
        let c = f.discover();
        assert!(c.roots.contains(&cloud));
        assert!(!c.excludes.contains(&ordinary));
        assert!(
            c.excludes
                .contains(&f.path("Volumes/Boot/System").to_str().unwrap().into())
        );
        assert!(
            !c.roots
                .contains(&f.path("Volumes/Unmounted").to_str().unwrap().into())
        );
        assert!(
            c.catalog_roots
                .contains(&f.path("Volumes/Boot/Users").to_str().unwrap().into())
        );
    }
    #[test]
    fn unavailable_roots_and_known_clouds_survive_denied_parent_listing() {
        let mut f = Fixture::new();
        let home = f.mkdir("Users/person");
        let cloud = f.mkdir("Users/person/Library/CloudStorage");
        f.denied
            .extend([PathBuf::from(&home), f.path("Users/person/Library")]);
        let c = f.discover();
        assert!(c.roots.contains(&home));
        assert!(c.roots.contains(&cloud));
        assert!(c.unavailable.contains(&home));
        assert!(!c.unavailable.contains(&cloud));
    }
    #[test]
    fn account_database_errors_are_reported_and_service_accounts_filtered() {
        let mut f = Fixture::new();
        let outside = f.mkdir("Custom/person");
        let service = f.mkdir("Custom/service");
        f.accounts = vec![
            Account {
                uid: 501,
                name: "person".into(),
                home: outside.clone(),
                shell: "/bin/zsh".into(),
            },
            Account {
                uid: 502,
                name: "_service".into(),
                home: service,
                shell: "/bin/zsh".into(),
            },
        ];
        assert_eq!(f.discover().roots, vec![outside]);
        f.account_error = true;
        assert!(
            f.discover()
                .unavailable
                .contains(&f.path("Users").to_str().unwrap().into())
        );
    }
    #[test]
    fn account_home_aliases_cannot_expand_to_operating_system_roots() {
        let mut f = Fixture::new();
        let person = f.mkdir("Custom/person");
        f.accounts = vec![
            "/./".to_owned(),
            "/Users/person/../..".into(),
            "/var/../var/empty".into(),
            format!("{person}/../person"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, home)| Account {
            uid: 501 + i as u32,
            name: format!("person{i}"),
            home,
            shell: "/bin/zsh".into(),
        })
        .collect();
        assert_eq!(f.discover().roots, vec![person]);
    }
    #[test]
    fn no_document_recursion_symlink_library_or_startup_alias() {
        let mut f = Fixture::new();
        let home = f.mkdir("Users/person/Documents/deep");
        let outside = f.mkdir("Other/CloudStorage");
        symlink(
            Path::new(&outside).parent().unwrap(),
            f.path("Users/person/Library"),
        )
        .unwrap();
        let alias = f.mkdir("Volumes/Startup");
        f.mounts.insert(PathBuf::from(alias));
        f.devices.insert(fs::metadata(&home).unwrap().dev());
        let c = f.discover();
        assert_eq!(
            c.roots,
            vec![f.path("Users/person").to_str().unwrap().to_owned()]
        );
        assert!(
            !f.listings
                .borrow()
                .iter()
                .any(|p| p.components().any(|c| c.as_os_str() == "Documents"))
        );
    }
}
