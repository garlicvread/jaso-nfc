//! Single-worker source lifecycle; native callbacks only commit or signal work.
#[cfg(not(test))]
use crate::model::now;
use crate::{
    config::{Config, atomic_json},
    coverage::{CatalogReader, Coverage, NativeCatalog, discover_user_coverage},
    events::{self, Callback, CursorInvalidError, Wake},
    index::Index,
    model::{Event, Volume},
    normalizer::Normalizer,
    policy::{Policy, absolute, nfc, within},
};
#[cfg(test)]
thread_local! {
    static TEST_NOW: std::cell::Cell<Option<f64>> = const {std::cell::Cell::new(None)};
}
#[cfg(test)]
fn now() -> f64 {
    TEST_NOW.get().unwrap_or_else(crate::model::now)
}
use anyhow::{Result, bail};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

const REFRESH: u32 = events::MUST_SCAN_SUBDIRS
    | events::USER_DROPPED
    | events::KERNEL_DROPPED
    | events::EVENT_IDS_WRAPPED
    | events::ROOT_CHANGED
    | events::MOUNT
    | events::UNMOUNT;
const RECONFIGURE: u32 = events::ROOT_CHANGED | events::MOUNT | events::UNMOUNT;

/// The inventory uses UUIDs; stream keys additionally encode root layout.
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct DriveInventoryItem {
    pub uuid: String,
    pub mount: String,
    pub name: String,
    pub connected: bool,
    pub included: bool,
    pub reconnect: String,
    pub availability: String,
}
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct DriveInventoryIssue {
    pub mount: String,
    pub reason: String,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct DriveInventory {
    pub items: Vec<DriveInventoryItem>,
    pub issues: Vec<DriveInventoryIssue>,
    pub complete: bool,
}
#[derive(Default, Debug)]
struct ConnectedDrives {
    volumes: Vec<Volume>,
    issues: Vec<DriveInventoryIssue>,
}
impl ConnectedDrives {
    fn unavailable(&mut self, mount: &Path, reason: String) {
        // A canceled shared deadline may affect every remaining mount. Keep
        // diagnostics bounded and mark omitted locations as unknown too.
        const MAX_ISSUES: usize = 32;
        if self.issues.len() < MAX_ISSUES {
            self.issues.push(DriveInventoryIssue {
                mount: mount.to_string_lossy().into_owned(),
                reason: reason.chars().take(2048).collect(),
            });
        } else {
            self.issues[MAX_ISSUES - 1] = DriveInventoryIssue {
                mount: "/Volumes".into(),
                reason: "Additional mounted drives could not be checked.".into(),
            };
        }
    }
}
fn external_display_mount(mount: &str) -> Option<&str> {
    // Native FSEvents identities preserve the physical APFS Data mount. Drive
    // preferences and coverage use the public firmlink spelling; normalize only
    // this comparison/display boundary, never the persisted stream identity.
    let display = mount.strip_prefix("/System/Volumes/Data").unwrap_or(mount);
    let name = display.strip_prefix("/Volumes/")?;
    (!name.is_empty() && !name.contains('/') && !matches!(name, "." | "..")).then_some(display)
}
fn external_mount(mount: &str) -> bool {
    external_display_mount(mount).is_some()
}
fn drive_included(config: &Config, uuid: &str) -> bool {
    if config.drives.mode == "selected" {
        config
            .drives
            .included
            .iter()
            .any(|reference| reference.uuid == uuid)
    } else {
        !config
            .drives
            .excluded
            .iter()
            .any(|reference| reference.uuid == uuid)
    }
}
fn volume_included(config: &Config, volume: &Volume) -> bool {
    !external_mount(&volume.mount)
        || if config.scope == "configured" {
            !config
                .drives
                .excluded
                .iter()
                .any(|reference| reference.uuid == volume.uuid)
        } else {
            drive_included(config, &volume.uuid)
        }
}
fn selected_uuid(config: &Config, uuid: &str, known: &[Volume]) -> bool {
    drive_included(config, uuid)
        && (config
            .drives
            .included
            .iter()
            .any(|reference| reference.uuid == uuid)
            || known.iter().any(|volume| volume.uuid == uuid))
}
fn configured_uuid(config: &Config, uuid: &str, roots: &[String], known: &[Volume]) -> bool {
    if config
        .drives
        .excluded
        .iter()
        .any(|reference| reference.uuid == uuid)
    {
        return false;
    }
    if config
        .drives
        .included
        .iter()
        .any(|reference| reference.uuid == uuid)
    {
        return true;
    }
    let mut pinned = false;
    for saved in known.iter().filter(|saved| external_mount(&saved.mount)) {
        if saved.roots.iter().any(|saved_root| {
            roots
                .iter()
                .any(|root| within(root, saved_root) || within(saved_root, root))
        }) {
            pinned = true;
            if saved.uuid == uuid {
                return true;
            }
        }
    }
    for (saved_uuid, mount) in config
        .drives
        .included
        .iter()
        .chain(config.drives.excluded.iter())
        .map(|saved| (&saved.uuid, &saved.mount))
        .chain(
            config
                .drives
                .reconnect
                .iter()
                .map(|saved| (&saved.uuid, &saved.mount)),
        )
    {
        if roots.iter().any(|root| within(root, mount)) {
            pinned = true;
            if saved_uuid == uuid {
                return true;
            }
        }
    }
    !pinned
}
fn configured_roots_on(config: &Config, mount: &str) -> Vec<String> {
    let mount = external_display_mount(mount).unwrap_or(mount);
    config
        .roots
        .iter()
        .filter(|root| within(root, mount))
        .cloned()
        .collect()
}
fn selected_volume(config: &Config, volume: &Volume, known: &[Volume]) -> bool {
    if !external_mount(&volume.mount) {
        return true;
    }
    if config.scope == "configured" {
        return configured_uuid(config, &volume.uuid, &volume.roots, known);
    }
    selected_uuid(config, &volume.uuid, known)
}
fn saved_volumes(config: &Config) -> Result<Vec<Volume>> {
    let path = config.state_path("index.sqlite3");
    if path.exists() {
        Index::new(path, true)?.known_volumes()
    } else {
        Ok(vec![])
    }
}
fn drive_request_path(config: &Config, uuid: &str, consumed: bool) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    config.state_path("drive-starts").join(format!(
        "{:x}.{}",
        Sha256::digest(uuid.as_bytes()),
        if consumed { "consumed" } else { "json" }
    ))
}
fn write_drive_start(config: &Config, volume: &Volume) -> Result<()> {
    let nonce = uuid::Uuid::new_v4().to_string();
    atomic_json(
        drive_request_path(config, &volume.uuid, false),
        &serde_json::json!({
            "uuid":volume.uuid,"device":volume.device,"mount":volume.mount,"nonce":nonce
        }),
    )?;
    atomic_json(config.state_path("drive-start-signal.json"), &nonce)?;
    crate::control::signal_wakeup(config.state_path("wake.fifo"));
    Ok(())
}
/// Approve one currently connected identity; source discovery consumes the
/// request once and retains permission only for that connection.
pub fn request_drive_start(config: &Config, uuid: &str) -> Result<()> {
    let connected = connected_drives(&NativeCatalog, &NativePlatform);
    let volume = connected
        .volumes
        .iter()
        .find(|volume| volume.uuid == uuid)
        .ok_or_else(|| anyhow::anyhow!("Connect this drive before starting it."))?;
    let known = saved_volumes(config)?;
    let selected = if config.scope == "configured" {
        let roots = configured_roots_on(config, &volume.mount);
        !roots.is_empty() && configured_uuid(config, uuid, &roots, &known)
    } else {
        selected_uuid(config, uuid, &known)
    };
    anyhow::ensure!(
        selected,
        "Add this drive to your folders before starting it."
    );
    anyhow::ensure!(
        config.drives.reconnect_mode(uuid) == Some("manual"),
        "This drive already resumes automatically."
    );
    write_drive_start(config, volume)
}
fn inventory_for(
    config: &Config,
    connected: &[Volume],
    known: &[Volume],
) -> Vec<DriveInventoryItem> {
    let mut inventory = BTreeMap::new();
    let mut add = |uuid: &str, mount: &str, connected: bool| {
        let Some(mount) = external_display_mount(mount) else {
            return;
        };
        let included = if config.scope == "configured" {
            let roots = configured_roots_on(config, mount);
            config
                .drives
                .included
                .iter()
                .any(|reference| reference.uuid == uuid)
                || (!roots.is_empty() && configured_uuid(config, uuid, &roots, known))
        } else {
            selected_uuid(config, uuid, known)
        };
        if connected || included {
            inventory.insert(
                uuid.to_owned(),
                DriveInventoryItem {
                    uuid: uuid.to_owned(),
                    mount: mount.to_owned(),
                    name: Path::new(mount)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    connected,
                    included,
                    reconnect: config
                        .drives
                        .reconnect_mode(uuid)
                        .unwrap_or("automatic")
                        .to_owned(),
                    availability: if connected {
                        "connected"
                    } else {
                        "disconnected"
                    }
                    .into(),
                },
            );
        }
    };
    for volume in known {
        add(&volume.uuid, &volume.mount, false);
    }
    for reference in &config.drives.included {
        add(&reference.uuid, &reference.mount, false);
    }
    // Current identity and mount spelling take precedence over saved display.
    for volume in connected {
        add(&volume.uuid, &volume.mount, true);
    }
    let mut result: Vec<_> = inventory.into_values().collect();
    result.sort_by(|a, b| (&a.name, &a.uuid).cmp(&(&b.name, &b.uuid)));
    result
}
fn absent_device(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENODEV))
}
fn connected_drives(reader: &impl CatalogReader, platform: &impl Platform) -> ConnectedDrives {
    // Inventory needs only the mount catalog. A slow account or cloud provider
    // must not consume the deadline before drive identities can be inspected.
    let mut connected = ConnectedDrives::default();
    let entries = match reader.list(Path::new("/Volumes")) {
        Ok(entries) => entries,
        Err(error) if absent_device(&error) => return connected,
        Err(error) => {
            connected.unavailable(
                Path::new("/Volumes"),
                format!("Cannot list mounted drives: {error}"),
            );
            return connected;
        }
    };
    let startup = reader.startup_devices();
    for path in entries {
        if path
            .file_name()
            .is_none_or(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let node = match reader.metadata(&path) {
            Ok(node) => node,
            Err(error) if absent_device(&error) => continue,
            Err(error) => {
                connected.unavailable(&path, format!("Cannot inspect mounted drive: {error}"));
                continue;
            }
        };
        if !node.directory || startup.contains(&node.device) {
            continue;
        }
        match reader.is_mount(&path) {
            Ok(true) => (),
            Ok(false) => continue,
            Err(error) if absent_device(&error) => continue,
            Err(error) => {
                connected.unavailable(&path, format!("Cannot identify mounted drive: {error}"));
                continue;
            }
        }
        let Some(root) = path.to_str() else {
            connected.unavailable(&path, "Drive path is not UTF-8".into());
            continue;
        };
        match platform.volumes(&[root.to_owned()]) {
            Ok(volumes) if !volumes.is_empty() => connected.volumes.extend(volumes),
            Ok(_) => connected.unavailable(&path, "Mounted drive identity is unavailable".into()),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(absent_device) => {}
            Err(error) => connected.unavailable(
                &path,
                format!("Cannot read mounted drive identity: {error:#}"),
            ),
        }
    }
    connected
}
fn inventory_known_volumes(config: &Config) -> Result<Vec<Volume>> {
    let path = config.state_path("index.sqlite3");
    Ok(if path.exists() {
        let index = Index::new(path, true)?;
        if index.status()?["indexed"] == false {
            vec![]
        } else {
            index.known_volumes()?
        }
    } else {
        vec![]
    })
}
fn inventory_report(
    config: &Config,
    mut connected: ConnectedDrives,
    known: Result<Vec<Volume>>,
) -> DriveInventory {
    let known_unavailable = known.is_err();
    let mut all_unverified = connected
        .issues
        .iter()
        .any(|issue| issue.mount == "/Volumes");
    let known = match known {
        Ok(known) => known,
        Err(error) => {
            all_unverified = true;
            connected.unavailable(
                &config.state_path("index.sqlite3"),
                format!("Cannot read remembered drive identities: {error:#}"),
            );
            vec![]
        }
    };
    let mut items = inventory_for(config, &connected.volumes, &known);
    for item in &mut items {
        if known_unavailable && config.scope == "configured" {
            // Missing index evidence must not make an existing configured root
            // look like a first-time selection. Retain only UUID authorization
            // that can independently be established from saved configuration.
            let roots = configured_roots_on(config, &item.mount);
            item.included = config
                .drives
                .included
                .iter()
                .any(|reference| reference.uuid == item.uuid)
                || (!roots.is_empty()
                    && !configured_uuid(config, "", &roots, &[])
                    && configured_uuid(config, &item.uuid, &roots, &[]));
        }
        if !item.connected
            && (all_unverified
                || connected.issues.iter().any(|issue| {
                    external_display_mount(&issue.mount).unwrap_or(&issue.mount) == item.mount
                }))
        {
            item.availability = "unavailable".into();
        }
    }
    DriveInventory {
        items,
        complete: connected.issues.is_empty(),
        issues: connected.issues,
    }
}
pub fn unavailable_drive_inventory(config: &Config, reason: String) -> DriveInventory {
    let mut connected = ConnectedDrives::default();
    connected.unavailable(Path::new("/Volumes"), reason);
    inventory_report(config, connected, Ok(vec![]))
}
pub fn drive_inventory(config: &Config) -> DriveInventory {
    inventory_report(
        config,
        connected_drives(&NativeCatalog, &NativePlatform),
        inventory_known_volumes(config),
    )
}
fn disconnected_error(
    config: &Config,
    coverage: &Coverage,
    saved: Option<&Volume>,
    error: &anyhow::Error,
) -> bool {
    let Some(mount) = saved.and_then(|volume| external_display_mount(&volume.mount)) else {
        return false;
    };
    config.scope == "all-user-files"
        && coverage.catalog_roots.iter().any(|root| root == "/Volumes")
        && !coverage
            .unavailable
            .iter()
            .any(|root| within(mount, root) || within(root, mount))
        && !coverage.roots.iter().any(|root| within(root, mount))
        && error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENODEV)))
}

fn resolve_coverage_with(config: &Config, platform: &impl Platform) -> Coverage {
    let mut coverage = platform.coverage(config);
    let known = match saved_volumes(config) {
        Ok(known) => known,
        Err(error) => {
            coverage.roots.clear();
            let path = config
                .state_path("index.sqlite3")
                .to_string_lossy()
                .into_owned();
            coverage.unavailable.push(path.clone());
            coverage
                .unavailable_reasons
                .insert(path, format!("Cannot read selected drives: {error:#}"));
            return coverage;
        }
    };
    let mut selected = Vec::new();
    for root in std::mem::take(&mut coverage.roots) {
        // A newly chosen configured folder keeps its explicit path semantics.
        // Once a drive identity is saved, preview and one-shot work enforce it too.
        if config.scope == "configured"
            && configured_uuid(config, "", std::slice::from_ref(&root), &known)
        {
            selected.push(root);
            continue;
        }
        match platform.volumes(std::slice::from_ref(&root)) {
            Ok(volumes)
                if !volumes.is_empty()
                    && volumes
                        .iter()
                        .all(|volume| selected_volume(config, volume, &known)) =>
            {
                selected.push(root)
            }
            Ok(volumes) => {
                for volume in volumes
                    .iter()
                    .filter(|volume| !selected_volume(config, volume, &known))
                {
                    let mount = external_display_mount(&volume.mount).unwrap_or(&volume.mount);
                    coverage.unavailable.retain(|path| !within(path, mount));
                    coverage
                        .unavailable_reasons
                        .retain(|path, _| !within(path, mount));
                }
            }
            Err(error) => {
                if !coverage.unavailable.contains(&root) {
                    coverage.unavailable.push(root.clone());
                }
                coverage
                    .unavailable_reasons
                    .insert(root, format!("{error:#}"));
            }
        }
    }
    coverage.roots = selected;
    coverage
}
pub fn resolve_coverage(config: &Config) -> Coverage {
    resolve_coverage_with(config, &NativePlatform)
}
fn discovered_coverage(config: &Config) -> Coverage {
    if config.scope == "all-user-files" {
        discover_user_coverage()
    } else {
        Coverage {
            roots: config.roots.clone(),
            ..Default::default()
        }
    }
}
pub fn policy_for(config: &Config, coverage: &Coverage, roots: Option<&[String]>) -> Policy {
    let mut excludes = config.excludes.clone();
    excludes.push(config.state_dir.clone());
    if let Some(log) = &config.log_dir {
        excludes.push(log.clone());
    }
    let mut seen = HashSet::new();
    excludes.retain(|p| seen.insert(p.clone()));
    Policy::new(
        roots.unwrap_or(&coverage.roots).to_vec(),
        excludes,
        config.exclude_names.clone(),
        if config.scope == "configured" {
            config.skip_hidden_tops.clone()
        } else {
            vec![]
        },
        coverage.root_excludes.clone(),
    )
}

pub trait WatchStream: Send {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn start_id(&self) -> Option<u64>;
    fn error(&self) -> Option<String>;
}
impl WatchStream for events::Stream {
    fn start(&mut self) -> Result<()> {
        self.start()
    }
    fn stop(&mut self) -> Result<()> {
        self.stop()
    }
    fn start_id(&self) -> Option<u64> {
        self.start_id()
    }
    fn error(&self) -> Option<String> {
        self.error()
    }
}
pub trait Platform: Send + Sync {
    fn coverage(&self, config: &Config) -> Coverage;
    fn volumes(&self, roots: &[String]) -> Result<Vec<Volume>>;
    fn stream(
        &self,
        volume: Volume,
        since: Option<u64>,
        callback: Callback,
        wake: Wake,
    ) -> Result<Box<dyn WatchStream>>;
}
pub struct NativePlatform;
impl Platform for NativePlatform {
    fn coverage(&self, config: &Config) -> Coverage {
        discovered_coverage(config)
    }
    fn volumes(&self, roots: &[String]) -> Result<Vec<Volume>> {
        events::discover_volumes(roots)
    }
    fn stream(
        &self,
        volume: Volume,
        since: Option<u64>,
        callback: Callback,
        wake: Wake,
    ) -> Result<Box<dyn WatchStream>> {
        Ok(Box::new(events::Stream::new(
            volume, since, callback, wake,
        )?))
    }
}
#[derive(Default)]
struct Signals {
    requested: bool,
    force: bool,
    error: Option<String>,
    unmounted_uuids: HashSet<String>,
    unmounted_paths: HashSet<String>,
}
struct Watch {
    volume: Volume,
    stream: Box<dyn WatchStream>,
    active: Arc<AtomicBool>,
    cursor: Arc<Mutex<Option<u64>>>,
}
#[derive(Default)]
struct LaneControl {
    generation: u64,
    refresh: bool,
    force: bool,
    error: Option<String>,
}
/// One source lane owns discovery and stream lifecycle. The mutation worker
/// never waits for coverage probing, and event callbacks still commit directly.
pub struct SourceWorker {
    shared: Arc<(Mutex<LaneControl>, std::sync::Condvar)>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
    drive_signal: std::path::PathBuf,
    last_drive_signal: Mutex<Option<Vec<u8>>>,
}
impl SourceWorker {
    pub fn new(config: Config, index: Arc<Index>, wake: Wake) -> Result<Self> {
        Self::start(SourceManager::new(config, index, wake))
    }
    fn start(mut manager: SourceManager) -> Result<Self> {
        let drive_signal = manager.config.state_path("drive-start-signal.json");
        let last_drive_signal = Mutex::new(std::fs::read(&drive_signal).ok());
        let shared = Arc::new((
            Mutex::new(LaneControl {
                refresh: true,
                ..Default::default()
            }),
            std::sync::Condvar::new(),
        ));
        let stop = manager.cancelled.clone();
        let notification = shared.clone();
        let foreground = manager.wake.clone();
        manager.wake = Arc::new(move || {
            let (state, changed) = &*notification;
            if let Ok(mut state) = state.lock() {
                state.generation = state.generation.wrapping_add(1);
            }
            changed.notify_one();
            foreground();
        });
        let lane = shared.clone();
        let thread =
            std::thread::Builder::new()
                .name("sources".into())
                .stack_size(512 * 1024)
                .spawn(move || {
                    let cancellation =
                        crate::directory_io::CancellationScope::new(manager.cancelled.clone());
                    let mut observer = Normalizer::new(Policy::default(), None, None, None, false)?;
                    let result =
                        (|| -> Result<()> {
                            loop {
                                let (lock, changed) = &*lane;
                                let (generation, refresh, force) = {
                                    let mut state = lock.lock().map_err(|_| {
                                        anyhow::anyhow!("source lane lock poisoned")
                                    })?;
                                    (
                                        state.generation,
                                        std::mem::take(&mut state.refresh),
                                        std::mem::take(&mut state.force),
                                    )
                                };
                                if manager.cancelled.load(Ordering::Acquire) {
                                    break;
                                }
                                manager.check()?;
                                if refresh || manager.refresh_requested() {
                                    manager.refresh(&mut observer, force)?;
                                    (manager.wake)();
                                }
                                let state = lock
                                    .lock()
                                    .map_err(|_| anyhow::anyhow!("source lane lock poisoned"))?;
                                if manager.cancelled.load(Ordering::Acquire) {
                                    break;
                                }
                                if state.generation != generation || state.refresh {
                                    continue;
                                }
                                if let Some(deadline) = manager.next_retry_time {
                                    let duration = std::time::Duration::from_secs_f64(
                                        (deadline - now()).max(0.0),
                                    );
                                    drop(changed.wait_timeout(state, duration).map_err(|_| {
                                        anyhow::anyhow!("source lane lock poisoned")
                                    })?);
                                } else {
                                    drop(changed.wait(state).map_err(|_| {
                                        anyhow::anyhow!("source lane lock poisoned")
                                    })?);
                                }
                            }
                            Ok(())
                        })();
                    let result = if manager.cancelled.load(Ordering::Acquire) {
                        Ok(())
                    } else {
                        result
                    };
                    drop(cancellation);
                    let result = result.and(manager.close());
                    if let Err(error) = &result
                        && let Ok(mut state) = lane.0.lock()
                    {
                        state.error = Some(format!("{error:#}"));
                    }
                    (manager.wake)();
                    result
                })?;
        Ok(Self {
            shared,
            stop,
            thread: Some(thread),
            drive_signal,
            last_drive_signal,
        })
    }
    pub fn request(&self, force: bool) {
        if let Ok(mut state) = self.shared.0.lock() {
            state.refresh = true;
            state.force |= force;
        }
        self.shared.1.notify_one();
    }
    pub fn check(&self) -> Result<()> {
        let current = std::fs::read(&self.drive_signal).ok();
        let mut previous = self
            .last_drive_signal
            .lock()
            .map_err(|_| anyhow::anyhow!("drive start signal lock poisoned"))?;
        if *previous != current {
            *previous = current;
            self.request(false);
        }
        drop(previous);
        let state = self
            .shared
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("source lane lock poisoned"))?;
        if let Some(error) = &state.error {
            bail!("source lane failed: {error}");
        }
        if self
            .thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
            && !self.stop.load(Ordering::Acquire)
        {
            bail!("source lane stopped unexpectedly");
        }
        Ok(())
    }
    pub fn close(&mut self) -> Result<()> {
        {
            // Serialize with the lane's final predicate check and wait so an
            // idle lane cannot miss shutdown. Release before watchdog/joins.
            let _state = self
                .shared
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.stop.store(true, Ordering::Release);
            self.shared.1.notify_one();
        }
        crate::directory_io::notify_cancellation();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow::anyhow!("source lane panicked"))??;
        }
        Ok(())
    }
}
impl Drop for SourceWorker {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub struct SourceManager {
    config: Config,
    cancelled: Arc<AtomicBool>,
    index: Arc<Index>,
    wake: Wake,
    platform: Arc<dyn Platform>,
    signals: Arc<Mutex<Signals>>,
    data: BTreeMap<String, Watch>,
    catalogs: BTreeMap<String, Watch>,
    catalog_desired: BTreeMap<String, Volume>,
    catalog_cursors: BTreeMap<String, (Volume, u64)>,
    start_failures: BTreeMap<String, String>,
    last_signature: Option<String>,
    last_status: Option<serde_json::Value>,
    root_exclusions: HashMap<String, Vec<String>>,
    closed: bool,
    pub roots: Vec<String>,
    pub active_roots: Vec<String>,
    pub next_retry_time: Option<f64>,
    pub unavailable: BTreeMap<String, String>,
    pub disconnected_roots: Vec<String>,
    pub manual_waiting_roots: Vec<String>,
    manual_grants: HashMap<String, (u64, String)>,
    pub catalog_unavailable: BTreeMap<String, String>,
}
impl SourceManager {
    fn manual_permitted(&mut self, volume: &Volume) -> Result<bool> {
        if !external_mount(&volume.mount)
            || self.config.drives.reconnect_mode(&volume.uuid) != Some("manual")
        {
            return Ok(true);
        }
        let identity = (volume.device, volume.mount.clone());
        if self.manual_grants.get(&volume.uuid) == Some(&identity) {
            return Ok(true);
        }
        let request: serde_json::Value =
            match std::fs::read(drive_request_path(&self.config, &volume.uuid, false)) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.into()),
            };
        let nonce = request
            .get("nonce")
            .and_then(serde_json::Value::as_str)
            .filter(|nonce| !nonce.is_empty());
        let consumed = std::fs::read(drive_request_path(&self.config, &volume.uuid, true))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<String>(&bytes).ok());
        if nonce.is_none() || nonce == consumed.as_deref() {
            return Ok(false);
        }
        // Consume even a stale identity so reconnecting an old UUID/device pair
        // cannot revive an approval after an intervening mismatch.
        atomic_json(
            drive_request_path(&self.config, &volume.uuid, true),
            &nonce.unwrap(),
        )?;
        if request["uuid"] == volume.uuid
            && request["device"] == volume.device
            && request["mount"] == volume.mount
        {
            self.manual_grants.insert(volume.uuid.clone(), identity);
            return Ok(true);
        }
        Ok(false)
    }
    pub fn new(config: Config, index: Arc<Index>, wake: Wake) -> Self {
        Self::with_platform(config, index, wake, Arc::new(NativePlatform))
    }
    pub fn with_platform(
        config: Config,
        index: Arc<Index>,
        wake: Wake,
        platform: Arc<dyn Platform>,
    ) -> Self {
        Self {
            config,
            cancelled: Arc::new(AtomicBool::new(false)),
            index,
            wake,
            platform,
            signals: Arc::new(Mutex::new(Signals::default())),
            data: BTreeMap::new(),
            catalogs: BTreeMap::new(),
            catalog_desired: BTreeMap::new(),
            catalog_cursors: BTreeMap::new(),
            start_failures: BTreeMap::new(),
            last_signature: None,
            last_status: None,
            root_exclusions: HashMap::new(),
            closed: false,
            roots: vec![],
            active_roots: vec![],
            next_retry_time: None,
            unavailable: BTreeMap::new(),
            disconnected_roots: vec![],
            manual_waiting_roots: vec![],
            manual_grants: HashMap::new(),
            catalog_unavailable: BTreeMap::new(),
        }
    }
    pub fn refresh_requested(&self) -> bool {
        self.signals.lock().map(|s| s.requested).unwrap_or(true)
    }
    fn callback_error(&self) -> Result<()> {
        let signals = self
            .signals
            .lock()
            .map_err(|_| anyhow::anyhow!("source signal lock poisoned"))?;
        if let Some(error) = &signals.error {
            bail!("event ingestion failed: {error}");
        }
        Ok(())
    }
    fn signal_refresh(&self, force: bool) -> Result<()> {
        signal(&self.signals, &self.wake, force)
    }
    fn discover_one(&self, root: &str) -> Result<Volume> {
        self.check_cancelled()?;
        let mut volumes = self.platform.volumes(&[root.to_owned()])?;
        self.check_cancelled()?;
        if volumes.len() != 1 || volumes[0].roots != [root] {
            bail!("native discovery must return exactly one stream for each root");
        }
        Ok(volumes.remove(0))
    }
    fn make_watch(&self, volume: Volume, since: Option<u64>, catalog: bool) -> Result<Watch> {
        let active = Arc::new(AtomicBool::new(true));
        let cursor = Arc::new(Mutex::new(since));
        let checkpoint = cursor.clone();
        let token = active.clone();
        let index = self.index.clone();
        let signals = self.signals.clone();
        let wake = self.wake.clone();
        let key = volume.key.clone();
        let uuid = volume.uuid.clone();
        let roots: HashSet<_> = volume.roots.iter().map(|r| nfc(&absolute(r))).collect();
        let callback: Callback = Arc::new(move |batch: Vec<Event>| {
            if !token.load(Ordering::Acquire) {
                return Ok(());
            }
            {
                let mut state = signals
                    .lock()
                    .map_err(|_| anyhow::anyhow!("source signal lock poisoned"))?;
                for event in batch
                    .iter()
                    .filter(|event| event.flags & events::UNMOUNT != 0)
                {
                    if catalog {
                        if !event.path.is_empty() {
                            state.unmounted_paths.insert(absolute(&event.path));
                        }
                    } else {
                        state.unmounted_uuids.insert(uuid.clone());
                    }
                }
            }
            if catalog {
                {
                    let mut cursor = checkpoint
                        .lock()
                        .map_err(|_| anyhow::anyhow!("catalog checkpoint lock poisoned"))?;
                    for event in &batch {
                        if event.flags & events::EVENT_IDS_WRAPPED != 0 {
                            *cursor = None;
                        }
                        *cursor = Some(cursor.unwrap_or(0).max(event.id));
                    }
                }
                for event in batch {
                    if event.flags & REFRESH != 0 {
                        signal(&signals, &wake, event.flags & RECONFIGURE != 0)?;
                        break;
                    }
                    if !event.path.is_empty()
                        && Path::new(&absolute(&event.path))
                            .parent()
                            .and_then(|p| p.to_str())
                            .is_some_and(|p| roots.contains(&nfc(p)))
                    {
                        signal(&signals, &wake, false)?;
                        break;
                    }
                }
            } else {
                if let Err(error) = index.enqueue(&key, &batch) {
                    if let Ok(mut state) = signals.lock() {
                        state.error.get_or_insert_with(|| format!("{error:#}"));
                    }
                    wake();
                    return Err(error);
                }
                if batch.iter().any(|event| event.flags & REFRESH != 0) {
                    signal(
                        &signals,
                        &wake,
                        batch.iter().any(|event| event.flags & RECONFIGURE != 0),
                    )?;
                } else {
                    wake();
                }
            }
            Ok(())
        });
        let stream = self
            .platform
            .stream(volume.clone(), since, callback, self.wake.clone())?;
        Ok(Watch {
            volume,
            stream,
            active,
            cursor,
        })
    }
    fn start_watch(&self, volume: Volume, catalog: bool) -> Result<Watch> {
        let since = if catalog {
            self.catalog_cursors
                .get(&volume.roots[0])
                .filter(|(saved, _)| saved == &volume)
                .map(|(_, id)| *id)
        } else {
            self.index.cursor(&volume.key)?
        };
        let mut watch = self.make_watch(volume.clone(), since, catalog)?;
        if let Err(error) = watch.stream.start() {
            stop_watch(&mut watch)?;
            self.callback_error()?;
            if error.is::<CursorInvalidError>() {
                if !catalog {
                    self.index.invalidate_volume(&volume.key)?;
                }
                watch = self.make_watch(volume, None, catalog)?;
                if let Err(error) = watch.stream.start() {
                    stop_watch(&mut watch)?;
                    self.callback_error()?;
                    return Err(error);
                }
            } else {
                return Err(error);
            }
        }
        let completion = (|| {
            self.callback_error()?;
            if let Some(error) = watch.stream.error() {
                bail!("native stream failed: {error}");
            }
            if catalog {
                let mut cursor = watch
                    .cursor
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalog checkpoint lock poisoned"))?;
                if cursor.is_none() {
                    *cursor = watch.stream.start_id();
                }
            } else if self.index.cursor(&watch.volume.key)?.is_none() {
                let start = watch
                    .stream
                    .start_id()
                    .ok_or_else(|| anyhow::anyhow!("started stream has no initial cursor"))?;
                self.index.seed_cursor(&watch.volume.key, start)?;
            }
            Ok(())
        })();
        if let Err(error) = completion {
            stop_watch(&mut watch)?;
            return Err(error);
        }
        Ok(watch)
    }
    fn sync_catalogs(&mut self, roots: &[String], retry_due: bool, force: bool) -> Result<bool> {
        let mut desired = BTreeMap::new();
        let mut failures = BTreeMap::new();
        let mut changed = false;
        for root in roots {
            match self.discover_one(root) {
                Ok(volume) => {
                    desired.insert(root.clone(), volume);
                }
                Err(error) if error.is::<std::io::Error>() => {
                    failures.insert(root.clone(), format!("{error:#}"));
                }
                Err(error) => return Err(error),
            }
        }
        let removed: Vec<_> = self
            .catalogs
            .iter()
            .filter(|(root, watch)| force || desired.get(*root) != Some(&watch.volume))
            .map(|(r, _)| r.clone())
            .collect();
        for root in removed {
            let mut watch = self.catalogs.remove(&root).unwrap();
            stop_watch(&mut watch)?;
            if desired.get(&root) == Some(&watch.volume) {
                if let Some(id) = *watch
                    .cursor
                    .lock()
                    .map_err(|_| anyhow::anyhow!("catalog checkpoint lock poisoned"))?
                {
                    self.catalog_cursors.insert(root, (watch.volume, id));
                }
            } else {
                self.catalog_cursors.remove(&root);
            }
            changed = true;
        }
        for (root, volume) in &desired {
            if self.catalogs.contains_key(root) {
                continue;
            }
            if !force
                && !retry_due
                && self.catalog_desired.get(root) == Some(volume)
                && let Some(error) = self.catalog_unavailable.get(root)
            {
                failures.insert(root.clone(), error.clone());
                continue;
            }
            match self.start_watch(volume.clone(), true) {
                Ok(watch) => {
                    self.catalogs.insert(root.clone(), watch);
                    changed = true;
                }
                Err(error) if recoverable(&error) => {
                    failures.insert(root.clone(), format!("{error:#}"));
                }
                Err(error) => return Err(error),
            }
        }
        self.catalog_cursors
            .retain(|root, (volume, _)| desired.get(root) == Some(volume));
        self.catalog_desired = desired;
        self.catalog_unavailable = failures;
        Ok(changed)
    }
    fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            bail!("source shutdown requested");
        }
        Ok(())
    }
    fn filter_drive_catalogs(&self, coverage: &mut Coverage) -> Result<()> {
        if self.config.scope != "all-user-files"
            || (self.config.drives.mode == "automatic" && self.config.drives.excluded.is_empty())
        {
            return Ok(());
        }
        let mut excluded_mounts = Vec::new();
        for root in &coverage.roots {
            match self.discover_one(root) {
                Ok(volume) if !volume_included(&self.config, &volume) => excluded_mounts.push(
                    external_display_mount(&volume.mount)
                        .unwrap_or(&volume.mount)
                        .to_owned(),
                ),
                Ok(_) => (),
                Err(error) if error.is::<std::io::Error>() => (),
                Err(error) => return Err(error),
            }
        }
        let included = |path: &String| !excluded_mounts.iter().any(|mount| within(path, mount));
        coverage.catalog_roots.retain(included);
        coverage.unavailable.retain(included);
        coverage
            .unavailable_reasons
            .retain(|path, _| included(path));
        Ok(())
    }
    pub fn refresh(&mut self, normalizer: &mut Normalizer, force: bool) -> Result<bool> {
        self.check_cancelled()?;
        if self.closed {
            bail!("source manager is closed");
        }
        self.callback_error()?;
        let (force, mut unmounted_uuids, unmounted_paths) = {
            let mut state = self
                .signals
                .lock()
                .map_err(|_| anyhow::anyhow!("source signal lock poisoned"))?;
            let force = force || state.force;
            state.force = false;
            state.requested = false;
            (
                force,
                std::mem::take(&mut state.unmounted_uuids),
                std::mem::take(&mut state.unmounted_paths),
            )
        };
        for volume in self.index.known_volumes()? {
            let mount = external_display_mount(&volume.mount).unwrap_or(&volume.mount);
            if unmounted_paths
                .iter()
                .any(|path| within(path, mount) || within(mount, path))
            {
                unmounted_uuids.insert(volume.uuid);
            }
        }
        for uuid in unmounted_uuids {
            self.manual_grants.remove(&uuid);
            match std::fs::read(drive_request_path(&self.config, &uuid, false)) {
                Ok(bytes) => {
                    let request: serde_json::Value = serde_json::from_slice(&bytes)?;
                    if let Some(nonce) = request["nonce"].as_str() {
                        atomic_json(drive_request_path(&self.config, &uuid, true), &nonce)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let timestamp = now();
        let retry_due = self.next_retry_time.is_some_and(|t| timestamp >= t);
        let mut coverage = self.platform.coverage(&self.config);
        self.check_cancelled()?;
        let mut catalog_changed = false;
        // Read metadata again after each new catalog starts. New nested catalogs
        // receive the same treatment, bounded under sustained creation activity.
        for attempt in 0..4 {
            self.filter_drive_catalogs(&mut coverage)?;
            let changed =
                self.sync_catalogs(&coverage.catalog_roots, retry_due, force && attempt == 0)?;
            catalog_changed |= changed;
            if !changed {
                break;
            }
            coverage = self.platform.coverage(&self.config);
            self.check_cancelled()?;
            if attempt == 3 {
                self.signal_refresh(false)?;
            }
        }
        let known = self.index.known_volumes()?;
        self.manual_waiting_roots.clear();
        let mut present_drives = HashMap::new();
        let mut seen = HashSet::new();
        let mut desired: Vec<String> = coverage
            .roots
            .iter()
            .filter(|root| seen.insert((*root).clone()))
            .cloned()
            .collect();
        if self.config.scope == "all-user-files" {
            // Apply UUID preferences before remembered roots can re-enter the
            // executable plan. Non-drive account and cloud roots keep their
            // existing explicit/discovery semantics.
            for volume in known
                .iter()
                .filter(|volume| selected_volume(&self.config, volume, &known))
            {
                for root in &volume.roots {
                    if seen.insert(root.clone()) {
                        desired.push(root.clone());
                    }
                }
            }
        }
        let mut unavailable: BTreeMap<_, _> = coverage
            .unavailable
            .iter()
            .map(|root| {
                (
                    root.clone(),
                    coverage
                        .unavailable_reasons
                        .get(root)
                        .cloned()
                        .unwrap_or_else(|| "metadata access unavailable".to_owned()),
                )
            })
            .collect();
        let mut candidates = Vec::new();
        let mut disconnected = Vec::new();
        self.roots.clear();
        for root in desired {
            let saved = known.iter().find(|volume| volume.roots.contains(&root));
            // A remembered removed UUID is never an implicit discovery root.
            if !coverage.roots.contains(&root)
                && saved.is_some_and(|volume| !volume_included(&self.config, volume))
            {
                continue;
            }
            match self.discover_one(&root) {
                Ok(volume) if selected_volume(&self.config, &volume, &known) => {
                    present_drives
                        .insert(volume.uuid.clone(), (volume.device, volume.mount.clone()));
                    self.roots.push(root.clone());
                    if self.manual_permitted(&volume)? {
                        candidates.push(volume);
                    } else {
                        self.manual_waiting_roots.push(root);
                    }
                }
                Ok(volume) => {
                    let mount = external_display_mount(&volume.mount).unwrap_or(&volume.mount);
                    unavailable.retain(|path, _| !within(path, mount));
                    // Keep an included, saved identity dormant if another UUID
                    // occupies its old path; never inherit the old cursor.
                    if saved.is_some_and(|old| {
                        old.uuid != volume.uuid && volume_included(&self.config, old)
                    }) {
                        // A verified different UUID proves the selected drive
                        // is absent even though another device owns its path.
                        disconnected.push(root.clone());
                        self.roots.push(root);
                    }
                }
                Err(error) if error.is::<std::io::Error>() => {
                    if saved.is_some_and(|volume| !volume_included(&self.config, volume)) {
                        unavailable.remove(&root);
                        continue;
                    }
                    self.roots.push(root.clone());
                    if disconnected_error(&self.config, &coverage, saved, &error) {
                        unavailable.remove(&root);
                        disconnected.push(root);
                    } else {
                        unavailable.insert(root, format!("{error:#}"));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        self.manual_grants
            .retain(|uuid, identity| present_drives.get(uuid) == Some(identity));
        self.disconnected_roots = disconnected;
        let desired_policy = policy_for(&self.config, &coverage, None);
        let mut names = desired_policy.exclude_names.clone();
        names.sort();
        let signature=serde_json::json!({"config":self.config.signature(),"roots":self.roots,"volumes":candidates,
            "excludes":desired_policy.excludes,"exclude_names":names,"hidden":desired_policy.skip_hidden_tops,
            "root_excludes":desired_policy.root_excludes}).to_string();
        let changed = force
            || self.last_signature.as_ref() != Some(&signature)
            || (retry_due && !self.start_failures.is_empty());
        self.check_cancelled()?;
        if changed {
            let previous_roots: HashSet<_> = self.active_roots.iter().cloned().collect();
            // Native Stop waits for callbacks to finish. Do not hold the index
            // mutex across it: callbacks must still commit against the old keys.
            let removed: Vec<_> = self
                .data
                .iter()
                .filter(|(_, watch)| force || !candidates.contains(&watch.volume))
                .map(|(key, _)| key.clone())
                .collect();
            for key in removed {
                stop_watch(&mut self.data.remove(&key).unwrap())?;
            }
            self.callback_error()?;
            let candidate_roots: Vec<_> = candidates.iter().flat_map(|v| v.roots.clone()).collect();
            normalizer.policy = policy_for(&self.config, &coverage, Some(&candidate_roots));
            let baseline = self.index.prepare_sources(
                &self.config,
                &candidates,
                &self.roots,
                normalizer.policy.clone(),
                &self.data.keys().cloned().collect(),
            )?;
            if baseline {
                self.index.bootstrap_jobs()?;
            }
            self.start_failures.clear();
            for volume in &candidates {
                self.check_cancelled()?;
                if self.data.contains_key(&volume.key) {
                    continue;
                }
                match self.start_watch(volume.clone(), false) {
                    Ok(watch) => {
                        self.data.insert(volume.key.clone(), watch);
                        self.index.activate_volume(&volume.key)?;
                        (self.wake)();
                    }
                    Err(error) if recoverable(&error) => {
                        self.callback_error()?;
                        for root in &volume.roots {
                            self.start_failures
                                .insert(root.clone(), format!("{error:#}"));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            let available: Vec<_> = self.data.values().map(|w| w.volume.clone()).collect();
            self.active_roots = available.iter().flat_map(|v| v.roots.clone()).collect();
            normalizer.policy = policy_for(&self.config, &coverage, Some(&self.active_roots));
            let policy_changes: Vec<_> = self
                .active_roots
                .iter()
                .filter(|root| {
                    previous_roots.contains(*root)
                        && self.root_exclusions.get(&nfc(root))
                            != desired_policy.root_excludes.get(&nfc(root))
                })
                .cloned()
                .collect();
            if !policy_changes.is_empty() {
                self.index.request_reconcile(Some(&policy_changes))?;
            }
            self.root_exclusions = desired_policy.root_excludes;
            self.last_signature = Some(signature);
        }
        for (root, error) in &self.start_failures {
            if self.roots.contains(root) {
                unavailable.insert(root.clone(), error.clone());
            }
        }
        self.unavailable = unavailable;
        let status = serde_json::json!({"roots":self.roots,"active_roots":self.active_roots,"unavailable":self.unavailable,"disconnected_roots":self.disconnected_roots,"manual_waiting_roots":self.manual_waiting_roots,"catalog_unavailable":self.catalog_unavailable,"root_excludes":self.root_exclusions});
        if self.last_status.as_ref() != Some(&status) {
            atomic_json(self.config.state_path("coverage.json"), &status)?;
            self.last_status = Some(status);
        }
        let completed_at = now();
        if !self.unavailable.is_empty()
            || !self.catalog_unavailable.is_empty()
            || !self.disconnected_roots.is_empty()
        {
            if retry_due || changed || self.next_retry_time.is_none_or(|t| completed_at >= t) {
                // Discovery can itself exceed the retry interval. Leave the
                // worker a full interval after this refresh has completed.
                self.next_retry_time = Some(completed_at + 30.0);
            }
        } else {
            self.next_retry_time = None;
        }
        Ok(changed || catalog_changed)
    }
    pub fn check(&self) -> Result<bool> {
        self.callback_error()?;
        for watch in self.catalogs.values().chain(self.data.values()) {
            if let Some(error) = watch.stream.error() {
                bail!("native stream failed: {error}");
            }
        }
        if self.next_retry_time.is_some_and(|t| now() >= t) {
            self.signal_refresh(false)?;
        }
        Ok(self.refresh_requested())
    }
    pub fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let data = stop_watches(&mut self.data);
        let catalogs = stop_watches(&mut self.catalogs);
        data.and(catalogs)
    }
}

impl Drop for SourceManager {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
fn recoverable(error: &anyhow::Error) -> bool {
    error.is::<std::io::Error>() || error.is::<CursorInvalidError>()
}
fn signal(signals: &Mutex<Signals>, wake: &Wake, force: bool) -> Result<()> {
    {
        let mut state = signals
            .lock()
            .map_err(|_| anyhow::anyhow!("source signal lock poisoned"))?;
        state.requested = true;
        state.force |= force;
    }
    wake();
    Ok(())
}
fn stop_watch(watch: &mut Watch) -> Result<()> {
    let result = watch.stream.stop();
    watch.active.store(false, Ordering::Release);
    result?;
    if let Some(error) = watch.stream.error() {
        bail!("native stream failed: {error}");
    }
    Ok(())
}
fn stop_watches(watches: &mut BTreeMap<String, Watch>) -> Result<()> {
    let mut failure = None;
    for (_, mut watch) in std::mem::take(watches).into_iter().rev() {
        if let Err(error) = stop_watch(&mut watch) {
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    struct Record {
        volume: Volume,
        since: Option<u64>,
        callback: Callback,
        stopped: AtomicBool,
    }
    type StartHook = Arc<dyn Fn(&Volume) + Send + Sync>;

    type DiscoverHook = Arc<dyn Fn(&String) + Send + Sync>;
    struct FixturePlatform {
        coverage: Mutex<Coverage>,
        records: Mutex<Vec<Arc<Record>>>,
        failures: Mutex<HashSet<String>>,
        historical_mount: AtomicBool,
        on_start: Mutex<Option<StartHook>>,
        on_discover: Mutex<Option<DiscoverHook>>,
        invalid_once: AtomicBool,
        stop_batches: Mutex<HashMap<String, Vec<Event>>>,
        discovery_failures: Mutex<HashMap<String, f64>>,
        volume_overrides: Mutex<HashMap<String, Volume>>,
        discovery_errors: Mutex<HashMap<String, i32>>,
    }
    struct FakeStream {
        record: Arc<Record>,
        platform: Arc<FixturePlatform>,
    }
    impl WatchStream for FakeStream {
        fn start(&mut self) -> Result<()> {
            if self.platform.invalid_once.swap(false, Ordering::SeqCst) {
                return Err(CursorInvalidError("fixture expired cursor".into()).into());
            }
            if let Some(action) = self.platform.on_start.lock().unwrap().clone() {
                action(&self.record.volume);
            }
            if self
                .platform
                .failures
                .lock()
                .unwrap()
                .contains(&self.record.volume.roots[0])
            {
                return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
            }
            if self.platform.historical_mount.load(Ordering::SeqCst)
                && self.record.since.unwrap_or(0) < 100
            {
                (self.record.callback)(vec![
                    Event {
                        path: self.record.volume.roots[0].clone(),
                        flags: events::MOUNT,
                        id: 100,
                    },
                    Event {
                        path: String::new(),
                        flags: events::HISTORY_DONE,
                        id: 200,
                    },
                ])?;
            }
            Ok(())
        }
        fn stop(&mut self) -> Result<()> {
            let batch = self
                .platform
                .stop_batches
                .lock()
                .unwrap()
                .remove(&self.record.volume.roots[0]);
            if let Some(batch) = batch {
                (self.record.callback)(batch)?;
            }
            self.record.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn start_id(&self) -> Option<u64> {
            Some(self.record.since.unwrap_or(50))
        }
        fn error(&self) -> Option<String> {
            None
        }
    }
    impl Platform for Arc<FixturePlatform> {
        fn coverage(&self, config: &Config) -> Coverage {
            if config.scope == "configured" {
                Coverage {
                    roots: config.roots.clone(),
                    ..Default::default()
                }
            } else {
                self.coverage.lock().unwrap().clone()
            }
        }
        fn volumes(&self, roots: &[String]) -> Result<Vec<Volume>> {
            for root in roots {
                if let Some(hook) = self.on_discover.lock().unwrap().clone() {
                    hook(root);
                }
                if let Some(code) = self.discovery_errors.lock().unwrap().get(root) {
                    return Err(std::io::Error::from_raw_os_error(*code).into());
                }
                if let Some(elapsed) = self.discovery_failures.lock().unwrap().get(root) {
                    TEST_NOW.set(TEST_NOW.get().map(|time| time + elapsed));
                    return Err(anyhow::Error::new(std::io::Error::from_raw_os_error(
                        libc::ETIMEDOUT,
                    ))
                    .context("watch root open"));
                }
            }
            Ok(roots
                .iter()
                .map(|r| {
                    self.volume_overrides
                        .lock()
                        .unwrap()
                        .get(r)
                        .cloned()
                        .unwrap_or_else(|| Volume {
                            key: format!("uuid:{r}"),
                            uuid: "uuid".into(),
                            device: 1,
                            mount: "/".into(),
                            roots: vec![r.clone()],
                        })
                })
                .collect())
        }
        fn stream(
            &self,
            volume: Volume,
            since: Option<u64>,
            callback: Callback,
            _: Wake,
        ) -> Result<Box<dyn WatchStream>> {
            let record = Arc::new(Record {
                volume,
                since,
                callback,
                stopped: AtomicBool::new(false),
            });
            self.records.lock().unwrap().push(record.clone());
            Ok(Box::new(FakeStream {
                record,
                platform: self.clone(),
            }))
        }
    }
    struct Fixture {
        temp: tempfile::TempDir,
        platform: Arc<FixturePlatform>,
        manager: SourceManager,
        index: Arc<Index>,
        normalizer: Normalizer,
        a: String,
        b: String,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let a = temp.path().join("a").to_str().unwrap().to_owned();
            let b = temp.path().join("b").to_str().unwrap().to_owned();
            fs::create_dir_all(Path::new(&a).join("child")).unwrap();
            fs::create_dir_all(&b).unwrap();
            fs::write(Path::new(&a).join("child/file"), b"preserved").unwrap();
            let config = Config {
                scope: "all-user-files".into(),
                roots: vec![],
                excludes: vec![],
                state_dir: temp.path().join("state").to_str().unwrap().into(),
                log_dir: Some(temp.path().join("logs").to_str().unwrap().into()),
                ..Config::default()
            };
            let index = Arc::new(Index::new(temp.path().join("index.sqlite3"), false).unwrap());
            let platform = Arc::new(FixturePlatform {
                coverage: Mutex::new(Coverage {
                    roots: vec![a.clone()],
                    ..Default::default()
                }),
                records: Mutex::new(vec![]),
                failures: Mutex::new(HashSet::new()),
                historical_mount: AtomicBool::new(false),
                on_start: Mutex::new(None),
                on_discover: Mutex::new(None),
                invalid_once: AtomicBool::new(false),
                stop_batches: Mutex::new(HashMap::new()),
                discovery_failures: Mutex::new(HashMap::new()),
                volume_overrides: Mutex::new(HashMap::new()),
                discovery_errors: Mutex::new(HashMap::new()),
            });
            let manager = SourceManager::with_platform(
                config,
                index.clone(),
                Arc::new(|| {}),
                Arc::new(platform.clone()),
            );
            let normalizer = Normalizer::new(Policy::default(), None, None, None, false).unwrap();
            Self {
                temp,
                platform,
                manager,
                index,
                normalizer,
                a,
                b,
            }
        }
        fn refresh(&mut self) -> bool {
            self.manager.refresh(&mut self.normalizer, false).unwrap()
        }
        fn drain(&mut self) {
            for _ in 0..20 {
                if !self.index.work(&mut self.normalizer).unwrap() {
                    return;
                }
            }
            panic!("work did not become idle");
        }
    }
    fn external_fixture() -> Fixture {
        let mut f = Fixture::new();
        f.manager.config.drives = preferences("automatic", true, false);
        f.platform.volume_overrides.lock().unwrap().insert(
            f.a.clone(),
            Volume {
                key: format!("external:{}", f.a),
                uuid: "external-uuid".into(),
                device: 8,
                mount: "/System/Volumes/Data/Volumes/Media".into(),
                roots: vec![f.a.clone()],
            },
        );
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push("/Volumes".into());
        f
    }
    fn saved_coverage(f: &Fixture) -> serde_json::Value {
        serde_json::from_slice(&fs::read(f.manager.config.state_path("coverage.json")).unwrap())
            .unwrap()
    }
    fn disconnect(f: &mut Fixture, code: i32) {
        f.platform
            .coverage
            .lock()
            .unwrap()
            .roots
            .retain(|root| root != &f.a);
        f.platform
            .discovery_errors
            .lock()
            .unwrap()
            .insert(f.a.clone(), code);
        f.refresh();
    }
    fn restart(f: &mut Fixture) {
        f.manager.close().unwrap();
        let config = f.manager.config.clone();
        f.index = Arc::new(Index::new(f.temp.path().join("index.sqlite3"), false).unwrap());
        f.manager = SourceManager::with_platform(
            config,
            f.index.clone(),
            Arc::new(|| {}),
            Arc::new(f.platform.clone()),
        );
        f.refresh();
    }
    #[test]
    fn persisted_coverage_retains_dynamic_root_exclusions_for_status() {
        let mut f = Fixture::new();
        let excluded = format!("{}/child", f.a);
        f.platform
            .coverage
            .lock()
            .unwrap()
            .root_excludes
            .insert(f.a.clone(), vec![excluded.clone()]);
        f.refresh();
        assert_eq!(
            saved_coverage(&f)["root_excludes"][&f.a],
            serde_json::json!([excluded])
        );
    }
    #[test]
    fn external_disconnect_restart_reconnect_preserves_saved_work_and_cursor() {
        let mut f = external_fixture();
        f.refresh();
        f.drain();
        let key = format!("external:{}", f.a);
        f.index
            .enqueue(
                &key,
                &[Event {
                    path: format!("{}/child/file", f.a),
                    flags: 0x100,
                    id: 91,
                }],
            )
            .unwrap();
        let before = f.index.status().unwrap();
        disconnect(&mut f, libc::ENOENT);
        let coverage = saved_coverage(&f);
        assert_eq!(coverage["disconnected_roots"], serde_json::json!([f.a]));
        assert_eq!(coverage["unavailable"], serde_json::json!({}));
        assert!(f.manager.next_retry_time.is_some());
        assert!(!f.index.work(&mut f.normalizer).unwrap());
        restart(&mut f);
        let dormant = f.index.status().unwrap();
        assert_eq!(dormant["cursors"], before["cursors"]);
        assert_eq!(dormant["indexed_entries"], before["indexed_entries"]);
        assert_eq!(dormant["pending_jobs"], before["pending_jobs"]);
        assert_eq!(dormant["current"]["pending_jobs"], 0);
        f.platform.discovery_errors.lock().unwrap().clear();
        f.platform.coverage.lock().unwrap().roots.push(f.a.clone());
        f.refresh();
        assert_eq!(f.index.cursor(&key).unwrap(), Some(91));
        assert_eq!(
            saved_coverage(&f)["disconnected_roots"],
            serde_json::json!([])
        );
        f.drain();
        assert_eq!(
            f.index.status().unwrap()["baseline_walks"],
            before["baseline_walks"]
        );
    }
    fn preferences(mode: &str, included: bool, excluded: bool) -> crate::config::DrivePreferences {
        serde_json::from_value(serde_json::json!({"mode":mode,
            "included":if included { vec![serde_json::json!({"uuid":"external-uuid","mount":"/Volumes/Media"})] } else { vec![] },
            "excluded":if excluded { vec![serde_json::json!({"uuid":"external-uuid","mount":"/Volumes/Media"})] } else { vec![] }
        })).unwrap()
    }
    #[test]
    fn automatic_reconnect_does_not_admit_a_new_drive_uuid() {
        let mut f = external_fixture();
        f.refresh();
        f.drain();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.platform.volume_overrides.lock().unwrap().insert(
            f.b.clone(),
            Volume {
                key: "new-device".into(),
                uuid: "unselected-uuid".into(),
                device: 9,
                mount: "/Volumes/New".into(),
                roots: vec![f.b.clone()],
            },
        );
        f.refresh();
        assert!(f.manager.active_roots.contains(&f.a));
        assert!(
            !f.manager.active_roots.contains(&f.b),
            "new UUIDs must wait for explicit selection"
        );
    }
    #[test]
    fn a_manual_drive_waits_without_changing_other_roots() {
        let mut f = external_fixture();
        f.refresh();
        f.drain();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.manager.config.drives.reconnect = vec![crate::config::DriveReconnectPreference {
            uuid: "external-uuid".into(),
            mount: "/Volumes/Media".into(),
            mode: "manual".into(),
        }];
        f.refresh();
        assert!(
            !f.manager.active_roots.contains(&f.a),
            "manual drive must wait for a location-specific start"
        );
        assert!(f.manager.active_roots.contains(&f.b));
        assert!(
            f.index.known_roots().unwrap().contains(&f.a),
            "waiting retains its saved index"
        );
        let volume = f.platform.volume_overrides.lock().unwrap()[&f.a].clone();
        let config = f.manager.config.clone();
        crate::control::set_paused(&config, true).unwrap();
        write_drive_start(&config, &volume).unwrap();
        f.refresh();
        assert!(f.manager.active_roots.contains(&f.a));
        assert!(
            crate::control::paused(&config).unwrap(),
            "a drive start must preserve global pause"
        );
        assert_eq!(config.apply, f.manager.config.apply);
        disconnect(&mut f, libc::ENOENT);
        f.platform.discovery_errors.lock().unwrap().clear();
        f.platform.coverage.lock().unwrap().roots.push(f.a.clone());
        f.refresh();
        assert!(
            !f.manager.active_roots.contains(&f.a),
            "reconnecting must not reuse the consumed approval"
        );
        assert!(f.manager.active_roots.contains(&f.b));
        write_drive_start(&config, &volume).unwrap();
        f.refresh();
        assert!(f.manager.active_roots.contains(&f.a));
        restart(&mut f);
        assert!(
            !f.manager.active_roots.contains(&f.a),
            "restarting must not replay an earlier connection approval"
        );
        assert!(crate::control::paused(&config).unwrap());
    }
    #[test]
    fn configured_folders_require_explicit_approval_for_a_replacement_uuid() {
        let mut f = external_fixture();
        f.manager.config.scope = "configured".into();
        f.manager.config.roots = vec![f.a.clone()];
        f.refresh();
        f.drain();
        let original = f.platform.volume_overrides.lock().unwrap()[&f.a].clone();
        let replacement = Volume {
            uuid: "replacement-id".into(),
            key: "replacement-key".into(),
            device: 9,
            ..original.clone()
        };
        f.platform
            .volume_overrides
            .lock()
            .unwrap()
            .insert(f.a.clone(), replacement.clone());
        f.refresh();
        assert!(
            !f.manager.active_roots.contains(&f.a),
            "a configured mount path must not automatically adopt another drive UUID"
        );
        assert!(f.index.known_roots().unwrap().contains(&f.a));
        assert!(!selected_volume(
            &f.manager.config,
            &replacement,
            &[original]
        ));
        f.manager
            .config
            .drives
            .included
            .push(crate::config::DriveReference {
                uuid: "replacement-id".into(),
                mount: "/Volumes/Media".into(),
            });
        f.refresh();
        assert!(
            f.manager.active_roots.contains(&f.a),
            "explicitly adding the replacement drive permits the selected folder"
        );
    }
    #[test]
    fn configured_preview_and_inventory_require_the_same_replacement_approval() {
        let f = external_fixture();
        let mut config = f.manager.config.clone();
        config.scope = "configured".into();
        config.roots = vec!["/Volumes/Media/Documents".into()];
        config.drives.reconnect = vec![crate::config::DriveReconnectPreference {
            uuid: "external-uuid".into(),
            mount: "/Volumes/Media".into(),
            mode: "manual".into(),
        }];
        let original = Volume {
            key: "original".into(),
            uuid: "external-uuid".into(),
            device: 8,
            mount: "/Volumes/Media".into(),
            roots: config.roots.clone(),
        };
        let replacement = Volume {
            key: "replacement".into(),
            uuid: "replacement-id".into(),
            device: 9,
            ..original.clone()
        };
        f.platform
            .volume_overrides
            .lock()
            .unwrap()
            .insert(config.roots[0].clone(), replacement.clone());
        assert!(resolve_coverage_with(&config, &f.platform).roots.is_empty());
        let inventory = inventory_for(
            &config,
            std::slice::from_ref(&replacement),
            std::slice::from_ref(&original),
        );
        assert!(
            !inventory
                .iter()
                .find(|drive| drive.uuid == replacement.uuid)
                .unwrap()
                .included
        );
        assert!(
            inventory
                .iter()
                .find(|drive| drive.uuid == original.uuid)
                .unwrap()
                .included
        );
        config.drives.included.push(crate::config::DriveReference {
            uuid: replacement.uuid.clone(),
            mount: replacement.mount.clone(),
        });
        assert_eq!(
            resolve_coverage_with(&config, &f.platform).roots,
            config.roots
        );
        assert!(
            inventory_for(&config, std::slice::from_ref(&replacement), &[original])
                .iter()
                .find(|drive| drive.uuid == replacement.uuid)
                .unwrap()
                .included
        );
    }
    #[test]
    fn a_fast_reconnect_expires_manual_permission() {
        let mut f = external_fixture();
        f.manager.config.drives.reconnect = vec![crate::config::DriveReconnectPreference {
            uuid: "external-uuid".into(),
            mount: "/Volumes/Media".into(),
            mode: "manual".into(),
        }];
        f.refresh();
        let volume = f.platform.volume_overrides.lock().unwrap()[&f.a].clone();
        write_drive_start(&f.manager.config, &volume).unwrap();
        f.refresh();
        assert!(f.manager.active_roots.contains(&f.a));
        let record = f
            .platform
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|record| {
                record.volume.key == volume.key && !record.stopped.load(Ordering::SeqCst)
            })
            .unwrap()
            .clone();
        (record.callback)(vec![
            Event {
                path: f.a.clone(),
                flags: events::UNMOUNT,
                id: 210,
            },
            Event {
                path: f.a.clone(),
                flags: events::MOUNT,
                id: 211,
            },
        ])
        .unwrap();
        f.refresh();
        assert!(
            !f.manager.active_roots.contains(&f.a),
            "an unmount event must expire approval even when the same device has already reconnected"
        );
    }
    #[test]
    fn configured_excluded_uuid_is_not_selected_by_retained_folders() {
        let mut f = external_fixture();
        f.manager.config.scope = "configured".into();
        f.manager.config.roots = vec![f.a.clone()];
        f.refresh();
        f.drain();
        disconnect(&mut f, libc::ENOENT);
        f.manager.config.drives = preferences("automatic", false, true);
        f.refresh();
        assert!(!f.index.known_roots().unwrap().contains(&f.a));
        restart(&mut f);
        f.platform.discovery_errors.lock().unwrap().clear();
        f.refresh();
        assert!(!f.manager.active_roots.contains(&f.a));
        assert!(!f.index.known_roots().unwrap().contains(&f.a));
    }
    #[test]
    fn configured_excluded_uuid_is_not_reintroduced_by_preview_or_inventory() {
        let f = external_fixture();
        let mut config = f.manager.config.clone();
        config.scope = "configured".into();
        config.roots = vec!["/Volumes/Media/Documents".into()];
        config.drives = preferences("automatic", false, true);
        let volume = Volume {
            key: "removed-volume".into(),
            uuid: "external-uuid".into(),
            mount: "/Volumes/Media".into(),
            device: 8,
            roots: config.roots.clone(),
        };
        f.platform
            .volume_overrides
            .lock()
            .unwrap()
            .insert(config.roots[0].clone(), volume.clone());
        assert!(resolve_coverage_with(&config, &f.platform).roots.is_empty());
        let inventory = inventory_for(
            &config,
            std::slice::from_ref(&volume),
            std::slice::from_ref(&volume),
        );
        assert_eq!(inventory.len(), 1);
        assert!(!inventory[0].included);
        assert!(inventory_for(&config, &[], &[volume]).is_empty());
    }
    #[test]
    fn configured_drive_removal_preserves_other_work_after_restart() {
        for legacy_index in [false, true] {
            let mut f = external_fixture();
            f.manager.config.scope = "configured".into();
            f.manager.config.roots = vec![f.a.clone(), f.b.clone()];
            f.refresh();
            f.drain();
            let healthy_key = format!("uuid:{}", f.b);
            let removed_key = format!("external:{}", f.a);
            for (key, root, id) in [(&healthy_key, &f.b, 77), (&removed_key, &f.a, 91)] {
                f.index
                    .enqueue(
                        key,
                        &[Event {
                            path: root.clone(),
                            flags: 0,
                            id,
                        }],
                    )
                    .unwrap();
            }
            disconnect(&mut f, libc::ENOENT);
            let before = f.index.status().unwrap();
            if legacy_index {
                rusqlite::Connection::open(f.temp.path().join("index.sqlite3"))
                    .unwrap()
                    .execute("DELETE FROM meta WHERE key='source_config_contract'", [])
                    .unwrap();
            }
            f.manager.config.roots.retain(|root| root != &f.a);
            f.manager.config.drives = preferences("automatic", false, true);
            // Saving settings restarts the worker before it sees the reduced roots.
            restart(&mut f);
            assert!(!f.index.known_roots().unwrap().contains(&f.a));
            assert_eq!(f.index.cursor(&healthy_key).unwrap(), Some(77));
            assert!(
                f.index
                    .cursor(&removed_key)
                    .unwrap_err()
                    .to_string()
                    .contains("Unknown volume")
            );
            assert_eq!(
                f.index.status().unwrap()["baseline_walks"],
                before["baseline_walks"]
            );
            f.platform.discovery_errors.lock().unwrap().clear();
            f.refresh();
            assert_eq!(f.manager.active_roots, vec![f.b.clone()]);
            assert_eq!(f.index.cursor(&healthy_key).unwrap(), Some(77));
            assert!(Path::new(&f.a).join("child/file").is_file());
            let db = rusqlite::Connection::open(f.temp.path().join("index.sqlite3")).unwrap();
            for table in ["jobs", "deferred_jobs"] {
                let count: i64 = db
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                        [&removed_key],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0);
            }
        }
    }
    #[test]
    fn configured_root_removal_with_other_policy_changes_still_revalidates() {
        for legacy_index in [false, true] {
            let mut f = external_fixture();
            f.manager.config.scope = "configured".into();
            f.manager.config.roots = vec![f.a.clone(), f.b.clone()];
            f.refresh();
            f.drain();
            let healthy_key = format!("uuid:{}", f.b);
            f.index
                .enqueue(
                    &healthy_key,
                    &[Event {
                        path: f.b.clone(),
                        flags: 0,
                        id: 77,
                    }],
                )
                .unwrap();
            if legacy_index {
                rusqlite::Connection::open(f.temp.path().join("index.sqlite3"))
                    .unwrap()
                    .execute("DELETE FROM meta WHERE key='source_config_contract'", [])
                    .unwrap();
            }
            f.manager.config.roots.retain(|root| root != &f.a);
            f.manager.config.exclude_names.push("private".into());
            restart(&mut f);
            assert_eq!(f.index.cursor(&healthy_key).unwrap(), Some(50));
            assert_eq!(f.index.status().unwrap()["baseline_complete"], false);
        }
    }
    #[test]
    fn removed_drive_stays_excluded_after_restart_and_reconnect_until_explicit_add() {
        let mut f = external_fixture();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.refresh();
        f.drain();
        let healthy_key = format!("uuid:{}", f.b);
        f.index
            .enqueue(
                &healthy_key,
                &[Event {
                    path: f.b.clone(),
                    flags: 0,
                    id: 77,
                }],
            )
            .unwrap();
        let before = f.index.status().unwrap();
        let config = f.manager.config.clone();
        fs::write(config.state_path("pending.json"), b"pending recovery").unwrap();
        fs::write(config.state_path("journal.jsonl"), b"journal history").unwrap();
        disconnect(&mut f, libc::ENOENT);
        f.manager.config.drives = preferences("automatic", false, true);
        f.refresh();
        assert!(!f.index.known_roots().unwrap().contains(&f.a));
        assert_eq!(f.index.cursor(&healthy_key).unwrap(), Some(77));
        assert_eq!(
            f.index.status().unwrap()["baseline_walks"],
            before["baseline_walks"]
        );
        restart(&mut f);
        f.platform.discovery_errors.lock().unwrap().clear();
        f.platform.coverage.lock().unwrap().roots.push(f.a.clone());
        f.refresh();
        assert!(!f.manager.active_roots.contains(&f.a));
        assert!(!f.index.known_roots().unwrap().contains(&f.a));
        assert_eq!(f.index.cursor(&healthy_key).unwrap(), Some(77));
        assert_eq!(
            fs::read(config.state_path("pending.json")).unwrap(),
            b"pending recovery"
        );
        assert_eq!(
            fs::read(config.state_path("journal.jsonl")).unwrap(),
            b"journal history"
        );
        f.manager.config.drives = preferences("automatic", true, false);
        f.refresh();
        assert!(f.manager.active_roots.contains(&f.a));
        assert_eq!(f.index.cursor(&healthy_key).unwrap(), Some(77));
    }
    #[test]
    fn selected_drive_mode_ignores_unknown_uuid_without_affecting_home_or_cloud() {
        let mut f = external_fixture();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.manager.config.drives = preferences("selected", false, false);
        f.refresh();
        f.drain();
        assert_eq!(f.manager.active_roots, vec![f.b.clone()]);
        f.manager.config.drives = preferences("selected", true, false);
        f.refresh();
        f.drain();
        assert!(f.manager.active_roots.contains(&f.a));
        let old_key = format!("external:{}", f.a);
        f.index
            .enqueue(
                &old_key,
                &[Event {
                    path: f.a.clone(),
                    flags: 0,
                    id: 55,
                }],
            )
            .unwrap();
        assert_eq!(f.index.cursor(&old_key).unwrap(), Some(55));
        let replacement = Volume {
            key: "replacement".into(),
            uuid: "new-uuid".into(),
            device: 9,
            mount: "/Volumes/Media".into(),
            roots: vec![f.a.clone()],
        };
        f.platform
            .volume_overrides
            .lock()
            .unwrap()
            .insert(f.a.clone(), replacement);
        f.refresh();
        assert!(!f.manager.active_roots.contains(&f.a));
        assert_eq!(
            saved_coverage(&f)["disconnected_roots"],
            serde_json::json!([f.a])
        );
        f.manager.config.drives = preferences("automatic", true, false);
        f.refresh();
        assert!(
            !f.manager.active_roots.contains(&f.a),
            "a replacement UUID still requires explicit selection"
        );
        f.manager
            .config
            .drives
            .included
            .push(crate::config::DriveReference {
                uuid: "new-uuid".into(),
                mount: "/Volumes/Media".into(),
            });
        f.refresh();
        assert!(f.manager.active_roots.contains(&f.a));
        assert_ne!(f.index.cursor("replacement").unwrap(), Some(55));
        assert!(f.index.cursor(&old_key).is_err());
    }

    #[test]
    fn excluded_external_catalogs_do_not_leave_actionable_failures() {
        let mut f = external_fixture();
        let catalog = "/Volumes/Media/Users".to_string();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push(catalog.clone());
        f.platform
            .discovery_errors
            .lock()
            .unwrap()
            .insert(catalog.clone(), libc::EACCES);
        f.manager.config.drives = preferences("automatic", false, true);
        f.refresh();
        assert!(!f.manager.catalog_unavailable.contains_key(&catalog));
        assert!(
            f.manager.catalogs.contains_key("/Volumes"),
            "mount discovery remains active"
        );
        assert!(!f.manager.catalogs.contains_key(&catalog));
    }
    #[test]
    fn inventory_reads_only_mount_catalog_and_reports_access_failures() {
        struct MountCatalog {
            root: std::path::PathBuf,
            error: Option<i32>,
        }
        impl CatalogReader for MountCatalog {
            fn metadata(&self, _: &Path) -> std::io::Result<crate::coverage::Node> {
                if let Some(code) = self.error {
                    return Err(std::io::Error::from_raw_os_error(code));
                }
                Ok(crate::coverage::Node {
                    directory: true,
                    regular: false,
                    device: 8,
                })
            }
            fn list(&self, path: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
                assert_eq!(path, Path::new("/Volumes"));
                Ok(vec![self.root.clone()])
            }
            fn readable(&self, _: &Path) -> std::io::Result<()> {
                panic!("inventory does not enumerate documents")
            }
            fn is_mount(&self, _: &Path) -> std::io::Result<bool> {
                Ok(true)
            }
            fn accounts(&self) -> std::io::Result<Vec<crate::coverage::Account>> {
                panic!("inventory does not inspect accounts or cloud providers")
            }
            fn startup_devices(&self) -> std::collections::BTreeSet<u64> {
                Default::default()
            }
        }
        let f = external_fixture();
        let reader = MountCatalog {
            root: f.a.clone().into(),
            error: None,
        };
        let result = connected_drives(&reader, &f.platform);
        assert_eq!(result.volumes.len(), 1);
        assert_eq!(result.volumes[0].uuid, "external-uuid");
        assert!(result.issues.is_empty());
        for code in [libc::EACCES, libc::ETIMEDOUT, libc::ECANCELED] {
            let reader = MountCatalog {
                root: f.a.clone().into(),
                error: Some(code),
            };
            let result = connected_drives(&reader, &f.platform);
            assert!(result.volumes.is_empty());
            assert_eq!(result.issues.len(), 1);
            assert!(
                result.issues[0]
                    .reason
                    .contains(&std::io::Error::from_raw_os_error(code).to_string())
            );
        }
        f.platform
            .discovery_errors
            .lock()
            .unwrap()
            .insert(f.a.clone(), libc::EACCES);
        assert_eq!(connected_drives(&reader, &f.platform).issues.len(), 1);
        f.platform
            .discovery_errors
            .lock()
            .unwrap()
            .insert(f.a.clone(), libc::ENOENT);
        let result = connected_drives(&reader, &f.platform);
        assert!(result.volumes.is_empty() && result.issues.is_empty());
    }

    struct InventoryCatalog {
        roots: Vec<std::path::PathBuf>,
        failure: Option<(String, i32)>,
        catalog_error: Option<i32>,
    }
    impl CatalogReader for InventoryCatalog {
        fn metadata(&self, path: &Path) -> std::io::Result<crate::coverage::Node> {
            if let Some((failed, code)) = &self.failure
                && path == Path::new(failed)
            {
                return Err(std::io::Error::from_raw_os_error(*code));
            }
            Ok(crate::coverage::Node {
                directory: true,
                regular: false,
                device: 8,
            })
        }
        fn list(&self, path: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
            assert_eq!(path, Path::new("/Volumes"));
            match self.catalog_error {
                Some(code) => Err(std::io::Error::from_raw_os_error(code)),
                None => Ok(self.roots.clone()),
            }
        }
        fn readable(&self, _: &Path) -> std::io::Result<()> {
            panic!("no document reads")
        }
        fn is_mount(&self, _: &Path) -> std::io::Result<bool> {
            Ok(true)
        }
        fn accounts(&self) -> std::io::Result<Vec<crate::coverage::Account>> {
            panic!("no account discovery")
        }
        fn startup_devices(&self) -> std::collections::BTreeSet<u64> {
            Default::default()
        }
    }

    #[test]
    fn unreadable_drive_identity_does_not_discard_readable_drives() {
        let f = external_fixture();
        f.platform
            .discovery_errors
            .lock()
            .unwrap()
            .insert(f.a.clone(), libc::ECANCELED);
        let reader = InventoryCatalog {
            roots: vec![f.a.clone().into(), f.b.clone().into()],
            failure: None,
            catalog_error: None,
        };
        let result = connected_drives(&reader, &f.platform);
        assert_eq!(
            result.volumes.len(),
            1,
            "One unavailable identity must not discard readable mounted drives: {result:?}"
        );
        assert_eq!(result.volumes[0].roots, vec![f.b]);
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0].mount, f.a);
        assert!(
            result.issues[0]
                .reason
                .contains("Cannot read mounted drive identity")
        );
    }

    #[test]
    fn inventory_degradation_keeps_verified_uuids_and_marks_saved_mount_unknown() {
        let f = external_fixture();
        let bad = "/Volumes/Media";
        let good = "/Volumes/Healthy";
        let known = f.platform.volume_overrides.lock().unwrap()[&f.a].clone();
        f.platform.volume_overrides.lock().unwrap().insert(
            good.into(),
            Volume {
                key: "healthy-key".into(),
                uuid: "healthy-uuid".into(),
                device: 9,
                mount: good.into(),
                roots: vec![good.into()],
            },
        );
        for stage in ["metadata", "identity"] {
            for code in [libc::EACCES, libc::ETIMEDOUT, libc::ECANCELED] {
                f.platform.discovery_errors.lock().unwrap().clear();
                if stage == "identity" {
                    f.platform
                        .discovery_errors
                        .lock()
                        .unwrap()
                        .insert(bad.into(), code);
                }
                let reader = InventoryCatalog {
                    roots: vec![bad.into(), good.into()],
                    failure: (stage == "metadata").then(|| (bad.into(), code)),
                    catalog_error: None,
                };
                let result = inventory_report(
                    &f.manager.config,
                    connected_drives(&reader, &f.platform),
                    Ok(vec![known.clone()]),
                );
                assert!(!result.complete);
                assert_eq!(result.issues.len(), 1);
                assert_eq!(result.issues[0].mount, bad);
                let saved = result
                    .items
                    .iter()
                    .find(|item| item.uuid == known.uuid)
                    .unwrap();
                assert!(saved.included && !saved.connected);
                assert_eq!(saved.availability, "unavailable");
                let healthy = result
                    .items
                    .iter()
                    .find(|item| item.uuid == "healthy-uuid")
                    .unwrap();
                assert!(
                    healthy.connected && !healthy.included,
                    "new UUID still needs explicit inclusion"
                );
                assert_eq!(healthy.availability, "connected");
                assert_eq!(result.items.len(), 2, "failure must not invent an identity");
            }
        }
    }

    #[test]
    fn catalog_and_index_failures_preserve_saved_choices_as_unavailable() {
        let f = external_fixture();
        let config = &f.manager.config;
        for code in [libc::EACCES, libc::ECANCELED] {
            let reader = InventoryCatalog {
                roots: vec![],
                failure: None,
                catalog_error: Some(code),
            };
            let result =
                inventory_report(config, connected_drives(&reader, &f.platform), Ok(vec![]));
            assert!(!result.complete);
            assert_eq!(result.issues[0].mount, "/Volumes");
            assert_eq!(result.items.len(), 1);
            assert!(result.items[0].included && !result.items[0].connected);
            assert_eq!(result.items[0].availability, "unavailable");
        }
        fs::create_dir_all(config.state_path("index.sqlite3").parent().unwrap()).unwrap();
        fs::write(config.state_path("index.sqlite3"), b"not a SQLite database").unwrap();
        let result = inventory_report(
            config,
            ConnectedDrives::default(),
            inventory_known_volumes(config),
        );
        assert!(!result.complete);
        assert_eq!(result.items[0].uuid, "external-uuid");
        assert_eq!(result.items[0].availability, "unavailable");
        assert_eq!(
            result.issues[0].mount,
            config.state_path("index.sqlite3").to_string_lossy()
        );
        assert_eq!(
            fs::read(config.state_path("index.sqlite3")).unwrap(),
            b"not a SQLite database"
        );
    }

    #[test]
    fn canceled_remaining_mounts_produce_bounded_unknown_diagnostics() {
        let f = external_fixture();
        let roots: Vec<std::path::PathBuf> = (0..80)
            .map(|i| format!("/Volumes/Drive{i}").into())
            .collect();
        for root in &roots {
            f.platform
                .discovery_errors
                .lock()
                .unwrap()
                .insert(root.to_string_lossy().into_owned(), libc::ECANCELED);
        }
        let reader = InventoryCatalog {
            roots,
            failure: None,
            catalog_error: None,
        };
        let connected = connected_drives(&reader, &f.platform);
        assert!(connected.volumes.is_empty());
        assert_eq!(connected.issues.len(), 32);
        assert_eq!(connected.issues.last().unwrap().mount, "/Volumes");
        let result = inventory_report(&f.manager.config, connected, Ok(vec![]));
        assert!(!result.complete);
        assert_eq!(result.items[0].availability, "unavailable");
    }

    #[test]
    fn unreadable_known_index_does_not_authorize_a_configured_replacement_uuid() {
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            scope: "configured".into(),
            roots: vec!["/Volumes/Media/Documents".into()],
            state_dir: temp.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let volume = Volume {
            key: "replacement-key".into(),
            uuid: "replacement-uuid".into(),
            device: 9,
            mount: "/Volumes/Media".into(),
            roots: config.roots.clone(),
        };
        let result = inventory_report(
            &config,
            ConnectedDrives {
                volumes: vec![volume],
                issues: vec![],
            },
            Err(anyhow::anyhow!("remembered identities could not be read")),
        );
        assert!(result.items[0].connected);
        assert!(
            !result.items[0].included,
            "An unreadable index cannot erase an existing UUID boundary"
        );
    }

    #[test]
    fn cancelled_inventory_reports_unknown_instead_of_disconnected() {
        let cancelled = Arc::new(AtomicBool::new(true));
        let _scope = crate::directory_io::CancellationScope::new(cancelled);
        let temp = tempfile::tempdir().unwrap();
        let config = Config {
            scope: "configured".into(),
            state_dir: temp.path().join("state").to_string_lossy().into_owned(),
            ..Config::default()
        };
        let result = drive_inventory(&config);
        assert!(!result.complete);
        assert!(result.issues.iter().any(|issue| issue.mount == "/Volumes"));
    }

    #[test]
    fn preview_and_one_shot_coverage_obey_the_same_drive_selection() {
        let mut f = external_fixture();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.manager.config.drives = preferences("selected", false, false);
        let resolved = resolve_coverage_with(&f.manager.config, &f.platform);
        assert_eq!(resolved.roots, vec![f.b.clone()]);
        f.manager.config.drives = preferences("automatic", false, true);
        assert_eq!(
            resolve_coverage_with(&f.manager.config, &f.platform).roots,
            vec![f.b.clone()]
        );
        f.manager.config.drives = preferences("selected", true, false);
        assert_eq!(
            resolve_coverage_with(&f.manager.config, &f.platform).roots,
            vec![f.a.clone(), f.b.clone()]
        );
        f.platform
            .discovery_errors
            .lock()
            .unwrap()
            .insert(f.a.clone(), libc::EACCES);
        let resolved = resolve_coverage_with(&f.manager.config, &f.platform);
        assert!(!resolved.roots.contains(&f.a));
        assert!(resolved.unavailable.contains(&f.a));
        f.manager.config.scope = "configured".into();
        f.manager.config.roots = vec![f.a.clone()];
        assert_eq!(
            resolve_coverage_with(&f.manager.config, &f.platform).roots,
            vec![f.a.clone()]
        );
    }
    #[test]
    fn native_physical_mounts_use_logical_drive_display_without_changing_identity() {
        let f = external_fixture();
        let volume = f.platform.volume_overrides.lock().unwrap()[&f.a].clone();
        assert_eq!(volume.mount, "/System/Volumes/Data/Volumes/Media");
        let inventory = inventory_for(&f.manager.config, &[], std::slice::from_ref(&volume));
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].mount, "/Volumes/Media");
        assert_eq!(inventory[0].name, "Media");
        assert!(!inventory[0].connected);
        assert!(inventory[0].included);
        let current = inventory_for(&f.manager.config, std::slice::from_ref(&volume), &[]);
        assert!(current[0].connected);
        assert_eq!(volume.mount, "/System/Volumes/Data/Volumes/Media");
        let error = anyhow::Error::new(std::io::Error::from_raw_os_error(libc::ENOENT));
        let coverage = Coverage {
            roots: vec!["/Volumes/Media".into()],
            catalog_roots: vec!["/Volumes".into()],
            ..Default::default()
        };
        assert!(
            !disconnected_error(&f.manager.config, &coverage, Some(&volume), &error),
            "a missing child of a connected mount is actionable"
        );
        assert!(!external_mount("/System/Volumes/Data"));
        assert!(!external_mount("/System/Volumes/Data/Volumes"));
        assert!(!external_mount("/System/Volumes/Data/Volumes/Media/child"));
    }

    #[test]
    fn inventory_uses_uuid_and_retains_only_included_disconnected_drives() {
        let mut config = Config {
            scope: "all-user-files".into(),
            ..Config::default()
        };
        let old = Volume {
            key: "old".into(),
            uuid: "external-uuid".into(),
            device: 1,
            mount: "/Volumes/Media".into(),
            roots: vec!["/Volumes/Media".into()],
        };
        let new = Volume {
            key: "new".into(),
            uuid: "new-uuid".into(),
            ..old.clone()
        };
        let local = Volume {
            key: "home".into(),
            uuid: "home".into(),
            mount: "/".into(),
            roots: vec!["/Users/me".into()],
            device: 0,
        };
        let inventory = inventory_for(&config, &[new.clone(), local], std::slice::from_ref(&old));
        assert_eq!(inventory.len(), 2);
        assert!(
            inventory
                .iter()
                .any(|item| item.uuid == old.uuid && !item.connected && item.included)
        );
        assert!(
            inventory
                .iter()
                .any(|item| item.uuid == new.uuid && item.connected && !item.included)
        );
        config.drives = preferences("automatic", false, true);
        assert!(inventory_for(&config, &[], std::slice::from_ref(&old)).is_empty());
        assert!(!inventory_for(&config, std::slice::from_ref(&old), &[])[0].included);
        config.drives = preferences("selected", true, false);
        let inventory = inventory_for(&config, &[new], &[]);
        assert_eq!(inventory.len(), 2);
        assert!(
            inventory
                .iter()
                .any(|item| item.uuid == old.uuid && !item.connected && item.included)
        );
        assert!(
            inventory
                .iter()
                .any(|item| item.uuid == "new-uuid" && item.connected && !item.included)
        );
    }

    #[test]
    fn disconnected_classification_never_hides_access_cloud_or_configured_errors() {
        for code in [libc::EACCES, libc::ETIMEDOUT, libc::EINTR] {
            let mut f = external_fixture();
            f.refresh();
            disconnect(&mut f, code);
            assert!(f.manager.unavailable.contains_key(&f.a));
            assert_eq!(
                saved_coverage(&f)["disconnected_roots"],
                serde_json::json!([])
            );
        }
        for configured in [false, true] {
            let mut f = Fixture::new();
            if configured {
                f.manager.config.scope = "configured".into();
                f.manager.config.roots = vec![f.a.clone()];
            }
            f.refresh();
            disconnect(&mut f, libc::ENOENT);
            assert!(f.manager.unavailable.contains_key(&f.a));
            assert_eq!(
                saved_coverage(&f)["disconnected_roots"],
                serde_json::json!([])
            );
        }
        let mut f = external_fixture();
        f.refresh();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .unavailable
            .push("/Volumes".into());
        disconnect(&mut f, libc::ENOENT);
        assert!(f.manager.unavailable.contains_key(&f.a));
        assert_eq!(
            saved_coverage(&f)["disconnected_roots"],
            serde_json::json!([])
        );
    }

    #[test]
    fn coverage_reasons_and_watch_error_chains_reach_persisted_status() {
        for code in [libc::EACCES, libc::ETIMEDOUT, libc::EINTR] {
            let mut f = Fixture::new();
            let reason = format!("readable: {}", std::io::Error::from_raw_os_error(code));
            *f.platform.coverage.lock().unwrap() = serde_json::from_value(serde_json::json!({
                "roots": [f.a], "excludes": [], "catalog_roots": [],
                "unavailable": [f.b], "root_excludes": {},
                "unavailable_reasons": {f.b.clone(): reason}
            }))
            .unwrap();
            f.refresh();
            let status: serde_json::Value = serde_json::from_slice(
                &fs::read(f.manager.config.state_path("coverage.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(status["unavailable"][&f.b], reason);
            assert!(f.manager.next_retry_time.is_some());
        }
        let expected = format!(
            "watch root open: {}",
            std::io::Error::from_raw_os_error(libc::ETIMEDOUT)
        );
        for catalog in [false, true] {
            let mut f = Fixture::new();
            if catalog {
                f.platform
                    .coverage
                    .lock()
                    .unwrap()
                    .catalog_roots
                    .push(f.b.clone());
            }
            let root = if catalog { &f.b } else { &f.a }.clone();
            f.platform
                .discovery_failures
                .lock()
                .unwrap()
                .insert(root.clone(), 0.0);
            f.refresh();
            let status: serde_json::Value = serde_json::from_slice(
                &fs::read(f.manager.config.state_path("coverage.json")).unwrap(),
            )
            .unwrap();
            let field = if catalog {
                "catalog_unavailable"
            } else {
                "unavailable"
            };
            assert_eq!(status[field][&root], expected);
        }
        let mut f = Fixture::new();
        // Previously serialized coverage has no reasons map and remains valid.
        *f.platform.coverage.lock().unwrap() = serde_json::from_value(serde_json::json!({
            "roots": [f.a], "excludes": [], "catalog_roots": [],
            "unavailable": [f.b], "root_excludes": {}
        }))
        .unwrap();
        f.refresh();
        assert_eq!(f.manager.unavailable[&f.b], "metadata access unavailable");
    }
    #[test]
    fn slow_discovery_leaves_a_full_retry_interval_for_work_after_refresh_finishes() {
        struct RestoreClock;
        impl Drop for RestoreClock {
            fn drop(&mut self) {
                TEST_NOW.set(None);
            }
        }
        let _restore = RestoreClock;
        for catalog_failure in [false, true] {
            TEST_NOW.set(Some(100.0));
            let mut f = Fixture::new();
            {
                let mut coverage = f.platform.coverage.lock().unwrap();
                if catalog_failure {
                    coverage.catalog_roots.push(f.b.clone());
                } else {
                    coverage.roots.push(f.b.clone());
                }
            }
            f.platform
                .discovery_failures
                .lock()
                .unwrap()
                .insert(f.b.clone(), 90.0);
            f.refresh();
            assert_eq!(now(), 190.0);
            assert_eq!(f.manager.next_retry_time, Some(220.0));
            assert!(
                !f.manager.check().unwrap(),
                "completed discovery must leave time for ordinary work"
            );
            f.drain();
            assert_eq!(f.index.status().unwrap()["indexed_entries"], 2);
            TEST_NOW.set(Some(219.0));
            assert!(!f.manager.check().unwrap());
            // An unrelated catalog event may begin discovery just before the
            // old deadline; an unchanged signature must not retain that now-
            // expired deadline after the slow refresh returns.
            f.manager.signal_refresh(false).unwrap();
            assert!(!f.refresh());
            assert_eq!(now(), 309.0);
            assert_eq!(f.manager.next_retry_time, Some(339.0));
            assert!(!f.manager.check().unwrap());
            TEST_NOW.set(Some(338.0));
            assert!(!f.manager.check().unwrap());
            TEST_NOW.set(Some(339.0));
            assert!(
                f.manager.check().unwrap(),
                "retry must become due after the full interval"
            );
            f.refresh();
            assert_eq!(now(), 429.0);
            assert_eq!(f.manager.next_retry_time, Some(459.0));
            assert!(
                !f.manager.check().unwrap(),
                "persistent failure must not restart discovery immediately"
            );
        }
    }
    #[test]
    fn historical_mount_checkpoint_converges_before_initial_traversal() {
        let mut f = Fixture::new();
        f.platform.historical_mount.store(true, Ordering::SeqCst);
        f.refresh();
        assert!(f.manager.refresh_requested());
        f.refresh();
        assert!(!f.manager.refresh_requested());
        f.drain();
        let records = f.platform.records.lock().unwrap();
        assert_eq!(
            records.iter().map(|r| r.since).collect::<Vec<_>>(),
            vec![None, Some(200)]
        );
        let status = f.index.status().unwrap();
        assert_eq!(status["baseline_walks"], 1);
        assert_eq!(status["indexed_entries"], 2);
    }
    #[test]
    fn catalog_historical_mount_checkpoint_survives_force_restart() {
        let mut f = Fixture::new();
        let catalog = f.temp.path().join("Users").to_str().unwrap().to_owned();
        fs::create_dir(&catalog).unwrap();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push(catalog.clone());
        f.platform.historical_mount.store(true, Ordering::SeqCst);
        f.refresh();
        assert!(f.manager.refresh_requested());
        f.refresh();
        assert!(
            !f.manager.refresh_requested(),
            "historical catalog Mount must converge"
        );
        assert!(!f.refresh());
        let records = f.platform.records.lock().unwrap();
        let cursors: Vec<_> = records
            .iter()
            .filter(|r| r.volume.roots[0] == catalog)
            .map(|r| r.since)
            .collect();
        assert_eq!(cursors, vec![None, Some(200)]);
    }
    #[test]
    fn catalog_wrap_discards_only_the_previous_epoch() {
        let mut f = Fixture::new();
        let catalog = f.temp.path().join("Users").to_str().unwrap().to_owned();
        fs::create_dir(&catalog).unwrap();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push(catalog.clone());
        f.refresh();
        let record = f
            .platform
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.volume.roots[0] == catalog)
            .unwrap()
            .clone();
        (record.callback)(vec![
            Event {
                path: String::new(),
                flags: events::HISTORY_DONE,
                id: 900,
            },
            Event {
                path: String::new(),
                flags: events::EVENT_IDS_WRAPPED,
                id: 1,
            },
            Event {
                path: String::new(),
                flags: events::HISTORY_DONE,
                id: 2,
            },
        ])
        .unwrap();
        f.manager.refresh(&mut f.normalizer, true).unwrap();
        let records = f.platform.records.lock().unwrap();
        assert_eq!(
            records
                .iter()
                .rev()
                .find(|r| r.volume.roots[0] == catalog)
                .unwrap()
                .since,
            Some(2)
        );
    }
    #[test]
    fn saved_jobs_wait_until_their_stream_has_started() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        let key = format!("uuid:{}", f.a);
        f.index
            .enqueue(
                &key,
                &[Event {
                    path: format!("{}/child/file", f.a),
                    flags: 0x100,
                    id: 400,
                }],
            )
            .unwrap();
        let index = f.index.clone();
        *f.platform.on_start.lock().unwrap() = Some(Arc::new(move |_| {
            let mut observer = Normalizer::new(Policy::default(), None, None, None, false).unwrap();
            assert!(
                !index.work(&mut observer).unwrap(),
                "saved work ran before its watch was ready"
            );
        }));
        f.manager.refresh(&mut f.normalizer, true).unwrap();
        assert_eq!(f.index.status().unwrap()["pending_jobs"], 1);
        f.drain();
    }
    #[test]
    fn slow_source_probe_does_not_hold_up_foreground_jobs() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        let (entered, observed) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let blocked = Mutex::new(blocked);
        let slow = f.b.clone();
        *f.platform.on_discover.lock().unwrap() = Some(Arc::new(move |root| {
            if root == &slow {
                let _ = entered.send(());
                let _ = blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2));
            }
        }));
        let key = format!("uuid:{}", f.a);
        let started = std::time::Instant::now();
        let mut lane = SourceWorker::start(f.manager).unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "source setup blocked foreground dispatch"
        );
        observed
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        f.index
            .enqueue(
                &key,
                &[Event {
                    path: format!("{}/child/file", f.a),
                    flags: 0x100,
                    id: 300,
                }],
            )
            .unwrap();
        assert!(f.index.work(&mut f.normalizer).unwrap());
        assert_eq!(f.index.cursor(&key).unwrap(), Some(300));
        release.send(()).unwrap();
        lane.close().unwrap();
    }
    #[test]
    fn source_shutdown_wakes_a_lane_between_its_stop_check_and_wait() {
        let shared = Arc::new((
            Mutex::new(LaneControl::default()),
            std::sync::Condvar::new(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let mut lane = SourceWorker {
            shared: shared.clone(),
            stop: stop.clone(),
            thread: None,
            drive_signal: std::path::PathBuf::new(),
            last_drive_signal: Mutex::new(None),
        };
        // Pause an idle waiter immediately after checking the stop predicate,
        // while it still owns the mutex that Condvar::wait will release.
        let state = shared.0.lock().unwrap();
        assert!(!stop.load(Ordering::Acquire));
        let (started, ready) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            started.send(()).unwrap();
            lane.close().unwrap();
            drop(lane);
            finished.send(()).unwrap();
        });
        ready.recv().unwrap();
        // An unsynchronized closer completes its notification before the
        // waiter registers. A synchronized closer must wait for this mutex.
        let _ = done.recv_timeout(std::time::Duration::from_millis(100));
        let (state, waited) = shared
            .1
            .wait_timeout(state, std::time::Duration::from_millis(200))
            .unwrap();
        drop(state);
        closer.join().unwrap();
        assert!(
            !waited.timed_out(),
            "shutdown notification was lost before the source lane began waiting"
        );
        assert!(stop.load(Ordering::Acquire));
    }

    #[test]
    fn source_shutdown_interrupts_only_its_readonly_probe() {
        let f = Fixture::new();
        let (entered, observed) = std::sync::mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let recorded = result.clone();
        *f.platform.on_discover.lock().unwrap() = Some(Arc::new(move |_| {
            let deadline = crate::directory_io::DirectoryIo::begin().unwrap();
            entered.send(()).unwrap();
            let interrupted = crate::directory_io::tests::blocking_read();
            *recorded.lock().unwrap() = Some((
                interrupted,
                deadline
                    .check()
                    .err()
                    .and_then(|error| error.raw_os_error()),
            ));
        }));
        let foreground = crate::directory_io::DirectoryIo::begin().unwrap();
        let mut lane = SourceWorker::start(f.manager).unwrap();
        observed
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        let started = std::time::Instant::now();
        lane.close().unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "shutdown waited for the rest of a directory probe"
        );
        assert_eq!(
            *result.lock().unwrap(),
            Some(((-1, libc::EINTR), Some(libc::ECANCELED)))
        );
        foreground.check().unwrap();
    }
    #[test]
    fn cancelled_source_drains_its_live_stream_callbacks() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        let record = f.platform.records.lock().unwrap()[0].clone();
        f.platform.stop_batches.lock().unwrap().insert(
            f.a.clone(),
            vec![Event {
                path: format!("{}/last", f.a),
                flags: 0x100,
                id: 501,
            }],
        );
        let (entered, observed) = std::sync::mpsc::channel();
        *f.platform.on_discover.lock().unwrap() = Some(Arc::new(move |_| {
            let _deadline = crate::directory_io::DirectoryIo::begin().unwrap();
            entered.send(()).unwrap();
            assert_eq!(
                crate::directory_io::tests::blocking_read(),
                (-1, libc::EINTR)
            );
        }));
        let mut lane = SourceWorker::start(f.manager).unwrap();
        observed
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        lane.close().unwrap();
        assert!(record.stopped.load(Ordering::Acquire));
        assert_eq!(
            f.index.cursor(&record.volume.key).unwrap(),
            Some(501),
            "final callbacks must commit after discovery cancellation"
        );
    }

    #[test]
    fn failed_neighbor_does_not_restart_a_healthy_stream() {
        let mut f = Fixture::new();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.refresh();
        f.drain();
        let healthy = f
            .platform
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.volume.roots == [f.a.clone()])
            .unwrap()
            .clone();
        f.platform
            .discovery_failures
            .lock()
            .unwrap()
            .insert(f.b.clone(), 0.0);
        f.refresh();
        assert!(
            !healthy.stopped.load(Ordering::SeqCst),
            "unrelated failure restarted an unchanged stream"
        );
        assert_eq!(f.manager.active_roots, [f.a]);
    }
    #[test]
    fn transient_root_failure_preserves_saved_cursor_observations_and_jobs() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        let key = format!("uuid:{}", f.a);
        f.index
            .enqueue(
                &key,
                &[Event {
                    path: format!("{}/child/file", f.a),
                    flags: 0x100,
                    id: 91,
                }],
            )
            .unwrap();
        let before = f.index.status().unwrap();
        f.platform
            .discovery_failures
            .lock()
            .unwrap()
            .insert(f.a.clone(), 0.0);
        f.refresh();
        let after = f.index.status().unwrap();
        assert_eq!(
            after["cursors"], before["cursors"],
            "transient access is not identity removal"
        );
        assert_eq!(after["pending_jobs"], before["pending_jobs"]);
        assert_eq!(after["indexed_entries"], before["indexed_entries"]);
        assert!(
            !f.index.work(&mut f.normalizer).unwrap(),
            "dormant work cannot run"
        );
        assert_eq!(
            f.index.next_wakeup().unwrap(),
            None,
            "dormant work cannot spin"
        );
        f.platform.discovery_failures.lock().unwrap().clear();
        f.manager.next_retry_time = Some(0.0);
        f.refresh();
        assert_eq!(f.index.cursor(&key).unwrap(), Some(91));
        f.drain();
        assert_eq!(
            f.index.status().unwrap()["baseline_walks"],
            before["baseline_walks"]
        );
    }
    #[test]
    fn new_root_and_unavailable_retry_preserve_existing_baseline() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.platform.failures.lock().unwrap().insert(f.b.clone());
        f.refresh();
        f.drain();
        assert_eq!(f.manager.active_roots, vec![f.a.clone()]);
        assert!(f.manager.unavailable.contains_key(&f.b));
        assert!(f.manager.next_retry_time.is_some());
        assert_eq!(f.index.status().unwrap()["baseline_walks"], 1);
        f.platform.failures.lock().unwrap().clear();
        f.manager.next_retry_time = Some(0.0);
        f.refresh();
        f.drain();
        assert_eq!(f.manager.active_roots.len(), 2);
        assert_eq!(f.index.status().unwrap()["baseline_walks"], 2);
        assert_eq!(f.manager.next_retry_time, None);
    }
    #[test]
    fn catalog_start_gap_rediscovers_new_roots_before_baseline() {
        let mut f = Fixture::new();
        let catalog = f.temp.path().join("Users").to_str().unwrap().to_owned();
        fs::create_dir(&catalog).unwrap();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push(catalog.clone());
        let weak = Arc::downgrade(&f.platform);
        let b = f.b.clone();
        let watched = catalog.clone();
        *f.platform.on_start.lock().unwrap() = Some(Arc::new(move |v| {
            if v.roots[0] == watched {
                weak.upgrade()
                    .unwrap()
                    .coverage
                    .lock()
                    .unwrap()
                    .roots
                    .push(b.clone());
            }
        }));
        f.refresh();
        assert_eq!(f.manager.active_roots.len(), 2);
        let records = f.platform.records.lock().unwrap();
        assert_eq!(records[0].volume.roots[0], catalog);
        assert_eq!(f.index.status().unwrap()["indexed_entries"], 0);
    }
    #[test]
    fn invalid_catalog_checkpoint_recovers_without_disabling_catalog() {
        let mut f = Fixture::new();
        let catalog = f.temp.path().join("Users").to_str().unwrap().to_owned();
        fs::create_dir(&catalog).unwrap();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push(catalog.clone());
        f.refresh();
        f.platform.invalid_once.store(true, Ordering::SeqCst);
        f.manager.refresh(&mut f.normalizer, true).unwrap();
        assert!(f.manager.catalog_unavailable.is_empty());
        let records = f.platform.records.lock().unwrap();
        let catalogs: Vec<_> = records
            .iter()
            .filter(|r| r.volume.roots[0] == catalog)
            .collect();
        assert_eq!(
            catalogs.iter().map(|r| r.since).collect::<Vec<_>>(),
            vec![None, Some(50), None]
        );
        assert!(catalogs[1].stopped.load(Ordering::SeqCst));
    }
    #[test]
    fn invalid_cursor_restarts_once_and_deactivates_failed_callback() {
        let mut f = Fixture::new();
        f.platform.invalid_once.store(true, Ordering::SeqCst);
        f.refresh();
        let records = f.platform.records.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records[0].stopped.load(Ordering::SeqCst));
        (records[0].callback)(vec![Event {
            path: format!("{}/stale", f.a),
            flags: 0x100,
            id: 999,
        }])
        .unwrap();
        assert_eq!(f.index.cursor(&records[0].volume.key).unwrap(), Some(50));
        (records[1].callback)(vec![Event {
            path: format!("{}/new", f.a),
            flags: 0x100,
            id: 100,
        }])
        .unwrap();
        assert_eq!(f.index.cursor(&records[0].volume.key).unwrap(), Some(100));
    }
    #[test]
    fn drained_callbacks_commit_before_reconfiguration() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        f.platform.stop_batches.lock().unwrap().insert(
            f.a.clone(),
            vec![Event {
                path: format!("{}/last", f.a),
                flags: 0x100,
                id: 500,
            }],
        );
        f.platform.coverage.lock().unwrap().roots.push(f.b.clone());
        f.manager.refresh(&mut f.normalizer, true).unwrap();
        let records = f.platform.records.lock().unwrap();
        let reopened = records
            .iter()
            .rev()
            .find(|r| r.volume.roots[0] == f.a)
            .unwrap();
        assert_eq!(reopened.since, Some(500));
        assert!(records[0].stopped.load(Ordering::SeqCst));
        drop(records);
        f.drain();
        assert_eq!(f.index.status().unwrap()["baseline_walks"], 2);
    }
    #[test]
    fn catalog_only_direct_children_or_controls_trigger_discovery() {
        let mut f = Fixture::new();
        let catalog = f.temp.path().join("Users").to_str().unwrap().to_owned();
        fs::create_dir(&catalog).unwrap();
        f.platform
            .coverage
            .lock()
            .unwrap()
            .catalog_roots
            .push(catalog.clone());
        f.refresh();
        let record = f
            .platform
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.volume.roots[0] == catalog)
            .unwrap()
            .clone();
        (record.callback)(vec![Event {
            path: format!("{catalog}/person/Documents/content"),
            flags: 0x11000,
            id: 501,
        }])
        .unwrap();
        assert!(!f.manager.check().unwrap());
        (record.callback)(vec![Event {
            path: format!("{catalog}/new-user"),
            flags: 0x100,
            id: 502,
        }])
        .unwrap();
        assert!(f.manager.check().unwrap());
        f.refresh();
        (record.callback)(vec![Event {
            path: String::new(),
            flags: events::USER_DROPPED,
            id: 503,
        }])
        .unwrap();
        assert!(f.manager.check().unwrap());
    }
    #[test]
    fn unchanged_metadata_is_idle_and_stopped_callbacks_cannot_enqueue() {
        let mut f = Fixture::new();
        f.refresh();
        f.drain();
        let record = f.platform.records.lock().unwrap()[0].clone();
        assert!(!f.refresh());
        assert_eq!(f.platform.records.lock().unwrap().len(), 1);
        f.manager.close().unwrap();
        (record.callback)(vec![Event {
            path: format!("{}/late", f.a),
            flags: 0x100,
            id: 999,
        }])
        .unwrap();
        assert_eq!(f.index.cursor(&record.volume.key).unwrap(), Some(50));
    }
}
