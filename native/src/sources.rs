//! Single-worker source lifecycle; native callbacks only commit or signal work.
#[cfg(not(test))]
use crate::model::now;
use crate::{
    config::{Config, atomic_json},
    coverage::{Coverage, discover_user_coverage},
    events::{self, Callback, CursorInvalidError, Wake},
    index::Index,
    model::{Event, Volume},
    normalizer::Normalizer,
    policy::{Policy, absolute, nfc},
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

pub fn resolve_coverage(config: &Config) -> Coverage {
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
        resolve_coverage(config)
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
}
impl SourceWorker {
    pub fn new(config: Config, index: Arc<Index>, wake: Wake) -> Result<Self> {
        Self::start(SourceManager::new(config, index, wake))
    }
    fn start(mut manager: SourceManager) -> Result<Self> {
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
    pub catalog_unavailable: BTreeMap<String, String>,
}
impl SourceManager {
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
        let roots: HashSet<_> = volume.roots.iter().map(|r| nfc(&absolute(r))).collect();
        let callback: Callback = Arc::new(move |batch: Vec<Event>| {
            if !token.load(Ordering::Acquire) {
                return Ok(());
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
    pub fn refresh(&mut self, normalizer: &mut Normalizer, force: bool) -> Result<bool> {
        self.check_cancelled()?;
        if self.closed {
            bail!("source manager is closed");
        }
        self.callback_error()?;
        let force = {
            let mut state = self
                .signals
                .lock()
                .map_err(|_| anyhow::anyhow!("source signal lock poisoned"))?;
            let force = force || state.force;
            state.force = false;
            state.requested = false;
            force
        };
        let timestamp = now();
        let retry_due = self.next_retry_time.is_some_and(|t| timestamp >= t);
        let mut coverage = self.platform.coverage(&self.config);
        self.check_cancelled()?;
        let mut catalog_changed = false;
        // Read metadata again after each new catalog starts. New nested catalogs
        // receive the same treatment, bounded under sustained creation activity.
        for attempt in 0..4 {
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
        let mut seen = HashSet::new();
        self.roots = coverage
            .roots
            .iter()
            .filter(|p| seen.insert((*p).clone()))
            .cloned()
            .collect();
        if self.config.scope == "all-user-files" {
            // Disconnected media and unreadable account catalogs remain desired.
            // A later successful identity check decides reuse or replacement.
            for root in self.index.known_roots()? {
                if !self.roots.contains(&root) {
                    self.roots.push(root);
                }
            }
        }
        let mut unavailable: BTreeMap<_, _> = coverage
            .unavailable
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    coverage
                        .unavailable_reasons
                        .get(p)
                        .cloned()
                        .unwrap_or_else(|| "metadata access unavailable".to_owned()),
                )
            })
            .collect();
        let mut candidates = Vec::new();
        for root in &self.roots {
            match self.discover_one(root) {
                Ok(volume) => candidates.push(volume),
                Err(error) if error.is::<std::io::Error>() => {
                    unavailable.insert(root.clone(), format!("{error:#}"));
                }
                Err(error) => return Err(error),
            }
        }
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
                &self.config.signature(),
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
        let status = serde_json::json!({"roots":self.roots,"active_roots":self.active_roots,"unavailable":self.unavailable,"catalog_unavailable":self.catalog_unavailable});
        if self.last_status.as_ref() != Some(&status) {
            atomic_json(self.config.state_path("coverage.json"), &status)?;
            self.last_status = Some(status);
        }
        let completed_at = now();
        if !self.unavailable.is_empty() || !self.catalog_unavailable.is_empty() {
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
        fn coverage(&self, _: &Config) -> Coverage {
            self.coverage.lock().unwrap().clone()
        }
        fn volumes(&self, roots: &[String]) -> Result<Vec<Volume>> {
            for root in roots {
                if let Some(hook) = self.on_discover.lock().unwrap().clone() {
                    hook(root);
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
                .map(|r| Volume {
                    key: format!("uuid:{r}"),
                    uuid: "uuid".into(),
                    device: 1,
                    mount: "/".into(),
                    roots: vec![r.clone()],
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
