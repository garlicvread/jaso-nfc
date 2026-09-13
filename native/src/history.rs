//! Rebuildable disk-backed history. Journals remain authoritative; a page never
//! loads all operations into memory. Restore requests are consumed by the worker.
use crate::{
    config::Config,
    journal::{self, JournalLocks},
    native_names::validate_name,
};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryQuery {
    pub limit: usize,
    pub offset: usize,
    pub search: String,
    pub date: Option<String>,
    pub result: Option<String>,
}
impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            search: String::new(),
            date: None,
            result: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: String,
    pub revision: String,
    pub old_path: String,
    pub new_path: String,
    pub old_name: String,
    pub new_name: String,
    pub timestamp: String,
    pub result: String,
    pub kind: String,
    pub restored: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    pub total: u64,
    pub today_count: u64,
    pub limit: usize,
    pub offset: usize,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct RestorePreview {
    pub can_restore: bool,
    pub reason: String,
    pub allowed: bool,
    pub message: String,
    pub item: HistoryItem,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    pub request_id: String,
    pub operation_id: String,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreResult {
    pub request_id: String,
    pub operation_id: String,
    pub state: String,
    pub message: String,
}
fn log_path(config: &Config) -> PathBuf {
    Path::new(&config.logs()).join("renames.jsonl")
}
fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn timezone_signature() -> String {
    let time = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&time, tm.as_mut_ptr()) }.is_null() {
        return String::new();
    }
    let tm = unsafe { tm.assume_init() };
    let zone = if tm.tm_zone.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(tm.tm_zone) }
            .to_string_lossy()
            .into_owned()
    };
    format!(
        "{}:{}:{}",
        zone,
        tm.tm_gmtoff,
        std::env::var("TZ").unwrap_or_default()
    )
}
fn local_day() -> String {
    local_date(unsafe { libc::time(std::ptr::null_mut()) })
}
fn local_date(time: libc::time_t) -> String {
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let mut out = [0i8; 32];
    if unsafe { libc::localtime_r(&time, tm.as_mut_ptr()) }.is_null() {
        return String::new();
    }
    unsafe {
        libc::strftime(
            out.as_mut_ptr(),
            out.len(),
            c"%Y-%m-%d".as_ptr(),
            tm.as_ptr(),
        );
        CStr::from_ptr(out.as_ptr()).to_string_lossy().into_owned()
    }
}
// Existing journals use local ISO timestamps without offsets. Offset-bearing
// imported records are converted to the machine's local calendar day.
fn timestamp_day(ts: &str) -> String {
    if ts.len() < 19 {
        return String::new();
    }
    let Some(date) = ts.get(..10) else {
        return String::new();
    };
    if !ts.is_ascii() {
        return String::new();
    }
    let tail = &ts[19..];
    let offset_start = tail.find(['+', '-', 'Z']);
    let Some(start) = offset_start else {
        return date.into();
    };
    let zone = &tail[start..];
    let offset = if zone == "Z" {
        0
    } else {
        let parts: Vec<_> = zone[1..].split(':').collect();
        if parts.len() != 2 {
            return String::new();
        }
        let (Ok(hours), Ok(minutes)) = (parts[0].parse::<i64>(), parts[1].parse::<i64>()) else {
            return String::new();
        };
        if hours > 23 || minutes > 59 {
            return String::new();
        }
        (hours * 3600 + minutes * 60) * if zone.starts_with('-') { -1 } else { 1 }
    };
    let Ok(raw) = CString::new(&ts[..19]) else {
        return String::new();
    };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::strptime(raw.as_ptr(), c"%Y-%m-%dT%H:%M:%S".as_ptr(), &mut tm) }.is_null() {
        return String::new();
    }
    local_date(unsafe { libc::timegm(&mut tm) } - offset as libc::time_t)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Source {
    path: PathBuf,
    dev: u64,
    ino: u64,
    size: u64,
    modified: i64,
}
fn sources(config: &Config) -> Result<Vec<Source>> {
    let log = log_path(config);
    let mut paths = if log.exists()
        || journal::suffix(&log, ".history").is_dir()
        || journal::suffix(&log, ".recovery.jsonl").exists()
    {
        journal::record_sources(&log)?
    } else {
        vec![]
    };
    // Diagnostics are bounded by the existing journal retention policy.
    let errors = log.with_extension("errors.jsonl");
    let mut diagnostics = vec![];
    for n in (1..=3).rev() {
        let p = journal::suffix(&errors, &format!(".{n}"));
        if p.exists() {
            diagnostics.push(p);
        }
    }
    if errors.exists() {
        diagnostics.push(errors);
    }
    diagnostics.append(&mut paths);
    diagnostics
        .into_iter()
        .map(|path| {
            let m = fs::metadata(&path)?;
            Ok(Source {
                path,
                dev: m.dev(),
                ino: m.ino(),
                size: m.len(),
                modified: m
                    .mtime()
                    .saturating_mul(1_000_000_000)
                    .saturating_add(m.mtime_nsec()),
            })
        })
        .collect()
}
fn stream(source: &Source, offset: u64, mut visit: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let mut file = File::open(&source.path)?;
    let reader: Box<dyn Read> = if source.path.extension().is_some_and(|e| e == "gz") {
        ensure!(offset == 0, "compressed history cannot be resumed");
        Box::new(GzDecoder::new(file))
    } else {
        file.seek(SeekFrom::Start(offset))?;
        Box::new(file.take(source.size.saturating_sub(offset)))
    };
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        ensure!(
            n <= 1024 * 1024,
            "history record exceeds the safe read limit"
        );
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        visit(
            serde_json::from_slice(&line)
                .with_context(|| format!("invalid history in {}", source.path.display()))?,
        )?;
    }
    Ok(())
}
fn index(config: &Config) -> Result<Connection> {
    crate::control::prepare_directories(config)?;
    let log = log_path(config);
    let _locks = JournalLocks::acquire(&[&log])?;
    let mut db = Connection::open(config.state_path("history.sqlite3"))?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; CREATE TABLE IF NOT EXISTS history_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;
    let events_kind: Option<String> = db
        .query_row(
            "SELECT type FROM sqlite_master WHERE name='history_events'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let old_operations: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='history_operations') AND (SELECT COUNT(*) FROM pragma_table_info('history_operations') WHERE name IN ('visible','event_seq','event_record'))<>3", [], |row| row.get(0))?;
    let migrated_schema = events_kind.as_deref() == Some("table") || old_operations;
    let schema = db.transaction()?;
    if migrated_schema {
        // Cache schema changes are atomic; even a partially upgraded older
        // cache is rebuilt entirely from its authoritative journals.
        if events_kind.as_deref() == Some("table") {
            schema.execute_batch("DROP TABLE history_events;")?;
        } else {
            schema.execute_batch("DROP VIEW IF EXISTS history_events;")?;
        }
        schema
            .execute_batch("DROP TABLE IF EXISTS history_operations; DELETE FROM history_meta;")?;
    }
    schema.execute_batch("CREATE TABLE IF NOT EXISTS history_operations(id TEXT PRIMARY KEY,record TEXT NOT NULL,seq INTEGER NOT NULL,confirmed INTEGER NOT NULL,day TEXT NOT NULL,restored INTEGER NOT NULL DEFAULT 0,undo_request TEXT,visible INTEGER NOT NULL,event_seq INTEGER NOT NULL,event_record TEXT); CREATE INDEX IF NOT EXISTS history_order ON history_operations(seq DESC); CREATE INDEX IF NOT EXISTS history_days ON history_operations(confirmed,day); CREATE INDEX IF NOT EXISTS history_event_order ON history_operations(event_seq); CREATE VIEW IF NOT EXISTS history_events AS SELECT id,COALESCE(event_record,record) AS record,event_seq AS seq FROM history_operations; CREATE TABLE IF NOT EXISTS history_restores(original_id TEXT PRIMARY KEY,request_id TEXT NOT NULL);")?;
    schema.commit()?;
    let current = sources(config)?;
    let prior: Vec<Source> = db
        .query_row(
            "SELECT value FROM history_meta WHERE key='sources'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let signature = format!("v4:{}", timezone_signature());
    let previous_signature: Option<String> = db
        .query_row(
            "SELECT value FROM history_meta WHERE key='schema-timezone'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let same_schema = previous_signature.as_deref() == Some(&signature);
    if current == prior && same_schema {
        return Ok(db);
    }
    let errors = log.with_extension("errors.jsonl");
    let incremental = same_schema
        && current.len() == prior.len()
        && current.iter().zip(&prior).all(|(a, b)| {
            a == b
                || (a.path == b.path
                    && (a.path == log || a.path == errors)
                    && a.dev == b.dev
                    && a.ino == b.ino
                    && a.size > b.size)
        });
    let tx = db.transaction()?;
    if !incremental {
        tx.execute_batch("DELETE FROM history_operations; DELETE FROM history_restores;")?;
    }
    let mut seq: i64 =
        tx.query_row("SELECT COALESCE(MAX(seq),0) FROM history_events", [], |r| {
            r.get(0)
        })?;
    for (i, source) in current.iter().enumerate() {
        let offset = if incremental {
            if source == &prior[i] {
                continue;
            }
            prior[i].size
        } else {
            0
        };
        stream(source, offset, |record| {
            seq += 1;
            let status = record["status"].as_str().unwrap_or("");
            if !matches!(status, "renamed" | "reverted" | "error")
                || record["dir"].as_str().is_none()
                || record["old"].as_str().is_none()
                || record["new"].as_str().is_none()
            {
                return Ok(());
            }
            let raw = serde_json::to_string(&record)?;
            let id = record["operation_id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("legacy-{}", digest(&raw)));
            if status == "reverted"
                && let (Some(original), Some(request)) = (
                    record["restores_operation_id"].as_str(),
                    record["operation_id"].as_str(),
                )
            {
                tx.execute("INSERT INTO history_restores VALUES(?1,?2) ON CONFLICT(original_id) DO UPDATE SET request_id=excluded.request_id",params![original,request])?;
            }
            let visible =
                i64::from(status != "reverted" || !record["restores_operation_id"].is_string());
            let confirmed = i64::from(status == "renamed");
            let day = timestamp_day(record["ts"].as_str().unwrap_or(""));
            // Usually the event is the confirmed payload itself. A later
            // diagnostic needs only the path/identity projection for restore
            // checks, without replacing or duplicating the confirmed payload.
            let event = serde_json::to_string(
                &serde_json::json!({"dir":record["dir"],"old":record["old"],"new":record["new"],"status":record["status"],"identity":record["identity"],"recovery_required":record["recovery_required"]}),
            )?;
            // Recovery diagnostics can follow committed records; confirmation is
            // monotonic and only confirmed records can replace a confirmed fact.
            tx.execute("INSERT INTO history_operations(id,record,seq,confirmed,day,visible,event_seq) VALUES(?1,?2,?3,?4,?5,?6,?3) ON CONFLICT(id) DO UPDATE SET record=CASE WHEN excluded.confirmed>=confirmed THEN excluded.record ELSE record END,seq=CASE WHEN excluded.confirmed>=confirmed THEN excluded.seq ELSE seq END,day=CASE WHEN confirmed=0 AND excluded.confirmed=1 THEN excluded.day ELSE day END,confirmed=MAX(confirmed,excluded.confirmed),visible=CASE WHEN excluded.confirmed>=confirmed THEN excluded.visible ELSE visible END,event_seq=excluded.event_seq,event_record=CASE WHEN excluded.confirmed>=confirmed THEN NULL ELSE ?7 END",params![id,raw,seq,confirmed,day,visible,event])?;
            Ok(())
        })?;
    }
    tx.execute("UPDATE history_operations SET restored=EXISTS(SELECT 1 FROM history_restores WHERE original_id=history_operations.id),undo_request=(SELECT request_id FROM history_restores WHERE original_id=history_operations.id)",[])?;
    tx.execute("INSERT INTO history_meta VALUES('sources',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&current)?])?;
    tx.execute("INSERT INTO history_meta VALUES('schema-timezone',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[signature])?;
    tx.commit()?;
    if migrated_schema {
        db.execute_batch("VACUUM")?;
    }
    Ok(db)
}
fn item(id: String, raw: String, seq: i64, restored: bool) -> Result<HistoryItem> {
    let record: Value = serde_json::from_str(&raw)?;
    let old = record["old"].as_str().unwrap_or("");
    let new = record["new"].as_str().unwrap_or("");
    let dir = record["dir"].as_str().unwrap_or("");
    Ok(HistoryItem {
        id,
        revision: digest(format!("{seq}:{restored}:{raw}")),
        old_path: Path::new(dir).join(old).to_string_lossy().into(),
        new_path: Path::new(dir).join(new).to_string_lossy().into(),
        old_name: old.into(),
        new_name: new.into(),
        timestamp: record["ts"].as_str().unwrap_or("").into(),
        result: if restored {
            "restored"
        } else if record["status"] == "renamed" {
            "renamed"
        } else if record["status"] == "reverted" {
            "reverted"
        } else if record["recovery_required"] == true {
            "recovery_required"
        } else {
            "failed"
        }
        .into(),
        kind: record["type"].as_str().unwrap_or("unknown").into(),
        restored,
    })
}
fn today_from(db: &Connection) -> Result<u64> {
    Ok(db.query_row(
        "SELECT COUNT(*) FROM history_operations WHERE confirmed=1 AND day=?1",
        [local_day()],
        |r| r.get::<_, i64>(0),
    )? as u64)
}
pub fn today_count(config: &Config) -> Result<u64> {
    today_from(&index(config)?)
}
/// Storage maintenance runs only while the caller owns the runtime lock.
pub fn maintain(config: &Config) -> Result<Value> {
    // Validate every deletion surface before either retention pass can run.
    // Checking each owned intermediate directory prevents traversal through a
    // mailbox/state symlink, while normal ancestor aliases remain usable.
    for directory in [
        PathBuf::from(&config.state_dir),
        PathBuf::from(config.logs()),
        config.state_path(""),
        config.state_path("history-requests"),
        config.state_path("history-requests/completed"),
    ] {
        journal::retention_path(&directory, true)?;
    }
    for file in [
        config.state_path("history.sqlite3"),
        config.state_path("pending.json"),
        journal::suffix(&log_path(config), ".lock"),
        journal::suffix(&config.state_path("history-requests"), ".lock"),
    ] {
        journal::retention_path(&file, false)?;
    }
    journal::validate_retention_journal(&log_path(config))?;
    crate::control::prepare_directories(config)?;
    let path = log_path(config);
    let cache = config.state_path("history.sqlite3");
    let before = fs::metadata(&cache).map_or(0, |m| m.len());
    let archives = {
        let _lock = JournalLocks::acquire(&[&path])?;
        let mailbox = config.state_path("history-requests");
        let _mailbox_lock = JournalLocks::acquire(&[&mailbox])?;
        let mut protected = BTreeSet::new();
        fn collect(value: &Value, ids: &mut BTreeSet<String>) {
            match value {
                Value::Object(fields) => {
                    for (key, value) in fields {
                        if matches!(
                            key.as_str(),
                            "operation_id" | "request_id" | "restores_operation_id"
                        ) && let Some(id) = value.as_str()
                        {
                            ids.insert(id.to_owned());
                        }
                        collect(value, ids);
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        collect(value, ids);
                    }
                }
                _ => {}
            }
        }
        let pending = config.state_path("pending.json");
        if pending.exists() {
            collect(&read_small::<Value>(&pending)?, &mut protected);
        }
        if mailbox.is_dir() {
            for entry in fs::read_dir(&mailbox)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".request.json")
                {
                    collect(&read_small::<Value>(&entry.path())?, &mut protected);
                }
            }
        }
        journal::maintain_archives(&path, &protected)?
    };
    let completed_requests_removed = prune_completed_requests(config)?;
    let db = index(config)?;
    let free_pages: i64 = db.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    if free_pages >= 256 {
        db.execute_batch("VACUUM")?;
    }
    drop(db);
    Ok(
        serde_json::json!({"archives_removed":archives.archives_removed,"recovery_records_retained":archives.recovery_records_retained,"completed_requests_removed":completed_requests_removed,"cache_bytes_before":before,"cache_bytes_after":fs::metadata(cache)?.len()}),
    )
}
/// A status-only read: never refreshes journals or waits for the rename lock.
/// Absence or a concurrent cache writer means the count is unknown for now.
pub fn cached_today_count(config: &Config) -> Result<Option<u64>> {
    let path = config.state_path("history.sqlite3");
    if !path.exists() {
        return Ok(None);
    }
    let read = (|| -> Result<Option<u64>> {
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(std::time::Duration::ZERO)?;
        db.execute_batch("PRAGMA cache_size=-2048;")?;
        let signature: Option<String> = db
            .query_row(
                "SELECT value FROM history_meta WHERE key='schema-timezone'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if signature.as_deref() != Some(&format!("v4:{}", timezone_signature())) {
            return Ok(None);
        }
        Ok(Some(today_from(&db)?))
    })();
    match read {
        Err(error)
            if error
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| {
                    matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        )
                    )
                }) =>
        {
            Ok(None)
        }
        value => value,
    }
}
pub fn list(config: &Config, query: &HistoryQuery) -> Result<HistoryPage> {
    ensure!(query.search.len() <= 4096, "history search is too long");
    if let Some(date) = &query.date {
        ensure!(
            date.len() == 10
                && date.as_bytes()[4] == b'-'
                && date.as_bytes()[7] == b'-'
                && date
                    .bytes()
                    .enumerate()
                    .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()),
            "history date must be YYYY-MM-DD"
        );
    }
    if let Some(result) = &query.result {
        ensure!(
            matches!(
                result.as_str(),
                "renamed" | "restored" | "reverted" | "failed" | "recovery_required"
            ),
            "invalid history result filter"
        );
    }
    let db = index(config)?;
    let limit = query.limit.clamp(1, 100);
    let filter = "WHERE visible=1 AND (?1='' OR instr(lower(json_extract(record,'$.dir') || '/' || json_extract(record,'$.old') || '/' || json_extract(record,'$.new')),lower(?1))>0) AND (?2 IS NULL OR day=?2) AND (?3 IS NULL OR CASE WHEN restored=1 THEN 'restored' WHEN json_extract(record,'$.status')='renamed' THEN 'renamed' WHEN json_extract(record,'$.status')='reverted' THEN 'reverted' WHEN json_extract(record,'$.recovery_required')=1 THEN 'recovery_required' ELSE 'failed' END=?3)";
    let total = db.query_row(
        &format!("SELECT COUNT(*) FROM history_operations {filter}"),
        params![query.search, query.date, query.result],
        |r| r.get::<_, i64>(0),
    )? as u64;
    let mut statement=db.prepare(&format!("SELECT id,record,seq,restored FROM history_operations {filter} ORDER BY seq DESC,id LIMIT ?4 OFFSET ?5"))?;
    let rows = statement.query_map(
        params![
            query.search,
            query.date,
            query.result,
            limit as i64,
            query.offset.min(i64::MAX as usize) as i64
        ],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut items = Vec::with_capacity(limit);
    for row in rows {
        let (id, raw, seq, restored) = row?;
        items.push(item(id, raw, seq, restored)?);
    }
    Ok(HistoryPage {
        items,
        total,
        today_count: today_from(&db)?,
        limit,
        offset: query.offset,
    })
}
fn load(db: &Connection, id: &str) -> Result<(HistoryItem, Value, i64)> {
    let (raw, seq, restored): (String, i64, bool) = db
        .query_row(
            "SELECT record,seq,restored FROM history_operations WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .context("history operation was not found")?;
    Ok((
        item(id.into(), raw.clone(), seq, restored)?,
        serde_json::from_str(&raw)?,
        seq,
    ))
}
fn related(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}
fn refusal(
    db: &Connection,
    item: &HistoryItem,
    record: &Value,
    seq: i64,
    revision: &str,
    config: &Config,
) -> Result<Option<String>> {
    let _materialization = crate::native_names::DirectoryMaterialization::deny()?;
    let reject = |s: &str| Ok(Some(s.into()));
    if !config.apply {
        return reject("Turn on filename changes before restoring an item.");
    }
    if item.revision != revision {
        return reject("History changed. Refresh this operation before restoring.");
    }
    if item.restored {
        return reject("This operation has already been restored.");
    }
    if record["history_retention_incomplete"] == true {
        return reject(
            "The intervening history has expired. This recovery record cannot be safely restored.",
        );
    }
    if item.result != "renamed" || record["operation_id"].as_str().is_none() {
        return reject("This record has no confirmed, identifiable rename to restore.");
    }
    if config.state_path("pending.json").exists() {
        return reject("A pending operation must finish recovery before restoring.");
    }
    let dir = Path::new(record["dir"].as_str().unwrap_or(""));
    if !dir.is_absolute()
        || validate_name(&item.old_name).is_err()
        || validate_name(&item.new_name).is_err()
    {
        return reject("The recorded paths are not safe directory entry names.");
    }
    if record["identity_finalization_unavailable"] == true
        || record["marker"].is_object()
        || record["rename_mode"] == "provider"
        || record["provider"].is_string()
        || item.new_path.contains("/Library/CloudStorage/")
        || item.new_path.contains("/Library/Mobile Documents/")
    {
        return reject("The cloud provider identity cannot be verified for a safe restore.");
    }
    let Some(ids) = record["identity"]
        .as_array()
        .filter(|ids| ids.len() == 2 && ids.iter().all(Value::is_u64))
    else {
        return reject("This record has no verified file identity.");
    };
    let metadata = match fs::symlink_metadata(&item.new_path) {
        Ok(m) => m,
        Err(_) => return reject("The renamed item is no longer at its recorded path."),
    };
    if Some(metadata.dev()) != ids[0].as_u64() || Some(metadata.ino()) != ids[1].as_u64() {
        return reject("Another item occupies the recorded path.");
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        if metadata.st_flags() & 0x40000000 != 0 {
            return reject("Download and verify this cloud item before restoring its name.");
        }
    }
    match fs::symlink_metadata(&item.old_path) {
        Ok(m) if m.dev() == metadata.dev() && m.ino() == metadata.ino() => {}
        Ok(_) => return reject("The original name is occupied by another item."),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return reject("The original destination cannot be verified."),
    };
    let mut statement = db.prepare("SELECT record FROM history_events WHERE seq>?1 AND id<>?2")?;
    let records = statement.query_map(params![seq, item.id], |r| r.get::<_, String>(0))?;
    for raw in records {
        let later: Value = serde_json::from_str(&raw?)?;
        if later["status"] != "renamed"
            && later["status"] != "reverted"
            && later["recovery_required"] != true
        {
            continue;
        }
        let later_dir = Path::new(later["dir"].as_str().unwrap_or(""));
        let paths = [
            later_dir.join(later["old"].as_str().unwrap_or("")),
            later_dir.join(later["new"].as_str().unwrap_or("")),
        ];
        if later["identity"] == record["identity"]
            || paths.iter().any(|p| {
                related(p, Path::new(&item.new_path)) || related(p, Path::new(&item.old_path))
            })
        {
            return reject(
                "A later rename depends on this item or directory. Restore is unavailable.",
            );
        }
    }
    Ok(None)
}
pub fn preview(config: &Config, id: &str, revision: &str) -> Result<RestorePreview> {
    let db = index(config)?;
    let (item, record, seq) = load(&db, id)?;
    let reason = refusal(&db, &item, &record, seq, revision, config)?;
    let allowed = reason.is_none();
    let message = reason
        .clone()
        .unwrap_or_else(|| "The original name is available and the recorded item matches.".into());
    Ok(RestorePreview {
        allowed,
        message,
        can_restore: reason.is_none(),
        reason: reason.unwrap_or_else(|| {
            "The original name is available and the recorded item matches.".into()
        }),
        item,
    })
}

fn request_path(config: &Config, id: &str, kind: &str) -> Result<PathBuf> {
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && !id.contains('/') && id.len() <= 36,
        "request_id must be a UUID"
    );
    Ok(config
        .state_path("history-requests")
        .join(format!("{id}.{kind}.json")))
}
fn completed_path(path: &Path) -> PathBuf {
    path.parent()
        .unwrap()
        .join("completed")
        .join(path.file_name().unwrap())
}
fn saved_path(config: &Config, id: &str, kind: &str) -> Result<PathBuf> {
    let path = request_path(config, id, kind)?;
    Ok(if path.exists() {
        path
    } else {
        completed_path(&path)
    })
}
fn archive_request(config: &Config, id: &str) -> Result<()> {
    let directory = config.state_path("history-requests");
    let _lock = JournalLocks::acquire(&[&directory])?;
    fs::create_dir_all(directory.join("completed"))?;
    // Keep the result visible throughout: readers look in both locations.
    for kind in ["request", "result"] {
        let path = request_path(config, id, kind)?;
        if path.exists() {
            fs::rename(&path, completed_path(&path))?;
        }
    }
    journal::sync_directory(&directory.join("completed"))?;
    journal::sync_directory(&directory)?;
    Ok(())
}
fn prune_completed_requests(config: &Config) -> Result<usize> {
    let mailbox = config.state_path("history-requests");
    let completed = mailbox.join("completed");
    journal::retention_path(&mailbox, true)?;
    if !journal::retention_path(&completed, true)? {
        return Ok(0);
    }
    let _lock = JournalLocks::acquire(&[&mailbox])?;
    let mut pairs = Vec::new();
    for entry in fs::read_dir(&completed)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".result.json") else {
            continue;
        };
        let Ok(active) = request_path(config, id, "request") else {
            continue;
        };
        if active.exists() {
            continue;
        }
        let request_path = completed_path(&active);
        let (Ok(request), Ok(result)) = (
            read_small::<RestoreRequest>(&request_path),
            read_small::<RestoreResult>(&entry.path()),
        ) else {
            continue;
        };
        if request.request_id != id
            || result.request_id != id
            || request.operation_id != result.operation_id
            || !matches!(result.state.as_str(), "restored" | "rejected")
        {
            continue;
        }
        let metadata = entry.metadata()?;
        pairs.push((
            (metadata.mtime(), metadata.mtime_nsec(), id.to_owned()),
            request_path,
            entry.path(),
        ));
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let remove = pairs.len().saturating_sub(128);
    for (_, request, result) in pairs.into_iter().take(remove) {
        fs::remove_file(request)?;
        fs::remove_file(result)?;
    }
    if remove > 0 {
        journal::sync_directory(&completed)?;
    }
    Ok(remove)
}
fn read_small<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 16384,
        "invalid history request file"
    );
    Ok(serde_json::from_reader(file.take(16385))?)
}
fn read_saved<T: serde::de::DeserializeOwned>(config: &Config, id: &str, kind: &str) -> Result<T> {
    let path = request_path(config, id, kind)?;
    match read_small(&path) {
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            read_small(&completed_path(&path))
        }
        value => value,
    }
}
fn queued(request: &RestoreRequest) -> RestoreResult {
    RestoreResult {
        request_id: request.request_id.clone(),
        operation_id: request.operation_id.clone(),
        state: "queued".into(),
        message: "Waiting for the worker to restore this item.".into(),
    }
}
pub fn request_restore(config: &Config, request: &RestoreRequest) -> Result<RestoreResult> {
    ensure!(
        !request.operation_id.is_empty()
            && request.operation_id.len() <= 256
            && request.revision.len() == 64
            && request.revision.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid operation id or history revision"
    );
    let path = request_path(config, &request.request_id, "request")?;
    crate::control::prepare_directories(config)?;
    fs::create_dir_all(path.parent().unwrap())?;
    let _lock = JournalLocks::acquire(&[&config.state_path("history-requests")])?;
    let saved = saved_path(config, &request.request_id, "request")?;
    if saved.exists() {
        let original: RestoreRequest = read_saved(config, &request.request_id, "request")?;
        ensure!(
            original == *request,
            "this request ID is already bound to another operation"
        );
        return restore_result(config, &request.request_id);
    }
    let mut outstanding = 0;
    for entry in fs::read_dir(path.parent().unwrap())? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".request.json") {
            outstanding += 1;
            ensure!(
                outstanding < 64,
                "too many pending restores; wait for the worker"
            );
        }
    }
    // Atomic creation plus the mailbox lock binds an idempotency key exactly
    // once. A crash cannot expose a partial request to the running worker.
    journal::atomic_json(&path, request)?;
    journal::sync_directory(path.parent().unwrap())?;
    crate::control::signal_wakeup(config.state_path("wake.fifo"));
    Ok(queued(request))
}
pub fn restore_result(config: &Config, request_id: &str) -> Result<RestoreResult> {
    let request: RestoreRequest = read_saved(config, request_id, "request")?;
    let result_path = saved_path(config, request_id, "result")?;
    if result_path.exists() {
        let result: RestoreResult = read_saved(config, request_id, "result")?;
        ensure!(
            result.request_id == request.request_id && result.operation_id == request.operation_id,
            "history result does not match its request"
        );
        Ok(result)
    } else {
        Ok(queued(&request))
    }
}
/// Called only by the existing worker while it owns RuntimeLock, before the
/// paused wait. A bounded batch never creates a second mutation worker.
pub fn process_requests(
    config: &Config,
    normalizer: &mut crate::normalizer::Normalizer,
) -> Result<usize> {
    let directory = config.state_path("history-requests");
    if !directory.exists() {
        return Ok(0);
    }
    let mut requests = Vec::with_capacity(8);
    let mut more = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".request.json") else {
            continue;
        };
        let path = request_path(config, id, "request")?;
        let result_path = request_path(config, id, "result")?;
        if result_path.exists() {
            let result: RestoreResult = read_small(&result_path)?;
            if matches!(result.state.as_str(), "restored" | "rejected") {
                archive_request(config, id)?;
                continue;
            }
        }
        if requests.len() == 8 {
            more = true;
            break;
        }
        let request: RestoreRequest = read_small(&path)?;
        ensure!(
            request.request_id == id,
            "history request ID does not match its file"
        );
        requests.push(request);
    }
    if requests.is_empty() {
        return Ok(0);
    }
    let recovery = normalizer.recover();
    let recovery_error = recovery
        .err()
        .map(|error| format!("Recovery must finish first: {error:#}"));
    for request in &requests {
        let mut result = queued(request);
        let attempted = (|| -> Result<()> {
            if let Some(error) = &recovery_error {
                result.state = "recovery_required".into();
                result.message = error.clone();
                return Ok(());
            }
            let db = index(config)?;
            let existing: Option<String> = db
                .query_row(
                    "SELECT original_id FROM history_restores WHERE request_id=?1",
                    [&request.request_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(original) = existing {
                ensure!(
                    original == request.operation_id,
                    "restore request ID was used by another operation"
                );
                result.state = "restored".into();
                result.message = "The original name was restored.".into();
                return Ok(());
            }
            let (item, record, seq) = load(&db, &request.operation_id)?;
            if let Some(reason) = refusal(&db, &item, &record, seq, &request.revision, config)? {
                result.state = "rejected".into();
                result.message = reason;
                return Ok(());
            }
            drop(db);
            let committed = normalizer.restore_history_record(&record, &request.request_id)?;
            ensure!(
                committed["status"] == "reverted"
                    && committed["operation_id"] == request.request_id
                    && committed["restores_operation_id"] == request.operation_id,
                "restore has no matching confirmed journal record"
            );
            ensure!(
                !config.state_path("pending.json").exists(),
                "restore still requires pending recovery"
            );
            result.state = "restored".into();
            result.message = "The original name was restored.".into();
            Ok(())
        })();
        if let Err(error) = attempted {
            result.state = if config.state_path("pending.json").exists() {
                "recovery_required"
            } else {
                "rejected"
            }
            .into();
            result.message = format!("{error:#}");
        }
        journal::atomic_json(
            &request_path(config, &request.request_id, "result")?,
            &result,
        )?;
        journal::sync_directory(&directory)?;
        if matches!(result.state.as_str(), "restored" | "rejected") {
            archive_request(config, &request.request_id)?;
        }
    }
    if more {
        crate::control::signal_wakeup(config.state_path("wake.fifo"));
    }
    Ok(requests.len())
}
#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
