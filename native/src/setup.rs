//! App setup: an editable draft, an observation-only preview, and a locked reload.
use crate::{
    config::Config,
    control,
    directory_io::{self, CancellationScope},
    normalizer::Normalizer,
    policy::{Policy, absolute, nfc},
    sources,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub scope: String,
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub apply: bool,
}
impl From<&Config> for Draft {
    fn from(config: &Config) -> Self {
        Self {
            scope: config.scope.clone(),
            roots: config.roots.clone(),
            excludes: config.excludes.clone(),
            apply: config.apply,
        }
    }
}
impl Draft {
    pub fn parse(text: &str) -> Result<Self> {
        ensure!(
            text.len() <= 1024 * 1024,
            "The folder settings are too large."
        );
        serde_json::from_str(text).context("The folder settings are incomplete or invalid.")
    }
    fn merge(&self, saved: &Config) -> Result<Config> {
        ensure!(
            matches!(self.scope.as_str(), "configured" | "all-user-files"),
            "Choose selected folders or all user files."
        );
        ensure!(
            self.scope != "configured" || !self.roots.is_empty(),
            "Choose at least one folder."
        );
        ensure!(
            self.scope != "all-user-files" || self.roots.is_empty(),
            "All user files cannot be combined with selected folders."
        );
        for paths in [&self.roots, &self.excludes] {
            ensure!(
                paths.len() <= 256,
                "Choose no more than 256 folders or exclusions."
            );
            ensure!(
                paths.iter().all(|path| Path::new(path).is_absolute()
                    && !path.contains('\0')
                    && path.len() <= 4096),
                "Folder paths must be absolute and valid."
            );
        }
        let mut config = saved.clone();
        config.scope = self.scope.clone();
        config.roots = self.roots.clone();
        config.excludes = self.excludes.clone();
        config.apply = self.apply;
        config.validate()?;
        Ok(config)
    }
}

struct Saved {
    bytes: Vec<u8>,
    config: Config,
    revision: String,
}
impl Saved {
    fn read(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "The configuration path must be absolute."
        );
        let bytes = std::fs::read(path).context("Cannot read the saved folder settings.")?;
        let mut config: Config =
            serde_json::from_slice(&bytes).context("The saved folder settings are invalid.")?;
        config.validate()?;
        let revision = revision(&bytes);
        Ok(Self {
            bytes,
            config,
            revision,
        })
    }
}
fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn read(path: &Path) -> Result<Value> {
    let saved = Saved::read(path)?;
    Ok(
        json!({"config":Draft::from(&saved.config),"revision":saved.revision,
        "running":control::running(&saved.config)?,"paused":control::paused(&saved.config)?}),
    )
}

/// The timeout token only targets read-only directory I/O, never state writes.
struct PreviewDeadline {
    cancelled: Arc<AtomicBool>,
    done: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl PreviewDeadline {
    fn new(timeout: Duration) -> Result<Self> {
        let cancelled = Arc::new(AtomicBool::new(timeout.is_zero()));
        let token = cancelled.clone();
        let (done, wait) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("setup-preview-deadline".into())
            .spawn(move || {
                if wait.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                    token.store(true, Ordering::Release);
                    directory_io::notify_cancellation();
                }
            })?;
        Ok(Self {
            cancelled,
            done: Some(done),
            thread: Some(thread),
        })
    }
    fn expired(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    fn cancelled_io(&self, error: &anyhow::Error) -> bool {
        self.expired()
            && error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.raw_os_error() == Some(libc::ECANCELED))
    }
}
impl Drop for PreviewDeadline {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn root_problem(root: &str, policy: &Policy) -> Option<anyhow::Error> {
    if !policy.accepts_lexically(root) {
        return Some(anyhow::anyhow!(
            "This folder is excluded by the saved folder rules."
        ));
    }
    match directory_io::metadata(Path::new(root), false) {
        Ok(info) if info.st_mode as u32 & libc::S_IFMT as u32 != libc::S_IFDIR as u32 => Some(
            anyhow::anyhow!("Choose a folder, not a file or symbolic link."),
        ),
        Ok(_) if Policy::blocked_directory(root, true) => Some(anyhow::anyhow!(
            "This application or library package is protected. Choose a document folder."
        )),
        Ok(_) => crate::native_names::open_at(
            libc::AT_FDCWD,
            root,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
        .err()
        .map(|error| anyhow::Error::new(error).context("Cannot open this folder")),
        Err(error) => Some(anyhow::Error::new(error).context("This folder is unavailable")),
    }
}

fn validate_new_roots(config: &Config, saved: &Config) -> Result<()> {
    if config.scope != "configured" {
        return Ok(());
    }
    let policy = config.policy();
    for root in &config.roots {
        let retained =
            saved.scope == "configured" && saved.roots.iter().any(|old| nfc(old) == nfc(root));
        if !retained && let Some(error) = root_problem(root, &policy) {
            return Err(error.context(root.clone()));
        }
    }
    Ok(())
}

pub fn preview(path: &Path, draft: &Draft) -> Result<Value> {
    preview_with_limits(path, draft, 200, 10_000, Duration::from_secs(10))
}
fn partition_preview_checks(checks: Vec<Value>, timed_out: bool) -> (Vec<Value>, Vec<Value>) {
    // ECANCELED does not identify who cancelled an operation. Retain the
    // original diagnostic separately when a time limit also occurred, including
    // cancellations already recorded before that limit. Never discard evidence.
    let cancellation = std::io::Error::from_raw_os_error(libc::ECANCELED).to_string();
    checks.into_iter().partition(|check| {
        !timed_out
            || !check["error"]
                .as_str()
                .is_some_and(|error| error.ends_with(&cancellation))
    })
}
fn preview_with_limits(
    path: &Path,
    draft: &Draft,
    sample_limit: usize,
    entry_limit: usize,
    timeout: Duration,
) -> Result<Value> {
    let saved = Saved::read(path)?;
    let mut config = draft.merge(&saved.config)?;
    config.apply = false;
    let deadline = PreviewDeadline::new(timeout)?;
    let _cancellation = CancellationScope::new(deadline.cancelled.clone());
    let mut errors = Vec::new();
    if let Err(error) = validate_new_roots(&config, &saved.config) {
        if !deadline.cancelled_io(&error) {
            return Err(error);
        }
        errors.push(json!({"path":error.to_string(),"error":format!("{error:#}")}));
    }
    let coverage = sources::resolve_coverage(&config);
    let policy = sources::policy_for(&config, &coverage, None);
    let mut queue = VecDeque::new();
    let mut queued = HashSet::new();
    for root in &coverage.roots {
        if let Some(error) = root_problem(root, &policy) {
            errors.push(json!({"path":root,"error":format!("{error:#}")}));
        } else if queued.insert(nfc(root)) {
            queue.push_back(root.clone());
        }
    }
    for root in &coverage.unavailable {
        let error = coverage
            .unavailable_reasons
            .get(root)
            .cloned()
            .unwrap_or_else(|| "This location is unavailable.".into());
        errors.push(json!({"path":root,"error":error}));
    }
    let mut normalizer = Normalizer::new(policy, None, None, None, false)?;
    let mut entries = 0;
    let mut candidates = Vec::new();
    let mut truncated = false;
    let mut traversal_complete = true;
    while let Some(directory) = queue.pop_front() {
        if entries >= entry_limit || deadline.cancelled.load(Ordering::Acquire) {
            truncated = true;
            traversal_complete = false;
            break;
        }
        let result = match normalizer.reconcile_step(&directory, false) {
            Ok(result) => result,
            Err(error) if deadline.cancelled_io(&error) => {
                errors.push(json!({"path":directory,"error":format!("{error:#}")}));
                break;
            }
            Err(error) => return Err(error),
        };
        for error in result.errors {
            errors.push(json!({"path":error.path,"error":error.error}));
        }
        for entry in result.entries {
            entries += 1;
            let name = Path::new(&entry.path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let after = nfc(name);
            if after != name
                && !config
                    .roots
                    .iter()
                    .any(|root| nfc(root) == nfc(&entry.path))
            {
                if candidates.len() < sample_limit {
                    candidates.push(json!({"path":entry.path,"before":name,"after":after}));
                } else {
                    truncated = true;
                }
            }
            if matches!(entry.kind.as_str(), "dir" | "directory")
                && normalizer.policy.descend(&entry.path)
                && queued.insert(nfc(&entry.path))
            {
                queue.push_back(entry.path);
            }
        }
        if !result.complete {
            queue.push_front(directory);
        }
        if errors.len() >= 100 {
            truncated = true;
            traversal_complete = false;
            break;
        }
    }
    let timed_out = deadline.expired();
    if timed_out {
        truncated = true;
        traversal_complete = false;
    }
    candidates.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let (errors, interrupted_checks) = partition_preview_checks(errors, timed_out);
    Ok(
        json!({"candidates":candidates,"entries":entries,"errors":errors,"truncated":truncated,
        "complete":traversal_complete && errors.is_empty(),"revision":saved.revision,
        "interrupted_checks":interrupted_checks,
        "stop_reason":if timed_out {Some("time_limit")} else {None}}),
    )
}

pub fn save(path: &Path, draft: &Draft, expected_revision: &str, start: bool) -> Result<Value> {
    // Reject stale or malformed requests before creating any lifecycle artifact.
    let before = Saved::read(path)?;
    check_revision(&before, expected_revision)?;
    let _draft = draft.merge(&before.config)?;
    let _operation = crate::install::lifecycle_lock()?;
    let user_home = std::env::var("HOME").context("The account home is unavailable.")?;
    let registration = Path::new(&user_home)
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", crate::service::LABEL));
    save_with(
        path,
        draft,
        expected_revision,
        start,
        &registration,
        &mut NativeSetupHost,
    )
}

trait SetupHost: crate::lifecycle::LaunchHost {
    fn registered_config(&mut self, registration: &Path) -> Result<PathBuf>;
    fn wait_ready(&mut self, config: &Config, path: &Path) -> Result<()>;
}
struct NativeSetupHost;
impl crate::lifecycle::LaunchHost for NativeSetupHost {}
impl SetupHost for NativeSetupHost {
    fn registered_config(&mut self, registration: &Path) -> Result<PathBuf> {
        let output = std::process::Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "--"])
            .arg(registration)
            .output()?;
        ensure!(
            output.status.success(),
            "Cannot read the installed worker registration. Reinstall Jaso NFC to repair it."
        );
        registration_config(&serde_json::from_slice(&output.stdout)?)
    }
    fn wait_ready(&mut self, config: &Config, path: &Path) -> Result<()> {
        for _ in 0..150 {
            if acknowledged(config, path)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!(
            "The worker did not confirm the new folder settings. The previous settings will be restored."
        )
    }
}
fn registration_config(value: &Value) -> Result<PathBuf> {
    ensure!(
        value["Label"] == crate::service::LABEL,
        "The installed worker registration has an unexpected label."
    );
    let args = value["ProgramArguments"]
        .as_array()
        .context("The worker registration has no arguments.")?;
    ensure!(
        args.len() == 4
            && args[0]
                .as_str()
                .is_some_and(|path| Path::new(path).is_absolute())
            && matches!(args[1].as_str(), Some("run" | "watch"))
            && args[2] == "--config",
        "The installed worker registration is not a native configuration."
    );
    let path = args[3]
        .as_str()
        .filter(|path| Path::new(path).is_absolute() && !path.contains('\0'))
        .context("The worker registration has an invalid configuration path.")?;
    Ok(PathBuf::from(path))
}
fn acknowledged(config: &Config, path: &Path) -> Result<bool> {
    if !control::running(config)? {
        return Ok(false);
    }
    let value: Value = match std::fs::read(config.state_path("runtime.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let pid = value["pid"]
        .as_i64()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as i64);
    Ok(value["config_signature"] == config.signature()
        && value["config_path"] == path.to_string_lossy().as_ref()
        && pid.is_some_and(|pid| unsafe { libc::kill(pid as i32, 0) } == 0))
}

/// Acknowledgement published by the worker itself while it owns RuntimeLock.
/// An instance token prevents teardown from deleting a replacement's record.
pub(crate) struct RuntimeReady {
    path: PathBuf,
    instance: String,
}
impl RuntimeReady {
    pub(crate) fn publish(config: &Config, config_path: Option<&Path>) -> Result<Self> {
        let instance = uuid::Uuid::new_v4().to_string();
        let path = config.state_path("runtime.json");
        crate::config::atomic_json(
            &path,
            &json!({"pid":std::process::id(),"config_signature":config.signature(),
            "config_path":config_path,"instance":instance}),
        )?;
        Ok(Self { path, instance })
    }
}
impl Drop for RuntimeReady {
    fn drop(&mut self) {
        if std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| value["instance"] == self.instance)
        {
            let _ = remove_optional(&self.path);
        }
    }
}
fn check_revision(saved: &Saved, expected: &str) -> Result<()> {
    ensure!(
        saved.revision == expected,
        "Folder settings changed while this window was open. Reload them and preview again."
    );
    Ok(())
}
fn optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn remove_optional(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn write_bytes(path: &Path, bytes: &[u8], permissions: Option<std::fs::Permissions>) -> Result<()> {
    use std::{
        io::Write,
        os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    };
    let parent = path
        .parent()
        .context("The settings file has no parent directory.")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".setup-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        if let Some(mode) = permissions {
            file.set_permissions(mode)?;
        }
        ensure!(
            unsafe { libc::fsync(file.as_raw_fd()) } == 0,
            "Cannot flush saved folder settings: {}",
            std::io::Error::last_os_error()
        );
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = remove_optional(&temporary);
    result
}
fn save_with(
    path: &Path,
    draft: &Draft,
    expected_revision: &str,
    start: bool,
    registration: &Path,
    host: &mut impl SetupHost,
) -> Result<Value> {
    use crate::lifecycle::{
        disabled_from_output, domain, host_checked, host_loaded, host_stop, start_with,
        wait_for_job_absence,
    };
    let saved = Saved::read(path)?;
    check_revision(&saved, expected_revision)?;
    let config = draft.merge(&saved.config)?;
    ensure!(
        !start || config.apply,
        "Choose automatic cleanup before starting it."
    );
    {
        let deadline = PreviewDeadline::new(Duration::from_secs(10))?;
        let _cancellation = CancellationScope::new(deadline.cancelled.clone());
        validate_new_roots(&config, &saved.config)?;
        ensure!(
            !deadline.cancelled.load(Ordering::Acquire),
            "Checking the selected folders timed out. Reconnect them and try again."
        );
    }
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "The settings file must be a regular file, not a symbolic link."
    );
    ensure!(
        registration.is_file(),
        "Install Jaso NFC before saving its folder settings."
    );
    let registered = host.registered_config(registration)?;
    ensure!(
        nfc(&absolute(&registered.to_string_lossy())) == nfc(&absolute(&path.to_string_lossy())),
        "The worker registration uses another configuration. Reopen the installed app before editing settings."
    );
    let label = crate::service::LABEL;
    let loaded = host_loaded(host, label)?;
    let startup = host.launch(&["print-disabled", &domain()])?;
    ensure!(
        startup.status.success(),
        "Cannot inspect the login startup preference."
    );
    let disabled = disabled_from_output(&String::from_utf8_lossy(&startup.stdout), label);
    let was_paused = control::paused(&saved.config)?;
    let control_path = config.state_path("control.json");
    let control_bytes = optional_bytes(&control_path)?;
    let should_run = start || loaded;
    let desired_pause = if start { false } else { was_paused };
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    let backup = Path::new(&config.state_dir)
        .join("backups")
        .join(format!("setup-{}", uuid::Uuid::new_v4()));
    let mut lock = None;
    let mut changed = false;
    let mut pending_removals = BTreeSet::new();
    let result = (|| -> Result<()> {
        host_stop(host, label, &mut pending_removals)?;
        lock = Some(control::RuntimeLock::acquire(
            &saved.config,
            Duration::from_secs(15),
        )?);
        // Compare again after shutdown, before overwriting an independent editor.
        check_revision(&Saved::read(path)?, expected_revision)?;
        write_bytes(&backup.join("config.json"), &saved.bytes, None)?;
        if let Some(control) = &control_bytes {
            write_bytes(&backup.join("control.json"), control, None)?;
        }
        crate::config::atomic_json(
            backup.join("settings.json"),
            &json!({"config_path":path,"was_running":loaded,"was_paused":was_paused,"startup_disabled":disabled}),
        )?;
        changed = true;
        // No filename mutation can begin before the replacement acknowledges
        // the exact configuration. The user explicitly resumes at commit.
        if should_run {
            control::set_paused(&config, true)?;
        }
        write_bytes(path, &bytes, Some(metadata.permissions()))?;
        remove_optional(&config.state_path("runtime.json"))?;
        drop(lock.take());
        if should_run {
            start_with(host, registration)?;
            host.wait_ready(&config, &registered)?;
            control::set_paused(&config, desired_pause)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let restore = (|| -> Result<()> {
            if changed {
                host_stop(host, label, &mut pending_removals)?;
                if lock.is_none() {
                    lock = Some(control::RuntimeLock::acquire(
                        &saved.config,
                        Duration::from_secs(15),
                    )?);
                }
                write_bytes(path, &saved.bytes, Some(metadata.permissions()))?;
                match &control_bytes {
                    Some(bytes) => write_bytes(&control_path, bytes, None)?,
                    None => remove_optional(&control_path)?,
                }
                remove_optional(&config.state_path("runtime.json"))?;
            }
            drop(lock.take());
            if pending_removals.contains(label) {
                wait_for_job_absence(host, label)?;
                pending_removals.remove(label);
            }
            if loaded {
                if !host_loaded(host, label)? {
                    start_with(host, registration)?;
                }
            } else if host_loaded(host, label)? {
                host_stop(host, label, &mut pending_removals)?;
            }
            host_checked(
                host,
                &[
                    if disabled { "disable" } else { "enable" },
                    &format!("{}/{label}", domain()),
                ],
            )?;
            Ok(())
        })();
        if let Err(restore) = restore {
            anyhow::bail!(
                "{error:#}; restoring previous settings also failed: {restore:#}. Recovery copy: {}",
                backup.display()
            );
        }
        return Err(error);
    }
    Ok(
        json!({"config":Draft::from(&config),"revision":revision(&bytes),"started":should_run,"paused":desired_pause}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LaunchHost;
    use std::os::unix::process::ExitStatusExt;

    struct FakeHost {
        config: PathBuf,
        loaded: bool,
        disabled: bool,
        fail_start: bool,
        fail_ready: bool,
        fail_stop: bool,
        removal_delay: usize,
        pending_removal: usize,
        starts: usize,
        commands: Vec<String>,
    }
    impl LaunchHost for FakeHost {
        fn wait_for_job_removal(&mut self) {}
        fn launch(&mut self, arguments: &[&str]) -> Result<std::process::Output> {
            let verb = arguments[0];
            self.commands.push(verb.into());
            let mut code = 0;
            let mut stdout = String::new();
            match verb {
                "print-disabled" => {
                    stdout = format!("\"{}\" => {}", crate::service::LABEL, self.disabled)
                }
                "print" => {
                    if !self.loaded {
                        code = 113;
                    }
                    if self.pending_removal > 0 {
                        self.pending_removal -= 1;
                        if self.pending_removal == 0 {
                            self.loaded = false;
                        }
                    }
                }
                "enable" => self.disabled = false,
                "disable" => self.disabled = true,
                "bootout" => {
                    if std::mem::take(&mut self.fail_stop) {
                        anyhow::bail!("injected stop failure");
                    }
                    if self.removal_delay > 0 && self.loaded {
                        self.pending_removal = self.removal_delay;
                    } else {
                        self.loaded = false;
                    }
                }
                "bootstrap" => {
                    if std::mem::take(&mut self.fail_start) {
                        anyhow::bail!("injected start failure");
                    }
                    let config = Config::load(&self.config)?;
                    let lock = control::RuntimeLock::acquire(&config, Duration::ZERO)?;
                    drop(lock);
                    ensure!(!self.disabled && !self.loaded, "invalid startup order");
                    self.loaded = true;
                    self.starts += 1;
                }
                "kickstart" => anyhow::bail!("replacement must fully drain before startup"),
                _ => anyhow::bail!("unexpected launch call {arguments:?}"),
            }
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.into_bytes(),
                stderr: vec![],
            })
        }
    }
    impl SetupHost for FakeHost {
        fn registered_config(&mut self, _: &Path) -> Result<PathBuf> {
            Ok(self.config.clone())
        }
        fn wait_ready(&mut self, config: &Config, path: &Path) -> Result<()> {
            ensure!(
                path.as_os_str() == self.config.as_os_str(),
                "acknowledgement must use the exact registered configuration path"
            );
            ensure!(
                control::paused(config)?,
                "a new worker must be staged paused"
            );
            if std::mem::take(&mut self.fail_ready) {
                anyhow::bail!("injected worker acknowledgement failure");
            }
            Ok(())
        }
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        path: PathBuf,
        registration: PathBuf,
        original: Vec<u8>,
        saved: Config,
        draft: Draft,
        host: FakeHost,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("files");
            std::fs::create_dir(&root).unwrap();
            let ignored = root.join("keep");
            std::fs::create_dir(&ignored).unwrap();
            let mut saved = Config {
                roots: vec![root.to_str().unwrap().into()],
                excludes: vec![ignored.to_str().unwrap().into()],
                exclude_names: vec![".git".into(), "private".into()],
                skip_hidden_tops: vec![root.to_str().unwrap().into()],
                state_dir: temp.path().join("state").to_str().unwrap().into(),
                log_dir: Some(temp.path().join("history").to_str().unwrap().into()),
                apply: false,
                ..Config::default()
            };
            saved.validate().unwrap();
            let path = temp.path().join("custom settings.json");
            let original = serde_json::to_vec(&saved).unwrap();
            std::fs::write(&path, &original).unwrap();
            let registration = temp.path().join(format!("{}.plist", crate::service::LABEL));
            std::fs::write(&registration, b"unchanged registration").unwrap();
            std::fs::create_dir_all(saved.logs()).unwrap();
            std::fs::write(
                Path::new(&saved.logs()).join("renames.jsonl"),
                b"retained history",
            )
            .unwrap();
            control::set_paused(&saved, true).unwrap();
            let mut draft = Draft::from(&saved);
            draft.apply = true;
            let host = FakeHost {
                config: path.clone(),
                loaded: true,
                disabled: true,
                fail_start: false,
                fail_ready: false,
                fail_stop: false,
                removal_delay: 0,
                pending_removal: 0,
                starts: 0,
                commands: vec![],
            };
            Self {
                _temp: temp,
                path,
                registration,
                original,
                saved,
                draft,
                host,
            }
        }
        fn save(&mut self, start: bool) -> Result<Value> {
            save_with(
                &self.path,
                &self.draft,
                &revision(&self.original),
                start,
                &self.registration,
                &mut self.host,
            )
        }
        fn unchanged(&self) {
            assert_eq!(std::fs::read(&self.path).unwrap(), self.original);
            assert!(control::paused(&self.saved).unwrap());
            assert!(self.host.loaded && self.host.disabled);
            assert_eq!(
                std::fs::read(&self.registration).unwrap(),
                b"unchanged registration"
            );
            assert_eq!(
                std::fs::read(Path::new(&self.saved.logs()).join("renames.jsonl")).unwrap(),
                b"retained history"
            );
        }
    }
    #[test]
    fn save_preserves_unrelated_fields_history_and_login_preference() {
        let mut f = Fixture::new();
        let result = f.save(true).unwrap();
        let after = Config::load(&f.path).unwrap();
        assert!(after.apply && result["started"] == true && result["paused"] == false);
        assert_eq!(after.excludes, f.saved.excludes);
        assert_eq!(after.exclude_names, f.saved.exclude_names);
        assert_eq!(after.skip_hidden_tops, f.saved.skip_hidden_tops);
        assert_eq!(after.state_dir, f.saved.state_dir);
        assert_eq!(after.log_dir, f.saved.log_dir);
        assert!(f.host.loaded && f.host.disabled);
        assert_eq!(
            std::fs::read(&f.registration).unwrap(),
            b"unchanged registration"
        );
        assert_eq!(
            std::fs::read(Path::new(&f.saved.logs()).join("renames.jsonl")).unwrap(),
            b"retained history"
        );
    }
    #[test]
    fn save_without_start_preserves_paused_and_stopped_choices() {
        let mut f = Fixture::new();
        let result = f.save(false).unwrap();
        assert_eq!(result["paused"], true);
        assert!(control::paused(&f.saved).unwrap());
        let mut f = Fixture::new();
        f.host.loaded = false;
        let result = f.save(false).unwrap();
        assert_eq!(result["started"], false);
        assert!(!f.host.loaded && f.host.disabled);
        assert_eq!(f.host.starts, 0);
    }
    #[test]
    fn startup_and_acknowledgement_failures_restore_exact_config_and_state() {
        for failure in ["start", "ready", "stop"] {
            let mut f = Fixture::new();
            f.host.fail_start = failure == "start";
            f.host.fail_ready = failure == "ready";
            f.host.fail_stop = failure == "stop";
            let error = f.save(true).unwrap_err();
            assert!(error.to_string().contains("injected"), "{error:#}");
            f.unchanged();
        }
    }
    #[test]
    fn registered_config_mismatch_never_touches_lifecycle_or_saved_settings() {
        let mut f = Fixture::new();
        f.host.config = f.path.with_file_name("other.json");
        assert!(
            f.save(true)
                .unwrap_err()
                .to_string()
                .contains("registration")
        );
        assert!(f.host.commands.is_empty());
        f.unchanged();
    }
    #[test]
    fn equivalent_settings_path_acknowledges_the_registered_spelling() {
        let mut f = Fixture::new();
        let supplied = f
            .path
            .parent()
            .unwrap()
            .join(".")
            .join(f.path.file_name().unwrap());
        assert_ne!(supplied.as_os_str(), f.host.config.as_os_str());
        let result = save_with(
            &supplied,
            &f.draft,
            &revision(&f.original),
            true,
            &f.registration,
            &mut f.host,
        )
        .unwrap();
        assert_eq!(result["started"], true);
        assert_eq!(result["paused"], false);
        assert!(Config::load(&f.path).unwrap().apply);
    }

    #[test]
    fn stale_revision_never_touches_lifecycle() {
        let mut f = Fixture::new();
        std::fs::write(&f.path, b"{}\n").unwrap();
        assert!(f.save(true).unwrap_err().to_string().contains("changed"));
        assert!(f.host.commands.is_empty());
        assert_eq!(std::fs::read(&f.path).unwrap(), b"{}\n");
    }
    #[test]
    fn preview_retains_cancelled_checks_recorded_before_a_later_deadline() {
        let cancelled = json!({"path":"/already-checked","error":format!("metadata: {}", std::io::Error::from_raw_os_error(libc::ECANCELED))});
        let genuine: Vec<Value> = [libc::EACCES, libc::ENOENT, libc::ETIMEDOUT]
            .into_iter().map(|code| json!({"path":format!("/error-{code}"),"error":std::io::Error::from_raw_os_error(code).to_string()})).collect();
        let mut recorded = vec![cancelled.clone()];
        recorded.extend(genuine.clone());
        let (errors, interrupted) = partition_preview_checks(recorded.clone(), false);
        assert_eq!(errors, recorded);
        assert!(interrupted.is_empty());
        let later_deadline = PreviewDeadline::new(Duration::ZERO).unwrap();
        let (errors, interrupted) = partition_preview_checks(recorded, later_deadline.expired());
        assert_eq!(errors, genuine);
        assert_eq!(interrupted, vec![cancelled]);
    }

    #[test]
    fn preview_deadline_reports_a_time_limit_without_synthetic_folder_errors() {
        let f = Fixture::new();
        let extra = f.path.parent().unwrap().join("another folder");
        std::fs::create_dir(&extra).unwrap();
        for roots in [f.draft.roots.clone(), vec![extra.to_str().unwrap().into()]] {
            let mut draft = f.draft.clone();
            draft.roots = roots;
            let result = preview_with_limits(&f.path, &draft, 200, 10_000, Duration::ZERO).unwrap();
            assert_eq!(result["stop_reason"], "time_limit");
            assert_eq!(result["complete"], false);
            assert_eq!(result["truncated"], true);
            assert_eq!(result["errors"], json!([]));
            assert!(!result["interrupted_checks"].as_array().unwrap().is_empty());
        }
        assert_eq!(std::fs::read(&f.path).unwrap(), f.original);
    }

    #[test]
    fn preview_keeps_a_real_disconnected_folder_error() {
        let f = Fixture::new();
        std::fs::remove_dir_all(&f.saved.roots[0]).unwrap();
        let result =
            preview_with_limits(&f.path, &f.draft, 200, 10_000, Duration::from_secs(2)).unwrap();
        assert_eq!(result["stop_reason"], Value::Null);
        assert_eq!(result["complete"], false);
        assert_eq!(result["errors"][0]["path"], f.saved.roots[0]);
        assert!(
            result["errors"][0]["error"]
                .as_str()
                .unwrap()
                .contains("unavailable")
        );
    }

    #[test]
    fn preview_samples_are_bounded_without_persistent_changes() {
        use unicode_normalization::UnicodeNormalization;
        let f = Fixture::new();
        for n in 0..8 {
            std::fs::write(
                Path::new(&f.saved.roots[0]).join(format!("한글{n}.txt").nfd().collect::<String>()),
                b"untouched",
            )
            .unwrap();
        }
        let result =
            preview_with_limits(&f.path, &f.draft, 3, 100, Duration::from_secs(2)).unwrap();
        assert_eq!(result["candidates"].as_array().unwrap().len(), 3);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["complete"], true);
        assert_eq!(std::fs::read(&f.path).unwrap(), f.original);
    }

    #[test]
    fn delayed_shutdown_is_drained_before_rollback_restarts_worker() {
        let mut f = Fixture::new();
        f.host.removal_delay = 155;
        let error = f.save(true).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(!f.host.commands.iter().any(|command| command == "kickstart"));
        assert!(f.host.loaded && f.host.disabled);
        assert_eq!(f.host.pending_removal, 0);
        f.unchanged();
    }
}
