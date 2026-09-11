//! Durable reconciliation jobs and observed filesystem state.
//!
//! Event consequences and cursors commit together. A separate worker mutex
//! serializes observations, while the SQLite mutex is released for every scan.
//! Recursive work advances through a durable frontier one directory at a time.
use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
    params_from_iter, types::Value as SqlValue,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use unicode_normalization::is_nfc;

use crate::model::{
    Entry, Event, PendingRecoveryError, Reconciler, ScanError, ScanResult, Volume, now,
};
use crate::policy::{Policy, absolute};

const MUST_SCAN: u32 = 0x1;
const LOST_EVENTS: u32 = 0x2 | 0x4 | 0x8;
const HISTORY_DONE: u32 = 0x10;
const ROOT_CHANGED: u32 = 0x20;
const MOUNT_CHANGED: u32 = 0x40 | 0x80;
const CREATED: u32 = 0x100;
const REMOVED: u32 = 0x200;
const RENAMED: u32 = 0x800;
const IS_DIR: u32 = 0x20000;
const IS_FILE: u32 = 0x10000;
const CONTENT_FLAGS: u32 = 0x400 | 0x1000 | 0x2000 | 0x4000 | 0x8000;
const MAX_JOBS: i64 = 4096;
const ORDINARY_BURST_LIMIT: u32 = 256;
const ORDINARY_BURST_TIME: Duration = Duration::from_secs(1);
const QUEUE_ROWID_HIGH_WATER: &str = "queue_rowid_high_water";

fn scheduling_now() -> Instant {
    #[cfg(test)]
    if let Some(now) = tests::SCHEDULER_CLOCK.get() {
        return now;
    }
    Instant::now()
}

// Keep lexical spellings in SQLite, including canonically equivalent names.
fn within(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&(root.trim_end_matches('/').to_owned() + "/"))
}
fn parent(path: &str) -> String {
    Path::new(path)
        .parent()
        .unwrap_or(Path::new("/"))
        .to_string_lossy()
        .into_owned()
}
fn sqlite_unsigned(value: u64) -> SqlValue {
    if value <= i64::MAX as u64 {
        SqlValue::Integer(value as i64)
    } else {
        SqlValue::Text(format!("u:{value}"))
    }
}
fn is_directory(kind: &str) -> bool {
    matches!(kind, "directory" | "dir")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    path: String,
    volume_key: String,
    recursive: bool,
    baseline: bool,
    generation: i64,
    attempts: i64,
    ready_class: i64,
    ordinary_scope: i64,
}
impl Job {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            path: row.get("path")?,
            volume_key: row.get("volume_key")?,
            recursive: row.get("recursive")?,
            baseline: row.get("baseline")?,
            generation: row.get("generation")?,
            attempts: row.get("attempts")?,
            ready_class: row.get("ready_class")?,
            ordinary_scope: row.get("ordinary_scope")?,
        })
    }
    fn scans_baseline(&self) -> bool {
        self.baseline && self.ordinary_scope == 0
    }
    fn scans_recursively(&self) -> bool {
        if self.ordinary_scope > 0 {
            self.ordinary_scope == 2
        } else {
            self.recursive
        }
    }
}
#[derive(Clone, Copy)]
struct QueueIntent {
    recursive: bool,
    baseline: bool,
    ordinary_scope: i64,
    due: f64,
    preserve_delay: bool,
}
struct State {
    db: Connection,
    policy: Option<Policy>,
    active_job: Option<Job>,
    next_ready_class: i64,
    ordinary_burst: Option<(Instant, u32)>,
    startup_rowid: i64,
}

pub struct Index {
    pub path: PathBuf,
    read_only: bool,
    state: Mutex<State>,
    worker: Mutex<()>,
}

impl Index {
    pub fn new(path: impl AsRef<Path>, read_only: bool) -> Result<Self> {
        let path = PathBuf::from(absolute(
            path.as_ref()
                .to_str()
                .context("Index path must be valid Unicode")?,
        ));
        if !read_only {
            std::fs::create_dir_all(path.parent().context("Index has no parent directory")?)?;
        }
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        };
        let db = Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        db.busy_timeout(Duration::from_secs(10))?;
        if !read_only {
            db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
            // Publish the complete schema together. Status readers may already
            // see this file while a new worker is still initializing it.
            let transaction = Transaction::new_unchecked(&db, TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS volumes (
                    key TEXT PRIMARY KEY, uuid TEXT NOT NULL, device INTEGER NOT NULL,
                    mount TEXT NOT NULL, roots TEXT NOT NULL, cursor TEXT);
                CREATE TABLE IF NOT EXISTS jobs (
                    path TEXT PRIMARY KEY, volume_key TEXT NOT NULL,
                    recursive INTEGER NOT NULL, baseline INTEGER NOT NULL DEFAULT 0,
                    generation INTEGER NOT NULL DEFAULT 1, attempts INTEGER NOT NULL DEFAULT 0,
                    next_attempt REAL NOT NULL DEFAULT 0, error TEXT,
                    ordinary_scope INTEGER NOT NULL DEFAULT 0);
                CREATE INDEX IF NOT EXISTS jobs_due ON jobs(next_attempt);
                CREATE INDEX IF NOT EXISTS jobs_events ON jobs(volume_key, recursive, baseline);
                CREATE TABLE IF NOT EXISTS inactive_volumes (key TEXT PRIMARY KEY);
                CREATE TABLE IF NOT EXISTS scan_runs (path TEXT PRIMARY KEY, scan_id TEXT NOT NULL, job TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS scan_seen (scope TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY(scope,path));
                CREATE TABLE IF NOT EXISTS deferred_jobs (
                    path TEXT PRIMARY KEY, volume_key TEXT NOT NULL,
                    recursive INTEGER NOT NULL, baseline INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS entries (
                    path TEXT PRIMARY KEY, parent TEXT NOT NULL, kind TEXT NOT NULL,
                    dev INTEGER, ino INTEGER, mtime_ns INTEGER, ctime_ns INTEGER,
                    size INTEGER, mode INTEGER);
                CREATE INDEX IF NOT EXISTS entries_parent ON entries(parent);
                CREATE TABLE IF NOT EXISTS directories (path TEXT PRIMARY KEY);
                CREATE TABLE IF NOT EXISTS metrics (key TEXT PRIMARY KEY, value INTEGER NOT NULL);",
            )?;
            let has_ordinary_scope = transaction
                .prepare("PRAGMA table_info(jobs)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .any(|name| name == "ordinary_scope");
            if !has_ordinary_scope {
                transaction.execute(
                    "ALTER TABLE jobs ADD COLUMN ordinary_scope INTEGER NOT NULL DEFAULT 0",
                    [],
                )?;
            }
            transaction.commit()?;
        }
        let mut state = State {
            db,
            policy: None,
            active_job: None,
            next_ready_class: 0,
            ordinary_burst: None,
            startup_rowid: 0,
        };
        if !read_only {
            let transaction =
                Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
            state.drain_deferred()?;
            let startup_rowid =
                state
                    .db
                    .query_row("SELECT COALESCE(MAX(rowid),0) FROM jobs", [], |row| {
                        row.get(0)
                    })?;
            let high_water: i64 = state.get(QUEUE_ROWID_HIGH_WATER, 0)?;
            if startup_rowid > high_water {
                // A separate CLI must allocate above this worker's floor even
                // after the jobs establishing that floor have been drained.
                state.set(QUEUE_ROWID_HIGH_WATER, &startup_rowid)?;
            }
            transaction.commit()?;
            // Persisted deferred requests are backlog too. Capture the floor
            // after their recovery, before this worker accepts any live intake.
            state.startup_rowid = startup_rowid;
        }
        Ok(Self {
            path,
            read_only,
            state: Mutex::new(state),
            worker: Mutex::new(()),
        })
    }
    fn writable(&self) -> Result<()> {
        if self.read_only {
            bail!("The index is open read-only")
        }
        Ok(())
    }
    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("Index database mutex poisoned"))
    }
    fn lock_worker(&self) -> Result<MutexGuard<'_, ()>> {
        self.worker
            .lock()
            .map_err(|_| anyhow::anyhow!("Index worker mutex poisoned"))
    }
    pub fn bind_policy(&self, policy: Policy) -> Result<()> {
        self.lock()?.policy = Some(policy);
        Ok(())
    }
    pub fn configure(&self, signature: &str, volumes: &[Volume], roots: &[String]) -> Result<bool> {
        self.configure_sources(signature, volumes, roots, false, None, None)
    }
    /// Availability is not deletion. Retain a known desired root's identity,
    /// cursor and frontier while its stream cannot currently be started.
    pub fn configure_available(
        &self,
        signature: &str,
        volumes: &[Volume],
        desired: &[String],
    ) -> Result<bool> {
        self.configure_sources(signature, volumes, desired, true, None, None)
    }
    pub fn configure_policy(
        &self,
        signature: &str,
        volumes: &[Volume],
        desired: &[String],
        policy: Policy,
    ) -> Result<bool> {
        self.configure_sources(signature, volumes, desired, true, Some(policy), None)
    }
    pub fn prepare_sources(
        &self,
        signature: &str,
        volumes: &[Volume],
        desired: &[String],
        policy: Policy,
        running: &BTreeSet<String>,
    ) -> Result<bool> {
        self.configure_sources(
            signature,
            volumes,
            desired,
            true,
            Some(policy),
            Some(running),
        )
    }
    pub fn activate_volume(&self, key: &str) -> Result<()> {
        self.writable()?;
        self.lock()?
            .db
            .execute("DELETE FROM inactive_volumes WHERE key=?", [key])?;
        Ok(())
    }
    pub fn current_policy(&self) -> Result<Option<Policy>> {
        Ok(self.lock()?.policy.clone())
    }
    pub fn known_roots(&self) -> Result<Vec<String>> {
        self.lock()?.get("roots", Vec::new())
    }
    fn configure_sources(
        &self,
        signature: &str,
        volumes: &[Volume],
        roots: &[String],
        preserve_unavailable: bool,
        policy: Option<Policy>,
        running: Option<&BTreeSet<String>>,
    ) -> Result<bool> {
        self.writable()?;
        let _worker = self.lock_worker()?;
        let mut state = self.lock()?;
        let mut roots: Vec<String> = roots
            .iter()
            .map(|r| absolute(r))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut identities = volumes.to_vec();
        for volume in &mut identities {
            volume.mount = absolute(&volume.mount);
            volume.roots = volume.roots.iter().map(|r| absolute(r)).collect();
            volume.roots.sort();
        }
        let mut inactive = BTreeSet::new();
        if preserve_unavailable {
            let active_roots: BTreeSet<_> = identities
                .iter()
                .flat_map(|v| v.roots.iter().cloned())
                .collect();
            for volume in state.volumes()?.into_values() {
                if volume
                    .roots
                    .iter()
                    .all(|root| roots.contains(root) && !active_roots.contains(root))
                {
                    inactive.insert(volume.key.clone());
                    identities.push(volume);
                }
            }
        }
        if let Some(running) = running {
            inactive.extend(
                identities
                    .iter()
                    .filter(|volume| !running.contains(&volume.key))
                    .map(|volume| volume.key.clone()),
            );
        }
        identities.sort_by(|a, b| a.key.cmp(&b.key));
        let covered: BTreeSet<_> = identities
            .iter()
            .flat_map(|v| v.roots.iter().cloned())
            .collect();
        if preserve_unavailable {
            // Desired roots without any verified identity remain in coverage,
            // not in the executable index until first successful discovery.
            roots = covered.iter().cloned().collect();
        }
        if roots.iter().cloned().collect::<BTreeSet<_>>() != covered
            || identities
                .iter()
                .map(|v| &v.key)
                .collect::<BTreeSet<_>>()
                .len()
                != identities.len()
        {
            bail!("Roots must match uniquely identified volume coverage")
        }
        let identity = json!([
            signature,
            roots,
            identities
                .iter()
                .map(|v| json!([v.key, v.uuid, v.mount, v.roots]))
                .collect::<Vec<_>>()
        ]);
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        let previous: Value = state.get("identity", Value::Null)?;
        let mut old = state.volumes()?;
        let mut pending: BTreeMap<String, String> =
            match state.get::<Option<BTreeMap<String, String>>>("pending_baseline_roots", None)? {
                Some(value) => value,
                None if !state.get("baseline_started", false)? => old
                    .values()
                    .flat_map(|v| v.roots.iter().map(|r| (r.clone(), v.key.clone())))
                    .collect(),
                None => BTreeMap::new(),
            };
        if previous.is_null() || previous.get(0) != identity.get(0) {
            for table in [
                "jobs",
                "deferred_jobs",
                "entries",
                "directories",
                "volumes",
                "scan_runs",
                "scan_seen",
            ] {
                state.db.execute(&format!("DELETE FROM {table}"), [])?;
            }
            old.clear();
            pending.clear();
        }
        let revalidate: BTreeSet<String> = if state.get("needs_revalidation", false)? {
            state
                .get("revalidation_keys", old.keys().cloned().collect::<Vec<_>>())?
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };
        let retained: BTreeSet<String> = identities
            .iter()
            .filter(|v| {
                old.get(&v.key)
                    .is_some_and(|o| o.uuid == v.uuid && o.mount == v.mount && o.roots == v.roots)
            })
            .map(|v| v.key.clone())
            .collect();
        // Cached observations belong to an exact verified identity. Even when
        // a replacement recreates the same job path, its scan starts afresh.
        let mut scans = state.db.prepare("SELECT path, job FROM scan_runs")?;
        let obsolete = scans
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|row| match row {
                Ok((path, saved)) => match serde_json::from_str::<Job>(&saved) {
                    Ok(job) if retained.contains(&job.volume_key) => None,
                    Ok(_) => Some(Ok(path)),
                    Err(error) => Some(Err(anyhow::Error::from(error))),
                },
                Err(error) => Some(Err(anyhow::Error::from(error))),
            })
            .collect::<Result<Vec<_>>>()?;
        drop(scans);
        for scope in obsolete {
            state.clear_scan(&scope)?;
        }
        let retained_owners: Vec<(String, String)> = retained
            .iter()
            .flat_map(|k| old[k].roots.iter().map(|r| (r.clone(), k.clone())))
            .collect();
        // Stream job rows; never materialize a potentially whole-tree frontier.
        for table in ["jobs", "deferred_jobs"] {
            let mut statement = state
                .db
                .prepare(&format!("SELECT path, volume_key FROM {table}"))?;
            let rows = statement
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (path, key) = row?;
                if !retained.contains(&key)
                    && let Some((_, owner)) = retained_owners
                        .iter()
                        .filter(|(r, _)| within(&path, r))
                        .max_by(|a, b| {
                            (a.0.chars().count(), &a.1).cmp(&(b.0.chars().count(), &b.1))
                        })
                {
                    state.db.execute(
                        &format!("UPDATE {table} SET volume_key=? WHERE path=?"),
                        params![owner, path],
                    )?;
                }
            }
        }
        for (key, volume) in &old {
            if retained.contains(key) {
                continue;
            }
            for table in ["jobs", "deferred_jobs", "volumes"] {
                let column = if table == "volumes" {
                    "key"
                } else {
                    "volume_key"
                };
                state
                    .db
                    .execute(&format!("DELETE FROM {table} WHERE {column}=?"), [key])?;
            }
            for root in &volume.roots {
                let preserve = retained_owners
                    .iter()
                    .filter(|(r, _)| r != root && within(r, root))
                    .map(|(r, _)| r.clone())
                    .collect::<Vec<_>>();
                state.prune(root, true, &preserve)?;
            }
        }
        pending.retain(|_, key| retained.contains(key));
        for key in revalidate.intersection(&retained) {
            for root in &old[key].roots {
                pending.insert(root.clone(), key.clone());
            }
        }
        for volume in &identities {
            if !retained.contains(&volume.key) {
                state.db.execute(
                    "INSERT INTO volumes VALUES (?, ?, ?, ?, ?, NULL)",
                    params![
                        volume.key,
                        volume.uuid,
                        sqlite_unsigned(volume.device),
                        volume.mount,
                        serde_json::to_string(&volume.roots)?
                    ],
                )?;
                for root in &volume.roots {
                    pending.insert(root.clone(), volume.key.clone());
                }
            } else {
                state.db.execute(
                    "UPDATE volumes SET device=? WHERE key=?",
                    params![sqlite_unsigned(volume.device), volume.key],
                )?;
            }
        }
        for table in ["jobs", "deferred_jobs"] {
            let mut statement = state
                .db
                .prepare(&format!("SELECT path, volume_key FROM {table}"))?;
            for row in
                statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            {
                let (path, key) = row?;
                if let Some(owner) = state.volume_for(&path)?
                    && owner != key
                {
                    state.db.execute(
                        &format!("UPDATE {table} SET volume_key=? WHERE path=?"),
                        params![owner, path],
                    )?;
                }
            }
        }
        state.db.execute("DELETE FROM inactive_volumes", [])?;
        for key in inactive {
            state
                .db
                .execute("INSERT INTO inactive_volumes VALUES (?)", [key])?;
        }
        state.set("identity", &identity)?;
        state.set("roots", &roots)?;
        state.set("pending_baseline_roots", &pending)?;
        state.set("baseline_started", &pending.is_empty())?;
        state.set("needs_revalidation", &false)?;
        state.set("revalidation_keys", &Vec::<String>::new())?;
        state.refresh_baseline()?;
        let unfinished = !state.get("baseline_complete", false)?;
        transaction.commit()?;
        if let Some(policy) = policy {
            state.policy = Some(policy);
        }
        Ok(unfinished)
    }
    pub fn cursor(&self, key: &str) -> Result<Option<u64>> {
        self.lock()?.cursor(key)
    }
    pub fn seed_cursor(&self, key: &str, id: u64) -> Result<()> {
        self.writable()?;
        let state = self.lock()?;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        state.volume_roots(key)?;
        state.db.execute(
            "UPDATE volumes SET cursor=? WHERE key=? AND cursor IS NULL",
            params![id.to_string(), key],
        )?;
        transaction.commit()?;
        Ok(())
    }
    pub fn invalidate_volume(&self, key: &str) -> Result<()> {
        self.writable()?;
        let state = self.lock()?;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        for root in state.volume_roots(key)? {
            state.queue(&root, key, true, false, 0.0, false)?;
        }
        state
            .db
            .execute("UPDATE volumes SET cursor=NULL WHERE key=?", [key])?;
        state.metric("recovery_requests", 1)?;
        transaction.commit()?;
        Ok(())
    }
    pub fn enqueue(&self, key: &str, events: &[Event]) -> Result<()> {
        self.writable()?;
        let state = self.lock()?;
        let policy = state
            .policy
            .as_ref()
            .context("Bind the normalizer policy before receiving events")?;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        let roots = state.volume_roots(key)?;
        let mut newest = state.cursor(key)?;
        for event in events {
            let flags = event.flags;
            newest = Some(if flags & 0x8 != 0 {
                event.id
            } else {
                newest.map_or(event.id, |v| v.max(event.id))
            });
            state.metric("events_received", 1)?;
            if flags & (ROOT_CHANGED | MOUNT_CHANGED) != 0 {
                state.mark_revalidation(key)?;
                continue;
            }
            if flags & LOST_EVENTS != 0 {
                for root in &roots {
                    state.queue(root, key, true, false, 0.0, false)?;
                }
                state.metric("recovery_requests", 1)?;
                continue;
            }
            if flags & HISTORY_DONE != 0 || event.path.is_empty() {
                continue;
            }
            let path = absolute(&event.path);
            if !state.accepts(&path) || !roots.iter().any(|r| within(&path, r)) {
                state.metric("excluded_events", 1)?;
                continue;
            }
            if flags & MUST_SCAN != 0 {
                state.queue(&path, key, true, false, 0.0, false)?;
                continue;
            }
            if flags & IS_FILE != 0
                && flags & CONTENT_FLAGS != 0
                && flags & !(IS_FILE | CONTENT_FLAGS) == 0
                && is_nfc(
                    Path::new(&path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(""),
                )
            {
                let mode: Option<Option<u32>> = state
                    .db
                    .query_row(
                        "SELECT mode FROM entries WHERE path=? AND kind='file'",
                        [&path],
                        |r| r.get(0),
                    )
                    .optional()?;
                if mode.flatten().is_some_and(|m| m & 0o170000 == 0o100000) {
                    state.metric("ignored_content_events", 1)?;
                    continue;
                }
            }
            let owner = state.volume_for(&path)?.unwrap_or_else(|| key.to_owned());
            let owner_roots = state.volume_roots(&owner)?;
            let parent = if owner_roots.contains(&path) {
                path.clone()
            } else {
                parent(&path)
            };
            if state.accepts(&parent) {
                state.queue(&parent, key, false, false, 0.0, false)?;
            }
            if flags & IS_DIR != 0 && flags & REMOVED == 0 && policy.accepts_lexically(&path) {
                let known = state.known_directory(&path)?;
                if !known || flags & (CREATED | RENAMED) != 0 {
                    state.queue(&path, key, true, false, 0.0, false)?;
                }
            }
            state.bound_queue(key)?;
        }
        if let Some(newest) = newest {
            state.db.execute(
                "UPDATE volumes SET cursor=? WHERE key=?",
                params![newest.to_string(), key],
            )?;
        }
        state.bound_queue(key)?;
        transaction.commit()?;
        Ok(())
    }
    pub fn bootstrap_jobs(&self) -> Result<()> {
        self.writable()?;
        let state = self.lock()?;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        for (root, key) in state.get("pending_baseline_roots", BTreeMap::<String, String>::new())? {
            state.queue(&root, &key, true, true, 0.0, false)?;
        }
        state.set("pending_baseline_roots", &BTreeMap::<String, String>::new())?;
        state.set("baseline_started", &true)?;
        state.refresh_baseline()?;
        transaction.commit()?;
        Ok(())
    }
    pub fn request_reconcile(&self, paths: Option<&[String]>) -> Result<()> {
        self.writable()?;
        let state = self.lock()?;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        let configured = state.get("roots", Vec::<String>::new())?;
        for path in paths.unwrap_or(&configured) {
            let path = absolute(path);
            let owner = state.volume_for(&path)?;
            if owner.is_none() || !state.accepts(&path) {
                bail!("Reconciliation path is outside the active policy: {path}")
            }
            state.queue(&path, &owner.unwrap(), true, false, 0.0, false)?;
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn work(&self, normalizer: &mut impl Reconciler) -> Result<bool> {
        self.writable()?;
        let _worker = self.lock_worker()?;
        let Some(policy) = self.lock()?.policy.clone() else {
            return Ok(false);
        };
        normalizer.set_policy(policy);
        if self.lock()?.get("needs_revalidation", false)? {
            bail!("Event roots changed; revalidate volume identity and roots before continuing")
        }
        let retries = normalizer.retry_paths(now())?;
        let retained = {
            let state = self.lock()?;
            let mut retained = Vec::new();
            for path in normalizer.active_scans() {
                let exists = state.db.query_row("SELECT EXISTS(SELECT 1 FROM jobs JOIN scan_runs ON scan_runs.path=jobs.path WHERE jobs.path=? AND volume_key NOT IN (SELECT key FROM inactive_volumes))", [&path], |row| row.get::<_, bool>(0))?;
                if exists {
                    retained.push(path);
                } else {
                    state.clear_scan(&path)?;
                }
            }
            retained
        };
        normalizer.retain_scans(&retained);
        let (mut job, configured_root, nested_roots) = {
            let mut state = self.lock()?;
            let transaction =
                Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
            state.drain_deferred()?;
            for value in retries {
                let path = absolute(&value);
                if let Some(owner) = state.volume_for(&path)?
                    && state.accepts(&path)
                {
                    state.queue(&path, &owner, false, false, 0.0, true)?;
                }
            }
            // Let cheap ordinary observations share a bounded burst before
            // rotating through failed, persisted backlog, and baseline work. Check elapsed time
            // before selection so a slow ordinary operation yields next turn.
            let selected_at = scheduling_now();
            let burst_expired = state.ordinary_burst.is_some_and(|(started, count)| {
                count >= ORDINARY_BURST_LIMIT
                    || selected_at.saturating_duration_since(started) >= ORDINARY_BURST_TIME
            });
            let next_ready_class = if burst_expired {
                2
            } else {
                state.next_ready_class
            };
            // Explicit intake is FIFO, ahead of implicit legacy/frontier work.
            // Repeated short paths must not outrank older directory requests.
            let job = state.db.query_row(
                "SELECT *, CASE WHEN ordinary_scope>0 THEN CASE WHEN rowid>? THEN 1 ELSE 3 END
                 WHEN attempts>0 AND error IS NOT NULL THEN 2 WHEN baseline=1 THEN 0 ELSE 3 END AS ready_class
                 FROM jobs WHERE next_attempt<=? AND volume_key NOT IN (SELECT key FROM inactive_volumes)
                 ORDER BY (ready_class+4-?)%4, CASE WHEN ordinary_scope>0 THEN 0 ELSE 1 END,
                 CASE WHEN ordinary_scope>0 THEN rowid ELSE next_attempt END,
                 recursive ASC, length(path), path LIMIT 1",
                params![state.startup_rowid, now(), next_ready_class], Job::from_row,
            ).optional()?;
            let roots = state.get("roots", Vec::<String>::new())?;
            let configured = job.as_ref().is_some_and(|j| roots.contains(&j.path));
            transaction.commit()?;
            let Some(job) = job else {
                state.ordinary_burst = None;
                return Ok(false);
            };
            if job.ready_class == 1 {
                let (started, count) = if burst_expired {
                    (selected_at, 0)
                } else {
                    state.ordinary_burst.unwrap_or((selected_at, 0))
                };
                state.ordinary_burst = Some((started, count + 1));
                state.next_ready_class = 1;
            } else {
                state.ordinary_burst = None;
                state.next_ready_class = (job.ready_class + 1) % 4;
            }
            let mut active = job.clone();
            active.recursive = job.scans_recursively();
            state.active_job = Some(active);
            let inactive: BTreeSet<String> = state
                .db
                .prepare("SELECT key FROM inactive_volumes")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let nested_roots: Vec<String> = state
                .volumes()?
                .into_values()
                .filter(|volume| volume.key != job.volume_key && !inactive.contains(&volume.key))
                .flat_map(|volume| volume.roots)
                .filter(|root| root != &job.path && within(root, &job.path))
                .collect();
            (job, configured, nested_roots)
        };
        // Filesystem work deliberately occurs with no SQLite lock or transaction.
        let observed = (|| -> Result<ScanResult> {
            if configured_root {
                match crate::directory_io::metadata(Path::new(&job.path), false) {
                    Ok(info)
                        if info.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFDIR as u32 => {}
                    Ok(_) => {
                        let state = self.lock()?;
                        let transaction =
                            Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
                        state.mark_revalidation(&job.volume_key)?;
                        transaction.commit()?;
                        bail!("Configured root is missing; revalidate roots before continuing")
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let state = self.lock()?;
                        let transaction =
                            Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
                        state.mark_revalidation(&job.volume_key)?;
                        transaction.commit()?;
                        bail!("Configured root is missing; revalidate roots before continuing")
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            normalizer.reconcile_step(&job.path, false)
        })();
        let result = match observed {
            Ok(result) => result,
            Err(error) => {
                if error.downcast_ref::<PendingRecoveryError>().is_some()
                    || error.downcast_ref::<std::io::Error>().is_none()
                {
                    self.lock()?.active_job = None;
                    return Err(error);
                }
                let errno = error
                    .downcast_ref::<std::io::Error>()
                    .and_then(|e| e.raw_os_error());
                ScanResult {
                    scope: job.path.clone(),
                    errors: vec![ScanError {
                        path: job.path.clone(),
                        error: error.to_string(),
                        errno,
                    }],
                    ..Default::default()
                }
            }
        };
        // Root disappearance is filesystem evidence, collected outside SQLite.
        // An inaccessible root remains protected; absence alone permits pruning.
        let missing_roots: BTreeSet<String> = if result.complete && result.errors.is_empty() {
            nested_roots
                .into_iter()
                .filter(|root| {
                    crate::directory_io::metadata(Path::new(root), false)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                })
                .collect()
        } else {
            BTreeSet::new()
        };
        let mut state = self.lock()?;
        state.active_job = None;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        if let Some(scan_id) = &result.scan_id {
            job = state.stage_scan(&job, &result, scan_id, &missing_roots)?;
        } else {
            state.clear_scan(&job.path)?;
            if !result.complete {
                bail!("partial observation has no scan identity");
            }
        }
        state.metric(
            "renamed",
            i64::try_from(result.renamed).context("Rename count exceeds SQLite integer")?,
        )?;
        if !result.complete {
            // Keep the original generation and baseline obligation durable.
            // Moving one incomplete turn behind other ready work permits bounded
            // directory slices without repeatedly selecting the same frontier.
            state.move_to_queue_tail(&job.path)?;
            state.db.execute(
                "UPDATE jobs SET next_attempt=MAX(next_attempt, ?) WHERE path=? AND attempts=0",
                params![now(), job.path],
            )?;
            state.drain_deferred()?;
            transaction.commit()?;
            return Ok(true);
        }
        let baseline_root = job.scans_baseline()
            && state
                .get("roots", Vec::<String>::new())?
                .contains(&job.path);
        state.metric(
            if baseline_root {
                "baseline_walks"
            } else if job.scans_recursively() {
                "subtree_scans"
            } else {
                "shallow_scans"
            },
            1,
        )?;
        let directories = if result.scan_id.is_some() {
            state.finish_scan(&job, &result, &missing_roots)?
        } else {
            state.replace(&job, &result, &missing_roots)?
        };
        let acknowledged = if job.baseline && job.ordinary_scope > 0 {
            // A shallow ordinary turn does not complete this row's independent
            // baseline traversal. New generations retain their new requests.
            let acknowledged = state.db.execute(
                "UPDATE jobs SET ordinary_scope=0 WHERE path=? AND generation=?",
                params![job.path, job.generation],
            )?;
            if result.errors.is_empty() {
                state.db.execute("UPDATE jobs SET attempts=0, error=NULL, next_attempt=0 WHERE path=? AND generation=?", params![job.path, job.generation])?;
            }
            acknowledged
        } else {
            state.db.execute(
                "DELETE FROM jobs WHERE path=? AND generation=?",
                params![job.path, job.generation],
            )?
        };
        if acknowledged == 0
            && state
                .db
                .query_row(
                    "SELECT ordinary_scope>0 FROM jobs WHERE path=? AND generation<>?",
                    params![job.path, job.generation],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false)
        {
            // This observation consumed its turn. A newer in-flight request
            // remains durable, but must yield to other already queued work.
            state.move_to_queue_tail(&job.path)?;
        }
        let actual_scope = absolute(if result.scope.is_empty() {
            &job.path
        } else {
            &result.scope
        });
        if directories.is_empty() && result.errors.is_empty() {
            let parent = parent(&actual_scope);
            if parent != actual_scope && state.accepts(&parent) {
                state.queue(&parent, &job.volume_key, false, false, 0.0, false)?;
            }
        }
        // Acknowledge the parent before queueing children, or coalescing would
        // absorb the frontier back into the recursive parent that just finished.
        if result.scan_id.is_some() {
            let mut statement = state.db.prepare("SELECT seen.path FROM scan_seen seen JOIN entries ON entries.path=seen.path WHERE seen.scope=? AND entries.kind IN ('directory','dir')")?;
            for child in statement.query_map([&job.path], |row| row.get::<_, String>(0))? {
                state.queue_child(&job, &child?)?;
            }
        } else {
            for item in &result.entries {
                if is_directory(&item.kind) {
                    state.queue_child(&job, &item.path)?;
                }
            }
        }
        state.clear_scan(&job.path)?;
        for error in &result.errors {
            let mut failed = absolute(if error.path.is_empty() {
                &actual_scope
            } else {
                &error.path
            });
            if !within(&failed, &actual_scope) || !state.accepts(&failed) {
                failed = actual_scope.clone();
            }
            let mut recursive = job.recursive;
            if directories.contains(&parent(&failed)) && !directories.contains(&failed) {
                let kind: Option<String> = state
                    .db
                    .query_row("SELECT kind FROM entries WHERE path=?", [&failed], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if kind.as_deref().is_some_and(|kind| !is_directory(kind)) {
                    failed = parent(&failed);
                    recursive = false;
                }
            }
            let attempt = job.attempts.saturating_add(1);
            let delay = 2f64.powi(attempt.min(9) as i32).min(300.0);
            let due = now() + delay;
            state.queue(&failed, &job.volume_key, recursive, job.baseline, due, true)?;
            state.db.execute("UPDATE jobs SET attempts=MAX(attempts, ?), next_attempt=MAX(next_attempt, ?), error=? WHERE path=?", params![attempt, due, error.error, failed])?;
            state.metric("errors", 1)?;
            eprintln!("Reconciliation will retry {failed}: {}", error.error);
        }
        state.drain_deferred()?;
        state.refresh_baseline()?;
        transaction.commit()?;
        Ok(true)
    }
    pub fn next_wakeup(&self) -> Result<Option<f64>> {
        let state = self.lock()?;
        if state.policy.is_none() || state.get("needs_revalidation", false)? {
            return Ok(None);
        }
        if state
            .db
            .query_row("SELECT 1 FROM deferred_jobs WHERE volume_key NOT IN (SELECT key FROM inactive_volumes) LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some()
        {
            return Ok(Some(0.0));
        }
        Ok(state
            .db
            .query_row("SELECT MIN(next_attempt) FROM jobs WHERE volume_key NOT IN (SELECT key FROM inactive_volumes)", [], |r| r.get(0))?)
    }
    pub fn status(&self) -> Result<Value> {
        let state = self.lock()?;
        // A newly created file, including one whose schema transaction is not
        // committed yet, is not an initialized index. Do not suppress errors
        // for any existing schema objects or for an unreadable database.
        if self.read_only
            && !state
                .db
                .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master)", [], |row| {
                    row.get::<_, bool>(0)
                })?
        {
            return Ok(json!({"indexed": false}));
        }
        let mut result = serde_json::Map::new();
        for key in [
            "baseline_walks",
            "subtree_scans",
            "shallow_scans",
            "errors",
            "renamed",
            "events_received",
            "excluded_events",
            "ignored_content_events",
            "queue_overflows",
            "recovery_requests",
        ] {
            result.insert(key.into(), json!(0));
        }
        for row in state
            .db
            .prepare("SELECT key, value FROM metrics")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        {
            let (key, value) = row?;
            result.insert(key, json!(value));
        }
        result.insert(
            "baseline_complete".into(),
            json!(state.get("baseline_complete", false)?),
        );
        result.insert(
            "needs_revalidation".into(),
            json!(state.get("needs_revalidation", false)?),
        );
        result.insert(
            "pending_baseline_roots".into(),
            json!(
                state
                    .get("pending_baseline_roots", BTreeMap::<String, String>::new())?
                    .keys()
                    .collect::<Vec<_>>()
            ),
        );
        for (key, table) in [
            ("pending_jobs", "jobs"),
            ("indexed_entries", "entries"),
            ("indexed_directories", "directories"),
            ("deferred_jobs", "deferred_jobs"),
        ] {
            result.insert(
                key.into(),
                json!(
                    state
                        .db
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                            .get::<_, i64>(0))?
                ),
            );
        }
        result.insert(
            "pending_jobs".into(),
            json!(
                result["pending_jobs"].as_i64().unwrap()
                    + result["deferred_jobs"].as_i64().unwrap()
            ),
        );
        let next: Option<f64> = state.db.query_row(
            "SELECT MIN(next_attempt) FROM jobs WHERE next_attempt>0",
            [],
            |r| r.get(0),
        )?;
        result.insert("next_retry".into(), json!(next));
        // Queue-pressure deferrals and fresh delayed jobs have no failed scan
        // attempts. Sample only saved failures, including retries already due.
        let retry_count: i64 = state.db.query_row(
            "SELECT COUNT(*) FROM jobs WHERE attempts>0 AND error IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let retry_items = state.db.prepare(
            "SELECT path, error, attempts, next_attempt FROM jobs WHERE attempts>0 AND error IS NOT NULL ORDER BY next_attempt, path LIMIT 8",
        )?.query_map([], |r| Ok(json!({
            "path": r.get::<_, String>(0)?, "reason": r.get::<_, String>(1)?,
            "attempts": r.get::<_, i64>(2)?, "next_retry": r.get::<_, f64>(3)?
        })))?.collect::<rusqlite::Result<Vec<_>>>()?;
        result.insert("directory_retry_count".into(), json!(retry_count));
        result.insert("directory_retry_items".into(), json!(retry_items));
        let mut cursors = BTreeMap::new();
        for row in state
            .db
            .prepare("SELECT key, cursor FROM volumes")?
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
        {
            let (key, cursor) = row?;
            cursors.insert(key, cursor.map(|v| v.parse::<u64>()).transpose()?);
        }
        result.insert("cursors".into(), json!(cursors));
        Ok(Value::Object(result))
    }
}

impl State {
    fn get<T: DeserializeOwned>(&self, key: &str, default: T) -> Result<T> {
        let value: Option<String> = self
            .db
            .query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0))
            .optional()?;
        match value {
            Some(value) => Ok(serde_json::from_str(&value)?),
            None => Ok(default),
        }
    }
    fn set(&self, key: &str, value: &impl Serialize) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO meta VALUES (?, ?)",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }
    fn metric(&self, key: &str, amount: i64) -> Result<()> {
        self.db.execute("INSERT INTO metrics VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value=value+excluded.value", params![key, amount])?;
        Ok(())
    }
    fn volumes(&self) -> Result<BTreeMap<String, Volume>> {
        let mut volumes = BTreeMap::new();
        for row in self
            .db
            .prepare("SELECT key, uuid, mount, roots FROM volumes")?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
        {
            let (key, uuid, mount, roots) = row?;
            volumes.insert(
                key.clone(),
                Volume {
                    key,
                    uuid,
                    device: 0,
                    mount,
                    roots: serde_json::from_str(&roots)?,
                },
            );
        }
        Ok(volumes)
    }
    fn cursor(&self, key: &str) -> Result<Option<u64>> {
        let row: Option<Option<String>> = self
            .db
            .query_row("SELECT cursor FROM volumes WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?;
        let row = row.with_context(|| format!("Unknown volume: {key}"))?;
        Ok(row.map(|v| v.parse()).transpose()?)
    }
    fn volume_roots(&self, key: &str) -> Result<Vec<String>> {
        let row: Option<String> = self
            .db
            .query_row("SELECT roots FROM volumes WHERE key=?", [key], |r| r.get(0))
            .optional()?;
        Ok(serde_json::from_str(
            &row.with_context(|| format!("Unknown volume: {key}"))?,
        )?)
    }
    fn volume_for(&self, path: &str) -> Result<Option<String>> {
        let mut best: Option<(usize, String)> = None;
        for row in self
            .db
            .prepare("SELECT key, roots FROM volumes")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        {
            let (key, roots) = row?;
            for root in serde_json::from_str::<Vec<String>>(&roots)? {
                let candidate = (root.chars().count(), key.clone());
                if within(path, &root) && best.as_ref().is_none_or(|b| &candidate > b) {
                    best = Some(candidate);
                }
            }
        }
        Ok(best.map(|(_, key)| key))
    }
    fn accepts(&self, path: &str) -> bool {
        self.policy
            .as_ref()
            .is_some_and(|policy| policy.accepts_lexically(path))
    }
    fn known_directory(&self, path: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT 1 FROM directories WHERE path=?", [path], |_| Ok(()))
            .optional()?
            .is_some())
    }
    fn refresh_baseline(&self) -> Result<()> {
        let mut unfinished = !self
            .get("pending_baseline_roots", BTreeMap::<String, String>::new())?
            .is_empty();
        for table in ["jobs", "deferred_jobs"] {
            unfinished |= self
                .db
                .query_row(
                    &format!("SELECT 1 FROM {table} WHERE baseline=1 LIMIT 1"),
                    [],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
        }
        self.set("baseline_complete", &!unfinished)
    }
    fn mark_revalidation(&self, key: &str) -> Result<()> {
        let mut keys: BTreeSet<String> = self
            .get("revalidation_keys", Vec::<String>::new())?
            .into_iter()
            .collect();
        keys.insert(key.into());
        self.set("revalidation_keys", &keys)?;
        self.set("needs_revalidation", &true)
    }
    fn defer(&self, path: &str, key: &str, recursive: bool, baseline: bool) -> Result<()> {
        let rows: Vec<(String, bool)> = self
            .db
            .prepare("SELECT path, recursive FROM deferred_jobs WHERE volume_key=?")?
            .query_map([key], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let (mut path, mut recursive) = (path.to_owned(), recursive);
        if let Some((ancestor, _)) = rows.iter().find(|(p, r)| *r && within(&path, p)) {
            path = ancestor.clone();
            recursive = true;
        }
        self.db.execute("INSERT INTO deferred_jobs VALUES (?, ?, ?, ?) ON CONFLICT(path) DO UPDATE SET recursive=MAX(recursive, excluded.recursive), baseline=MAX(baseline, excluded.baseline)", params![path, key, recursive, baseline])?;
        if recursive {
            for (child, _) in &rows {
                if child != &path && within(child, &path) {
                    self.db
                        .execute("DELETE FROM deferred_jobs WHERE path=?", [child])?;
                }
            }
        }
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM deferred_jobs WHERE volume_key=?",
            [key],
            |r| r.get(0),
        )?;
        if count > MAX_JOBS {
            self.db
                .execute("DELETE FROM deferred_jobs WHERE volume_key=?", [key])?;
            for root in self.volume_roots(key)? {
                self.db.execute(
                    "INSERT INTO deferred_jobs VALUES (?, ?, 1, 0)",
                    params![root, key],
                )?;
            }
            self.metric("queue_overflows", 1)?;
        }
        Ok(())
    }
    fn drain_deferred(&self) -> Result<()> {
        // Deferred intake is bounded per volume, unlike the traversal frontier.
        let rows: Vec<(String, String, bool, bool)> = self
            .db
            .prepare("SELECT path, volume_key, recursive, baseline FROM deferred_jobs")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        self.db.execute("DELETE FROM deferred_jobs", [])?;
        for (path, key, recursive, baseline) in rows {
            self.queue(&path, &key, recursive, baseline, 0.0, false)?;
        }
        Ok(())
    }
    fn queue(
        &self,
        path: &str,
        key: &str,
        recursive: bool,
        baseline: bool,
        due: f64,
        preserve_delay: bool,
    ) -> Result<()> {
        self.queue_intent(
            path,
            key,
            QueueIntent {
                recursive,
                baseline,
                due,
                preserve_delay,
                ordinary_scope: if !baseline && due == 0.0 {
                    if recursive { 2 } else { 1 }
                } else {
                    0
                },
            },
        )
    }
    fn queue_intent(&self, path: &str, key: &str, intent: QueueIntent) -> Result<()> {
        let QueueIntent {
            recursive,
            baseline,
            ordinary_scope,
            due,
            preserve_delay,
        } = intent;
        let key = self.volume_for(path)?.unwrap_or_else(|| key.to_owned());
        if self
            .active_job
            .as_ref()
            .is_some_and(|job| job.recursive && job.volume_key == key && within(path, &job.path))
        {
            return self.defer(path, &key, recursive, baseline);
        }
        let (mut path, mut recursive) = (path.to_owned(), recursive);
        let mut ancestor = path.clone();
        loop {
            let found = self
                .db
                .query_row(
                    "SELECT path, baseline FROM jobs WHERE path=? AND volume_key=? AND recursive=1",
                    params![ancestor, key],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)),
                )
                .optional()?;
            if let Some((found, ancestor_baseline)) = found
                && (found == path || (!(ancestor_baseline && ordinary_scope > 0) && due == 0.0))
            {
                path = found;
                recursive = true;
                break;
            }
            let parent = parent(&ancestor);
            if parent == ancestor {
                break;
            }
            ancestor = parent;
        }
        let (stored_baseline, stored_failed, stored_scope, stored_rowid): (bool, bool, i64, i64) = self
            .db
            .query_row(
                "SELECT baseline, error IS NOT NULL, ordinary_scope, rowid FROM jobs WHERE path=?",
                [&path],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .unwrap_or((false, false, 0, 0));
        // Continuations already have ordinary scheduling through baseline=0.
        // Only overlapping baseline rows need an explicit ordinary scope; this
        // keeps the bounded intake distinct from an on-disk traversal frontier.
        // Incidental rediscovery also leaves failed work in its retry class.
        let ordinary_scope = if preserve_delay && (!stored_baseline || stored_failed) {
            0
        } else {
            ordinary_scope
        };
        self.db.execute("INSERT INTO jobs (path, volume_key, recursive, baseline, next_attempt, ordinary_scope) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(path) DO UPDATE SET recursive=MAX(jobs.recursive, excluded.recursive), baseline=MAX(jobs.baseline, excluded.baseline), generation=jobs.generation+1,
            ordinary_scope=MAX(jobs.ordinary_scope, excluded.ordinary_scope),
            next_attempt=CASE WHEN ? THEN jobs.next_attempt ELSE excluded.next_attempt END", params![path, key, recursive, baseline, due, ordinary_scope, preserve_delay])?;
        if ordinary_scope > 0
            && !preserve_delay
            && (stored_scope == 0 || stored_rowid <= self.startup_rowid)
        {
            // External intake promotes saved work to live FIFO. Incidental
            // rediscovery and coalescing already-live requests retain their age.
            self.move_to_queue_tail(&path)?;
        }
        if recursive {
            let prefix = path.trim_end_matches('/').to_owned() + "/";
            let upper = prefix.clone() + "\u{10ffff}";
            let child_baseline: Option<i64> = self.db.query_row(
                "SELECT MAX(baseline) FROM jobs WHERE volume_key=? AND path>=? AND path<?",
                params![key, prefix, upper],
                |r| r.get(0),
            )?;
            if child_baseline.is_some_and(|v| v != 0) {
                self.db
                    .execute("UPDATE jobs SET baseline=1 WHERE path=?", [&path])?;
            }
            self.db.execute(
                "DELETE FROM jobs WHERE volume_key=? AND path>=? AND path<? AND baseline=1 AND ordinary_scope=0 AND error IS NULL",
                params![key, prefix, upper],
            )?;
        }
        Ok(())
    }
    fn move_to_queue_tail(&self, path: &str) -> Result<()> {
        let current: i64 =
            self.db
                .query_row("SELECT rowid FROM jobs WHERE path=?", [path], |row| {
                    row.get(0)
                })?;
        let tail: i64 = self
            .db
            .query_row("SELECT MAX(rowid) FROM jobs", [], |row| row.get(0))?;
        let high_water: i64 = self.get(QUEUE_ROWID_HIGH_WATER, 0)?;
        let floor = self.startup_rowid.max(high_water);
        let tail = tail.max(floor);
        let allocated = if current != tail || current <= floor {
            let next = tail
                .checked_add(1)
                .context("Ordinary queue order exhausted")?;
            // All callers run inside the existing write transaction. Failure
            // leaves the incoming cursor, pending generations and jobs intact.
            self.db
                .execute("UPDATE jobs SET rowid=? WHERE path=?", params![next, path])?;
            next
        } else {
            current
        };
        if allocated > high_water {
            self.set(QUEUE_ROWID_HIGH_WATER, &allocated)?;
        }
        Ok(())
    }
    fn bound_queue(&self, key: &str) -> Result<()> {
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM jobs WHERE volume_key=? AND (ordinary_scope>0 OR (recursive=0 AND baseline=0 AND error IS NULL))",
            [key],
            |r| r.get(0),
        )?;
        if count > MAX_JOBS {
            // Collapse bounded intake into recursive root reconciliation. Queue
            // coalescing transfers baseline obligations and retains failed jobs.
            self.db.execute("DELETE FROM jobs WHERE volume_key=? AND recursive=0 AND baseline=0 AND error IS NULL", [key])?;
            self.db
                .execute("UPDATE jobs SET ordinary_scope=0 WHERE volume_key=?", [key])?;
            for root in self.volume_roots(key)? {
                self.queue(&root, key, true, false, 0.0, true)?;
            }
            self.metric("queue_overflows", 1)?;
        }
        Ok(())
    }
    fn prune(&self, path: &str, include_self: bool, preserve: &[String]) -> Result<()> {
        let prefix = path.trim_end_matches('/').to_owned() + "/";
        let mut condition = "(path>=? AND path<?".to_owned();
        let mut values = vec![
            SqlValue::Text(prefix.clone()),
            SqlValue::Text(prefix + "\u{10ffff}"),
        ];
        if include_self {
            condition += " OR path=?";
            values.push(SqlValue::Text(path.into()));
        }
        condition += ")";
        for root in preserve {
            let prefix = root.trim_end_matches('/').to_owned() + "/";
            condition += " AND NOT (path=? OR (path>=? AND path<?))";
            values.extend([
                SqlValue::Text(root.clone()),
                SqlValue::Text(prefix.clone()),
                SqlValue::Text(prefix + "\u{10ffff}"),
            ]);
        }
        for table in ["entries", "directories"] {
            self.db.execute(
                &format!("DELETE FROM {table} WHERE {condition}"),
                params_from_iter(values.iter()),
            )?;
        }
        Ok(())
    }
    fn queue_child(&self, job: &Job, child: &str) -> Result<()> {
        if self.accepts(child) && (job.scans_recursively() || !self.known_directory(child)?) {
            self.queue(
                child,
                &job.volume_key,
                true,
                job.scans_baseline(),
                0.0,
                true,
            )?;
        }
        Ok(())
    }
    fn clear_scan(&self, scope: &str) -> Result<()> {
        self.db
            .execute("DELETE FROM scan_seen WHERE scope=?", [scope])?;
        self.db
            .execute("DELETE FROM scan_runs WHERE path=?", [scope])?;
        Ok(())
    }
    fn protected_roots(&self, job: &Job, missing_roots: &BTreeSet<String>) -> Result<Vec<String>> {
        let mut protected = Vec::new();
        for volume in self.volumes()?.into_values() {
            if volume.key != job.volume_key {
                for root in volume.roots {
                    if root != job.path && within(&root, &job.path) {
                        if missing_roots.contains(&root) {
                            self.prune(&root, true, &[])?;
                        } else {
                            protected.push(root);
                        }
                    }
                }
            }
        }
        Ok(protected)
    }
    fn stage_scan(
        &self,
        job: &Job,
        result: &ScanResult,
        scan_id: &str,
        missing: &BTreeSet<String>,
    ) -> Result<Job> {
        let existing: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT scan_id,job FROM scan_runs WHERE path=?",
                [&job.path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let started = if let Some((id, saved)) = existing.filter(|(id, _)| id == scan_id) {
            let _ = id;
            serde_json::from_str(&saved)?
        } else {
            self.clear_scan(&job.path)?;
            self.db.execute(
                "INSERT INTO scan_runs VALUES (?,?,?)",
                params![job.path, scan_id, serde_json::to_string(job)?],
            )?;
            job.clone()
        };
        let protected = self.protected_roots(job, missing)?;
        let scope = if result.scope.is_empty() {
            &job.path
        } else {
            &result.scope
        };
        for item in &result.entries {
            if parent(&item.path) != *scope || !self.accepts(&item.path) {
                continue;
            }
            let old: Option<(String, SqlValue, SqlValue)> = self
                .db
                .query_row(
                    "SELECT kind,dev,ino FROM entries WHERE path=?",
                    [&item.path],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if old.is_some_and(|(kind, dev, ino)| {
                is_directory(&kind)
                    && (!is_directory(&item.kind)
                        || dev != sqlite_unsigned(item.dev)
                        || ino != sqlite_unsigned(item.ino))
            }) {
                // A positively observed replacement permits this pruning even
                // before the directory's final absence inference.
                self.prune(&item.path, false, &protected)?;
                self.db
                    .execute("DELETE FROM directories WHERE path=?", [&item.path])?;
            }
            self.db.execute("INSERT OR REPLACE INTO entries(path,parent,kind,dev,ino,mtime_ns,ctime_ns,size,mode) VALUES(?,?,?,?,?,?,?,?,?)", params![item.path,parent(&item.path),item.kind,sqlite_unsigned(item.dev),sqlite_unsigned(item.ino),item.mtime_ns,item.ctime_ns,sqlite_unsigned(item.size),item.mode])?;
            self.db.execute(
                "INSERT OR IGNORE INTO scan_seen VALUES (?,?)",
                params![job.path, item.path],
            )?;
        }
        Ok(started)
    }
    fn finish_scan(
        &self,
        job: &Job,
        result: &ScanResult,
        missing: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        if !result.errors.is_empty() {
            return Ok(BTreeSet::new());
        }
        let protected = self.protected_roots(job, missing)?;
        let directories: BTreeSet<_> = result
            .directories
            .iter()
            .map(|path| absolute(path))
            .collect();
        self.db.execute_batch("CREATE TEMP TABLE IF NOT EXISTS scan_prune_candidates(path TEXT PRIMARY KEY); DELETE FROM scan_prune_candidates;")?;
        for directory in &directories {
            if !self.accepts(directory) {
                continue;
            }
            // Materialize negative evidence in SQLite so iteration is independent
            // of the entries being pruned and RAM does not scale with directory size.
            self.db.execute("INSERT OR IGNORE INTO scan_prune_candidates SELECT path FROM entries WHERE parent=? AND NOT EXISTS(SELECT 1 FROM scan_seen WHERE scope=? AND scan_seen.path=entries.path)", params![directory,job.path])?;
            self.db
                .execute("INSERT OR IGNORE INTO directories VALUES (?)", [directory])?;
        }
        for path in self
            .db
            .prepare("SELECT path FROM scan_prune_candidates")?
            .query_map([], |row| row.get::<_, String>(0))?
        {
            self.prune(&path?, true, &protected)?;
        }
        self.db.execute("DELETE FROM scan_prune_candidates", [])?;
        Ok(directories)
    }
    fn replace(
        &self,
        job: &Job,
        result: &ScanResult,
        missing_roots: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>> {
        let directories: BTreeSet<String> =
            result.directories.iter().map(|p| absolute(p)).collect();
        let mut protected = vec![];
        for (key, volume) in self.volumes()? {
            if key == job.volume_key {
                continue;
            }
            for root in volume.roots {
                if root != job.path && within(&root, &job.path) && !directories.contains(&root) {
                    if missing_roots.contains(&root) {
                        self.prune(&root, true, &[])?;
                    } else {
                        protected.push(root);
                    }
                }
            }
        }
        let failed: Vec<String> = result
            .errors
            .iter()
            .map(|e| {
                absolute(if e.path.is_empty() {
                    &job.path
                } else {
                    &e.path
                })
            })
            .collect();
        let entries: Vec<&Entry> = result
            .entries
            .iter()
            .filter(|e| self.accepts(&e.path))
            .collect();
        // Worker observations are always shallow, even for recursive jobs.
        if directories.is_empty() && result.errors.is_empty() {
            self.prune(&job.path, true, &protected)?;
        }
        let mut by_parent: HashMap<String, HashMap<&str, &Entry>> = HashMap::new();
        for item in &entries {
            by_parent
                .entry(parent(&item.path))
                .or_default()
                .insert(&item.path, item);
        }
        for directory in &directories {
            if !self.accepts(directory) {
                continue;
            }
            let children = by_parent.get(directory);
            // Only the current, completely enumerated directory is materialized.
            let old: Vec<(String, String, SqlValue, SqlValue)> = self
                .db
                .prepare("SELECT path, kind, dev, ino FROM entries WHERE parent=?")?
                .query_map([directory], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (path, kind, dev, ino) in old {
                if failed.iter().any(|f| within(f, &path) || within(&path, f)) {
                    continue;
                }
                match children.and_then(|c| c.get(path.as_str())) {
                    None => self.prune(&path, true, &protected)?,
                    Some(child)
                        if is_directory(&kind)
                            && (!is_directory(&child.kind)
                                || dev != sqlite_unsigned(child.dev)
                                || ino != sqlite_unsigned(child.ino)) =>
                    {
                        self.prune(&path, false, &protected)?;
                        self.db
                            .execute("DELETE FROM directories WHERE path=?", [&path])?;
                    }
                    _ => {}
                }
            }
            self.db
                .execute("INSERT OR IGNORE INTO directories VALUES (?)", [directory])?;
        }
        for item in entries {
            if !result.errors.is_empty() && !directories.contains(&parent(&item.path)) {
                continue;
            }
            self.db.execute("INSERT OR REPLACE INTO entries (path, parent, kind, dev, ino, mtime_ns, ctime_ns, size, mode) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)", params![item.path, parent(&item.path), item.kind, sqlite_unsigned(item.dev), sqlite_unsigned(item.ino), item.mtime_ns, item.ctime_ns, sqlite_unsigned(item.size), item.mode])?;
        }
        Ok(directories)
    }
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
