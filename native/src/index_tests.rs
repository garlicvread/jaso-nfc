use super::*;
use serde_json::json;

#[test]
fn configured_baseline_is_staged_and_cursor_survives_device_renumbering() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    let root = temp.path().join("root").to_string_lossy().into_owned();
    std::fs::create_dir(&root)?;
    let mut volume = Volume {
        key: "disk".into(),
        uuid: "uuid".into(),
        device: 1,
        mount: "/".into(),
        roots: vec![root.clone()],
    };
    {
        let index = Index::new(&path, false)?;
        assert!(index.configure("signature", &[volume.clone()], std::slice::from_ref(&root))?);
        assert_eq!(index.status()?["pending_jobs"], 0);
        index.seed_cursor("disk", u64::MAX)?;
        index.seed_cursor("disk", 42)?;
        assert_eq!(index.cursor("disk")?, Some(u64::MAX));
    }
    volume.device = 8;
    let index = Index::new(&path, false)?;
    assert!(index.configure("signature", &[volume], std::slice::from_ref(&root))?);
    assert_eq!(index.cursor("disk")?, Some(u64::MAX));
    assert_eq!(index.status()?["pending_baseline_roots"], json!([root]));
    Ok(())
}

#[test]
fn readonly_status_does_not_initialize_or_mutate_database() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    let index = Index::new(&path, false)?;
    index.configure("signature", &[], &[])?;
    let before = std::fs::metadata(&path)?.modified()?;
    let reader = Index::new(&path, true)?;
    assert_eq!(reader.status()?["baseline_complete"], true);
    assert!(reader.configure("other", &[], &[]).is_err());
    assert_eq!(std::fs::metadata(&path)?.modified()?, before);
    assert!(Index::new(temp.path().join("missing.sqlite"), true).is_err());
    Ok(())
}

#[test]
fn status_samples_failed_directory_jobs_by_deadline_then_path() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    let writer = Index::new(&path, false)?;
    {
        let state = writer.lock()?;
        for n in (0..12).rev() {
            state.db.execute(
                "INSERT INTO jobs (path, volume_key, recursive, attempts, next_attempt, error) VALUES (?, 'disk', 1, 2, ?, 'Permission denied')",
                params![format!("/missing/{n:02}"), (3 - n / 3) as f64],
            )?;
        }
        state.db.execute("INSERT INTO jobs (path, volume_key, recursive, next_attempt) VALUES ('/ordinary', 'disk', 0, 50)", [])?;
        state.db.execute("INSERT INTO deferred_jobs (path, volume_key, recursive) VALUES ('/deferred', 'disk', 1)", [])?;
        state.db.execute(
            "INSERT INTO metrics (key, value) VALUES ('errors', 100)",
            [],
        )?;
    }
    let reader = Index::new(&path, true)?;
    let status = reader.status()?;
    assert_eq!(status["directory_retry_count"], 12);
    assert_eq!(status["pending_jobs"], 14);
    assert_eq!(status["errors"], 100);
    let items = status["directory_retry_items"]
        .as_array()
        .expect("retry sample");
    assert_eq!(items.len(), 8);
    for (n, item) in [9, 10, 11, 6, 7, 8, 3, 4].into_iter().zip(items) {
        assert_eq!(
            item,
            &json!({
                "path": format!("/missing/{n:02}"), "reason": "Permission denied",
                "attempts": 2, "next_retry": (3 - n / 3) as f64
            })
        );
    }
    assert_eq!(
        writer
            .lock()?
            .db
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get::<_, i64>(0))?,
        13
    );
    Ok(())
}

#[test]
fn readonly_status_reports_empty_database_until_initialization_commits() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    std::fs::write(&path, [])?;
    let reader = Index::new(&path, true)?;
    assert_eq!(reader.status()?, json!({"indexed": false}));
    assert_eq!(std::fs::metadata(&path)?.len(), 0);
    let writer = Index::new(&path, false)?;
    assert_eq!(reader.status()?["pending_jobs"], 0);
    assert_eq!(reader.status()?["baseline_complete"], false);
    drop(writer);
    Ok(())
}

#[test]
fn readonly_status_during_uncommitted_schema_does_not_hide_committed_partial_schema() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    let writer = Connection::open(&path)?;
    writer.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
    let reader = Index::new(&path, true)?;
    assert_eq!(reader.status()?, json!({"indexed": false}));
    writer.execute_batch("COMMIT;")?;
    assert!(
        reader.status().is_err(),
        "A committed partial schema is an error"
    );
    Ok(())
}

#[test]
fn schema_initialization_failure_rolls_back_all_new_schema_objects() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    let connection = Connection::open(&path)?;
    // The jobs index cannot be created on this view. Earlier schema statements
    // must not become visible when initialization fails partway through.
    connection.execute_batch("CREATE VIEW jobs AS SELECT 0 AS next_attempt;")?;
    assert!(Index::new(&path, false).is_err());
    let objects: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master ORDER BY name")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    assert_eq!(objects, ["jobs"]);
    Ok(())
}

#[test]
fn readonly_status_preserves_corrupt_database_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("index.sqlite3");
    let corrupt = b"This file is not a SQLite database and must not be treated as an empty index.";
    std::fs::write(&path, corrupt)?;
    assert!(
        Index::new(&path, true)
            .and_then(|index| index.status())
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, corrupt);
    Ok(())
}

#[test]
fn configure_rejects_inconsistent_coverage_without_overwriting_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let index = Index::new(temp.path().join("index.sqlite3"), false)?;
    index.configure("signature", &[], &[])?;
    let volume = Volume {
        key: "disk".into(),
        uuid: "uuid".into(),
        device: 1,
        mount: "/".into(),
        roots: vec!["/one".into()],
    };
    assert!(
        index
            .configure(
                "signature",
                std::slice::from_ref(&volume),
                &["/other".into()]
            )
            .is_err()
    );
    assert!(
        index
            .configure("signature", &[volume.clone(), volume], &["/one".into()])
            .is_err()
    );
    assert_eq!(index.status()?["cursors"], json!({}));
    Ok(())
}

use crate::model::{Entry, Event, PendingRecoveryError, Reconciler, ScanError, ScanResult};
use crate::policy::Policy;
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

thread_local! {
    pub(super) static SCHEDULER_CLOCK: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}
struct SchedulerClock;
impl SchedulerClock {
    fn start() -> Self {
        SCHEDULER_CLOCK.set(Some(std::time::Instant::now()));
        Self
    }
    fn advance(elapsed: Duration) {
        SCHEDULER_CLOCK.set(SCHEDULER_CLOCK.get().map(|time| time + elapsed));
    }
}
impl Drop for SchedulerClock {
    fn drop(&mut self) {
        SCHEDULER_CLOCK.set(None);
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    root: String,
    path: std::path::PathBuf,
    volume: Volume,
    index: Arc<Index>,
    scan: Scanner,
}
struct Scanner {
    policy: Policy,
    results: HashMap<String, ScanResult>,
    calls: Vec<String>,
    retries: Vec<String>,
    during: Option<Box<dyn FnOnce() -> Result<()> + Send>>,
    failure: Option<anyhow::Error>,
}
impl Reconciler for Scanner {
    fn policy(&self) -> &Policy {
        &self.policy
    }
    fn reconcile(&mut self, path: &str, recursive: bool) -> Result<ScanResult> {
        assert!(
            !recursive,
            "worker must persist traversal between shallow scans"
        );
        self.calls.push(path.to_string());
        if let Some(during) = self.during.take() {
            during()?;
        }
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        Ok(self
            .results
            .get(path)
            .cloned()
            .unwrap_or_else(|| ScanResult {
                scope: path.to_owned(),
                directories: vec![path.to_owned()],
                ..Default::default()
            }))
    }
    fn retry_paths(&mut self, _now: f64) -> Result<Vec<String>> {
        Ok(std::mem::take(&mut self.retries))
    }
}
fn policy(roots: Vec<String>) -> Policy {
    Policy::new(roots, vec![], vec![".git".into()], vec![], HashMap::new())
}
fn entry(path: &str, kind: &str, ino: u64) -> Entry {
    Entry {
        path: path.into(),
        kind: kind.into(),
        dev: 1,
        ino,
        mtime_ns: 1,
        ctime_ns: 1,
        size: 1,
        mode: 0o600
            | if kind == "directory" {
                0o40000
            } else if kind == "symlink" {
                0o120000
            } else {
                0o100000
            },
    }
}
fn event(path: &str, id: u64, flags: u32) -> Event {
    Event {
        path: path.into(),
        id,
        flags,
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("root").to_string_lossy().into_owned();
        std::fs::create_dir(&root)?;
        let path = temp.path().join("index.sqlite3");
        let volume = Volume {
            key: "disk".into(),
            uuid: "uuid".into(),
            device: 1,
            mount: "/".into(),
            roots: vec![root.clone()],
        };
        let scan = Scanner {
            policy: policy(vec![root.clone()]),
            results: HashMap::new(),
            calls: vec![],
            retries: vec![],
            during: None,
            failure: None,
        };
        let index = Arc::new(Index::new(&path, false)?);
        index.bind_policy(scan.policy.clone())?;
        index.configure(
            "signature",
            std::slice::from_ref(&volume),
            std::slice::from_ref(&root),
        )?;
        Ok(Self {
            temp,
            root,
            path,
            volume,
            index,
            scan,
        })
    }
    fn observed(&mut self, scope: &str, entries: Vec<Entry>) {
        self.scan.results.insert(
            scope.into(),
            ScanResult {
                scope: scope.into(),
                directories: vec![scope.into()],
                entries,
                ..Default::default()
            },
        );
    }
    fn baseline(&mut self, entries: Vec<Entry>) -> Result<()> {
        self.observed(&self.root.clone(), entries);
        self.index.bootstrap_jobs()?;
        self.drain()?;
        self.scan.calls.clear();
        Ok(())
    }
    fn drain(&mut self) -> Result<()> {
        for _ in 0..100 {
            if !self.index.work(&mut self.scan)? {
                return Ok(());
            }
        }
        anyhow::bail!("work failed to become idle")
    }
    fn paths(&self) -> Result<BTreeSet<String>> {
        let db = Connection::open(&self.path)?;
        Ok(db
            .prepare("SELECT path FROM entries")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    fn reopen(&mut self) -> Result<()> {
        self.index = Arc::new(Index::new(&self.path, false)?);
        self.index.bind_policy(self.scan.policy.clone())?;
        self.index.configure(
            "signature",
            std::slice::from_ref(&self.volume),
            std::slice::from_ref(&self.root),
        )?;
        Ok(())
    }
}

#[test]
fn recursive_frontier_is_durable_and_restart_does_not_rewalk_root() -> Result<()> {
    let mut f = Fixture::new()?;
    let child = format!("{}/child", f.root);
    let deep = format!("{child}/deep");
    let leaf = format!("{deep}/leaf");
    f.observed(&f.root.clone(), vec![entry(&child, "directory", 2)]);
    f.observed(&child, vec![entry(&deep, "directory", 3)]);
    f.observed(&deep, vec![entry(&leaf, "file", 4)]);
    f.index.bootstrap_jobs()?;
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.paths()?, BTreeSet::from([child.clone()]));
    assert_eq!(f.index.status()?["baseline_complete"], false);
    f.reopen()?;
    f.scan.calls.clear();
    f.index.bootstrap_jobs()?;
    f.drain()?;
    assert_eq!(f.scan.calls, [child.clone(), deep.clone()]);
    assert_eq!(f.paths()?, BTreeSet::from([child, deep, leaf]));
    assert_eq!(f.index.status()?["baseline_walks"], 1);
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[test]
fn cursor_and_jobs_rollback_atomically_on_database_failure() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    f.index.seed_cursor("disk", 5)?;
    let db = Connection::open(&f.path)?;
    db.execute_batch("CREATE TRIGGER reject_cursor BEFORE UPDATE OF cursor ON volumes BEGIN SELECT RAISE(ABORT, 'injected cursor failure'); END;")?;
    assert!(
        f.index
            .enqueue("disk", &[event(&format!("{}/a", f.root), 10, 0)])
            .is_err()
    );
    assert_eq!(f.index.cursor("disk")?, Some(5));
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(f.index.status()?["events_received"], 0);
    db.execute_batch("DROP TRIGGER reject_cursor;")?;
    f.index
        .enqueue("disk", &[event(&format!("{}/a", f.root), u64::MAX, 0)])?;
    f.reopen()?;
    assert_eq!(f.index.cursor("disk")?, Some(u64::MAX));
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    Ok(())
}

#[test]
fn event_intake_during_scan_does_not_block_or_lose_generation() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    f.index
        .enqueue("disk", &[event(&format!("{}/a", f.root), 1, 0)])?;
    let index = f.index.clone();
    let changed = format!("{}/b", f.root);
    f.scan.during = Some(Box::new(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(index.enqueue("disk", &[event(&changed, 2, 0)]))
                .ok();
        });
        rx.recv_timeout(std::time::Duration::from_secs(2))??;
        Ok(())
    }));
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(f.index.cursor("disk")?, Some(2));
    Ok(())
}

#[test]
fn recursive_events_become_scoped_followup_without_repeating_baseline() -> Result<()> {
    let mut f = Fixture::new()?;
    f.index.bootstrap_jobs()?;
    let index = f.index.clone();
    let changed = format!("{}/changed", f.root);
    f.scan.during = Some(Box::new(move || {
        index.enqueue("disk", &[event(&changed, 10, 0)])
    }));
    f.index.work(&mut f.scan)?;
    assert_eq!(f.index.status()?["baseline_complete"], true);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    f.drain()?;
    assert_eq!(f.scan.calls, [f.root.clone(), f.root.clone()]);
    assert_eq!(f.index.status()?["baseline_walks"], 1);
    Ok(())
}

#[test]
fn fatal_recovery_does_not_acknowledge_or_discard_arriving_events() -> Result<()> {
    let mut f = Fixture::new()?;
    f.index.bootstrap_jobs()?;
    let index = f.index.clone();
    let changed = format!("{}/changed", f.root);
    f.scan.during = Some(Box::new(move || {
        index.enqueue("disk", &[event(&changed, 10, 0)])
    }));
    f.scan.failure = Some(PendingRecoveryError("identity ambiguous".into()).into());
    let error = f.index.work(&mut f.scan).unwrap_err();
    assert!(error.downcast_ref::<PendingRecoveryError>().is_some());
    assert_eq!(f.index.status()?["baseline_walks"], 0);
    assert_eq!(f.index.status()?["deferred_jobs"], 1);
    f.reopen()?;
    f.drain()?;
    assert_eq!(f.index.cursor("disk")?, Some(10));
    assert_eq!(f.index.status()?["baseline_walks"], 1);
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    Ok(())
}

#[test]
fn strict_metadata_fast_path_only_skips_known_regular_nfc_files() -> Result<()> {
    let mut f = Fixture::new()?;
    let regular = format!("{}/known.txt", f.root);
    let nfd = format!("{}/e\u{301}.txt", f.root);
    let link = format!("{}/link", f.root);
    let fifo = format!("{}/fifo", f.root);
    let unknown_mode = format!("{}/unknown_mode", f.root);
    let mut special = entry(&fifo, "file", 4);
    special.mode = 0o10600;
    f.baseline(vec![
        entry(&regular, "file", 1),
        entry(&nfd, "file", 2),
        entry(&link, "symlink", 3),
        special,
        entry(&unknown_mode, "file", 5),
    ])?;
    Connection::open(&f.path)?
        .execute("UPDATE entries SET mode=NULL WHERE path=?", [&unknown_mode])?;
    let flags = [0x11000, 0x10400, 0x12000, 0x14000, 0x18000, 0x15400];
    for (i, flags) in flags.into_iter().enumerate() {
        f.index
            .enqueue("disk", &[event(&regular, i as u64 + 1, flags)])?;
    }
    assert!(!f.index.work(&mut f.scan)?);
    assert_eq!(f.index.status()?["ignored_content_events"], 6);
    for path in [nfd, link, fifo, unknown_mode, format!("{}/new", f.root)] {
        if path.ends_with("/unknown_mode") {
            Connection::open(&f.path)?
                .execute("UPDATE entries SET mode=NULL WHERE path=?", [&path])?;
        }
        f.index.enqueue("disk", &[event(&path, 9, 0x11000)])?;
        assert!(f.index.work(&mut f.scan)?, "metadata must scan {path}");
    }
    for flags in [
        0x11100, 0x11200, 0x11800, 0x411000, 0x10000, 0x1000, 0x11001, 0x11002,
    ] {
        f.index.enqueue("disk", &[event(&regular, 10, flags)])?;
        assert!(f.index.work(&mut f.scan)?);
        f.drain()?;
    }
    Ok(())
}

#[test]
fn unsigned_directory_identity_stays_exact_and_does_not_prune_children() -> Result<()> {
    let mut f = Fixture::new()?;
    let folder = format!("{}/folder", f.root);
    let child = format!("{folder}/child");
    let ino = u64::MAX - 210;
    f.observed(&folder, vec![entry(&child, "file", 2)]);
    f.baseline(vec![entry(&folder, "directory", ino)])?;
    let db = Connection::open(&f.path)?;
    let value: (String, String) = db.query_row(
        "SELECT ino, typeof(ino) FROM entries WHERE path=?",
        [&folder],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(value, (format!("u:{ino}"), "text".into()));
    f.index
        .enqueue("disk", &[event(&format!("{}/trigger", f.root), 1, 0)])?;
    assert!(f.index.work(&mut f.scan)?);
    assert!(!f.index.work(&mut f.scan)?);
    assert!(f.paths()?.contains(&child));
    f.observed(&f.root.clone(), vec![entry(&folder, "directory", 20)]);
    f.observed(&folder, vec![]);
    f.index.enqueue("disk", &[event(&folder, 2, 0)])?;
    f.drain()?;
    assert!(!f.paths()?.contains(&child));
    Ok(())
}

#[test]
fn failed_scan_preserves_evidence_and_metadata_does_not_reset_retry() -> Result<()> {
    let mut f = Fixture::new()?;
    let file = format!("{}/known", f.root);
    f.baseline(vec![entry(&file, "file", 1)])?;
    f.index.enqueue("disk", &[event(&file, 1, 0)])?;
    f.scan.failure = Some(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
    f.index.work(&mut f.scan)?;
    let deadline = f.index.next_wakeup()?.unwrap();
    assert!(deadline > crate::model::now());
    assert!(f.paths()?.contains(&file));
    f.index.enqueue("disk", &[event(&file, 2, 0x11000)])?;
    assert!(!f.index.work(&mut f.scan)?);
    assert_eq!(f.index.next_wakeup()?, Some(deadline));
    assert_eq!(f.index.status()?["errors"], 1);
    Connection::open(&f.path)?.execute("UPDATE jobs SET next_attempt=0", [])?;
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.index.next_wakeup()?, None);
    Ok(())
}

#[test]
fn failed_subtree_and_independent_nested_root_keep_last_observations() -> Result<()> {
    let mut f = Fixture::new()?;
    let blocked = format!("{}/blocked", f.root);
    let old = format!("{blocked}/old");
    f.observed(&blocked, vec![entry(&old, "file", 2)]);
    f.baseline(vec![entry(&blocked, "directory", 1)])?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            directories: vec![f.root.clone()],
            errors: vec![ScanError {
                path: blocked.clone(),
                error: "denied".into(),
                errno: Some(13),
            }],
            ..Default::default()
        },
    );
    f.index.request_reconcile(None)?;
    f.index.work(&mut f.scan)?;
    assert_eq!(f.paths()?, BTreeSet::from([blocked.clone(), old]));
    let db = Connection::open(&f.path)?;
    assert_eq!(
        db.query_row("SELECT path FROM jobs", [], |r| r.get::<_, String>(0))?,
        blocked
    );
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    Ok(())
}

#[test]
fn revalidated_control_cursor_and_unaffected_volume_state_survive_restart() -> Result<()> {
    let mut f = Fixture::new()?;
    let old = format!("{}/old", f.root);
    f.baseline(vec![entry(&old, "file", 1)])?;
    f.index.seed_cursor("disk", 51)?;
    let second_root = f.temp.path().join("second").to_string_lossy().into_owned();
    std::fs::create_dir(&second_root)?;
    let second = Volume {
        key: "second".into(),
        uuid: "second-uuid".into(),
        device: 2,
        mount: "/".into(),
        roots: vec![second_root.clone()],
    };
    f.scan.policy = policy(vec![f.root.clone(), second_root.clone()]);
    f.index.bind_policy(f.scan.policy.clone())?;
    let volumes = [f.volume.clone(), second];
    let roots = [f.root.clone(), second_root.clone()];
    assert!(f.index.configure("signature", &volumes, &roots)?);
    f.index.bootstrap_jobs()?;
    f.drain()?;
    f.index.enqueue(
        "second",
        &[event(&second_root, 100, 0x40), event("", 200, 0x10)],
    )?;
    assert!(
        f.index
            .work(&mut f.scan)
            .unwrap_err()
            .to_string()
            .contains("revalidat")
    );
    assert!(f.index.configure("signature", &volumes, &roots)?);
    assert_eq!(f.index.cursor("second")?, Some(200));
    assert_eq!(f.index.cursor("disk")?, Some(51));
    f.index = Arc::new(Index::new(&f.path, false)?);
    f.index.bind_policy(f.scan.policy.clone())?;
    assert!(f.index.configure("signature", &volumes, &roots)?);
    f.scan.calls.clear();
    f.index.bootstrap_jobs()?;
    f.drain()?;
    assert_eq!(f.scan.calls, [second_root]);
    assert!(f.paths()?.contains(&old));
    assert_eq!(f.index.cursor("second")?, Some(200));
    Ok(())
}

#[test]
fn nested_jobs_are_rehomed_before_parent_source_removal() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let nested_root = format!("{}/nested", f.root);
    std::fs::create_dir(&nested_root)?;
    let nested = Volume {
        key: "nested".into(),
        uuid: "uuid".into(),
        device: 1,
        mount: "/".into(),
        roots: vec![nested_root.clone()],
    };
    f.index.configure(
        "signature",
        &[f.volume.clone(), nested.clone()],
        &[f.root.clone(), nested_root.clone()],
    )?;
    f.index.bootstrap_jobs()?;
    f.drain()?;
    let file = format!("{nested_root}/new");
    f.observed(&nested_root, vec![entry(&file, "file", 2)]);
    f.index.enqueue("disk", &[event(&file, 2, 0)])?;
    f.index.enqueue("nested", &[event(&file, 3, 0)])?;
    Connection::open(&f.path)?.execute(
        "UPDATE jobs SET volume_key='disk' WHERE path=?",
        [&nested_root],
    )?;
    f.index
        .configure("signature", &[nested], std::slice::from_ref(&nested_root))?;
    f.scan.calls.clear();
    f.drain()?;
    assert_eq!(f.scan.calls, [nested_root]);
    assert!(f.paths()?.contains(&file));
    assert_eq!(f.index.cursor("nested")?, Some(3));
    assert!(f.index.cursor("disk").is_err());
    Ok(())
}

#[test]
fn missing_configured_root_requires_revalidation_and_retains_job() -> Result<()> {
    let mut f = Fixture::new()?;
    f.index.bootstrap_jobs()?;
    std::fs::remove_dir(&f.root)?;
    assert!(
        f.index
            .work(&mut f.scan)
            .unwrap_err()
            .to_string()
            .contains("revalidat")
    );
    assert_eq!(f.index.status()?["needs_revalidation"], true);
    assert_eq!(f.index.status()?["baseline_complete"], false);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert!(f.scan.calls.is_empty());
    Ok(())
}

#[test]
fn missing_scope_schedules_parent_to_remove_canonical_old_entries() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let missing = format!("{}/e\u{301}", f.root);
    f.scan.results.insert(
        missing.clone(),
        ScanResult {
            scope: missing.clone(),
            ..Default::default()
        },
    );
    f.index
        .request_reconcile(Some(std::slice::from_ref(&missing)))?;
    f.drain()?;
    assert_eq!(f.scan.calls, [missing, f.root.clone()]);
    Ok(())
}

#[test]
fn queue_overflow_bounds_events_but_not_recursive_frontier() -> Result<()> {
    let mut f = Fixture::new()?;
    let children: Vec<_> = (0..4100)
        .map(|i| entry(&format!("{}/dir{i}", f.root), "directory", i + 1))
        .collect();
    f.observed(&f.root.clone(), children);
    f.index.bootstrap_jobs()?;
    f.index.work(&mut f.scan)?;
    f.index
        .enqueue("disk", &[event(&format!("{}/changed", f.root), 1, 0)])?;
    assert_eq!(f.index.status()?["pending_jobs"], 4101);
    assert_eq!(f.index.status()?["queue_overflows"], 0);
    // Remove the baseline frontier to isolate a burst of ordinary distinct parents.
    Connection::open(&f.path)?.execute("DELETE FROM jobs", [])?;
    let events: Vec<_> = (0..4200)
        .map(|i| event(&format!("{}/parent{i}/changed", f.root), i, 0))
        .collect();
    f.index.enqueue("disk", &events)?;
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert_eq!(f.index.status()?["queue_overflows"], 1);
    Ok(())
}

#[test]
fn excluded_events_only_advance_cursor_and_explicit_request_is_atomic() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    f.index
        .enqueue("disk", &[event(&format!("{}/.git/index", f.root), 90, 0)])?;
    assert_eq!(f.index.cursor("disk")?, Some(90));
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert!(
        f.index
            .request_reconcile(Some(&[f.root.clone(), "/outside".into()]))
            .is_err()
    );
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    f.index.invalidate_volume("disk")?;
    assert_eq!(f.index.cursor("disk")?, None);
    f.drain()?;
    f.index
        .enqueue("disk", &[event("", u64::MAX, 0x10), event("", 2, 0x8)])?;
    assert_eq!(f.index.cursor("disk")?, Some(2));
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    Ok(())
}

#[test]
fn added_removed_and_replaced_volume_only_reset_affected_coverage() -> Result<()> {
    let mut f = Fixture::new()?;
    let old = format!("{}/old", f.root);
    f.baseline(vec![entry(&old, "file", 1)])?;
    f.index.enqueue("disk", &[event(&old, 51, 0)])?;
    f.scan.failure = Some(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into());
    f.index.work(&mut f.scan)?;
    let retry_before = f.index.status()?["next_retry"].clone();
    let second_root = f.temp.path().join("second").to_string_lossy().into_owned();
    std::fs::create_dir(&second_root)?;
    let mut second = Volume {
        key: "second".into(),
        uuid: "second-uuid".into(),
        device: 2,
        mount: "/".into(),
        roots: vec![second_root.clone()],
    };
    f.scan.policy = policy(vec![f.root.clone(), second_root.clone()]);
    f.index.bind_policy(f.scan.policy.clone())?;
    assert!(f.index.configure(
        "signature",
        &[f.volume.clone(), second.clone()],
        &[f.root.clone(), second_root.clone()]
    )?);
    assert_eq!(f.index.cursor("disk")?, Some(51));
    assert_eq!(f.index.status()?["next_retry"], retry_before);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    let second_file = format!("{second_root}/file");
    f.observed(&second_root, vec![entry(&second_file, "file", 2)]);
    f.index.bootstrap_jobs()?;
    f.drain()?;
    f.index.seed_cursor("second", 72)?;
    assert!(f.paths()?.contains(&second_file));
    second.uuid = "replacement".into();
    assert!(f.index.configure(
        "signature",
        &[f.volume.clone(), second.clone()],
        &[f.root.clone(), second_root.clone()]
    )?);
    assert_eq!(f.index.cursor("disk")?, Some(51));
    assert_eq!(f.index.cursor("second")?, None);
    assert!(!f.paths()?.contains(&second_file));
    assert!(f.paths()?.contains(&old));
    assert_eq!(f.index.status()?["next_retry"], retry_before);
    assert!(
        !f.index
            .configure("signature", &[f.volume.clone()], &[f.root.clone()])?
    );
    assert!(f.index.cursor("second").is_err());
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert!(
        f.index
            .configure("changed-signature", &[f.volume.clone()], &[f.root.clone()])?
    );
    assert_eq!(f.index.cursor("disk")?, None);
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert!(f.paths()?.is_empty());
    Ok(())
}

#[test]
fn independent_nested_root_survives_unvisited_parent_until_known_missing() -> Result<()> {
    let mut f = Fixture::new()?;
    let parent = format!("{}/container", f.root);
    let nested_root = format!("{parent}/nested");
    std::fs::create_dir_all(&nested_root)?;
    f.observed(&parent, vec![]);
    f.baseline(vec![entry(&parent, "directory", 1)])?;
    let nested = Volume {
        key: "nested".into(),
        uuid: "uuid".into(),
        device: 1,
        mount: "/".into(),
        roots: vec![nested_root.clone()],
    };
    let child = format!("{nested_root}/child");
    f.observed(&nested_root, vec![entry(&child, "file", 2)]);
    f.index.configure(
        "signature",
        &[f.volume.clone(), nested],
        &[f.root.clone(), nested_root.clone()],
    )?;
    f.index.bootstrap_jobs()?;
    f.drain()?;
    f.observed(&f.root.clone(), vec![]);
    // Exercise parent pruning only; separately queueing the nested root would
    // correctly require revalidation if it disappears before its FIFO turn.
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.index.work(&mut f.scan)?;
    assert!(f.paths()?.contains(&child));
    // A known-missing independent root has no observation left to protect.
    std::fs::remove_dir(&nested_root)?;
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.index.work(&mut f.scan)?;
    assert!(!f.paths()?.contains(&child));
    Ok(())
}

#[test]
fn directory_removal_prunes_descendants_without_interpreting_sql_wildcards() -> Result<()> {
    let mut f = Fixture::new()?;
    let removed = format!("{}/%_folder", f.root);
    let retained = format!("{}/XXfolder", f.root);
    let removed_child = format!("{removed}/old");
    let retained_child = format!("{retained}/good");
    f.observed(&removed, vec![entry(&removed_child, "file", 3)]);
    f.observed(&retained, vec![entry(&retained_child, "file", 4)]);
    f.baseline(vec![
        entry(&removed, "directory", 1),
        entry(&retained, "directory", 2),
    ])?;
    f.observed(&f.root.clone(), vec![entry(&retained, "directory", 2)]);
    f.index.enqueue("disk", &[event(&removed, 1, 0x20200)])?;
    f.drain()?;
    assert_eq!(f.paths()?, BTreeSet::from([retained, retained_child]));
    Ok(())
}

#[test]
fn canonical_error_scope_retries_failed_child_and_ignores_unenumerated_entries() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let old = format!("{}/e\u{301}", f.root);
    let canonical = format!("{}/é", f.root);
    let failed = format!("{canonical}/blocked");
    let unsafe_child = format!("{failed}/partial");
    let good = format!("{canonical}/good");
    f.scan.results.insert(
        old.clone(),
        ScanResult {
            scope: canonical.clone(),
            directories: vec![canonical],
            entries: vec![entry(&unsafe_child, "file", 1), entry(&good, "file", 2)],
            errors: vec![ScanError {
                path: failed.clone(),
                error: "denied".into(),
                errno: Some(13),
            }],
            ..Default::default()
        },
    );
    f.index.request_reconcile(Some(&[old]))?;
    f.index.work(&mut f.scan)?;
    assert_eq!(f.paths()?, BTreeSet::from([good]));
    let db = Connection::open(&f.path)?;
    assert_eq!(
        db.query_row("SELECT path FROM jobs", [], |r| r.get::<_, String>(0))?,
        failed
    );
    Ok(())
}

#[test]
fn mutation_failure_retries_only_enumerated_parent_and_preserves_delay() -> Result<()> {
    let mut f = Fixture::new()?;
    let file = format!("{}/file", f.root);
    f.baseline(vec![entry(&file, "file", 1)])?;
    let mut observation = f.scan.results[&f.root].clone();
    observation.errors.push(ScanError {
        path: file.clone(),
        error: "busy".into(),
        errno: Some(16),
    });
    f.scan.results.insert(f.root.clone(), observation);
    f.index.request_reconcile(None)?;
    f.index.work(&mut f.scan)?;
    let db = Connection::open(&f.path)?;
    let (path, recursive, attempts): (String, bool, i64) =
        db.query_row("SELECT path, recursive, attempts FROM jobs", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    assert_eq!(path, f.root);
    assert!(!recursive);
    assert_eq!(attempts, 1);
    let deadline = f.index.next_wakeup()?;
    f.scan.retries = vec![f.root.clone()];
    assert!(!f.index.work(&mut f.scan)?);
    assert_eq!(f.index.next_wakeup()?, deadline);
    Ok(())
}

#[test]
fn transient_failure_baseline_retry_does_not_repeat_successful_root() -> Result<()> {
    let mut f = Fixture::new()?;
    let blocked = format!("{}/blocked", f.root);
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            directories: vec![f.root.clone()],
            entries: vec![entry(&blocked, "directory", 1)],
            errors: vec![ScanError {
                path: blocked.clone(),
                error: "denied".into(),
                errno: Some(13),
            }],
            ..Default::default()
        },
    );
    f.index.bootstrap_jobs()?;
    f.index.work(&mut f.scan)?;
    assert_eq!(f.index.status()?["baseline_complete"], false);
    assert!(!f.index.work(&mut f.scan)?);
    Connection::open(&f.path)?.execute("UPDATE jobs SET next_attempt=0", [])?;
    f.drain()?;
    assert_eq!(f.scan.calls, [f.root.clone(), blocked]);
    assert_eq!(f.index.status()?["baseline_walks"], 1);
    assert_eq!(f.index.status()?["subtree_scans"], 1);
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[test]
fn events_and_manual_requests_below_pending_baseline_keep_their_requested_scope() -> Result<()> {
    for manual in [false, true] {
        let mut f = Fixture::new()?;
        f.baseline(vec![])?;
        let ancestor = format!("{}/unvisited-ancestor", f.root);
        let target = format!("{ancestor}/new-folder");
        let file = format!("{target}/changed");
        f.observed(&ancestor, vec![entry(&target, "directory", 10)]);
        f.observed(&target, vec![entry(&file, "file", 11)]);
        {
            let state = f.index.lock()?;
            state.queue(&ancestor, "disk", true, true, 0.0, false)?;
            for n in 0..16 {
                state.queue(
                    &format!("{}/a{n:02}", f.root),
                    "disk",
                    true,
                    true,
                    0.0,
                    false,
                )?;
            }
            state.refresh_baseline()?;
        }
        if manual {
            f.index
                .request_reconcile(Some(std::slice::from_ref(&target)))?;
        } else {
            f.index.enqueue("disk", &[event(&file, 90, 0x10100)])?;
        }
        f.reopen()?;
        for _ in 0..2 {
            assert!(f.index.work(&mut f.scan)?);
        }
        assert!(
            f.scan.calls.contains(&target),
            "requested directory lost its ordinary turn: {:?}",
            f.scan.calls
        );
        assert!(f.paths()?.contains(&file));
        assert_eq!(f.index.status()?["baseline_complete"], false);
        f.reopen()?;
        f.drain()?;
        assert_eq!(f.index.status()?["baseline_complete"], true);
        assert!(f.paths()?.contains(&file));
        assert!(!f.index.work(&mut f.scan)?);
    }
    Ok(())
}

#[test]
fn shallow_event_consumes_only_ordinary_scope_of_the_same_baseline_directory() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let target = format!("{}/target", f.root);
    let child = format!("{target}/known-child");
    f.observed(&target, vec![entry(&child, "directory", 10)]);
    {
        let state = f.index.lock()?;
        state
            .db
            .execute("INSERT INTO directories VALUES (?)", [&child])?;
        state.queue(&target, "disk", true, true, 0.0, false)?;
        state.queue(&format!("{}/a", f.root), "disk", true, true, 0.0, false)?;
    }
    f.index
        .enqueue("disk", &[event(&format!("{target}/changed"), 91, 0x10100)])?;
    for _ in 0..2 {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(f.scan.calls.contains(&target));
    let pending: (bool, bool) = f.index.lock()?.db.query_row(
        "SELECT baseline, recursive FROM jobs WHERE path=?",
        [&target],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        pending,
        (true, true),
        "ordinary scan must preserve unfinished baseline intent"
    );
    assert!(!f.scan.calls.contains(&child));
    f.drain()?;
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[test]
fn baseline_rediscovery_preserves_ordinary_descendants_and_new_generations() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let ancestor = format!("{}/ancestor", f.root);
    let target = format!("{ancestor}/target");
    let file = format!("{target}/changed");
    f.observed(&ancestor, vec![entry(&target, "directory", 1)]);
    f.observed(&target, vec![entry(&file, "file", 2)]);
    f.index.enqueue("disk", &[event(&file, 1, 0x10100)])?;
    {
        let state = f.index.lock()?;
        state.queue(&ancestor, "disk", true, true, 0.0, true)?;
        state.queue(&target, "disk", true, true, 0.0, true)?;
    }
    f.reopen()?;
    // Baseline ancestor gets the first class; target must remain ordinary.
    assert!(f.index.work(&mut f.scan)?);
    let index = f.index.clone();
    let arrived = file.clone();
    f.scan.during = Some(Box::new(move || {
        index.enqueue("disk", &[event(&arrived, 2, 0x10100)])
    }));
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(
        f.index.lock()?.db.query_row(
            "SELECT ordinary_scope FROM jobs WHERE path=?",
            [&target],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(f.index.cursor("disk")?, Some(2));
    f.drain()?;
    assert_eq!(f.scan.calls.iter().filter(|p| *p == &target).count(), 3);
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[test]
fn event_during_recursive_baseline_keeps_a_scoped_ordinary_followup() -> Result<()> {
    let mut f = Fixture::new()?;
    let ancestor = format!("{}/ancestor", f.root);
    let target = format!("{ancestor}/target");
    let file = format!("{target}/changed");
    f.observed(&f.root.clone(), vec![entry(&ancestor, "directory", 1)]);
    f.observed(&ancestor, vec![entry(&target, "directory", 2)]);
    f.observed(&target, vec![entry(&file, "file", 3)]);
    f.index.bootstrap_jobs()?;
    let index = f.index.clone();
    let arrived = file.clone();
    f.scan.during = Some(Box::new(move || {
        index.enqueue("disk", &[event(&arrived, 3, 0x10100)])
    }));
    assert!(f.index.work(&mut f.scan)?);
    f.reopen()?;
    for _ in 0..2 {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(f.paths()?.contains(&file));
    assert_eq!(f.index.cursor("disk")?, Some(3));
    f.drain()?;
    assert_eq!(f.index.status()?["baseline_walks"], 1);
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[test]
fn recursive_ordinary_scope_reaches_known_children_before_baseline_backlog() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let target = format!("{}/target", f.root);
    let child = format!("{target}/known-child");
    let file = format!("{child}/changed");
    f.observed(&target, vec![entry(&child, "directory", 1)]);
    f.observed(&child, vec![entry(&file, "file", 2)]);
    {
        let state = f.index.lock()?;
        state
            .db
            .execute("INSERT INTO directories VALUES (?)", [&child])?;
        state.queue(&target, "disk", true, true, 0.0, false)?;
        for n in 0..16 {
            state.queue(
                &format!("{}/a{n:02}", f.root),
                "disk",
                true,
                true,
                0.0,
                false,
            )?;
        }
    }
    f.index
        .request_reconcile(Some(std::slice::from_ref(&target)))?;
    for _ in 0..4 {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(
        f.paths()?.contains(&file),
        "recursive ordinary request must visit its known child promptly"
    );
    assert_eq!(f.index.status()?["baseline_complete"], false);
    f.drain()?;
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[test]
fn explicit_recursive_requests_compete_with_shallow_work_before_bulk_frontier() -> Result<()> {
    for request in ["created-directory", "must-scan", "manual"] {
        let mut f = Fixture::new()?;
        f.baseline(vec![])?;
        let target = format!("{}/new-folder", f.root);
        let file = format!("{target}/changed");
        f.observed(&f.root.clone(), vec![entry(&target, "directory", 1)]);
        f.observed(&target, vec![entry(&file, "file", 2)]);
        {
            let state = f.index.lock()?;
            // Legacy shallow jobs have no ordinary_scope column value, and
            // share the same ready deadline as this new directory request.
            for n in 0..16 {
                state.db.execute(
                    "INSERT INTO jobs(path, volume_key, recursive) VALUES (?, 'disk', 0)",
                    [format!("{}/longer-existing-cloud-directory-{n:02}", f.root)],
                )?;
            }
        }
        match request {
            "created-directory" => f
                .index
                .enqueue("disk", &[event(&target, 1, CREATED | IS_DIR)])?,
            "must-scan" => f.index.enqueue("disk", &[event(&target, 1, MUST_SCAN)])?,
            _ => f
                .index
                .request_reconcile(Some(std::slice::from_ref(&target)))?,
        }
        for _ in 0..2 {
            assert!(f.index.work(&mut f.scan)?);
        }
        assert!(
            f.paths()?.contains(&file),
            "{request} was demoted behind longer shallow paths only because of its recursive intent: {:?}",
            f.scan.calls
        );
        f.drain()?;
    }
    Ok(())
}

#[test]
fn incidental_ordinary_traversal_does_not_promote_a_failed_baseline_child() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let child = format!("{}/failed", f.root);
    let deadline = now() + 300.0;
    {
        let state = f.index.lock()?;
        state.queue(&child, "disk", true, true, 0.0, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=4, error='failed', next_attempt=? WHERE path=?",
            params![deadline, child],
        )?;
        // A successful ordinary parent's recursive continuation is incidental
        // rediscovery, not a new event or an explicit reconciliation request.
        state.queue(&child, "disk", true, false, 0.0, true)?;
    }
    let saved: (i64, f64, i64) = f.index.lock()?.db.query_row(
        "SELECT ordinary_scope, next_attempt, attempts FROM jobs WHERE path=?",
        [&child],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(saved, (0, deadline, 4));
    assert!(!f.index.work(&mut f.scan)?);
    f.index
        .enqueue("disk", &[event(&format!("{child}/changed"), 1, 0x10100)])?;
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.scan.calls, [child]);
    Ok(())
}

#[test]
fn queue_overflow_finishes_baseline_coverage_and_preserves_failed_deadlines() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let deadline = now() + 300.0;
    let children: Vec<_> = (0..4100).map(|n| format!("{}/child{n}", f.root)).collect();
    f.observed(
        &f.root.clone(),
        children
            .iter()
            .enumerate()
            .map(|(n, path)| entry(path, "directory", n as u64 + 1))
            .collect(),
    );
    {
        let state = f.index.lock()?;
        let transaction = Transaction::new_unchecked(&state.db, TransactionBehavior::Immediate)?;
        for child in &children {
            state.db.execute("INSERT INTO jobs(path, volume_key, recursive, baseline, ordinary_scope) VALUES (?, 'disk', 1, 1, 1)", [child])?;
        }
        state.db.execute(
            "UPDATE jobs SET attempts=4, error='failed', next_attempt=? WHERE path=?",
            params![deadline, format!("{}/child0", f.root)],
        )?;
        transaction.commit()?;
    }
    f.index
        .enqueue("disk", &[event(&format!("{}/changed", f.root), 1, 0x10100)])?;
    assert_eq!(f.index.status()?["queue_overflows"], 1);
    assert!(f.index.work(&mut f.scan)?);
    // A fresh event after root expansion must not mistake its durable frontier
    // for thousands of new intake requests and trigger another overflow.
    f.index.enqueue(
        "disk",
        &[event(&format!("{}/child1/changed", f.root), 2, 0x10100)],
    )?;
    for _ in 0..children.len() * 2 + 4 {
        if !f.index.work(&mut f.scan)? {
            break;
        }
    }
    assert_eq!(f.index.status()?["queue_overflows"], 1);
    assert_eq!(f.index.status()?["baseline_complete"], false);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert!(
        children
            .iter()
            .skip(1)
            .all(|child| f.scan.calls.contains(child))
    );
    assert!(!f.scan.calls.contains(&children[0]));
    assert!(
        f.scan.calls.len() <= children.len() * 2 + 4,
        "frontier must drain finitely"
    );
    assert_eq!(
        f.index.lock()?.db.query_row(
            "SELECT next_attempt FROM jobs WHERE error IS NOT NULL",
            [],
            |r| r.get::<_, f64>(0)
        )?,
        deadline
    );
    f.index
        .lock()?
        .db
        .execute("UPDATE jobs SET next_attempt=0", [])?;
    f.drain()?;
    assert_eq!(f.index.status()?["baseline_complete"], true);
    assert!(f.scan.calls.contains(&children[0]));
    Ok(())
}

#[test]
fn legacy_jobs_migrate_without_losing_baseline_retry_or_deferred_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("legacy.sqlite3");
    drop(Index::new(&path, false)?);
    let connection = Connection::open(&path)?;
    connection.execute_batch("DROP TABLE jobs;
        CREATE TABLE jobs(path TEXT PRIMARY KEY, volume_key TEXT NOT NULL, recursive INTEGER NOT NULL,
        baseline INTEGER NOT NULL DEFAULT 0, generation INTEGER NOT NULL DEFAULT 1,
        attempts INTEGER NOT NULL DEFAULT 0, next_attempt REAL NOT NULL DEFAULT 0, error TEXT);
        INSERT INTO jobs VALUES('/baseline', 'disk', 1, 1, 7, 4, 12345, 'saved failure');
        INSERT INTO deferred_jobs VALUES('/ordinary', 'disk', 0, 0);")?;
    assert_eq!(Index::new(&path, true)?.status()?["pending_jobs"], 2);
    let migrated = Index::new(&path, false)?;
    let row: (bool, i64, i64, f64, String, i64) = migrated.lock()?.db.query_row(
        "SELECT baseline, generation, attempts, next_attempt, error, ordinary_scope FROM jobs WHERE path='/baseline'", [],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    assert_eq!(row, (true, 7, 4, 12345.0, "saved failure".into(), 0));
    assert_eq!(
        migrated.lock()?.db.query_row(
            "SELECT ordinary_scope FROM jobs WHERE path='/ordinary'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(
        migrated
            .lock()?
            .db
            .prepare("PRAGMA table_info(deferred_jobs)")?
            .query_map([], |_| Ok(()))?
            .count(),
        4
    );
    Ok(())
}

#[test]
fn due_event_and_baseline_work_both_receive_finite_service() -> Result<()> {
    let _clock = SchedulerClock::start();
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let ordinary = format!("{}/ordinary", f.root);
    {
        let state = f.index.lock()?;
        for n in 0..16 {
            state.queue(
                &format!("{}/baseline-{n:02}", f.root),
                "disk",
                true,
                true,
                0.0,
                false,
            )?;
        }
    }
    for _ in 0..ORDINARY_BURST_LIMIT * 2 + 8 {
        f.index.enqueue(
            "disk",
            &[event(&format!("{ordinary}/changed"), 1, CREATED | IS_FILE)],
        )?;
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(
        f.scan
            .calls
            .iter()
            .filter(|path| *path == &ordinary)
            .count()
            >= ORDINARY_BURST_LIMIT as usize * 2
    );
    assert!(
        f.scan
            .calls
            .windows(ORDINARY_BURST_LIMIT as usize + 1)
            .all(|window| window.iter().any(|path| path != &ordinary))
    );
    assert_eq!(f.index.status()?["baseline_complete"], false);
    Ok(())
}

#[test]
fn due_failed_work_receives_service_amid_continuous_fresh_jobs() -> Result<()> {
    let _clock = SchedulerClock::start();
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let ordinary = format!("{}/ordinary", f.root);
    let failed = format!("{}/failed", f.root);
    let future = format!("{}/future", f.root);
    let future_deadline = now() + 300.0;
    {
        let state = f.index.lock()?;
        state.queue(&failed, "disk", true, true, now() - 1.0, false)?;
        state.queue(&future, "disk", true, true, future_deadline, false)?;
        state
            .db
            .execute("UPDATE jobs SET attempts=9, error='retry fixture'", [])?;
    }
    f.scan.results.insert(
        failed.clone(),
        ScanResult {
            scope: failed.clone(),
            errors: vec![ScanError {
                path: failed.clone(),
                error: "retry fixture".into(),
                errno: Some(11),
            }],
            ..Default::default()
        },
    );
    for n in 0..ORDINARY_BURST_LIMIT * 2 + 8 {
        {
            let state = f.index.lock()?;
            state.queue(
                &format!("{}/fresh-{n}", f.root),
                "disk",
                true,
                true,
                0.0,
                false,
            )?;
            // Simulate the retry becoming due between turns without sleeps.
            state.db.execute(
                "UPDATE jobs SET next_attempt=? WHERE path=?",
                params![now() - 1.0, failed],
            )?;
        }
        f.index.enqueue(
            "disk",
            &[event(
                &format!("{ordinary}/changed"),
                n as u64 + 1,
                CREATED | IS_FILE,
            )],
        )?;
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(
        f.scan
            .calls
            .iter()
            .filter(|path| *path == &ordinary)
            .count()
            >= ORDINARY_BURST_LIMIT as usize * 2
    );
    for group in f.scan.calls.windows(ORDINARY_BURST_LIMIT as usize + 2) {
        assert!(group.contains(&failed), "failed work starved: {group:?}");
        assert!(
            group.iter().any(|path| path.contains("/fresh-")),
            "baseline work starved: {group:?}"
        );
    }
    assert!(!f.scan.calls.contains(&future));
    assert_eq!(
        f.index.lock()?.db.query_row(
            "SELECT next_attempt FROM jobs WHERE path=?",
            [future],
            |r| r.get::<_, f64>(0)
        )?,
        future_deadline
    );
    Ok(())
}

#[test]
fn ordinary_work_runs_in_a_bounded_burst_before_slow_background_attempts() -> Result<()> {
    let _clock = SchedulerClock::start();
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let failed = format!("{}/failed", f.root);
    let baseline = format!("{}/baseline", f.root);
    {
        let state = f.index.lock()?;
        state.queue(&baseline, "disk", true, true, 0.0, false)?;
        state.queue(&failed, "disk", true, true, 0.0, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=2,error='provider retry' WHERE path=?",
            [&failed],
        )?;
        for n in 0..ORDINARY_BURST_LIMIT + 8 {
            state.queue(
                &format!("{}/ordinary-{n:02}", f.root),
                "disk",
                false,
                false,
                0.0,
                false,
            )?;
        }
    }
    for _ in 0..ORDINARY_BURST_LIMIT {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(
        f.scan.calls.iter().all(|path| path.contains("/ordinary-")),
        "cheap ordinary work was interrupted by background attempts: {:?}",
        f.scan.calls
    );
    for expected in [&failed, &baseline] {
        f.scan.during = Some(Box::new(|| {
            SchedulerClock::advance(Duration::from_secs(15));
            Ok(())
        }));
        assert!(f.index.work(&mut f.scan)?);
        assert_eq!(f.scan.calls.last(), Some(expected));
    }
    // Empty background classes do not delay the rest of the ordinary work.
    f.drain()?;
    assert_eq!(f.scan.calls.len(), ORDINARY_BURST_LIMIT as usize + 10);
    Ok(())
}

#[test]
fn elapsed_ordinary_burst_yields_before_selecting_the_next_job() -> Result<()> {
    let _clock = SchedulerClock::start();
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let failed = format!("{}/failed", f.root);
    let baseline = format!("{}/baseline", f.root);
    {
        let state = f.index.lock()?;
        state.queue(&baseline, "disk", true, true, 0.0, false)?;
        state.queue(&failed, "disk", true, true, 0.0, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=2,error='provider retry' WHERE path=?",
            [&failed],
        )?;
        for n in 0..5 {
            state.queue(
                &format!("{}/ordinary-{n}", f.root),
                "disk",
                false,
                false,
                0.0,
                false,
            )?;
        }
    }
    for elapsed in [Duration::from_millis(500), Duration::from_secs(15)] {
        f.scan.during = Some(Box::new(move || {
            SchedulerClock::advance(elapsed);
            Ok(())
        }));
        assert!(f.index.work(&mut f.scan)?);
        assert!(f.scan.calls.last().unwrap().contains("/ordinary-"));
    }
    for expected in [&failed, &baseline] {
        assert!(f.index.work(&mut f.scan)?);
        assert_eq!(f.scan.calls.last(), Some(expected));
    }
    assert!(f.index.work(&mut f.scan)?);
    assert!(f.scan.calls.last().unwrap().contains("/ordinary-"));
    f.drain()?;
    Ok(())
}

#[test]
fn ordinary_fifo_serves_a_cold_directory_while_a_short_path_keeps_arriving() -> Result<()> {
    for baseline_hot in [false, true] {
        let mut f = Fixture::new()?;
        f.baseline(vec![])?;
        let hot = format!("{}/a", f.root);
        let cold = format!("{}/longer-directory/waiting-for-its-turn", f.root);
        let file = format!("{cold}/changed");
        f.observed(&cold, vec![entry(&file, "file", 1)]);
        if baseline_hot {
            f.index
                .lock()?
                .queue(&hot, "disk", true, true, 0.0, false)?;
        }
        f.index.enqueue(
            "disk",
            &[event(&format!("{hot}/changed"), 1, CREATED | IS_FILE)],
        )?;
        f.index
            .request_reconcile(Some(std::slice::from_ref(&cold)))?;
        for id in 2..=3 {
            let index = f.index.clone();
            let arriving = format!("{hot}/changed");
            f.scan.during = Some(Box::new(move || {
                index.enqueue("disk", &[event(&arriving, id, CREATED | IS_FILE)])
            }));
            assert!(f.index.work(&mut f.scan)?);
        }
        assert_eq!(
            f.scan.calls,
            [hot.clone(), cold],
            "new generations of a hot path must yield their completed turn"
        );
        assert!(f.paths()?.contains(&file));
        assert_eq!(f.index.cursor("disk")?, Some(3));
        f.drain()?;
        assert_eq!(f.index.status()?["baseline_complete"], true);
    }
    Ok(())
}

#[test]
fn ordinary_fifo_preserves_queued_age_and_appends_first_baseline_promotion() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let promoted = format!("{}/a", f.root);
    let waiting = format!("{}/older-long-request", f.root);
    let failed = format!("{}/failed", f.root);
    let due = now() + 300.0;
    {
        let state = f.index.lock()?;
        state.queue(&promoted, "disk", true, true, 0.0, false)?;
        state.queue(&failed, "disk", true, true, due, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=4,error='saved failure' WHERE path=?",
            [&failed],
        )?;
    }
    f.index.enqueue(
        "disk",
        &[event(&format!("{waiting}/one"), 1, CREATED | IS_FILE)],
    )?;
    f.index.enqueue(
        "disk",
        &[event(&format!("{promoted}/one"), 2, CREATED | IS_FILE)],
    )?;
    // Coalescing a second event must not put the oldest request behind the promotion.
    f.index.enqueue(
        "disk",
        &[event(&format!("{waiting}/two"), 3, CREATED | IS_FILE)],
    )?;
    f.reopen()?;
    for _ in 0..2 {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert_eq!(f.scan.calls, [waiting, promoted]);
    assert_eq!(
        f.index.lock()?.db.query_row(
            "SELECT next_attempt,attempts,error FROM jobs WHERE path=?",
            [&failed],
            |r| Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?
            ))
        )?,
        (due, 4, "saved failure".into())
    );
    Ok(())
}

#[test]
fn ordinary_fifo_rowid_exhaustion_rolls_back_intake_without_changing_queued_work() -> Result<()> {
    for promotion in [false, true] {
        let mut f = Fixture::new()?;
        f.baseline(vec![])?;
        let old = format!("{}/old-baseline", f.root);
        {
            let state = f.index.lock()?;
            state.queue(&old, "disk", true, true, 0.0, false)?;
            state.db.execute("INSERT INTO jobs(rowid,path,volume_key,recursive,baseline) VALUES (?,?,'disk',1,1)",params![i64::MAX,format!("{}/last-rowid",f.root)])?;
        }
        let target = if promotion {
            old.clone()
        } else {
            format!("{}/new-request", f.root)
        };
        let error = f
            .index
            .enqueue(
                "disk",
                &[event(&format!("{target}/changed"), 1, CREATED | IS_FILE)],
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("queue order exhausted"),
            "{error:#}"
        );
        let state = f.index.lock()?;
        assert_eq!(
            state
                .db
                .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get::<_, i64>(0))?,
            2
        );
        assert_eq!(
            state.db.query_row(
                "SELECT generation,ordinary_scope FROM jobs WHERE path=?",
                [&old],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )?,
            (1, 0)
        );
        assert_eq!(state.cursor("disk")?, None);
        assert_eq!(
            state
                .db
                .query_row("SELECT MAX(rowid) FROM jobs", [], |r| r.get::<_, i64>(0))?,
            i64::MAX
        );
    }
    Ok(())
}

#[test]
fn live_directory_event_precedes_persisted_backlog_after_reopen() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    {
        let state = f.index.lock()?;
        for n in 0..40 {
            state.queue(
                &format!("{}/old-cloud-{n:02}", f.root),
                "disk",
                false,
                false,
                0.0,
                false,
            )?;
        }
        state.db.execute(
            "INSERT INTO deferred_jobs VALUES (?,'disk',0,0)",
            [format!("{}/old-deferred", f.root)],
        )?;
    }
    f.reopen()?;
    let target = format!("{}/new-directory", f.root);
    let file = format!("{target}/changed");
    f.observed(&f.root.clone(), vec![entry(&target, "directory", 1)]);
    f.observed(&target, vec![entry(&file, "file", 2)]);
    f.index
        .enqueue("disk", &[event(&target, 100, CREATED | IS_DIR)])?;
    for _ in 0..2 {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert!(
        f.paths()?.contains(&file),
        "saved backlog delayed the live directory: {:?}",
        f.scan.calls
    );
    assert!(f.index.status()?["pending_jobs"].as_i64().unwrap() > 20);
    assert!(f.scan.calls.iter().all(|path| !path.contains("/old-")));
    f.drain()?;
    assert!(
        f.scan
            .calls
            .iter()
            .any(|path| path.ends_with("old-deferred"))
    );
    Ok(())
}

#[test]
fn live_promotion_preserves_incidental_age_and_exceeds_a_deleted_startup_tail() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let old = format!("{}/old", f.root);
    let backlog = format!("{}/backlog", f.root);
    let failed = format!("{}/failed", f.root);
    let due = now() + 300.0;
    {
        let state = f.index.lock()?;
        state.queue(&backlog, "disk", false, false, 0.0, false)?;
        state.queue(&old, "disk", true, false, 0.0, false)?;
        state.queue(&old, "disk", true, true, 0.0, true)?;
        state.queue(&failed, "disk", true, true, due, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=4,error='saved failure' WHERE path=?",
            [&failed],
        )?;
        state.db.execute(
            "INSERT INTO jobs(rowid,path,volume_key,recursive) VALUES (1000,?,'disk',0)",
            [format!("{}/deleted-tail", f.root)],
        )?;
    }
    f.reopen()?;
    {
        let state = f.index.lock()?;
        state.db.execute("DELETE FROM jobs WHERE rowid=1000", [])?;
        let before: i64 =
            state
                .db
                .query_row("SELECT rowid FROM jobs WHERE path=?", [&old], |r| r.get(0))?;
        state.queue(&old, "disk", true, false, 0.0, true)?;
        assert_eq!(
            state
                .db
                .query_row("SELECT rowid FROM jobs WHERE path=?", [&old], |r| r
                    .get::<_, i64>(0))?,
            before
        );
    }
    f.index
        .request_reconcile(Some(std::slice::from_ref(&old)))?;
    let promoted: i64 =
        f.index
            .lock()?
            .db
            .query_row("SELECT rowid FROM jobs WHERE path=?", [&old], |r| r.get(0))?;
    assert!(
        promoted > 1000,
        "external request did not move above the startup floor: {promoted}"
    );
    let later = format!("{}/later", f.root);
    f.index.enqueue(
        "disk",
        &[event(&format!("{later}/changed"), 1, CREATED | IS_FILE)],
    )?;
    f.index.enqueue(
        "disk",
        &[event(&format!("{old}/changed"), 2, CREATED | IS_FILE)],
    )?;
    assert_eq!(
        f.index
            .lock()?
            .db
            .query_row("SELECT rowid FROM jobs WHERE path=?", [&old], |r| r
                .get::<_, i64>(0))?,
        promoted,
        "coalescing changed the age of live intake"
    );
    for _ in 0..2 {
        assert!(f.index.work(&mut f.scan)?);
    }
    assert_eq!(f.scan.calls, [old, later]);
    assert_eq!(
        f.index.lock()?.db.query_row(
            "SELECT next_attempt,attempts,error FROM jobs WHERE path=?",
            [&failed],
            |r| Ok((
                r.get::<_, f64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?
            ))
        )?,
        (due, 4, "saved failure".into())
    );
    Ok(())
}

#[test]
fn separate_cli_allocates_live_intake_above_the_running_workers_deleted_floor() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    {
        let state = f.index.lock()?;
        state.queue(
            &format!("{}/old-cloud", f.root),
            "disk",
            false,
            false,
            0.0,
            false,
        )?;
        state.db.execute(
            "INSERT INTO jobs(rowid,path,volume_key,recursive) VALUES (1000,?,'disk',0)",
            [format!("{}/old-tail", f.root)],
        )?;
    }
    f.reopen()?;
    f.index
        .lock()?
        .db
        .execute("DELETE FROM jobs WHERE rowid=1000", [])?;
    let target = format!("{}/manual-request", f.root);
    let file = format!("{target}/changed");
    f.observed(&target, vec![entry(&file, "file", 1)]);
    // This independently opened CLI sees a lower current MAX(rowid), while
    // the existing worker still distinguishes live work above its old floor.
    let client = Index::new(&f.path, false)?;
    client.bind_policy(f.scan.policy.clone())?;
    client.request_reconcile(Some(std::slice::from_ref(&target)))?;
    assert!(f.index.work(&mut f.scan)?);
    assert!(
        f.paths()?.contains(&file),
        "separate CLI request was demoted below saved cloud work: {:?}",
        f.scan.calls
    );
    assert_eq!(f.scan.calls, [target]);
    f.drain()?;
    Ok(())
}

#[test]
fn live_bursts_leave_finite_turns_for_failed_backlog_and_baseline_work() -> Result<()> {
    let _clock = SchedulerClock::start();
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let failed = format!("{}/failed", f.root);
    {
        let state = f.index.lock()?;
        for n in 0..8 {
            state.queue(
                &format!("{}/backlog-{n}", f.root),
                "disk",
                false,
                false,
                0.0,
                false,
            )?;
            state.queue(
                &format!("{}/baseline-{n}", f.root),
                "disk",
                true,
                true,
                0.0,
                false,
            )?;
        }
        state.queue(&failed, "disk", true, true, 0.0, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=2,error='provider retry' WHERE path=?",
            [&failed],
        )?;
    }
    f.reopen()?;
    f.scan.results.insert(
        failed.clone(),
        ScanResult {
            scope: failed.clone(),
            errors: vec![ScanError {
                path: failed.clone(),
                error: "provider retry".into(),
                errno: Some(libc::ETIMEDOUT),
            }],
            ..Default::default()
        },
    );
    let live = format!("{}/live", f.root);
    for id in 0..ORDINARY_BURST_LIMIT * 2 + 8 {
        f.index
            .lock()?
            .db
            .execute("UPDATE jobs SET next_attempt=0 WHERE path=?", [&failed])?;
        f.index.enqueue(
            "disk",
            &[event(
                &format!("{live}/changed"),
                id as u64,
                CREATED | IS_FILE,
            )],
        )?;
        assert!(f.index.work(&mut f.scan)?);
    }
    for window in f.scan.calls.windows(ORDINARY_BURST_LIMIT as usize + 3) {
        assert!(window.contains(&failed), "failed class starved");
        assert!(
            window.iter().any(|path| path.contains("/backlog-")),
            "persisted backlog starved"
        );
        assert!(
            window.iter().any(|path| path.contains("/baseline-")),
            "baseline class starved"
        );
    }
    assert!(
        f.scan.calls.iter().filter(|path| *path == &live).count()
            >= ORDINARY_BURST_LIMIT as usize * 2
    );
    Ok(())
}

#[test]
fn reconciliation_waits_for_the_writer_before_reading_its_transaction_snapshot() -> Result<()> {
    use std::cell::RefCell;
    use std::sync::mpsc;
    thread_local! {
        static WRITER_RELEASE: RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
    }
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (committed_tx, committed_rx) = mpsc::channel();
    let path = f.path.clone();
    let writer = std::thread::spawn(move || -> Result<()> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "BEGIN IMMEDIATE; INSERT INTO metrics (key,value) VALUES ('concurrent_writer',1);",
        )?;
        ready_tx.send(())?;
        release_rx.recv_timeout(Duration::from_secs(5))?;
        connection.execute_batch("COMMIT;")?;
        let _ = committed_tx.send(());
        Ok(())
    });
    ready_rx.recv_timeout(Duration::from_secs(5))?;
    WRITER_RELEASE.with(|slot| *slot.borrow_mut() = Some((release_tx.clone(), committed_rx)));
    f.index.lock()?.db.busy_handler(Some(|_| {
        WRITER_RELEASE.with(|slot| {
            slot.borrow_mut()
                .take()
                .is_some_and(|(release, committed)| {
                    release.send(()).is_ok()
                        && committed.recv_timeout(Duration::from_secs(5)).is_ok()
                })
        })
    }))?;
    let result = f
        .index
        .request_reconcile(Some(std::slice::from_ref(&f.root)));
    let waited = WRITER_RELEASE.with(|slot| slot.borrow().is_none());
    let _ = release_tx.send(()); // Also release the writer when the regression fails.
    writer.join().unwrap()?;
    WRITER_RELEASE.with(|slot| *slot.borrow_mut() = None);
    assert!(
        result.is_ok(),
        "read-to-write promotion must not fail immediately: {result:?}"
    );
    assert!(
        waited,
        "the request must wait before it reads a stale snapshot"
    );
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert_eq!(f.index.status()?["concurrent_writer"], 1);
    Ok(())
}

#[test]
fn directory_retries_with_older_deadlines_run_before_newer_failures() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let older = format!("{}/z-older", f.root);
    let newer = format!("{}/a-newer", f.root);
    {
        let state = f.index.lock()?;
        state.queue(&newer, "disk", true, true, now() - 10.0, false)?;
        state.queue(&older, "disk", true, true, now() - 20.0, false)?;
    }
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.scan.calls, [older]);
    Ok(())
}

#[test]
fn successful_parent_scan_preserves_failed_child_backoff() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let blocked = format!("{}/blocked", f.root);
    let deadline = now() + 300.0;
    f.observed(&f.root.clone(), vec![entry(&blocked, "directory", 1)]);
    {
        let state = f.index.lock()?;
        state.queue(&blocked, "disk", true, true, deadline, false)?;
        state.db.execute(
            "UPDATE jobs SET attempts=9, error='Resource deadlock avoided' WHERE path=?",
            [&blocked],
        )?;
    }
    f.index
        .enqueue("disk", &[event(&format!("{}/sibling", f.root), 1, 0)])?;
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.index.next_wakeup()?, Some(deadline));
    assert!(!f.index.work(&mut f.scan)?);
    Ok(())
}

#[test]
fn unknown_failure_propagates_and_configured_file_replacement_requires_revalidation() -> Result<()>
{
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    f.index.request_reconcile(None)?;
    f.scan.failure = Some(anyhow::anyhow!("invalid reconciler result"));
    assert!(
        f.index
            .work(&mut f.scan)
            .unwrap_err()
            .to_string()
            .contains("invalid reconciler")
    );
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert_eq!(f.index.status()?["subtree_scans"], 0);
    std::fs::remove_dir(&f.root)?;
    std::fs::write(&f.root, "replacement")?;
    assert!(
        f.index
            .work(&mut f.scan)
            .unwrap_err()
            .to_string()
            .contains("revalidat")
    );
    assert_eq!(f.index.status()?["needs_revalidation"], true);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    Ok(())
}

#[test]
fn no_available_roots_is_complete_idle_and_reconnection_stages_only_new_roots() -> Result<()> {
    let mut f = Fixture::new()?;
    let old = format!("{}/old", f.root);
    f.baseline(vec![entry(&old, "file", 1)])?;
    f.index.enqueue("disk", &[event(&old, 51, 0)])?;
    assert!(!f.index.configure("signature", &[], &[])?);
    assert_eq!(f.index.status()?["baseline_complete"], true);
    assert_eq!(f.index.status()?["cursors"], json!({}));
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert!(f.paths()?.is_empty());
    f.index.bootstrap_jobs()?;
    assert!(!f.index.work(&mut f.scan)?);
    assert!(!f.index.configure("signature", &[], &[])?);
    assert!(
        f.index
            .configure("signature", &[f.volume.clone()], &[f.root.clone()])?
    );
    assert_eq!(f.index.cursor("disk")?, None);
    Ok(())
}

#[test]
fn event_intake_and_observation_commit_never_probe_filesystem_policy() -> Result<()> {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::policy::FORBID_FILESYSTEM.set(false);
        }
    }
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let directory = format!("{}/child", f.root);
    std::fs::create_dir(&directory)?;
    f.observed(&f.root.clone(), vec![entry(&directory, "directory", 1)]);
    let _restore = Restore;
    crate::policy::FORBID_FILESYSTEM.set(true);
    f.index
        .enqueue("disk", &[event(&directory, 81, CREATED | IS_DIR)])?;
    assert_eq!(f.index.cursor("disk")?, Some(81));
    assert!(f.index.work(&mut f.scan)?);
    assert!(f.paths()?.contains(&directory));
    Ok(())
}

#[test]
fn dormant_nested_root_is_not_pruned_by_its_available_parent() -> Result<()> {
    let mut f = Fixture::new()?;
    let nested_root = format!("{}/nested", f.root);
    std::fs::create_dir(&nested_root)?;
    f.baseline(vec![])?;
    let nested = Volume {
        key: "nested".into(),
        uuid: "nested-uuid".into(),
        device: 1,
        mount: "/".into(),
        roots: vec![nested_root.clone()],
    };
    let roots = vec![f.root.clone(), nested_root.clone()];
    f.index.bind_policy(policy(roots.clone()))?;
    f.index
        .configure("signature", &[f.volume.clone(), nested], &roots)?;
    let child = format!("{nested_root}/saved");
    f.observed(&nested_root, vec![entry(&child, "file", 17)]);
    f.index.bootstrap_jobs()?;
    f.drain()?;
    f.index
        .configure_available("signature", &[f.volume.clone()], &roots)?;
    std::fs::remove_dir(&nested_root)?;
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.index.work(&mut f.scan)?;
    assert!(
        f.paths()?.contains(&child),
        "a missing dormant root is not proof its files were deleted"
    );
    let saved = f.index.status()?;
    let reopened = Index::new(&f.path, false)?;
    reopened.bind_policy(policy(vec![f.root.clone()]))?;
    assert_eq!(reopened.status()?["cursors"], saved["cursors"]);
    Ok(())
}

#[test]
fn partial_observation_preserves_unseen_entries_and_finishes_against_all_chunks() -> Result<()> {
    let mut f = Fixture::new()?;
    let old = format!("{}/old", f.root);
    let a = format!("{}/a", f.root);
    let b = format!("{}/b", f.root);
    f.baseline(vec![entry(&old, "file", 1)])?;
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&a, "file", 2)],
            scan_id: Some("scan-one".into()),
            complete: false,
            ..Default::default()
        },
    );
    assert!(f.index.work(&mut f.scan)?);
    assert!(
        f.paths()?.contains(&old),
        "partial listing cannot prove absence"
    );
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&b, "file", 3)],
            directories: vec![f.root.clone()],
            scan_id: Some("scan-one".into()),
            ..Default::default()
        },
    );
    assert!(f.index.work(&mut f.scan)?);
    assert_eq!(f.paths()?, BTreeSet::from([a, b]));
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    Ok(())
}

#[test]
fn an_event_between_scan_chunks_retains_its_own_generation() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            scan_id: Some("scan-one".into()),
            complete: false,
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    f.index.enqueue(
        "disk",
        &[event(&format!("{}/late", f.root), 123, CREATED | IS_FILE)],
    )?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            directories: vec![f.root.clone()],
            scan_id: Some("scan-one".into()),
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    assert_eq!(
        f.index.status()?["pending_jobs"],
        1,
        "finishing an older snapshot cannot acknowledge a later event"
    );
    Ok(())
}

#[test]
fn policy_reset_discards_partial_scan_receipts() -> Result<()> {
    let mut f = Fixture::new()?;
    f.index.bootstrap_jobs()?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&format!("{}/old", f.root), "file", 2)],
            scan_id: Some("before-reset".into()),
            complete: false,
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    f.index
        .configure("changed-policy", &[f.volume.clone()], &[f.root.clone()])?;
    let state = f.index.lock()?;
    assert_eq!(
        state
            .db
            .query_row("SELECT COUNT(*) FROM scan_runs", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(
        state
            .db
            .query_row("SELECT COUNT(*) FROM scan_seen", [], |r| r.get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn partial_parent_observation_preserves_missing_active_nested_root() -> Result<()> {
    let mut f = Fixture::new()?;
    f.baseline(vec![])?;
    let nested_root = format!("{}/nested", f.root);
    std::fs::create_dir(&nested_root)?;
    let nested = Volume {
        key: "nested".into(),
        uuid: "nested-uuid".into(),
        device: 1,
        mount: "/".into(),
        roots: vec![nested_root.clone()],
    };
    let roots = vec![f.root.clone(), nested_root.clone()];
    f.index.bind_policy(policy(roots.clone()))?;
    f.index
        .configure("signature", &[f.volume.clone(), nested], &roots)?;
    let child = format!("{nested_root}/saved");
    f.observed(&nested_root, vec![entry(&child, "file", 17)]);
    f.index.bootstrap_jobs()?;
    f.drain()?;
    std::fs::remove_dir(&nested_root)?;
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            scan_id: Some("parent-partial".into()),
            complete: false,
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    assert!(
        f.paths()?.contains(&child),
        "partial work does not collect or act on negative nested-root evidence"
    );
    Ok(())
}

#[test]
fn failed_partial_scan_and_restart_never_reuse_absence_evidence() -> Result<()> {
    let mut f = Fixture::new()?;
    let old = format!("{}/old", f.root);
    let before_error = format!("{}/before-error", f.root);
    let before_restart = format!("{}/before-restart", f.root);
    let final_file = format!("{}/final", f.root);
    f.baseline(vec![entry(&old, "file", 1)])?;
    f.index.request_reconcile(Some(&[f.root.clone()]))?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&before_error, "file", 2)],
            scan_id: Some("failed".into()),
            complete: false,
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            errors: vec![ScanError {
                path: f.root.clone(),
                error: "interrupted read".into(),
                errno: Some(libc::EINTR),
            }],
            scan_id: Some("failed".into()),
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    assert_eq!(
        f.paths()?,
        BTreeSet::from([old.clone(), before_error.clone()])
    );
    assert_eq!(
        f.index
            .lock()?
            .db
            .query_row("SELECT COUNT(*) FROM scan_seen", [], |r| r.get::<_, i64>(0))?,
        0
    );
    assert_eq!(f.index.status()?["directory_retry_count"], 1);
    f.index
        .lock()?
        .db
        .execute("UPDATE jobs SET next_attempt=0", [])?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&before_restart, "file", 3)],
            scan_id: Some("crashed".into()),
            complete: false,
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    f.reopen()?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&final_file, "file", 4)],
            directories: vec![f.root.clone()],
            scan_id: Some("restarted".into()),
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    assert_eq!(f.paths()?, BTreeSet::from([final_file]));
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    Ok(())
}

#[test]
fn baseline_children_from_earlier_chunks_remain_in_durable_frontier() -> Result<()> {
    let mut f = Fixture::new()?;
    let child = format!("{}/child", f.root);
    let leaf = format!("{child}/leaf");
    f.index.bootstrap_jobs()?;
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            entries: vec![entry(&child, "directory", 2)],
            scan_id: Some("baseline".into()),
            complete: false,
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    assert_eq!(f.index.status()?["baseline_complete"], false);
    f.scan.results.insert(
        f.root.clone(),
        ScanResult {
            scope: f.root.clone(),
            directories: vec![f.root.clone()],
            scan_id: Some("baseline".into()),
            ..Default::default()
        },
    );
    f.index.work(&mut f.scan)?;
    assert_eq!(f.index.status()?["baseline_complete"], false);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    f.reopen()?;
    f.observed(&child, vec![entry(&leaf, "file", 3)]);
    f.drain()?;
    assert!(f.paths()?.contains(&leaf));
    assert_eq!(f.index.status()?["baseline_complete"], true);
    Ok(())
}

#[cfg(feature = "normalizer")]
#[test]
fn a_real_wide_directory_yields_to_a_fresh_directory_event() -> Result<()> {
    let mut f = Fixture::new()?;
    let wide = format!("{}/wide", f.root);
    let fresh = format!("{}/fresh", f.root);
    std::fs::create_dir(&wide)?;
    std::fs::create_dir(&fresh)?;
    f.baseline(vec![])?;
    for n in 0..600 {
        std::fs::write(format!("{wide}/{n:04}"), "fixture")?;
    }
    let fresh_file = format!("{fresh}/fresh-file");
    std::fs::write(&fresh_file, "fixture")?;
    let mut normalizer =
        crate::normalizer::Normalizer::new(f.scan.policy.clone(), None, None, None, false)?;
    f.index
        .request_reconcile(Some(std::slice::from_ref(&wide)))?;
    assert!(f.index.work(&mut normalizer)?);
    assert_eq!(f.index.status()?["pending_jobs"], 1);
    assert_eq!(
        normalizer.active_scans().as_slice(),
        std::slice::from_ref(&wide)
    );
    f.index
        .enqueue("disk", &[event(&fresh_file, 500, CREATED | IS_FILE)])?;
    for _ in 0..4 {
        if f.paths()?.contains(&fresh_file) {
            break;
        }
        assert!(f.index.work(&mut normalizer)?);
    }
    assert!(f.paths()?.contains(&fresh_file));
    assert!(
        normalizer.active_scans().contains(&wide),
        "fresh work finishes while the wide snapshot remains incomplete"
    );
    for _ in 0..40 {
        if !f.index.work(&mut normalizer)? {
            break;
        }
    }
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(
        f.paths()?
            .iter()
            .filter(|path| parent(path) == wide)
            .count(),
        600
    );
    Ok(())
}

#[cfg(all(feature = "normalizer", target_os = "macos"))]
fn wide_alias_fixture() -> Result<(Fixture, crate::normalizer::Normalizer, String, String)> {
    let mut f = Fixture::new()?;
    let stored = format!("{}/café", f.root);
    let alias = format!("{}/cafe\u{301}", f.root);
    std::fs::create_dir(&stored)?;
    for n in 0..600 {
        std::fs::write(format!("{stored}/{n:04}"), "fixture")?;
    }
    f.baseline(vec![])?;
    let normalizer =
        crate::normalizer::Normalizer::new(f.scan.policy.clone(), None, None, None, false)?;
    f.index
        .request_reconcile(Some(std::slice::from_ref(&alias)))?;
    Ok((f, normalizer, stored, alias))
}

#[cfg(all(feature = "normalizer", target_os = "macos"))]
fn saved_scan_id(f: &Fixture, request: &str) -> Result<Option<String>> {
    Ok(f.index
        .lock()?
        .db
        .query_row(
            "SELECT scan_id FROM scan_runs WHERE path=?",
            [request],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(all(feature = "normalizer", target_os = "macos"))]
#[test]
fn a_real_wide_directory_queued_with_an_alias_finishes() -> Result<()> {
    let (f, mut normalizer, stored, alias) = wide_alias_fixture()?;
    assert!(f.index.work(&mut normalizer)?);
    let first = saved_scan_id(&f, &alias)?.unwrap();
    assert!(f.index.work(&mut normalizer)?);
    assert_eq!(
        Some(first),
        saved_scan_id(&f, &alias)?,
        "a resolved spelling must retain its snapshot"
    );
    for _ in 0..40 {
        if !f.index.work(&mut normalizer)? {
            break;
        }
    }
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(f.paths()?.len(), 600);
    assert!(f.paths()?.iter().all(|path| parent(path) == stored));
    Ok(())
}

#[cfg(all(feature = "normalizer", target_os = "macos"))]
#[test]
fn a_real_wide_alias_scan_preserves_an_event_between_chunks() -> Result<()> {
    let (f, mut normalizer, stored, alias) = wide_alias_fixture()?;
    assert!(f.index.work(&mut normalizer)?);
    let first = saved_scan_id(&f, &alias)?;
    std::fs::write(format!("{stored}/late"), "late event")?;
    f.index.enqueue(
        "disk",
        &[event(&format!("{alias}/late"), 123, CREATED | IS_FILE)],
    )?;
    assert!(f.index.work(&mut normalizer)?);
    assert_eq!(first, saved_scan_id(&f, &alias)?);
    for _ in 0..40 {
        if saved_scan_id(&f, &alias)?.is_none() {
            break;
        }
        assert!(f.index.work(&mut normalizer)?);
    }
    assert!(saved_scan_id(&f, &alias)?.is_none());
    assert_eq!(
        f.index.status()?["pending_jobs"],
        1,
        "the older snapshot cannot acknowledge the newer event"
    );
    for _ in 0..40 {
        if !f.index.work(&mut normalizer)? {
            break;
        }
    }
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(f.paths()?.len(), 601);
    assert!(f.paths()?.contains(&format!("{stored}/late")));
    Ok(())
}

#[cfg(all(feature = "normalizer", target_os = "macos"))]
#[test]
fn a_real_wide_alias_scan_restarts_after_process_restart() -> Result<()> {
    let (mut f, mut normalizer, _, alias) = wide_alias_fixture()?;
    assert!(f.index.work(&mut normalizer)?);
    let first = saved_scan_id(&f, &alias)?;
    drop(normalizer);
    f.reopen()?;
    let mut normalizer =
        crate::normalizer::Normalizer::new(f.scan.policy.clone(), None, None, None, false)?;
    assert!(f.index.work(&mut normalizer)?);
    let restarted = saved_scan_id(&f, &alias)?;
    assert_ne!(first, restarted);
    assert!(f.index.work(&mut normalizer)?);
    assert_eq!(restarted, saved_scan_id(&f, &alias)?);
    for _ in 0..40 {
        if !f.index.work(&mut normalizer)? {
            break;
        }
    }
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(f.paths()?.len(), 600);
    Ok(())
}

#[cfg(all(feature = "normalizer", target_os = "macos"))]
#[test]
fn a_real_wide_alias_scan_restarts_after_index_reset() -> Result<()> {
    let (f, mut normalizer, _, alias) = wide_alias_fixture()?;
    assert!(f.index.work(&mut normalizer)?);
    let first = saved_scan_id(&f, &alias)?;
    f.index.configure(
        "reset-signature",
        std::slice::from_ref(&f.volume),
        std::slice::from_ref(&f.root),
    )?;
    assert!(saved_scan_id(&f, &alias)?.is_none());
    f.index
        .request_reconcile(Some(std::slice::from_ref(&alias)))?;
    assert!(f.index.work(&mut normalizer)?);
    let restarted = saved_scan_id(&f, &alias)?;
    assert_ne!(first, restarted);
    assert!(f.index.work(&mut normalizer)?);
    assert_eq!(restarted, saved_scan_id(&f, &alias)?);
    for _ in 0..40 {
        if !f.index.work(&mut normalizer)? {
            break;
        }
    }
    assert_eq!(f.index.status()?["pending_jobs"], 0);
    assert_eq!(f.paths()?.len(), 600);
    Ok(())
}
