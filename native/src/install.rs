//! Canonical Applications bundle with per-user jobs and locked rollback.
use crate::{
    config::{Config, atomic_json},
    control::RuntimeLock,
    lifecycle::{
        LaunchHost, NativeLaunchHost, disabled_from_output, domain, host_checked, host_loaded,
        host_stop, start_with, stop_label, wait_for_job_absence,
    },
    service::LABEL,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is required"))
}
fn agent(label: &str) -> PathBuf {
    home()
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}
fn menu_label() -> String {
    format!("{LABEL}.menu")
}
pub(crate) fn lifecycle_lock() -> Result<crate::lifecycle::OperationLock> {
    use std::os::unix::fs::DirBuilderExt;
    let directory = home().join("Library/Application Support/jaso-nfc");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    crate::lifecycle::OperationLock::acquire(&directory.join("lifecycle.lock"))
}
pub fn stop() -> Result<()> {
    let _operation = lifecycle_lock()?;
    stop_label(LABEL)
}
/// Pause commands share the setup/install transaction lock. Internal setup
/// staging uses control::set_paused while already holding that lock.
pub fn set_paused(config: &Config, paused: bool) -> Result<bool> {
    let _operation = lifecycle_lock()?;
    crate::control::set_paused(config, paused)
}
pub fn start() -> Result<()> {
    let _operation = lifecycle_lock()?;
    start_with(&mut NativeLaunchHost, &agent(LABEL))
}
pub fn restart() -> Result<()> {
    let _operation = lifecycle_lock()?;
    crate::lifecycle::restart_with(&mut NativeLaunchHost, &agent(LABEL))
}
pub fn startup(mode: &str) -> Result<Value> {
    let _operation = lifecycle_lock()?;
    crate::lifecycle::startup_with(&mut NativeLaunchHost, &[(LABEL, &agent(LABEL))], mode)
}
pub fn uninstall() -> Result<()> {
    let _operation = lifecycle_lock()?;
    let mut applications = Vec::new();
    let executable = std::env::current_exe()?;
    if let Some(macos) = executable.parent()
        && macos.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents) = macos.parent()
        && contents.file_name().is_some_and(|name| name == "Contents")
        && let Some(app) = contents.parent()
        && app.extension().is_some_and(|extension| extension == "app")
    {
        applications.push(app.to_owned());
    }
    applications.extend([
        PathBuf::from("/Applications/Jaso NFC.app"),
        home().join("Applications/Jaso NFC.app"),
    ]);
    let source = applications
        .iter()
        .find(|app| app.join("Contents/MacOS/Jaso NFC").is_file())
        .cloned();
    let helper = source
        .as_ref()
        .map(|app| app.join("Contents/MacOS/Jaso NFC"));
    #[cfg(target_os = "macos")]
    if let Some(helper) = &helper {
        crate::app_bundle::validate_native_gui(helper)?;
    }
    let mut host = NativeHost {
        source: source.unwrap_or_default(),
    };
    uninstall_with(
        &mut host,
        &[agent(LABEL), agent(&menu_label())],
        helper
            .as_deref()
            .map(|helper| (helper, applications.as_slice())),
    )
}
fn uninstall_with(
    host: &mut impl InstallHost,
    paths: &[PathBuf; 2],
    menu: Option<(&Path, &[PathBuf])>,
) -> Result<()> {
    let mut pending = BTreeSet::new();
    host_stop(host, LABEL, &mut pending)?;
    host_stop(host, &menu_label(), &mut pending)?;
    // Stop registration first so a starting worker cannot race another menu
    // into existence after the verified process snapshot.
    if let Some((helper, applications)) = menu {
        let snapshot = host.menu_snapshot(helper, applications)?;
        host.menu_stop(helper, &snapshot)?;
    }
    for path in paths {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn plist(executable: &Path, config: &Path) -> String {
    let arguments = [
        executable.to_str().unwrap(),
        "run",
        "--config",
        config.to_str().unwrap(),
    ];
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>AssociatedBundleIdentifiers</key><array><string>{}</string></array><key>ProgramArguments</key><array>{}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><{}/><key>ThrottleInterval</key><integer>30</integer><key>ProcessType</key><string>{}</string><key>LowPriorityIO</key><true/><key>Nice</key><integer>10</integer><key>Umask</key><integer>63</integer><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>\n",
        xml(LABEL),
        xml(LABEL),
        arguments
            .iter()
            .map(|a| format!("<string>{}</string>", xml(a)))
            .collect::<String>(),
        "true",
        "Background"
    )
}
fn copy_app(source: &Path, destination: &Path) -> Result<()> {
    let out = Command::new("/usr/bin/ditto")
        .arg(source)
        .arg(destination)
        .output()?;
    ensure!(
        out.status.success(),
        "cannot stage application: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}
fn bundle_names(directory: &std::fs::File) -> Result<Vec<Vec<u8>>> {
    use std::os::fd::{AsRawFd, IntoRawFd};
    let listing = crate::native_names::open_at(
        directory.as_raw_fd(),
        ".",
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )?;
    let raw = listing.into_raw_fd();
    let pointer = unsafe { libc::fdopendir(raw) };
    if pointer.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(raw);
        };
        return Err(error.into());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let listing = Directory(pointer);
    let mut names = vec![];
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let next = unsafe { libc::readdir(listing.0) };
        if next.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error.into());
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*next).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    }
    names.sort();
    Ok(names)
}
fn bundle_child(directory: &std::fs::File, name: &[u8]) -> Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    };
    let name = std::ffi::CString::new(name)?;
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    ensure!(
        unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                info.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0,
        "cannot inspect bundle entry: {}",
        std::io::Error::last_os_error()
    );
    let info = unsafe { info.assume_init() };
    let kind = info.st_mode & libc::S_IFMT;
    ensure!(
        kind != libc::S_IFLNK,
        "unexpected symbolic link in application bundle: {}",
        name.to_string_lossy()
    );
    ensure!(
        kind == libc::S_IFDIR || kind == libc::S_IFREG,
        "unsupported entry in application bundle: {}",
        name.to_string_lossy()
    );
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if kind == libc::S_IFDIR {
            libc::O_DIRECTORY
        } else {
            0
        };
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let opened = file.metadata()?;
    ensure!(
        opened.dev() == info.st_dev as u64
            && opened.ino() == info.st_ino
            && opened.mode() & u32::from(libc::S_IFMT) == u32::from(kind),
        "application bundle entry changed while opening it"
    );
    Ok(file)
}
fn bundle_stamp(info: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    use std::os::unix::fs::MetadataExt;
    (
        info.dev(),
        info.ino(),
        info.len(),
        info.mtime(),
        info.mtime_nsec(),
        info.ctime(),
        info.ctime_nsec(),
    )
}
#[derive(Debug)]
struct BundleChanged(String);
impl std::fmt::Display for BundleChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for BundleChanged {}

#[cfg(test)]
type TestBundleHashHook = Box<dyn FnMut(&std::fs::File, &[u8]) -> Result<()>>;
#[cfg(test)]
thread_local! {
    static TEST_BUNDLE_HASH_HOOK: std::cell::RefCell<Option<TestBundleHashHook>> = const { std::cell::RefCell::new(None) };
}
fn hash_bundle_entry(file: &mut std::fs::File, path: &[u8], hash: &mut Sha256) -> Result<()> {
    use std::io::Read;
    let before = file.metadata()?;
    #[cfg(test)]
    TEST_BUNDLE_HASH_HOOK.with(|hook| -> Result<()> {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(file, path)?;
        }
        Ok(())
    })?;
    ensure!(
        before.is_dir() || before.is_file(),
        "unsupported application bundle entry"
    );
    hash.update(if before.is_dir() { b"D" } else { b"F" });
    hash.update((path.len() as u64).to_be_bytes());
    hash.update(path);
    if before.is_dir() {
        for name in bundle_names(file)? {
            let mut child = bundle_child(file, &name)?;
            let mut child_path = path.to_vec();
            if !child_path.is_empty() {
                child_path.push(b'/');
            }
            child_path.extend(name);
            hash_bundle_entry(&mut child, &child_path, hash)?;
        }
    } else {
        hash.update(before.len().to_be_bytes());
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut length = 0_u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            length += count as u64;
        }
        if length != before.len() {
            return Err(BundleChanged(format!(
                "application bundle file {} changed while hashing: expected {} bytes, read {length}",
                String::from_utf8_lossy(path), before.len()
            )).into());
        }
    }
    let after = file.metadata()?;
    if bundle_stamp(&before) != bundle_stamp(&after) {
        return Err(BundleChanged(format!(
            "application bundle entry {} changed while hashing: {:?} -> {:?}",
            String::from_utf8_lossy(path),
            bundle_stamp(&before),
            bundle_stamp(&after)
        ))
        .into());
    }
    Ok(())
}
fn bundle_digest_once(bundle: &Path) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    // Descriptor-relative traversal rejects links at every component. Hash only
    // relative names and file bytes, so identical copies have identical IDs.
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(bundle)?;
    let mut hash = Sha256::new();
    hash.update(b"jaso-nfc-bundle-v1\0");
    hash_bundle_entry(&mut directory, b"", &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
fn bundle_digest(bundle: &Path) -> Result<String> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match bundle_digest_once(bundle) {
            // Discard the entire interrupted digest and all descriptors. A
            // successful attempt still passes every stamp/link/length check;
            // callers compare it against their previously accepted digest.
            Err(error) if attempt < 3 && error.downcast_ref::<BundleChanged>().is_some() => {}
            result => return result,
        }
    }
}

struct ClassifiedApplication {
    previous: Vec<PathBuf>,
    identity: Option<(u64, u64)>,
    digest: Option<String>,
}
impl ClassifiedApplication {
    fn verify(&self, application: &Path) -> Result<()> {
        ensure!(
            entry_identity(application)? == self.identity,
            "application changed after ownership classification"
        );
        if let Some(digest) = &self.digest {
            ensure!(
                bundle_digest(application)? == *digest,
                "application contents changed after ownership classification"
            );
            ensure!(
                entry_identity(application)? == self.identity,
                "application changed while verifying ownership"
            );
        }
        Ok(())
    }
    fn verified(self, application: &Path) -> Result<Self> {
        self.verify(application)?;
        Ok(self)
    }
}

fn managed_application(application: &Path, releases: &Path) -> Result<ClassifiedApplication> {
    use std::os::unix::fs::MetadataExt;
    let info = match std::fs::symlink_metadata(application) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ClassifiedApplication {
                previous: vec![],
                identity: None,
                digest: None,
            }
            .verified(application);
        }
        Err(error) => return Err(error.into()),
    };
    // Bind ownership to the entry observed before resolving links or hashing.
    let identity = Some((info.dev(), info.ino()));
    if info.file_type().is_symlink() {
        let target = std::fs::read_link(application)?;
        let target = if target.is_absolute() {
            target
        } else {
            application.parent().unwrap().join(target)
        };
        ensure!(
            target.starts_with(releases)
                && !target
                    .components()
                    .any(|c| c == std::path::Component::ParentDir),
            "refusing to replace an unrelated application at {}",
            application.display()
        );
        if target.exists() {
            ensure!(
                std::fs::canonicalize(&target)?.starts_with(std::fs::canonicalize(releases)?),
                "application link points outside managed releases"
            );
        }
        return ClassifiedApplication {
            previous: vec![target],
            identity,
            digest: None,
        }
        .verified(application);
    }
    if info.is_dir() {
        let digest = bundle_digest(application)?;
        if let Ok(entries) = std::fs::read_dir(releases) {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && bundle_digest(&entry.path().join("Jaso NFC.app")).is_ok_and(|v| v == digest)
                {
                    // A prior registration may still launch the immutable copy.
                    return ClassifiedApplication {
                        previous: vec![application.to_owned(), entry.path().join("Jaso NFC.app")],
                        identity,
                        digest: Some(digest),
                    }
                    .verified(application);
                }
            }
        }
    }
    anyhow::bail!(
        "refusing to replace an unrelated application at {}",
        application.display()
    )
}

fn entry_identity(path: &Path) -> Result<Option<(u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(info) => Ok(Some((info.dev(), info.ino()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn rename_application(source: &Path, destination: &Path, exchange: bool) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            if exchange {
                libc::RENAME_SWAP
            } else {
                libc::RENAME_EXCL
            },
        )
    };
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            if exchange {
                libc::RENAME_EXCHANGE
            } else {
                libc::RENAME_NOREPLACE
            },
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn remove_application_entry(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(info) if info.is_dir() => Ok(std::fs::remove_dir_all(path)?),
        Ok(_) => Ok(std::fs::remove_file(path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct ApplicationSwap {
    path: PathBuf,
    staged: PathBuf,
    previous: Vec<PathBuf>,
    original_identity: Option<(u64, u64)>,
    staged_identity: Option<(u64, u64)>,
    original_digest: Option<String>,
    staged_digest: String,
    published: bool,
}
impl ApplicationSwap {
    fn prepare(application: &Path, installed: &Path, releases: &Path) -> Result<Self> {
        let previous = managed_application(application, releases)?;
        Self::prepare_classified(application, installed, previous)
    }
    fn prepare_classified(
        application: &Path,
        installed: &Path,
        classified: ClassifiedApplication,
    ) -> Result<Self> {
        classified.verify(application)?;
        let parent = application.parent().context("application has no parent")?;
        std::fs::create_dir_all(parent)?;
        let mut swap = Self {
            path: application.into(),
            staged: parent.join(format!(".jaso-application-{}", uuid::Uuid::new_v4())),
            previous: classified.previous.clone(),
            original_identity: classified.identity,
            staged_identity: None,
            original_digest: classified.digest.clone(),
            staged_digest: bundle_digest(installed)?,
            published: false,
        };
        // Copy before stopping any worker. A sibling staging path keeps the
        // atomic exchange on the Applications volume even with custom state_dir.
        copy_app(installed, &swap.staged)?;
        ensure!(
            bundle_digest(&swap.staged)? == swap.staged_digest,
            "application changed while staging Applications bundle"
        );
        swap.staged_identity = entry_identity(&swap.staged)?;
        classified.verify(application)?;
        Ok(swap)
    }
    fn publish(&mut self) -> Result<()> {
        ensure!(
            entry_identity(&self.path)? == self.original_identity,
            "application changed during installation"
        );
        ensure!(
            entry_identity(&self.staged)? == self.staged_identity
                && bundle_digest(&self.staged)? == self.staged_digest,
            "staged application changed during installation"
        );
        if let Some(digest) = &self.original_digest {
            ensure!(
                bundle_digest(&self.path)? == *digest,
                "existing application changed during installation"
            );
        }
        // Preserve both entries whenever an exchange outcome is uncertain.
        self.published = true;
        let renamed =
            rename_application(&self.staged, &self.path, self.original_identity.is_some());
        let destination = entry_identity(&self.path)?;
        let displaced = entry_identity(&self.staged)?;
        if destination == self.original_identity && displaced == self.staged_identity {
            self.published = false;
        }
        renamed?;
        ensure!(
            destination == self.staged_identity && displaced == self.original_identity,
            "application exchange did not preserve the expected entries"
        );
        ensure!(
            bundle_digest(&self.path)? == self.staged_digest,
            "published application failed content verification"
        );
        Ok(())
    }
    fn restore(&mut self) -> Result<()> {
        if self.published {
            ensure!(
                entry_identity(&self.path)? == self.staged_identity,
                "application changed before rollback"
            );
            ensure!(
                entry_identity(&self.staged)? == self.original_identity,
                "displaced application changed before rollback"
            );
            let restored = if self.original_identity.is_some() {
                rename_application(&self.staged, &self.path, true)
            } else {
                rename_application(&self.path, &self.staged, false)
            };
            if entry_identity(&self.path)? == self.original_identity
                && entry_identity(&self.staged)? == self.staged_identity
            {
                self.published = false;
            }
            restored?;
            ensure!(
                !self.published,
                "application rollback failed entry verification"
            );
        }
        Ok(())
    }
    fn finish(&mut self) {
        self.published = false;
        let _ = remove_application_entry(&self.staged);
    }
}
impl Drop for ApplicationSwap {
    fn drop(&mut self) {
        // A failed rollback keeps the displaced app at the recorded backup path.
        if !self.published {
            let _ = remove_application_entry(&self.staged);
        }
    }
}

struct LegacyApplication {
    path: PathBuf,
    backup: PathBuf,
    classified: ClassifiedApplication,
    moved: bool,
}
impl LegacyApplication {
    fn prepare(path: &Path, releases: &Path) -> Option<Self> {
        // An unrelated or unreadable user application is never migration input.
        let classified = managed_application(path, releases).ok()?;
        classified.identity?;
        Some(Self {
            path: path.into(),
            backup: path
                .parent()?
                .join(format!(".jaso-legacy-{}", uuid::Uuid::new_v4())),
            classified,
            moved: false,
        })
    }
    fn retire(&mut self) -> Result<()> {
        self.classified.verify(&self.path)?;
        ensure!(
            entry_identity(&self.backup)?.is_none(),
            "legacy application backup already exists"
        );
        // Only retire after the new worker starts. Keep the move reversible until
        // installation commits, and never recursively delete the public path.
        self.moved = true;
        let renamed = rename_application(&self.path, &self.backup, false);
        if entry_identity(&self.path)? == self.classified.identity
            && entry_identity(&self.backup)?.is_none()
        {
            self.moved = false;
        }
        renamed?;
        ensure!(
            entry_identity(&self.path)?.is_none(),
            "legacy application changed during migration"
        );
        self.classified.verify(&self.backup)?;
        Ok(())
    }
    fn restore(&mut self) -> Result<()> {
        if self.moved {
            // RENAME_EXCL preserves anything independently created at the old
            // path. An uncertain recovery retains the backup for inspection.
            rename_application(&self.backup, &self.path, false)?;
            self.moved = false;
        }
        Ok(())
    }
    fn finish(&mut self) {
        if self.moved && self.classified.verify(&self.backup).is_ok() {
            let _ = remove_application_entry(&self.backup);
            self.moved = false;
        }
    }
}

fn validate_source_bundle(source: &Path) -> Result<()> {
    let executable = source.join("Contents/MacOS/jaso-nfc");
    let menu = source.join("Contents/MacOS/Jaso NFC");
    ensure!(
        executable.is_file() && menu.is_file(),
        "expected a built Jaso NFC.app bundle"
    );
    crate::app_bundle::validate_metadata(source)
}

pub fn install(config: &Config, source: &Path) -> Result<Value> {
    let source = std::fs::canonicalize(source)?;
    validate_source_bundle(&source)?;
    let _operation = lifecycle_lock()?;
    let signature = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&source)
        .output()?;
    ensure!(
        signature.status.success(),
        "application signature verification failed"
    );
    let digest = bundle_digest(&source)?;
    let releases = Path::new(&config.state_dir).join("releases");
    let release = releases.join(format!(
        "{}-native-{}",
        env!("CARGO_PKG_VERSION"),
        &digest[..12]
    ));
    let installed = release.join("Jaso NFC.app");
    let paths = InstallPaths::for_user(config, &home());
    managed_application(&paths.application, &releases)?;
    // Build/copy failures occur before taking the working service down.
    std::fs::create_dir_all(&releases)?;
    if installed.exists() {
        ensure!(
            bundle_digest(&installed)? == digest,
            "existing immutable release does not match its content identity"
        );
    } else {
        let staging = releases.join(format!(".native-install-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&staging)?;
        let copied = staging.join("Jaso NFC.app");
        let result = (|| -> Result<()> {
            copy_app(&source, &copied)?;
            ensure!(
                bundle_digest(&copied)? == digest,
                "application bundle changed while staging the release"
            );
            std::fs::rename(&staging, &release)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result?;
    }
    activate(config, &installed, &paths, &mut NativeHost { source })?;
    if let Err(error) = crate::retention::prune_releases(Path::new(&config.state_dir), &installed) {
        crate::service::diagnostic(config, &format!("release retention: {error:#}"));
    }
    Ok(
        json!({"installed":paths.application,"application":paths.application,"rollback_release":installed,"launch_agent":paths.worker,"version":env!("CARGO_PKG_VERSION"),"recovery_history_retained":true}),
    )
}

struct InstallPaths {
    worker: PathBuf,
    menu: PathBuf,
    config: PathBuf,
    application: PathBuf,
    legacy_application: PathBuf,
}
impl InstallPaths {
    fn for_user(config: &Config, user_home: &Path) -> Self {
        let agents = user_home.join("Library/LaunchAgents");
        Self {
            worker: agents.join(format!("{LABEL}.plist")),
            menu: agents.join(format!("{}.plist", menu_label())),
            config: Path::new(&config.state_dir).join("config.json"),
            application: PathBuf::from("/Applications/Jaso NFC.app"),
            legacy_application: user_home.join("Applications/Jaso NFC.app"),
        }
    }
}
trait InstallHost: LaunchHost {
    type Lock;
    fn handoff_helper(&self) -> PathBuf;
    fn lock(&mut self, config: &Config) -> Result<Self::Lock>;
    fn menu_snapshot(&mut self, helper: &Path, previous: &[PathBuf]) -> Result<Value>;
    fn menu_stop(&mut self, helper: &Path, snapshot: &Value) -> Result<()>;
    fn menu_restore(&mut self, helper: &Path, snapshot: &Value) -> Result<()>;
}
struct NativeHost {
    source: PathBuf,
}
impl LaunchHost for NativeHost {}
fn menu_helper(helper: &Path, operation: &str, value: &Value) -> Result<Value> {
    let output = Command::new(helper)
        .args(["--menu-handoff", operation, &serde_json::to_string(value)?])
        .output()?;
    ensure!(
        output.status.success(),
        "menu handoff {operation}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}
impl InstallHost for NativeHost {
    type Lock = RuntimeLock;
    fn handoff_helper(&self) -> PathBuf {
        self.source.join("Contents/MacOS/Jaso NFC")
    }
    fn lock(&mut self, config: &Config) -> Result<Self::Lock> {
        RuntimeLock::acquire(config, Duration::from_secs(15))
    }
    fn menu_snapshot(&mut self, helper: &Path, previous: &[PathBuf]) -> Result<Value> {
        let mut allowed = vec![self.source.join("Contents/MacOS/Jaso NFC")];
        for previous in previous {
            allowed.push(previous.join("Contents/MacOS/Jaso NFC"));
        }
        menu_helper(helper, "snapshot", &json!(allowed))
    }
    fn menu_stop(&mut self, helper: &Path, snapshot: &Value) -> Result<()> {
        menu_helper(helper, "stop", snapshot)?;
        Ok(())
    }
    fn menu_restore(&mut self, helper: &Path, snapshot: &Value) -> Result<()> {
        menu_helper(helper, "restore", snapshot)?;
        Ok(())
    }
}
#[derive(Debug)]
struct JobSnapshot {
    label: String,
    plist: PathBuf,
    loaded: bool,
    disabled: bool,
}
struct FileSnapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    permissions: Option<std::fs::Permissions>,
}
impl FileSnapshot {
    fn read(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Self {
                path: path.into(),
                bytes: Some(bytes),
                permissions: Some(std::fs::metadata(path)?.permissions()),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path: path.into(),
                bytes: None,
                permissions: None,
            }),
            Err(error) => Err(error.into()),
        }
    }
    fn restore(&self) -> Result<()> {
        if let Some(bytes) = &self.bytes {
            write_atomic(&self.path, bytes, self.permissions.clone())
        } else {
            remove_optional(&self.path)
        }
    }
}
fn remove_optional(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn write_atomic(
    path: &Path,
    bytes: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::{fs::OpenOptionsExt, io::AsRawFd};
    let parent = path.parent().context("installation file has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".jaso-install-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        ensure!(
            unsafe { libc::fsync(file.as_raw_fd()) } == 0,
            "cannot sync installation file: {}",
            std::io::Error::last_os_error()
        );
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}
fn snapshots(host: &mut impl InstallHost, paths: &InstallPaths) -> Result<Vec<JobSnapshot>> {
    let out = host.launch(&["print-disabled", &domain()])?;
    ensure!(out.status.success(), "cannot inspect login startup state");
    let disabled = String::from_utf8_lossy(&out.stdout);
    let mut jobs = vec![];
    for (label, plist) in [
        (LABEL.to_owned(), paths.worker.clone()),
        (menu_label(), paths.menu.clone()),
    ] {
        jobs.push(JobSnapshot {
            loaded: host_loaded(host, &label)?,
            disabled: disabled_from_output(&disabled, &label),
            label,
            plist,
        });
    }
    Ok(jobs)
}
fn restore_jobs(
    host: &mut impl InstallHost,
    jobs: &[JobSnapshot],
    pending_removals: &mut BTreeSet<String>,
) -> Result<()> {
    let mut failures = vec![];
    for job in jobs {
        // A disabled service may still have been loaded. Enable just long enough
        // to restore that service, then put its original login setting back.
        let restored = (|| -> Result<()> {
            // A stop timeout must not turn a departing registration into proof
            // that the old job is restored. Drain it or report rollback failure.
            if pending_removals.contains(&job.label) {
                wait_for_job_absence(host, &job.label)?;
                pending_removals.remove(&job.label);
            }
            let loaded = host_loaded(host, &job.label)?;
            if job.loaded && !loaded {
                host_checked(host, &["enable", &format!("{}/{}", domain(), job.label)])?;
                host_checked(
                    host,
                    &[
                        "bootstrap",
                        &domain(),
                        job.plist.to_str().context("invalid launch job path")?,
                    ],
                )?;
            } else if !job.loaded && loaded {
                host_stop(host, &job.label, pending_removals)?;
            }
            Ok(())
        })();
        if let Err(error) = restored {
            failures.push(format!("{}: {error:#}", job.label));
        }
        if let Err(error) = host_checked(
            host,
            &[
                if job.disabled { "disable" } else { "enable" },
                &format!("{}/{}", domain(), job.label),
            ],
        ) {
            failures.push(format!("{} login state: {error:#}", job.label));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}
fn restore_artifacts(files: &[FileSnapshot], application: &mut ApplicationSwap) -> Result<()> {
    let mut failures = vec![];
    for file in files {
        if let Err(error) = file.restore() {
            failures.push(format!("{}: {error:#}", file.path.display()));
        }
    }
    if let Err(error) = application.restore() {
        failures.push(format!("{}: {error:#}", application.path.display()));
    }
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}
fn activate(
    config: &Config,
    installed: &Path,
    paths: &InstallPaths,
    host: &mut impl InstallHost,
) -> Result<()> {
    // Capture restorable bytes and effective launch state before stopping any
    // job. Permission/read failures must not be mistaken for missing files.
    let files = [
        FileSnapshot::read(&paths.worker)?,
        FileSnapshot::read(&paths.menu)?,
        FileSnapshot::read(&paths.config)?,
    ];
    let mut application = ApplicationSwap::prepare(
        &paths.application,
        installed,
        &Path::new(&config.state_dir).join("releases"),
    )?;
    let mut legacy = if paths.legacy_application != paths.application {
        LegacyApplication::prepare(
            &paths.legacy_application,
            &Path::new(&config.state_dir).join("releases"),
        )
    } else {
        None
    };
    let jobs = snapshots(host, paths)?;
    for (job, file) in jobs.iter().zip(&files) {
        ensure!(
            !job.loaded || file.bytes.is_some(),
            "cannot replace loaded job {} without its existing plist",
            job.label
        );
    }
    let helper = host.handoff_helper();
    // Snapshot before any stop so both launchd-owned and independently opened
    // menus can be restored with their original configuration on failure.
    let mut previous = application.previous.clone();
    if let Some(legacy) = &legacy {
        previous.push(legacy.path.clone());
        previous.extend(legacy.classified.previous.clone());
    }
    let previous_menu = host.menu_snapshot(&helper, &previous)?;
    let mut lock = None;
    let mut artifacts_changed = false;
    let mut pending_removals = BTreeSet::new();
    let backup = Path::new(&config.state_dir)
        .join("backups")
        .join(format!("native-install-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        // Every post-stop failure uses the same restoration path, including
        // the second stop, lock acquisition, and backup creation failures.
        // Stop the verified process before bootout can invalidate its identity
        // during termination. A failed handoff must not stop the worker job.
        host.menu_stop(&helper, &previous_menu)?;
        for job in &jobs {
            host_stop(host, &job.label, &mut pending_removals)?;
        }
        lock = Some(host.lock(config)?);
        std::fs::create_dir_all(&backup)?;
        for (file, name) in files
            .iter()
            .zip(["worker.plist", "menu.plist", "config.json"])
        {
            if let Some(bytes) = &file.bytes {
                write_atomic(&backup.join(name), bytes, None)?;
            }
        }
        atomic_json(
            backup.join("installation.json"),
            &json!({
                "previous_app":previous.first(),"previous_menu_paths":previous,
                "previous_application_backup":application.staged,
                "previous_legacy_application":legacy.as_ref().map(|l| &l.path),
                "legacy_application_backup":legacy.as_ref().map(|l| &l.backup),
                "application":paths.application,"new_app":installed,"previous_menu":previous_menu,
                "previous_jobs":jobs.iter().map(|j|json!({"label":j.label,"loaded":j.loaded,"disabled":j.disabled})).collect::<Vec<_>>()
            }),
        )?;
        artifacts_changed = true;
        config.save(&paths.config)?;
        write_atomic(
            &paths.worker,
            plist(
                &paths.application.join("Contents/MacOS/jaso-nfc"),
                &paths.config,
            )
            .as_bytes(),
            None,
        )?;
        // The worker opens the menu itself. Retire the legacy registration only
        // after both old jobs stopped and their bytes were preserved for rollback.
        remove_optional(&paths.menu)?;
        application.publish()?;
        drop(lock.take());
        let worker = &jobs[0];
        host_checked(host, &["enable", &format!("{}/{}", domain(), worker.label)])?;
        host_checked(
            host,
            &[
                "bootstrap",
                &domain(),
                worker.plist.to_str().context("invalid launch job path")?,
            ],
        )?;
        // Activation explicitly starts the worker. That temporary enablement
        // must not change a saved choice to keep future login startup off.
        if worker.disabled {
            host_checked(
                host,
                &["disable", &format!("{}/{}", domain(), worker.label)],
            )?;
        }
        if let Some(legacy) = &mut legacy {
            legacy.retire()?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let restored = (|| -> Result<()> {
            if let Some(legacy) = &mut legacy {
                legacy.restore()?;
            }
            if artifacts_changed {
                host_stop(host, LABEL, &mut pending_removals)
                    .context("could not stop new worker for rollback")?;
                // The menu survives worker shutdown. Stop it before restoring
                // the app, after the worker can no longer launch another GUI.
                let new_menu =
                    host.menu_snapshot(&helper, std::slice::from_ref(&paths.application))?;
                if new_menu.as_array().is_some_and(|menus| !menus.is_empty()) {
                    host.menu_stop(&helper, &new_menu)?;
                }
                if lock.is_none() {
                    lock = Some(
                        host.lock(config)
                            .context("could not acquire runtime lock for rollback")?,
                    );
                }
                // A started replacement may already have migrated its index.
                // Restore writable compatibility before restarting the previous
                // worker; its history cache is reconstructed from the journals.
                crate::index::restore_legacy_storage(config.state_path("index.sqlite3"))?;
                for name in [
                    "history.sqlite3",
                    "history.sqlite3-wal",
                    "history.sqlite3-shm",
                ] {
                    remove_optional(&config.state_path(name))?;
                }
                restore_artifacts(&files, &mut application)?;
            }
            // Never start the old worker while still owning its runtime lock.
            drop(lock.take());
            // Restore the original menu arguments before bootstrap can open
            // another instance using the registered defaults. Attempt both
            // restoration paths even when one fails.
            let menu_result = host.menu_restore(&helper, &previous_menu);
            let jobs_result = restore_jobs(host, &jobs, &mut pending_removals);
            match (menu_result, jobs_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(menu), Err(jobs)) => Err(anyhow::anyhow!("menu: {menu:#}; jobs: {jobs:#}")),
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        })();
        drop(lock.take());
        if let Err(rollback) = restored {
            return Err(anyhow::anyhow!(
                "Installation failed: {error:#}; rollback also failed: {rollback:#}. Preserved backup: {}",
                backup.display()
            ));
        }
        return Err(error);
    }
    application.finish();
    if let Some(legacy) = &mut legacy {
        legacy.finish();
    }
    // A cleanup failure after activation must not undo a working installation.
    if let Err(error) = crate::retention::mark_completed(&backup)
        .and_then(|_| crate::retention::prune_backups(Path::new(&config.state_dir)))
    {
        crate::service::diagnostic(config, &format!("backup retention: {error:#}"));
    }
    Ok(())
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod source_bundle_tests {
    use super::*;

    fn bundle(info: &str, binary: bool) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Jaso NFC.app");
        std::fs::create_dir_all(path.join("Contents/MacOS")).unwrap();
        for name in ["jaso-nfc", "Jaso NFC"] {
            std::fs::write(path.join("Contents/MacOS").join(name), b"executable").unwrap();
        }
        let plist = path.join("Contents/Info.plist");
        std::fs::write(&plist, info).unwrap();
        if binary {
            let output = Command::new("/usr/bin/plutil")
                .args(["-convert", "binary1", "--"])
                .arg(&plist)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        (temp, path)
    }
    fn metadata(identifier: &str, executable: &str, package: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>{identifier}</string>\
             <key>CFBundleExecutable</key><string>{executable}</string>\
             <key>CFBundlePackageType</key><string>{package}</string>\
             </dict></plist>"
        )
    }
    #[test]
    fn source_bundle_accepts_parsed_xml_and_binary_metadata() {
        // XML entities must be parsed rather than matched as source text.
        for binary in [false, true] {
            let (_temp, path) = bundle(&metadata(LABEL, "jaso&#45;nfc", "APPL"), binary);
            validate_source_bundle(&path).unwrap();
        }
    }
    #[test]
    fn source_bundle_rejects_unrelated_identifier() {
        let (_temp, path) = bundle(&metadata("example.unrelated", "jaso-nfc", "APPL"), false);
        let error = validate_source_bundle(&path).unwrap_err().to_string();
        assert!(error.contains("CFBundleIdentifier"), "{error}");
    }
    #[test]
    fn source_bundle_rejects_gui_as_main_executable() {
        let (temp, path) = bundle(&metadata(LABEL, "Jaso NFC", "APPL"), true);
        let config = Config {
            state_dir: temp.path().join("state").to_str().unwrap().into(),
            ..Config::default()
        };
        let error = install(&config, &path).unwrap_err().to_string();
        assert!(error.contains("CFBundleExecutable"), "{error}");
        assert!(!Path::new(&config.state_dir).exists());
    }
    #[test]
    fn source_bundle_rejects_non_application_package() {
        let (_temp, path) = bundle(&metadata(LABEL, "jaso-nfc", "BNDL"), false);
        let error = validate_source_bundle(&path).unwrap_err().to_string();
        assert!(error.contains("CFBundlePackageType"), "{error}");
    }
    #[test]
    fn source_bundle_rejects_missing_malformed_or_non_string_metadata() {
        for info in [
            "invalid plist".into(),
            "<plist version=\"1.0\"><dict/></plist>".into(),
            metadata(LABEL, "jaso-nfc", "APPL").replace(
                "<string>jaso-nfc</string>",
                "<array><string>jaso-nfc</string></array>",
            ),
        ] {
            let (_temp, path) = bundle(&info, false);
            assert!(validate_source_bundle(&path).is_err(), "{info}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_application_is_independent_of_home_and_state_directory() {
        let config = Config {
            state_dir: "/custom/state".into(),
            ..Config::default()
        };
        let paths = InstallPaths::for_user(&config, Path::new("/Users/example"));
        assert_eq!(paths.application, Path::new("/Applications/Jaso NFC.app"));
        assert_eq!(
            paths.legacy_application,
            Path::new("/Users/example/Applications/Jaso NFC.app")
        );
        assert_eq!(paths.config, Path::new("/custom/state/config.json"));
    }
    #[test]
    fn launch_job_associates_worker_with_the_application() {
        let value = plist(
            Path::new("/tmp/Jaso NFC.app/Contents/MacOS/jaso-nfc"),
            Path::new("/tmp/config.json"),
        );
        assert!(value.contains(&format!(
            "<key>AssociatedBundleIdentifiers</key><array><string>{LABEL}</string></array>"
        )));
    }
    #[test]
    fn launch_job_is_event_driven_and_xml_escapes_paths() {
        let value = plist(
            Path::new("/tmp/a&b/jaso-nfc"),
            Path::new("/tmp/config.json"),
        );
        assert!(value.contains("/tmp/a&amp;b/jaso-nfc"));
        assert!(!value.contains("StartInterval"));
        assert!(!value.contains("WatchPaths"));
        assert!(value.contains("<key>KeepAlive</key><true/>"));
    }
}

#[cfg(test)]
mod rollback_tests {
    use super::*;
    use crate::lifecycle::stop_label_with;
    use std::collections::BTreeMap;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn with_bundle_hash_hook<T>(hook: TestBundleHashHook, body: impl FnOnce() -> T) -> T {
        struct Reset(Option<TestBundleHashHook>);
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_BUNDLE_HASH_HOOK.with(|hook| hook.replace(self.0.take()));
            }
        }
        let _reset = Reset(TEST_BUNDLE_HASH_HOOK.with(|slot| slot.replace(Some(hook))));
        body()
    }

    #[test]
    fn bundle_hash_retries_one_metadata_change_with_a_fresh_read() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("fixture.app");
        write_fixture_app(&bundle, b"unchanged app bytes");
        let expected = bundle_digest(&bundle).unwrap();
        let attempts = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = attempts.clone();
        let actual = with_bundle_hash_hook(
            Box::new(move |file, path| {
                if path.is_empty() {
                    let count = observed.get() + 1;
                    observed.set(count);
                    if count == 1 {
                        file.set_modified(
                            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1),
                        )?;
                    }
                }
                Ok(())
            }),
            || bundle_digest(&bundle),
        );
        assert_eq!(actual.unwrap(), expected);
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn bundle_hash_rejects_persistent_metadata_changes_after_three_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("fixture.app");
        write_fixture_app(&bundle, b"unchanged app bytes");
        let attempts = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = attempts.clone();
        let error = with_bundle_hash_hook(
            Box::new(move |file, path| {
                if path.is_empty() {
                    let count = observed.get() + 1;
                    observed.set(count);
                    file.set_modified(
                        std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(count),
                    )?;
                }
                Ok(())
            }),
            || bundle_digest(&bundle),
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed while hashing"));
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn bundle_hash_retry_does_not_accept_changed_application_contents() {
        let f = Fixture::normal();
        let mut application = ApplicationSwap::prepare(
            &f.paths.application,
            &f.installed,
            &Path::new(&f.config.state_dir).join("releases"),
        )
        .unwrap();
        let target = application.staged.join("Contents/MacOS/Jaso NFC");
        let mut changed = false;
        let error = with_bundle_hash_hook(
            Box::new(move |file, path| {
                if !changed && path == b"Contents/MacOS/Jaso NFC" {
                    changed = true;
                    std::fs::write(&target, b"changed application contents")?;
                    file.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1))?;
                }
                Ok(())
            }),
            || application.publish(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("staged application changed during installation"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read_link(&f.paths.application).unwrap(),
            f.old_link
        );
        assert!(!application.published);
    }

    #[test]
    fn bundle_hash_never_retries_link_or_generic_io_failures() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("fixture.app");
        write_fixture_app(&bundle, b"unchanged app bytes");
        std::os::unix::fs::symlink("Contents", bundle.join("link")).unwrap();
        for injected_io in [false, true] {
            let attempts = std::rc::Rc::new(std::cell::Cell::new(0));
            let observed = attempts.clone();
            let error = with_bundle_hash_hook(
                Box::new(move |_, path| {
                    if path.is_empty() {
                        observed.set(observed.get() + 1);
                        if injected_io {
                            return Err(std::io::Error::from_raw_os_error(libc::EACCES).into());
                        }
                    }
                    Ok(())
                }),
                || bundle_digest(&bundle),
            )
            .unwrap_err();
            if injected_io {
                assert_eq!(
                    error
                        .downcast_ref::<std::io::Error>()
                        .unwrap()
                        .raw_os_error(),
                    Some(libc::EACCES)
                );
            } else {
                assert!(error.to_string().contains("unexpected symbolic link"));
            }
            assert_eq!(attempts.get(), 1);
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct JobState {
        loaded: bool,
        disabled: bool,
    }
    struct FakeLock(Arc<AtomicBool>);
    impl Drop for FakeLock {
        fn drop(&mut self) {
            assert!(self.0.swap(false, Ordering::SeqCst));
        }
    }
    struct FakeHost {
        helper: PathBuf,
        required_legacy_during_bootstrap: Option<PathBuf>,
        jobs: BTreeMap<String, JobState>,
        commands: Vec<Vec<String>>,
        fail_once: Option<(String, String)>,
        fail_after: Option<(String, String)>,
        fail_lock: bool,
        held: Arc<AtomicBool>,
        bootstrapped: Vec<(String, Vec<u8>)>,
        menu_process: Option<Value>,
        menu_stops: usize,
        menu_restores: usize,
        fail_menu_snapshot: bool,
        fail_menu_stop: bool,
        menu_identity_invalidated: bool,
        snapshot_previous: Option<PathBuf>,
        new_menu_executable: Option<PathBuf>,
        removal_delay: usize,
        pending_removals: BTreeMap<String, usize>,
    }
    impl InstallHost for FakeHost {
        type Lock = FakeLock;
        fn handoff_helper(&self) -> PathBuf {
            self.helper.clone()
        }
        fn menu_snapshot(&mut self, _: &Path, previous: &[PathBuf]) -> Result<Value> {
            self.snapshot_previous = previous.first().cloned();
            ensure!(!self.fail_menu_snapshot, "unrecognized menu process");
            if let Some(process) = &self.menu_process {
                ensure!(
                    previous
                        .iter()
                        .any(|path| process["executable"]
                            == json!(path.join("Contents/MacOS/Jaso NFC"))),
                    "previous menu executable is not allowed"
                );
            }
            Ok(self.menu_process.clone().map_or(json!([]), |p| json!([p])))
        }
        fn menu_stop(&mut self, _: &Path, snapshot: &Value) -> Result<()> {
            self.commands.push(vec!["menu-stop".into()]);
            self.menu_stops += 1;
            ensure!(!self.fail_menu_stop, "menu did not terminate");
            ensure!(
                !self.menu_identity_invalidated,
                "menu identity changed while exiting after bootout"
            );
            if let Some(process) = &self.menu_process {
                ensure!(snapshot[0] == *process, "wrong menu process");
                self.menu_process = None;
            }
            Ok(())
        }
        fn menu_restore(&mut self, _: &Path, snapshot: &Value) -> Result<()> {
            self.menu_restores += 1;
            if !snapshot[0].is_null() {
                self.menu_process = Some(snapshot[0].clone());
            }
            Ok(())
        }
        fn lock(&mut self, _: &Config) -> Result<FakeLock> {
            if std::mem::take(&mut self.fail_lock) {
                anyhow::bail!("injected runtime lock timeout");
            }
            ensure!(
                !self.held.swap(true, Ordering::SeqCst),
                "lock acquired twice"
            );
            Ok(FakeLock(self.held.clone()))
        }
    }
    impl LaunchHost for FakeHost {
        fn wait_for_job_removal(&mut self) {}
        fn launch(&mut self, args: &[&str]) -> Result<std::process::Output> {
            self.commands
                .push(args.iter().map(|a| (*a).into()).collect());
            let verb = args[0];
            let label = if verb == "bootstrap" {
                Path::new(args[2])
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            } else {
                args.get(1)
                    .unwrap_or(&"")
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned()
            };
            if self
                .fail_once
                .as_ref()
                .is_some_and(|(v, l)| v == verb && l == &label)
            {
                self.fail_once = self.fail_after.take();
                anyhow::bail!("injected {verb} failure for {label}");
            }
            let mut code = 0;
            let mut stdout = String::new();
            match verb {
                "print-disabled" => {
                    for (label, state) in &self.jobs {
                        stdout += &format!("\"{label}\" => {}\n", state.disabled);
                    }
                }
                "print" => {
                    if !self.jobs[&label].loaded {
                        code = 113;
                    }
                    if let Some(remaining) = self.pending_removals.get_mut(&label) {
                        *remaining -= 1;
                        if *remaining == 0 {
                            self.jobs.get_mut(&label).unwrap().loaded = false;
                            self.pending_removals.remove(&label);
                        }
                    }
                }
                "bootout" => {
                    let was_loaded = self.jobs[&label].loaded;
                    if self.removal_delay > 0 && self.jobs[&label].loaded {
                        self.pending_removals
                            .insert(label.clone(), self.removal_delay);
                    } else {
                        self.jobs.get_mut(&label).unwrap().loaded = false;
                    }
                    if label == menu_label() && was_loaded && self.menu_process.is_some() {
                        self.menu_identity_invalidated = true;
                    }
                }
                "enable" => {
                    self.jobs.get_mut(&label).unwrap().disabled = false;
                }
                "disable" => {
                    self.jobs.get_mut(&label).unwrap().disabled = true;
                }
                "kickstart" => {
                    ensure!(self.jobs[&label].loaded, "cannot kickstart absent job");
                    // Launchd may accept this while an earlier bootout still
                    // owns the pending removal; kickstart does not cancel it.
                }
                "bootstrap" => {
                    if let Some(legacy) = &self.required_legacy_during_bootstrap {
                        ensure!(
                            legacy.symlink_metadata().is_ok(),
                            "legacy application retired before worker started"
                        );
                    }
                    ensure!(
                        !self.held.load(Ordering::SeqCst),
                        "started worker while holding its runtime lock"
                    );
                    let state = self.jobs.get_mut(&label).unwrap();
                    ensure!(!state.disabled, "cannot bootstrap disabled job");
                    ensure!(!state.loaded, "cannot bootstrap loaded job");
                    self.bootstrapped
                        .push((label.clone(), std::fs::read(args[2])?));
                    state.loaded = true;
                    if label == LABEL
                        && std::fs::read(args[2])? != b"old worker"
                        && let Some(executable) = &self.new_menu_executable
                    {
                        self.menu_process = Some(
                            json!({"executable":executable,"arguments":["--config","/new/config.json"]}),
                        );
                    }
                }
                _ => anyhow::bail!("unexpected launch command {args:?}"),
            }
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.into_bytes(),
                stderr: vec![],
            })
        }
    }
    struct Fixture {
        temp: tempfile::TempDir,
        config: Config,
        paths: InstallPaths,
        installed: PathBuf,
        host: FakeHost,
        old: BTreeMap<String, JobState>,
        old_link: PathBuf,
    }
    impl Fixture {
        fn new(worker: JobState, menu: JobState) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let base = temp.path();
            let mut config = Config {
                state_dir: base.join("support").to_str().unwrap().into(),
                ..Config::default()
            };
            config.validate().unwrap();
            let agents = base.join("LaunchAgents");
            std::fs::create_dir_all(&agents).unwrap();
            std::fs::create_dir_all(&config.state_dir).unwrap();
            let paths = InstallPaths {
                worker: agents.join(format!("{LABEL}.plist")),
                menu: agents.join(format!("{}.plist", menu_label())),
                config: Path::new(&config.state_dir).join("config.json"),
                application: base.join("Applications/Jaso NFC.app"),
                legacy_application: base.join("Users/test/Applications/Jaso NFC.app"),
            };
            std::fs::write(&paths.worker, b"old worker").unwrap();
            std::fs::write(&paths.menu, b"old menu").unwrap();
            std::fs::write(&paths.config, b"old config").unwrap();
            std::fs::create_dir_all(paths.application.parent().unwrap()).unwrap();
            let old_link = Path::new(&config.state_dir).join("releases/old/Jaso NFC.app");
            write_fixture_app(&old_link, b"old application");
            std::os::unix::fs::symlink(&old_link, &paths.application).unwrap();
            let installed = Path::new(&config.state_dir).join("releases/new/Jaso NFC.app");
            write_fixture_app(&installed, b"new application");
            let jobs = BTreeMap::from([(LABEL.into(), worker), (menu_label(), menu)]);
            let old = jobs.clone();
            let host = FakeHost {
                helper: base.join("build/Jaso NFC.app/Contents/MacOS/Jaso NFC"),
                required_legacy_during_bootstrap: None,
                jobs,
                commands: vec![],
                fail_once: None,
                fail_after: None,
                fail_lock: false,
                held: Arc::new(AtomicBool::new(false)),
                bootstrapped: vec![],
                menu_process: None,
                menu_stops: 0,
                menu_restores: 0,
                fail_menu_snapshot: false,
                fail_menu_stop: false,
                menu_identity_invalidated: false,
                snapshot_previous: None,
                new_menu_executable: None,
                removal_delay: 0,
                pending_removals: BTreeMap::new(),
            };
            Self {
                temp,
                config,
                paths,
                installed,
                host,
                old,
                old_link,
            }
        }
        fn normal() -> Self {
            Self::new(
                JobState {
                    loaded: true,
                    disabled: false,
                },
                JobState {
                    loaded: true,
                    disabled: false,
                },
            )
        }
        fn activate(&mut self) -> Result<()> {
            activate(&self.config, &self.installed, &self.paths, &mut self.host)
        }
        fn assert_restored(&self) {
            assert_eq!(
                self.host.jobs, self.old,
                "prior loaded and disabled state must be restored"
            );
            assert_eq!(std::fs::read(&self.paths.worker).unwrap(), b"old worker");
            assert_eq!(std::fs::read(&self.paths.menu).unwrap(), b"old menu");
            assert_eq!(std::fs::read(&self.paths.config).unwrap(), b"old config");
            assert_eq!(
                std::fs::read_link(&self.paths.application).unwrap(),
                self.old_link
            );
            assert!(!self.host.held.load(Ordering::SeqCst));
        }
    }
    fn write_fixture_app(path: &Path, bytes: &[u8]) {
        for name in [
            "Contents/MacOS/Jaso NFC",
            "Contents/MacOS/jaso-nfc",
            "Contents/Info.plist",
            "Contents/Resources/JasoNFC.icns",
        ] {
            let file = path.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
        }
    }
    #[test]
    fn owned_legacy_gui_main_bundle_still_migrates_with_menu_handoff() {
        let mut f = Fixture::normal();
        std::fs::write(
            f.old_link.join("Contents/Info.plist"),
            format!(
                "<plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{LABEL}</string>\
                 <key>CFBundleExecutable</key><string>Jaso NFC</string>\
                 <key>CFBundlePackageType</key><string>APPL</string>\
                 </dict></plist>"
            ),
        )
        .unwrap();
        std::fs::remove_file(&f.paths.application).unwrap();
        copy_app(&f.old_link, &f.paths.application).unwrap();
        f.host.menu_process = Some(json!({
            "executable": f.paths.application.join("Contents/MacOS/Jaso NFC"),
            "arguments": ["--config", "/custom/config.json"]
        }));
        f.activate().unwrap();
        assert_eq!(f.host.snapshot_previous, Some(f.paths.application.clone()));
        assert_eq!(f.host.menu_stops, 1);
        assert_eq!(f.host.bootstrapped.len(), 1);
        assert_eq!(
            bundle_digest(&f.paths.application).unwrap(),
            bundle_digest(&f.installed).unwrap()
        );
    }
    #[test]
    fn successful_activation_persists_and_bootstraps_only_one_background_job() {
        let mut f = Fixture::normal();
        f.activate().unwrap();
        assert_eq!(f.host.bootstrapped.len(), 1);
        assert_eq!(f.host.bootstrapped[0].0, LABEL);
        assert!(f.host.jobs[LABEL].loaded);
        assert!(!f.host.jobs[&menu_label()].loaded);
        assert!(!f.paths.menu.exists());
        let worker = std::fs::read_to_string(&f.paths.worker).unwrap();
        assert!(worker.contains("<string>run</string>"));
        assert!(!worker.contains("<string>watch</string>"));
        assert_eq!(
            std::fs::read_dir(f.paths.worker.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            Config::load(&f.paths.config).unwrap().signature(),
            f.config.signature()
        );
        let backups: Vec<_> = std::fs::read_dir(Path::new(&f.config.state_dir).join("backups"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(
            backups[0].join("completed.json").is_file(),
            "successful install must enroll its backup in retention"
        );
    }
    #[test]
    fn uninstall_stops_independent_menu_and_removes_only_login_artifacts() {
        let mut f = Fixture::normal();
        f.activate().unwrap();
        f.host.menu_process = Some(
            json!({"executable":f.paths.application.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]}),
        );
        let config_before = std::fs::read(&f.paths.config).unwrap();
        let app_before = bundle_digest(&f.paths.application).unwrap();
        let helper = f.host.handoff_helper();
        uninstall_with(
            &mut f.host,
            &[f.paths.worker.clone(), f.paths.menu.clone()],
            Some((&helper, std::slice::from_ref(&f.paths.application))),
        )
        .unwrap();
        assert!(f.host.jobs.values().all(|job| !job.loaded));
        assert!(f.host.menu_process.is_none());
        assert!(!f.paths.worker.exists() && !f.paths.menu.exists());
        assert_eq!(std::fs::read(&f.paths.config).unwrap(), config_before);
        assert_eq!(bundle_digest(&f.paths.application).unwrap(), app_before);
    }
    #[test]
    fn rollback_stops_new_independent_menu_before_restoring_both_old_jobs() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().disabled = true;
        f.old = f.host.jobs.clone();
        f.host.new_menu_executable = Some(f.paths.application.join("Contents/MacOS/Jaso NFC"));
        let previous = json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]});
        f.host.menu_process = Some(previous.clone());
        f.host.fail_once = Some(("disable".into(), LABEL.into()));
        assert!(f.activate().is_err());
        f.assert_restored();
        assert_eq!(
            f.host.menu_stops, 2,
            "new detached GUI must be explicitly stopped"
        );
        assert_eq!(f.host.menu_process, Some(previous));
    }
    #[test]
    fn single_job_login_startup_mirrors_compatibility_keys_and_ignores_retired_menu() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.menu).unwrap();
        f.host.jobs.get_mut(&menu_label()).unwrap().disabled = true;
        for mode in ["status", "off", "on"] {
            let value =
                crate::lifecycle::startup_with(&mut f.host, &[(LABEL, &f.paths.worker)], mode)
                    .unwrap();
            assert_eq!(value["enabled"], mode != "off");
            assert_eq!(value["worker_enabled"], value["enabled"]);
            assert_eq!(value["menu_enabled"], value["enabled"]);
            assert_eq!(value["consistent"], true);
            assert_eq!(value["installed"], true);
        }
        assert!(
            !f.host
                .commands
                .iter()
                .any(|args| args.iter().any(|arg| arg.ends_with(&menu_label())))
        );
    }
    #[test]
    fn managed_symlink_becomes_a_complete_real_application() {
        let mut f = Fixture::normal();
        let previous_digest = bundle_digest(&f.old_link).unwrap();
        let release_digest = bundle_digest(&f.installed).unwrap();
        f.activate().unwrap();
        assert!(
            !f.paths
                .application
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(bundle_digest(&f.paths.application).unwrap(), release_digest);
        assert_eq!(bundle_digest(&f.old_link).unwrap(), previous_digest);
        assert!(!f.paths.menu.exists());
        let worker = std::fs::read_to_string(&f.paths.worker).unwrap();
        assert!(
            worker.contains(&xml(f
                .paths
                .application
                .join("Contents/MacOS/jaso-nfc")
                .to_str()
                .unwrap()))
        );
        assert!(!worker.contains(&xml(f.installed.to_str().unwrap())));
        assert_eq!(f.host.snapshot_previous, Some(f.old_link));
    }
    #[test]
    fn owned_user_application_migrates_only_after_canonical_worker_starts() {
        for real in [false, true] {
            let mut f = Fixture::normal();
            std::fs::create_dir_all(f.paths.legacy_application.parent().unwrap()).unwrap();
            std::fs::rename(&f.paths.application, &f.paths.legacy_application).unwrap();
            if real {
                std::fs::remove_file(&f.paths.legacy_application).unwrap();
                write_fixture_app(&f.paths.legacy_application, b"old application");
            }
            f.host.required_legacy_during_bootstrap = Some(f.paths.legacy_application.clone());
            f.host.menu_process = Some(
                json!({"executable":f.paths.legacy_application.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]}),
            );
            let old_digest = bundle_digest(&f.old_link).unwrap();
            f.activate().unwrap();
            assert!(
                entry_identity(&f.paths.legacy_application)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                bundle_digest(&f.paths.application).unwrap(),
                bundle_digest(&f.installed).unwrap()
            );
            assert_eq!(bundle_digest(&f.old_link).unwrap(), old_digest);
            assert_eq!(f.host.bootstrapped.len(), 1);
        }
    }
    #[test]
    fn failed_canonical_migration_keeps_old_user_bundle_and_job_state() {
        let mut f = Fixture::normal();
        std::fs::create_dir_all(f.paths.legacy_application.parent().unwrap()).unwrap();
        std::fs::rename(&f.paths.application, &f.paths.legacy_application).unwrap();
        let original = entry_identity(&f.paths.legacy_application).unwrap();
        let menu = json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]});
        f.host.menu_process = Some(menu.clone());
        f.host.fail_once = Some(("bootstrap".into(), LABEL.into()));
        let error = f.activate().unwrap_err().to_string();
        assert!(error.contains("bootstrap failure"), "{error}");
        assert_eq!(
            entry_identity(&f.paths.legacy_application).unwrap(),
            original
        );
        assert!(entry_identity(&f.paths.application).unwrap().is_none());
        assert_eq!(f.host.jobs, f.old);
        assert_eq!(f.host.menu_process, Some(menu));
        assert_eq!(std::fs::read(&f.paths.worker).unwrap(), b"old worker");
        assert_eq!(std::fs::read(&f.paths.config).unwrap(), b"old config");
    }
    #[test]
    fn unrelated_user_application_is_preserved_during_canonical_install() {
        let mut f = Fixture::normal();
        write_fixture_app(&f.paths.legacy_application, b"unrelated user app");
        let original = bundle_digest(&f.paths.legacy_application).unwrap();
        f.activate().unwrap();
        assert_eq!(
            bundle_digest(&f.paths.legacy_application).unwrap(),
            original
        );
    }
    #[test]
    fn legacy_retirement_rejects_a_replacement_and_rollback_never_clobbers_it() {
        for replaced_before_retirement in [false, true] {
            let f = Fixture::normal();
            std::fs::create_dir_all(f.paths.legacy_application.parent().unwrap()).unwrap();
            std::fs::rename(&f.paths.application, &f.paths.legacy_application).unwrap();
            let original = entry_identity(&f.paths.legacy_application).unwrap();
            let mut legacy = LegacyApplication::prepare(
                &f.paths.legacy_application,
                &Path::new(&f.config.state_dir).join("releases"),
            )
            .unwrap();
            if replaced_before_retirement {
                std::fs::remove_file(&f.paths.legacy_application).unwrap();
            } else {
                legacy.retire().unwrap();
                legacy.restore().unwrap();
                assert_eq!(
                    entry_identity(&f.paths.legacy_application).unwrap(),
                    original
                );
                legacy.retire().unwrap();
            }
            write_fixture_app(&f.paths.legacy_application, b"unrelated replacement");
            let replacement = bundle_digest(&f.paths.legacy_application).unwrap();
            if replaced_before_retirement {
                assert!(legacy.retire().is_err());
            } else {
                assert!(legacy.restore().is_err());
                assert_eq!(entry_identity(&legacy.backup).unwrap(), original);
            }
            assert_eq!(
                bundle_digest(&f.paths.legacy_application).unwrap(),
                replacement
            );
        }
    }
    #[test]
    fn managed_real_application_upgrade_snapshots_its_visible_path() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.application).unwrap();
        write_fixture_app(&f.paths.application, b"old application");
        f.activate().unwrap();
        assert_eq!(f.host.snapshot_previous, Some(f.paths.application.clone()));
        assert_eq!(
            bundle_digest(&f.paths.application).unwrap(),
            bundle_digest(&f.installed).unwrap()
        );
    }
    #[test]
    fn managed_real_application_handoffs_menu_from_matching_release() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.application).unwrap();
        write_fixture_app(&f.paths.application, b"old application");
        f.host.menu_process = Some(
            json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]}),
        );
        f.activate().unwrap();
        assert!(f.host.menu_process.is_none());
    }
    #[test]
    fn failed_real_application_upgrade_restores_bundle_and_menu_arguments() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.application).unwrap();
        write_fixture_app(&f.paths.application, b"old application");
        let original = bundle_digest(&f.paths.application).unwrap();
        let menu = json!({"executable":f.paths.application.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]});
        f.host.menu_process = Some(menu.clone());
        f.host.fail_once = Some(("bootstrap".into(), LABEL.into()));
        let error = f.activate().unwrap_err().to_string();
        assert!(error.contains("bootstrap failure"), "{error}");
        assert_eq!(bundle_digest(&f.paths.application).unwrap(), original);
        assert_eq!(f.host.menu_process, Some(menu));
        assert_eq!(f.host.jobs, f.old);
        assert_eq!(std::fs::read(&f.paths.worker).unwrap(), b"old worker");
        assert_eq!(std::fs::read(&f.paths.menu).unwrap(), b"old menu");
        assert_eq!(std::fs::read(&f.paths.config).unwrap(), b"old config");
        assert!(!f.host.held.load(Ordering::SeqCst));
    }
    #[test]
    fn unrelated_application_is_preserved_before_jobs_stop() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.application).unwrap();
        write_fixture_app(&f.paths.application, b"unrelated user application");
        let original = bundle_digest(&f.paths.application).unwrap();
        assert!(f.activate().is_err());
        assert_eq!(bundle_digest(&f.paths.application).unwrap(), original);
        assert!(!f.host.commands.iter().any(|args| args[0] == "bootout"));
    }
    #[test]
    fn changed_staged_application_is_rejected_before_publication() {
        let f = Fixture::normal();
        let mut application = ApplicationSwap::prepare(
            &f.paths.application,
            &f.installed,
            &Path::new(&f.config.state_dir).join("releases"),
        )
        .unwrap();
        std::fs::write(
            application.staged.join("Contents/MacOS/Jaso NFC"),
            b"unexpected replacement",
        )
        .unwrap();
        assert!(application.publish().is_err());
        assert_eq!(
            std::fs::read_link(&f.paths.application).unwrap(),
            f.old_link
        );
    }
    #[test]
    fn application_replaced_after_classification_is_not_adopted_as_managed() {
        let f = Fixture::normal();
        let classified = managed_application(
            &f.paths.application,
            &Path::new(&f.config.state_dir).join("releases"),
        )
        .unwrap();
        std::fs::remove_file(&f.paths.application).unwrap();
        write_fixture_app(&f.paths.application, b"unrelated replacement");
        let replacement = bundle_digest(&f.paths.application).unwrap();
        assert!(
            ApplicationSwap::prepare_classified(&f.paths.application, &f.installed, classified)
                .is_err()
        );
        assert_eq!(bundle_digest(&f.paths.application).unwrap(), replacement);
    }
    #[test]
    fn application_edited_after_classification_keeps_the_validated_digest() {
        let f = Fixture::normal();
        std::fs::remove_file(&f.paths.application).unwrap();
        write_fixture_app(&f.paths.application, b"old application");
        let classified = managed_application(
            &f.paths.application,
            &Path::new(&f.config.state_dir).join("releases"),
        )
        .unwrap();
        let identity = entry_identity(&f.paths.application).unwrap();
        std::fs::write(
            f.paths.application.join("Contents/MacOS/Jaso NFC"),
            b"user modification",
        )
        .unwrap();
        assert_eq!(entry_identity(&f.paths.application).unwrap(), identity);
        let replacement = bundle_digest(&f.paths.application).unwrap();
        assert!(
            ApplicationSwap::prepare_classified(&f.paths.application, &f.installed, classified)
                .is_err()
        );
        assert_eq!(bundle_digest(&f.paths.application).unwrap(), replacement);
    }
    #[test]
    fn copy_failure_does_not_stop_jobs_or_replace_existing_app() {
        let mut f = Fixture::normal();
        std::fs::remove_dir_all(&f.installed).unwrap();
        assert!(f.activate().is_err());
        f.assert_restored();
        assert!(!f.host.commands.iter().any(|args| args[0] == "bootout"));
        assert_eq!(
            std::fs::read_dir(f.paths.application.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn unrelated_symlink_is_preserved_before_jobs_stop() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.application).unwrap();
        let unrelated = f.temp.path().join("user-owned.app");
        write_fixture_app(&unrelated, b"user owned");
        std::os::unix::fs::symlink(&unrelated, &f.paths.application).unwrap();
        assert!(f.activate().is_err());
        assert_eq!(std::fs::read_link(&f.paths.application).unwrap(), unrelated);
        assert!(!f.host.commands.iter().any(|args| args[0] == "bootout"));
    }
    #[test]
    fn exchange_retains_the_previous_entry_and_restores_it_exactly() {
        for real_bundle in [false, true] {
            let f = Fixture::normal();
            if real_bundle {
                std::fs::remove_file(&f.paths.application).unwrap();
                write_fixture_app(&f.paths.application, b"old application");
            }
            let original = entry_identity(&f.paths.application).unwrap();
            let mut application = ApplicationSwap::prepare(
                &f.paths.application,
                &f.installed,
                &Path::new(&f.config.state_dir).join("releases"),
            )
            .unwrap();
            assert_eq!(application.staged.parent(), f.paths.application.parent());
            application.publish().unwrap();
            assert_eq!(entry_identity(&application.staged).unwrap(), original);
            assert_eq!(
                bundle_digest(&f.paths.application).unwrap(),
                bundle_digest(&f.installed).unwrap()
            );
            application.restore().unwrap();
            assert_eq!(entry_identity(&f.paths.application).unwrap(), original);
        }
    }
    #[test]
    fn unmanaged_menu_is_stopped_before_new_menu_activation() {
        let mut f = Fixture::normal();
        f.host.menu_process = Some(
            json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]}),
        );
        f.activate().unwrap();
        assert!(
            f.host.menu_process.is_none(),
            "the old unmanaged menu must not retain the singleton lock"
        );
        assert_eq!(f.host.menu_stops, 1);
        assert_eq!(f.host.menu_restores, 0);
    }
    #[test]
    fn verified_menu_handoff_precedes_bootout_that_invalidates_process_identity() {
        let mut f = Fixture::normal();
        f.host.menu_process = Some(
            json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]}),
        );
        f.activate().unwrap();
        let handoff = f
            .host
            .commands
            .iter()
            .position(|args| args[0] == "menu-stop")
            .unwrap();
        let first_bootout = f
            .host
            .commands
            .iter()
            .position(|args| args[0] == "bootout")
            .unwrap();
        assert!(handoff < first_bootout);
        assert!(!f.host.menu_identity_invalidated);
    }
    #[test]
    fn worker_stop_failure_restores_menu_already_stopped_by_verified_handoff() {
        let mut f = Fixture::normal();
        let previous = json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]});
        f.host.menu_process = Some(previous.clone());
        f.host.fail_once = Some(("bootout".into(), LABEL.into()));
        assert!(f.activate().is_err());
        f.assert_restored();
        assert_eq!(f.host.menu_stops, 1);
        assert_eq!(f.host.menu_restores, 1);
        assert_eq!(f.host.menu_process, Some(previous));
    }
    #[test]
    fn cli_stop_then_start_survives_delayed_registration_removal() {
        let mut f = Fixture::normal();
        f.host.removal_delay = 3;
        stop_label_with(&mut f.host, LABEL).unwrap();
        start_with(&mut f.host, &f.paths.worker).unwrap();

        // Let any accepted old teardown finish after the apparent restart.
        for _ in 0..3 {
            let _ = host_loaded(&mut f.host, LABEL).unwrap();
        }
        assert!(
            f.host.jobs[LABEL].loaded,
            "successful sequential stop/start must not leave a departing worker"
        );
        assert!(f.host.pending_removals.is_empty());
        assert_eq!(
            f.host.bootstrapped,
            vec![(LABEL.into(), b"old worker".to_vec())]
        );
        assert!(!f.host.commands.iter().any(|args| args[0] == "kickstart"));
        assert_eq!(f.host.jobs[&menu_label()], f.old[&menu_label()]);
    }
    #[test]
    fn cli_stop_reports_unconfirmed_registration_removal() {
        let mut f = Fixture::normal();
        f.host.removal_delay = usize::MAX;
        let error = stop_label_with(&mut f.host, LABEL).unwrap_err().to_string();
        assert!(
            error.contains("timed out waiting for launchctl to remove"),
            "{error}"
        );
        assert!(f.host.jobs[LABEL].loaded);
        assert!(f.host.pending_removals.contains_key(LABEL));
    }
    #[test]
    fn cli_start_keeps_an_existing_stable_registration() {
        let mut f = Fixture::normal();
        start_with(&mut f.host, &f.paths.worker).unwrap();
        assert!(f.host.jobs[LABEL].loaded);
        assert!(f.host.bootstrapped.is_empty());
        assert!(f.host.commands.iter().any(|args| args[0] == "kickstart"));
    }
    #[test]
    fn cli_start_preserves_disabled_login_startup() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().disabled = true;
        start_with(&mut f.host, &f.paths.worker).unwrap();
        assert!(f.host.jobs[LABEL].loaded);
        assert!(
            f.host.jobs[LABEL].disabled,
            "manual start must retain startup off"
        );
        assert_eq!(f.host.jobs[&menu_label()], f.old[&menu_label()]);
    }
    #[test]
    fn cli_start_preserves_disabled_login_startup_after_failed_bootstrap() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().loaded = false;
        f.host.jobs.get_mut(LABEL).unwrap().disabled = true;
        f.host.fail_once = Some(("bootstrap".into(), LABEL.into()));
        assert!(start_with(&mut f.host, &f.paths.worker).is_err());
        assert!(
            f.host.jobs[LABEL].disabled,
            "failed start must restore startup off"
        );
        assert!(!f.host.jobs[LABEL].loaded);
    }
    #[test]
    fn login_startup_change_restores_worker_setting_when_change_fails() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().disabled = false;
        f.host.jobs.get_mut(&menu_label()).unwrap().disabled = false;
        let previous = f.host.jobs.clone();
        f.host.fail_once = Some(("disable".into(), LABEL.into()));
        assert!(
            crate::lifecycle::startup_with(&mut f.host, &[(LABEL, &f.paths.worker)], "off")
                .is_err()
        );
        assert_eq!(
            f.host.jobs, previous,
            "failed startup update must preserve both original labels"
        );
    }
    #[test]
    fn login_startup_reports_the_worker_setting_despite_legacy_menu_mismatch() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().disabled = false;
        f.host.jobs.get_mut(&menu_label()).unwrap().disabled = true;
        let value =
            crate::lifecycle::startup_with(&mut f.host, &[(LABEL, &f.paths.worker)], "status")
                .unwrap();
        assert_eq!(value["enabled"], true);
        assert_eq!(value["worker_enabled"], true);
        assert_eq!(value["menu_enabled"], true);
        assert_eq!(value["consistent"], true);
    }
    #[test]
    fn cli_restart_drains_old_registration_and_preserves_login_setting() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().disabled = true;
        f.host.removal_delay = 3;
        crate::lifecycle::restart_with(&mut f.host, &f.paths.worker).unwrap();
        assert!(f.host.pending_removals.is_empty());
        assert!(f.host.jobs[LABEL].loaded);
        assert!(f.host.jobs[LABEL].disabled);
        assert!(!f.host.commands.iter().any(|args| args[0] == "kickstart"));
    }
    #[test]
    fn cli_restart_never_starts_while_old_registration_is_departing() {
        let mut f = Fixture::normal();
        f.host.removal_delay = usize::MAX;
        assert!(crate::lifecycle::restart_with(&mut f.host, &f.paths.worker).is_err());
        assert!(
            !f.host
                .commands
                .iter()
                .any(|args| matches!(args[0].as_str(), "enable" | "kickstart" | "bootstrap"))
        );
    }
    #[test]
    fn delayed_bootout_is_drained_before_rollback_restores_loaded_worker() {
        let mut f = Fixture::normal();
        f.host.removal_delay = 2;
        f.host.fail_once = Some(("bootout".into(), menu_label()));
        assert!(f.activate().is_err());
        assert!(
            f.host.pending_removals.is_empty(),
            "rollback must not accept a registration still being removed"
        );
        f.assert_restored();
        assert!(
            f.host
                .bootstrapped
                .iter()
                .any(|(label, bytes)| label == LABEL && bytes == b"old worker")
        );
    }
    #[test]
    fn bootout_does_not_succeed_while_registration_remains() {
        let mut f = Fixture::normal();
        f.host.removal_delay = usize::MAX;
        let mut pending = BTreeSet::new();
        assert!(host_stop(&mut f.host, LABEL, &mut pending).is_err());
        assert!(pending.contains(LABEL));
    }
    #[test]
    fn activation_reports_incomplete_rollback_while_old_worker_removal_is_unconfirmed() {
        let mut f = Fixture::normal();
        f.host.removal_delay = usize::MAX;
        let error = f.activate().unwrap_err().to_string();
        assert!(error.contains("rollback also failed"), "{error}");
        assert!(f.host.bootstrapped.is_empty());
        assert_eq!(std::fs::read(&f.paths.worker).unwrap(), b"old worker");
        assert_eq!(
            std::fs::read_link(&f.paths.application).unwrap(),
            f.old_link
        );
    }
    #[test]
    fn activation_rollback_waits_for_late_removal_then_restarts_old_worker() {
        let mut f = Fixture::normal();
        // The first bounded stop wait expires before registration disappears.
        f.host.removal_delay = 152;
        let error = f.activate().unwrap_err().to_string();
        assert!(!error.contains("rollback also failed"), "{error}");
        assert!(f.host.pending_removals.is_empty());
        f.assert_restored();
        assert!(
            f.host
                .bootstrapped
                .iter()
                .any(|(label, bytes)| label == LABEL && bytes == b"old worker")
        );
    }
    #[test]
    fn unmanaged_menu_and_original_arguments_return_after_activation_failure() {
        let mut f = Fixture::new(
            JobState {
                loaded: true,
                disabled: true,
            },
            JobState {
                loaded: false,
                disabled: true,
            },
        );
        let previous = json!({"executable":f.old_link.join("Contents/MacOS/Jaso NFC"),"arguments":["--config","/custom/config.json"]});
        f.host.menu_process = Some(previous.clone());
        f.host.fail_once = Some(("bootstrap".into(), LABEL.into()));
        assert!(f.activate().is_err());
        f.assert_restored();
        assert_eq!(f.host.menu_stops, 1);
        assert_eq!(f.host.menu_restores, 1);
        assert_eq!(f.host.menu_process, Some(previous));
    }
    #[test]
    fn unidentified_menu_aborts_before_existing_jobs_stop() {
        let mut f = Fixture::normal();
        f.host.fail_menu_snapshot = true;
        assert!(f.activate().is_err());
        f.assert_restored();
        assert!(!f.host.commands.iter().any(|c| c[0] == "bootout"));
    }
    #[test]
    fn menu_handoff_failure_restores_jobs_before_staging() {
        let mut f = Fixture::normal();
        f.host.fail_menu_stop = true;
        assert!(f.activate().is_err());
        f.assert_restored();
        assert!(!f.host.commands.iter().any(|args| args[0] == "bootout"));
    }
    #[test]
    fn runtime_lock_timeout_restores_jobs_stopped_before_staging() {
        let mut f = Fixture::normal();
        f.host.fail_lock = true;
        assert!(
            f.activate()
                .unwrap_err()
                .to_string()
                .contains("lock timeout")
        );
        f.assert_restored();
    }
    #[test]
    fn backup_directory_failure_restores_jobs_without_rewriting_artifacts() {
        let mut f = Fixture::normal();
        std::fs::write(
            Path::new(&f.config.state_dir).join("backups"),
            b"not a directory",
        )
        .unwrap();
        assert!(f.activate().is_err());
        f.assert_restored();
    }
    #[test]
    fn menu_shutdown_failure_restores_already_stopped_worker() {
        let mut f = Fixture::normal();
        f.host.fail_once = Some(("bootout".into(), menu_label()));
        assert!(
            f.activate()
                .unwrap_err()
                .to_string()
                .contains("bootout failure")
        );
        f.assert_restored();
    }
    #[test]
    fn failed_bootstrap_restores_disabled_and_unloaded_jobs_exactly() {
        let mut f = Fixture::new(
            JobState {
                loaded: true,
                disabled: true,
            },
            JobState {
                loaded: false,
                disabled: true,
            },
        );
        f.host.fail_once = Some(("bootstrap".into(), LABEL.into()));
        assert!(
            f.activate()
                .unwrap_err()
                .to_string()
                .contains("bootstrap failure")
        );
        f.assert_restored();
        assert!(
            f.host
                .bootstrapped
                .iter()
                .any(|(label, bytes)| label == LABEL && bytes == b"old worker")
        );
        assert!(
            !f.host
                .bootstrapped
                .iter()
                .any(|(label, bytes)| label == &menu_label() && bytes == b"old menu")
        );
    }
    #[test]
    fn failed_first_install_removes_new_artifacts_and_restores_disabled_state() {
        let mut f = Fixture::new(
            JobState {
                loaded: false,
                disabled: true,
            },
            JobState {
                loaded: false,
                disabled: true,
            },
        );
        for p in [
            &f.paths.worker,
            &f.paths.menu,
            &f.paths.config,
            &f.paths.application,
        ] {
            std::fs::remove_file(p).unwrap();
        }
        f.host.fail_once = Some(("bootstrap".into(), LABEL.into()));
        assert!(f.activate().is_err());
        assert_eq!(f.host.jobs, f.old);
        for p in [
            &f.paths.worker,
            &f.paths.menu,
            &f.paths.config,
            &f.paths.application,
        ] {
            assert!(p.symlink_metadata().is_err());
        }
    }
    #[test]
    fn successful_activation_loads_worker_and_preserves_disabled_login() {
        let mut f = Fixture::new(
            JobState {
                loaded: false,
                disabled: true,
            },
            JobState {
                loaded: false,
                disabled: true,
            },
        );
        f.activate().unwrap();
        assert!(f.host.jobs[LABEL].loaded && f.host.jobs[LABEL].disabled);
        assert!(!f.host.jobs[&menu_label()].loaded);
        assert!(!f.paths.menu.exists());
        assert!(
            !f.paths
                .application
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            bundle_digest(&f.paths.application).unwrap(),
            bundle_digest(&f.installed).unwrap()
        );
        assert_eq!(
            Config::load(&f.paths.config).unwrap().signature(),
            f.config.signature()
        );
        assert!(f.temp.path().join("support/backups").exists());
    }
    #[test]
    fn successful_activation_preserves_worker_login_preference_and_retires_menu() {
        let mut f = Fixture::new(
            JobState {
                loaded: true,
                disabled: true,
            },
            JobState {
                loaded: true,
                disabled: false,
            },
        );
        f.activate().unwrap();
        for (label, previous) in &f.old {
            assert_eq!(f.host.jobs[label].loaded, label == LABEL);
            assert_eq!(f.host.jobs[label].disabled, previous.disabled);
        }
    }
    #[test]
    fn activation_rolls_back_when_login_preference_cannot_be_restored() {
        let mut f = Fixture::new(
            JobState {
                loaded: true,
                disabled: true,
            },
            JobState {
                loaded: true,
                disabled: false,
            },
        );
        let index_path = f.config.state_path("index.sqlite3");
        drop(crate::index::Index::new(&index_path, false).unwrap());
        let history_path = f.config.state_path("history.sqlite3");
        std::fs::write(&history_path, b"rebuildable new history cache").unwrap();
        f.host.fail_once = Some(("disable".into(), LABEL.into()));
        assert!(f.activate().is_err());
        f.assert_restored();
        let db = rusqlite::Connection::open(index_path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT type FROM sqlite_master WHERE name='entries'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "table",
            "rollback must restore a schema the previous worker can open"
        );
        assert!(
            !history_path.exists(),
            "old worker must rebuild its own cache schema"
        );
    }
    #[test]
    fn login_state_parser_accepts_boolean_and_named_launchctl_formats() {
        for token in ["true", "disabled"] {
            assert!(
                disabled_from_output(&format!("\t\"{LABEL}\" => {token}\n"), LABEL),
                "disabled token {token}"
            );
        }
        for token in ["false", "enabled"] {
            assert!(!disabled_from_output(
                &format!("\t\"{LABEL}\" => {token}\n"),
                LABEL
            ));
        }
        assert!(!disabled_from_output(
            &format!("\"{}\" => disabled\n", menu_label()),
            LABEL
        ));
    }
    #[test]
    fn failed_snapshot_does_not_stop_existing_jobs() {
        let mut f = Fixture::normal();
        f.host.fail_once = Some(("print".into(), menu_label()));
        assert!(f.activate().is_err());
        f.assert_restored();
        assert!(!f.host.commands.iter().any(|a| a[0] == "bootout"));
    }
    #[test]
    fn loaded_job_without_a_restorable_plist_is_rejected_before_stop() {
        let mut f = Fixture::normal();
        std::fs::remove_file(&f.paths.worker).unwrap();
        assert!(
            f.activate()
                .unwrap_err()
                .to_string()
                .contains("existing plist")
        );
        assert_eq!(f.host.jobs, f.old);
        assert!(!f.host.commands.iter().any(|a| a[0] == "bootout"));
    }
    #[test]
    fn rollback_failure_reports_both_errors_and_retains_backup() {
        let mut f = Fixture::normal();
        f.host.jobs.get_mut(LABEL).unwrap().disabled = true;
        f.old = f.host.jobs.clone();
        f.host.fail_once = Some(("disable".into(), LABEL.into()));
        f.host.fail_after = Some(("bootout".into(), LABEL.into()));
        let error = f.activate().unwrap_err().to_string();
        assert!(error.contains("disable failure"));
        assert!(error.contains("rollback also failed"));
        assert!(error.contains("bootout failure"));
        assert!(error.contains("Preserved backup:"));
        let backup = std::fs::read_dir(Path::new(&f.config.state_dir).join("backups"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(backup.join("installation.json")).unwrap())
                .unwrap();
        let displaced = Path::new(manifest["previous_application_backup"].as_str().unwrap());
        assert_eq!(std::fs::read_link(displaced).unwrap(), f.old_link);
        // A live new worker must never see its files replaced during rollback.
        assert_ne!(std::fs::read(&f.paths.worker).unwrap(), b"old worker");
        assert!(
            !f.host
                .bootstrapped
                .iter()
                .any(|(_, bytes)| bytes == b"old worker")
        );
    }
}

#[cfg(test)]
mod bundle_digest_tests {
    use super::*;
    use std::fs;
    fn bundle(parent: &Path, name: &str, reverse: bool) -> PathBuf {
        let root = parent.join(name);
        let mut files = vec![
            ("Contents/MacOS/jaso-nfc", b"worker".as_slice()),
            ("Contents/MacOS/Jaso NFC", b"menu".as_slice()),
            ("Contents/Info.plist", b"info".as_slice()),
            ("Contents/Resources/JasoNFC.icns", b"icon".as_slice()),
        ];
        if reverse {
            files.reverse();
        }
        for (name, bytes) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        root
    }
    #[test]
    fn icon_and_info_changes_each_select_new_release() {
        let temp = tempfile::tempdir().unwrap();
        let root = bundle(temp.path(), "Jaso NFC.app", false);
        let original = bundle_digest(&root).unwrap();
        fs::write(root.join("Contents/Resources/JasoNFC.icns"), b"new icon").unwrap();
        let icon = bundle_digest(&root).unwrap();
        assert_ne!(
            original, icon,
            "resource-only update must select a new immutable release"
        );
        fs::write(root.join("Contents/Info.plist"), b"new info").unwrap();
        assert_ne!(
            icon,
            bundle_digest(&root).unwrap(),
            "metadata-only update must select a new release"
        );
    }
    #[test]
    fn digest_is_independent_of_bundle_location_and_creation_order() {
        let temp = tempfile::tempdir().unwrap();
        let a = bundle(temp.path(), "first.app", false);
        let b = bundle(temp.path(), "second.app", true);
        assert_eq!(bundle_digest(&a).unwrap(), bundle_digest(&b).unwrap());
    }
    #[test]
    fn resource_rename_or_empty_directory_changes_digest() {
        let temp = tempfile::tempdir().unwrap();
        let root = bundle(temp.path(), "Jaso NFC.app", false);
        let original = bundle_digest(&root).unwrap();
        fs::rename(
            root.join("Contents/Resources/JasoNFC.icns"),
            root.join("Contents/Resources/Other.icns"),
        )
        .unwrap();
        let renamed = bundle_digest(&root).unwrap();
        assert_ne!(original, renamed);
        fs::create_dir(root.join("Contents/Empty")).unwrap();
        assert_ne!(renamed, bundle_digest(&root).unwrap());
    }
    #[test]
    fn symbolic_link_entries_are_rejected_without_following_external_content() {
        let temp = tempfile::tempdir().unwrap();
        for directory in [false, true] {
            let name = if directory {
                "directory.app"
            } else {
                "file.app"
            };
            let root = bundle(temp.path(), name, false);
            let outside = temp.path().join(format!("outside-{directory}"));
            if directory {
                fs::create_dir(&outside).unwrap();
            } else {
                fs::write(&outside, b"outside private content").unwrap();
            }
            std::os::unix::fs::symlink(&outside, root.join("Contents/Resources/unexpected"))
                .unwrap();
            assert!(
                bundle_digest(&root).is_err(),
                "unexpected links must not become release contents"
            );
        }
    }
    #[test]
    fn special_file_entries_are_rejected_instead_of_opened() {
        let temp = tempfile::tempdir().unwrap();
        let root = bundle(temp.path(), "Jaso NFC.app", false);
        let fifo = std::ffi::CString::new(
            root.join("Contents/Resources/pipe")
                .as_os_str()
                .as_encoded_bytes(),
        )
        .unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(bundle_digest(&root).is_err());
    }
}
