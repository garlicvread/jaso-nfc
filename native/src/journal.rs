//! Python-compatible journals, retained archives, retries, and inverse operations.
use crate::model::now;
use anyhow::{Context, Result, bail};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub fn suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
// Match Python os.fsync: Rust File::sync_all requests F_FULLFSYNC on Darwin,
// which is stronger than the existing durability contract and stalls each hop.
pub fn sync_file(file: &File) -> io::Result<()> {
    loop {
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
pub fn sync_directory(path: &Path) -> io::Result<()> {
    sync_file(&File::open(if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    })?)
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".retry-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        sync_file(&file)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
pub struct JournalLocks {
    _files: Vec<File>,
}
impl JournalLocks {
    pub fn acquire(paths: &[&Path]) -> Result<Self> {
        let mut paths: Vec<PathBuf> = paths
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    std::env::current_dir().unwrap_or_default().join(p)
                }
            })
            .collect();
        paths.sort();
        paths.dedup();
        let mut files = Vec::new();
        for path in paths {
            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .open(suffix(&path, ".lock"))?;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
            files.push(file);
        }
        Ok(Self { _files: files })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetryRecord {
    pub signature: Vec<Value>,
    pub reason: String,
    pub count: u64,
    pub last_failure: f64,
    pub next_retry: f64,
}
#[derive(Serialize, Deserialize)]
struct RetryFile {
    version: u32,
    entries: BTreeMap<String, RetryRecord>,
}
pub struct RetryState {
    pub path: Option<PathBuf>,
    pub entries: BTreeMap<String, RetryRecord>,
    base: f64,
    maximum: f64,
}
pub fn path_signature(path: &Path, parent: bool) -> Vec<Value> {
    match fs::symlink_metadata(path) {
        Err(e) => vec![json!("unavailable"), json!(e.raw_os_error())],
        Ok(m) => {
            #[cfg(target_os = "macos")]
            let flags = {
                use std::os::macos::fs::MetadataExt as _;
                m.st_flags()
            };
            #[cfg(not(target_os = "macos"))]
            let flags = 0;
            let id = [m.dev(), m.ino()];
            #[cfg(test)]
            let id = crate::native_names::test_adjust_identity(id);
            let mut values = vec![
                json!(id[0]),
                json!(id[1]),
                json!(m.mode()),
                json!(m.uid()),
                json!(m.gid()),
                json!(flags),
            ];
            if !parent {
                values.push(json!(
                    m.ctime()
                        .saturating_mul(1_000_000_000)
                        .saturating_add(m.ctime_nsec())
                ));
            }
            values
        }
    }
}
pub fn candidate_signature(src: &str, dst: &str) -> Vec<Value> {
    vec![
        json!(path_signature(Path::new(src), false)),
        json!(path_signature(Path::new(dst), false)),
        json!(path_signature(
            Path::new(src).parent().unwrap_or(Path::new(".")),
            true
        )),
    ]
}
impl RetryState {
    pub fn new(path: Option<PathBuf>, base: f64, maximum: f64) -> Result<Self> {
        if !base.is_finite() || !maximum.is_finite() || base <= 0. || maximum < base {
            bail!("retry intervals must satisfy 0 < base <= maximum");
        }
        let entries = if let Some(path) = path.as_ref().filter(|p| p.exists()) {
            let data: RetryFile = serde_json::from_reader(File::open(path)?)?;
            if data.version != 1 {
                bail!("unsupported retry state; preserve it before resetting");
            }
            for rec in data.entries.values() {
                if rec.count < 1 || !rec.last_failure.is_finite() || !rec.next_retry.is_finite() {
                    bail!("invalid retry state record");
                }
            }
            data.entries
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            entries,
            base,
            maximum,
        })
    }
    pub fn deferred(&mut self, src: &str, dst: &str) -> bool {
        let signature = candidate_signature(src, dst);
        let Some(record) = self.entries.get_mut(src) else {
            return false;
        };
        if record.signature != signature {
            self.entries.remove(src);
            return false;
        }
        let time = now();
        record.next_retry = record.next_retry.min(time + self.maximum);
        time < record.next_retry
    }
    pub fn failure(&mut self, src: &str, dst: &str, reason: &str) {
        let signature = candidate_signature(src, dst);
        let count = self
            .entries
            .get(src)
            .filter(|r| r.signature == signature && r.reason == reason)
            .map_or(1, |r| r.count.saturating_add(1));
        let time = now();
        let delay = (self.base * 2f64.powi((count - 1).min(20) as i32)).min(self.maximum);
        self.entries.insert(
            src.into(),
            RetryRecord {
                signature,
                reason: reason.into(),
                count,
                last_failure: time,
                next_retry: time + delay,
            },
        );
    }
    pub fn save(&self) -> Result<()> {
        if let Some(path) = &self.path {
            atomic_json(path, &json!({"version":1,"entries":self.entries}))?;
        }
        Ok(())
    }
}
pub struct Journal {
    path: PathBuf,
    file: File,
    max_bytes: u64,
    error_path: PathBuf,
    error_max_bytes: u64,
    error_backups: usize,
}
impl Journal {
    pub fn standard(path: &Path) -> Result<Self> {
        Self::new(path, 8 * 1024 * 1024, 1024 * 1024, 3)
    }
    pub fn new(
        path: &Path,
        max_bytes: u64,
        error_max_bytes: u64,
        error_backups: usize,
    ) -> Result<Self> {
        if max_bytes == 0 || error_max_bytes == 0 || error_backups == 0 {
            bail!("log limits and backup count must be positive");
        }
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        let size = file.metadata()?.len();
        if size > 0 {
            let mut existing = File::open(path)?;
            existing.seek(SeekFrom::End(-1))?;
            let mut byte = [0];
            existing.read_exact(&mut byte)?;
            if byte != *b"\n" {
                let mut length = size.min(65536);
                loop {
                    existing.seek(SeekFrom::Start(size - length))?;
                    let mut tail = Vec::new();
                    existing.read_to_end(&mut tail)?;
                    if tail.contains(&b'\n') || length == size {
                        serde_json::from_slice::<Value>(
                            tail.rsplit(|b| *b == b'\n').next().unwrap_or_default(),
                        )
                        .context("incomplete journal tail; preserve and repair before mutations")?;
                        file.write_all(b"\n")?;
                        sync_file(&file)?;
                        break;
                    }
                    length = size.min(length.saturating_mul(2));
                }
            }
        }
        let error_path = path.with_extension("errors.jsonl");
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&error_path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            max_bytes,
            error_path,
            error_max_bytes,
            error_backups,
        })
    }
    fn rotate(&mut self) -> Result<()> {
        sync_file(&self.file)?;
        let history = suffix(&self.path, ".history");
        fs::create_dir_all(&history)?;
        let previous = fs::read_dir(&history)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                e.file_name()
                    .to_string_lossy()
                    .split('-')
                    .next()?
                    .parse::<u128>()
                    .ok()
            })
            .max()
            .unwrap_or(0);
        let sequence = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .max(previous + 1);
        let raw = history.join(format!(
            "{sequence:020}-{}.jsonl",
            uuid::Uuid::new_v4().simple()
        ));
        fs::rename(&self.path, &raw)?;
        sync_directory(&history)?;
        sync_directory(self.path.parent().unwrap_or(Path::new(".")))?;
        let temporary = suffix(&raw, ".gz.tmp");
        let compress = (|| -> Result<()> {
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            let mut encoder = GzEncoder::new(output, Compression::default());
            io::copy(&mut File::open(&raw)?, &mut encoder)?;
            let output = encoder.finish()?;
            sync_file(&output)?;
            fs::rename(&temporary, suffix(&raw, ".gz"))?;
            sync_directory(&history)?;
            fs::remove_file(&raw)?;
            sync_directory(&history)?;
            Ok(())
        })();
        if compress.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        self.file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)?;
        Ok(())
    }
    pub fn emit(&mut self, record: &Value) -> Result<()> {
        let mut data = serde_json::to_vec(record)?;
        data.push(b'\n');
        if matches!(record["status"].as_str(), Some("renamed" | "reverted"))
            || record["recovery_required"] == true
        {
            self.file.write_all(&data)?;
            sync_file(&self.file)?;
            if self.file.metadata()?.len() >= self.max_bytes {
                self.rotate()?;
            }
        } else {
            let size = fs::metadata(&self.error_path).map(|m| m.len()).unwrap_or(0);
            if size + data.len() as u64 >= self.error_max_bytes {
                for index in (1..=self.error_backups).rev() {
                    let to = suffix(&self.error_path, &format!(".{index}"));
                    let from = if index == 1 {
                        self.error_path.clone()
                    } else {
                        suffix(&self.error_path, &format!(".{}", index - 1))
                    };
                    if from.exists() {
                        fs::rename(from, to)?;
                    }
                }
            }
            let mut output = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.error_path)?;
            output.write_all(&data)?;
            output.flush()?;
        }
        Ok(())
    }
}
pub fn record_sources(path: &Path) -> Result<Vec<PathBuf>> {
    let history = suffix(path, ".history");
    let mut segments = BTreeMap::<String, PathBuf>::new();
    if history.is_dir() {
        for entry in fs::read_dir(history)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") {
                segments.insert(name, entry.path());
            } else if name.ends_with(".jsonl.gz") {
                segments
                    .entry(name.trim_end_matches(".gz").into())
                    .or_insert(entry.path());
            }
        }
    }
    let recovery = suffix(path, ".recovery.jsonl");
    let mut sources: Vec<PathBuf> = recovery.exists().then_some(recovery).into_iter().collect();
    sources.extend(segments.into_values());
    if path.exists() {
        sources.push(path.to_path_buf());
    } else if sources.is_empty() {
        return Err(io::Error::from_raw_os_error(libc::ENOENT).into());
    }
    Ok(sources)
}
#[derive(Default, Debug, Serialize)]
pub struct ArchiveMaintenance {
    pub archives_removed: usize,
    pub recovery_records_retained: usize,
}

fn visit_source(source: &Path, mut visitor: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let file = File::open(source)?;
    let stream: Box<dyn Read> = if source.extension().is_some_and(|e| e == "gz") {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Vec::new();
        let length = reader
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)?;
        if length == 0 {
            break;
        }
        if length > 1024 * 1024 {
            bail!("journal record exceeds the safe read limit");
        }
        if !line.iter().all(u8::is_ascii_whitespace) {
            let record: Value = serde_json::from_slice(&line)
                .with_context(|| format!("invalid journal record in {}", source.display()))?;
            if !record.is_object() {
                bail!("journal record is not an object in {}", source.display());
            }
            visitor(record)?;
        }
    }
    Ok(())
}

fn archived_bytes(path: &Path) -> Result<u64> {
    let mut file = File::open(path)?;
    if path.extension().is_some_and(|e| e == "gz") {
        // Rotated segments are smaller than 4 GiB; the gzip trailer records
        // their original size. Expired segments are fully validated below.
        file.seek(SeekFrom::End(-4))?;
        let mut size = [0; 4];
        file.read_exact(&mut size)?;
        Ok(u32::from_le_bytes(size).into())
    } else {
        Ok(file.metadata()?.len())
    }
}

fn files_equal(left: &Path, right: &Path) -> Result<bool> {
    if !left.exists() || fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = BufReader::new(File::open(left)?);
    let mut right = BufReader::new(File::open(right)?);
    loop {
        let a = left.fill_buf()?;
        let b = right.fill_buf()?;
        if a != b {
            return Ok(false);
        }
        let len = a.len();
        if len == 0 {
            return Ok(true);
        }
        left.consume(len);
        right.consume(len);
    }
}

/// Check the owned entry itself without rejecting platform aliases in its
/// ancestors (for example /var on macOS). Missing entries may be created later.
pub(crate) fn retention_path(path: &Path, directory: bool) -> Result<bool> {
    // A trailing slash asks lstat to dereference a directory symlink; remove
    // that syntactic suffix before checking the owned entry itself.
    let entry: PathBuf = path.components().collect();
    match fs::symlink_metadata(&entry) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || if directory {
                    !metadata.is_dir()
                } else {
                    !metadata.is_file()
                }
            {
                bail!(
                    "retention path is not a regular {}: {}",
                    if directory { "directory" } else { "file" },
                    path.display()
                );
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn validate_retention_journal(path: &Path) -> Result<()> {
    retention_path(path.parent().unwrap_or(Path::new(".")), true)?;
    retention_path(path, false)?;
    retention_path(&suffix(path, ".recovery.jsonl"), false)?;
    let history = suffix(path, ".history");
    if retention_path(&history, true)? {
        for entry in fs::read_dir(history)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") || name.ends_with(".jsonl.gz") {
                retention_path(&entry.path(), false)?;
            }
        }
    }
    Ok(())
}

/// Caller holds JournalLocks and the runtime lock. Keep a contiguous recent
/// suffix, so every retained completed rename retains all its later events.
/// Recovery evidence is published durably before any source is removed.
pub fn maintain_archives(
    path: &Path,
    protected_ids: &BTreeSet<String>,
) -> Result<ArchiveMaintenance> {
    const ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;
    validate_retention_journal(path)?;
    let recovery = suffix(path, ".recovery.jsonl");
    if !path.exists() && !suffix(path, ".history").exists() && !recovery.exists() {
        return Ok(ArchiveMaintenance::default());
    }
    if path.exists() && fs::metadata(path)?.len() >= 8 * 1024 * 1024 {
        Journal::standard(path)?.rotate()?;
    }
    let sources = record_sources(path)?;
    let archives: Vec<_> = sources
        .iter()
        .filter(|p| **p != recovery && **p != path)
        .collect();
    let mut first_retained = archives.len();
    let mut bytes = 0u64;
    for source in archives.iter().rev().take(2) {
        bytes = bytes.saturating_add(archived_bytes(source)?);
        if bytes > ARCHIVE_BYTES {
            break;
        }
        first_retained -= 1;
    }
    let expired = &archives[..first_retained];
    if expired.is_empty() && !recovery.exists() {
        return Ok(ArchiveMaintenance::default());
    }
    let mut retained = BTreeMap::<String, Value>::new();
    let key = |record: &Value| -> Result<String> {
        if let Some(id) = record["operation_id"].as_str() {
            return Ok(id.to_owned());
        }
        let mut identity = record.clone();
        identity
            .as_object_mut()
            .unwrap()
            .remove("history_retention_incomplete");
        Ok(format!(
            "legacy-{:x}",
            Sha256::digest(serde_json::to_vec(&identity)?)
        ))
    };
    // Memory grows only with unresolved/protected facts, never with ordinary
    // completed history. Repeated diagnostics for an operation collapse.
    for source in recovery
        .exists()
        .then_some(&recovery)
        .into_iter()
        .chain(expired.iter().copied())
    {
        visit_source(source, |mut record| {
            let id = key(&record)?;
            let committed = matches!(record["status"].as_str(), Some("renamed" | "reverted"));
            if protected_ids.contains(&id)
                || (!committed
                    && (record["recovery_required"] == true
                        || record["status"].as_str() != Some("error")))
            {
                if !committed
                    && retained.get(&id).is_some_and(|prior| {
                        matches!(prior["status"].as_str(), Some("renamed" | "reverted"))
                    })
                {
                    return Ok(());
                }
                record["history_retention_incomplete"] = json!(true);
                retained.insert(id, record);
            } else {
                retained.remove(&id);
            }
            Ok(())
        })?;
    }
    for source in archives[first_retained..]
        .iter()
        .map(|source| source.as_path())
        .chain(path.exists().then_some(path))
    {
        visit_source(source, |record| {
            if matches!(record["status"].as_str(), Some("renamed" | "reverted")) {
                retained.remove(&key(&record)?);
            }
            Ok(())
        })?;
    }
    let temporary = suffix(
        &recovery,
        &format!(".{}.tmp", uuid::Uuid::new_v4().simple()),
    );
    let publish = (|| -> Result<()> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        for record in retained.values() {
            serde_json::to_writer(&mut output, record)?;
            output.write_all(b"\n")?;
        }
        sync_file(&output)?;
        if files_equal(&recovery, &temporary)? {
            fs::remove_file(&temporary)?;
            return Ok(());
        }
        fs::rename(&temporary, &recovery)?;
        sync_directory(path.parent().unwrap_or(Path::new(".")))?;
        Ok(())
    })();
    if publish.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    publish?;
    for source in expired {
        // Python archives can contain a plain/compressed twin. Removing both
        // prevents a supposedly expired segment reappearing on cache rebuild.
        let raw = if source.extension().is_some_and(|e| e == "gz") {
            source.with_extension("")
        } else {
            source.to_path_buf()
        };
        for candidate in [&raw, &suffix(&raw, ".gz")] {
            match fs::remove_file(candidate) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                result => result?,
            }
        }
    }
    if !expired.is_empty() {
        sync_directory(&suffix(path, ".history"))?;
    }
    if retained.is_empty() {
        fs::remove_file(&recovery)?;
        sync_directory(path.parent().unwrap_or(Path::new(".")))?;
    }
    Ok(ArchiveMaintenance {
        archives_removed: expired.len(),
        recovery_records_retained: retained.len(),
    })
}
pub fn visit_records(path: &Path, mut visitor: impl FnMut(Value) -> Result<()>) -> Result<()> {
    for source in record_sources(path)? {
        visit_source(&source, &mut visitor)?;
    }
    Ok(())
}
pub fn journal_records(path: &Path) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    visit_records(path, |row| {
        rows.push(row);
        Ok(())
    })?;
    Ok(rows)
}
pub fn revert(log: &Path, output: Option<&Path>) -> Result<(u64, u64)> {
    crate::normalizer::revert(log, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_retention_rejects_symlink_surfaces_before_deleting_archives() {
        use std::os::unix::fs::symlink;
        for surface in ["active", "recovery", "history", "segment"] {
            let temp = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let path = temp.path().join("history.jsonl");
            let history = suffix(&path, ".history");
            let source_dir = if surface == "history" {
                outside.path().to_path_buf()
            } else {
                history.clone()
            };
            fs::create_dir_all(&source_dir).unwrap();
            for name in ["0001.jsonl", "0002.jsonl", "0003.jsonl"] {
                fs::write(source_dir.join(name), b"{\"status\":\"renamed\"}\n").unwrap();
            }
            let foreign = outside.path().join("foreign.jsonl");
            fs::write(
                &foreign,
                b"{\"status\":\"error\",\"recovery_required\":true}\n",
            )
            .unwrap();
            match surface {
                "active" => symlink(&foreign, &path).unwrap(),
                "recovery" => symlink(&foreign, suffix(&path, ".recovery.jsonl")).unwrap(),
                "history" => symlink(&source_dir, &history).unwrap(),
                "segment" => {
                    fs::remove_file(source_dir.join("0001.jsonl")).unwrap();
                    symlink(&foreign, source_dir.join("0001.jsonl")).unwrap();
                }
                _ => unreachable!(),
            }
            let result = maintain_archives(&path, &BTreeSet::new());
            assert!(
                source_dir.join("0001.jsonl").symlink_metadata().is_ok(),
                "{surface}: preflight must precede deletion"
            );
            assert!(
                result.is_err(),
                "{surface}: symlink retention surface was accepted"
            );
            assert_eq!(
                fs::read(&foreign).unwrap(),
                b"{\"status\":\"error\",\"recovery_required\":true}\n"
            );
        }
    }
    #[test]
    fn archive_retention_keeps_committed_pending_fact_after_a_later_diagnostic() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut log = Journal::new(&path, 1, 1024, 3).unwrap();
        log.emit(&json!({"status":"renamed","operation_id":"pending"}))
            .unwrap();
        log.emit(&json!({"status":"error","operation_id":"pending","recovery_required":true}))
            .unwrap();
        for id in ["recent-one", "recent-two"] {
            log.emit(&json!({"status":"renamed","operation_id":id}))
                .unwrap();
        }
        drop(log);
        maintain_archives(&path, &BTreeSet::from(["pending".into()])).unwrap();
        let records = journal_records(&path).unwrap();
        assert_eq!(
            records
                .iter()
                .find(|r| r["operation_id"] == "pending")
                .unwrap()["status"],
            "renamed",
            "a diagnostic must not erase the committed recovery confirmation"
        );
    }

    #[test]
    fn archive_retention_rejects_non_object_evidence_without_deleting_sources() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let history = suffix(&path, ".history");
        fs::create_dir(&history).unwrap();
        fs::write(history.join("0001.jsonl"), b"null\n").unwrap();
        for name in ["0002.jsonl", "0003.jsonl"] {
            fs::write(history.join(name), b"{\"status\":\"renamed\"}\n").unwrap();
        }
        assert!(maintain_archives(&path, &BTreeSet::new()).is_err());
        assert_eq!(fs::read_dir(history).unwrap().count(), 3);
    }
    #[test]
    fn interrupted_retention_deduplicates_legacy_recovery_without_operation_ids() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let history = suffix(&path, ".history");
        fs::create_dir(&history).unwrap();
        fs::write(
            history.join("0001.jsonl"),
            b"{\"status\":\"error\",\"recovery_required\":true}\n",
        )
        .unwrap();
        fs::write(suffix(&path, ".recovery.jsonl"), b"{\"status\":\"error\",\"recovery_required\":true,\"history_retention_incomplete\":true}\n").unwrap();
        for name in ["0002.jsonl", "0003.jsonl"] {
            fs::write(history.join(name), b"{\"status\":\"renamed\"}\n").unwrap();
        }
        maintain_archives(&path, &BTreeSet::new()).unwrap();
        assert_eq!(
            journal_records(&path)
                .unwrap()
                .iter()
                .filter(|row| row["recovery_required"] == true)
                .count(),
            1
        );
    }
    #[test]
    fn retained_archives_survive_rotation_and_diagnostics_are_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let mut journal = Journal::new(&path, 250, 100, 2).unwrap();
        for n in 0..40 {
            journal
                .emit(&serde_json::json!({"status":"renamed","old":n.to_string(),"new":"new"}))
                .unwrap();
        }
        for _ in 0..50 {
            journal
                .emit(&serde_json::json!({"status":"error","error":"diagnostic".repeat(10)}))
                .unwrap();
        }
        drop(journal);
        let rows = journal_records(&path).unwrap();
        assert_eq!(rows.len(), 40);
        for (n, row) in rows.iter().enumerate() {
            assert_eq!(row["old"], n.to_string());
        }
        assert!(std::fs::metadata(&path).unwrap().len() < 250);
        assert!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e
                    .file_name()
                    .to_string_lossy()
                    .starts_with("history.errors.jsonl"))
                .count()
                <= 3
        );
    }
    #[test]
    fn journal_rejects_partial_tail_before_append() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        std::fs::write(&path, b"{\"status\":").unwrap();
        assert!(Journal::new(&path, 1000, 1000, 3).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"status\":");
    }

    #[test]
    fn archive_failure_keeps_fsynced_success_in_active_journal() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        std::fs::write(suffix(&path, ".history"), b"owned obstruction").unwrap();
        let mut journal = Journal::new(&path, 1, 1000, 3).unwrap();
        assert!(
            journal
                .emit(&json!({"status":"renamed","operation_id":"must-survive"}))
                .is_err()
        );
        drop(journal);
        let records = journal_records(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["operation_id"], "must-survive");
    }
    #[test]
    fn completed_json_tail_without_newline_is_repaired_before_append() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        std::fs::write(&path, b"{\"status\":\"renamed\",\"old\":\"first\"}").unwrap();
        let mut journal = Journal::new(&path, 10000, 1000, 3).unwrap();
        journal
            .emit(&json!({"status":"renamed","old":"second"}))
            .unwrap();
        drop(journal);
        let rows = journal_records(&path).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["old"], "first");
        assert_eq!(rows[1]["old"], "second");
    }
    #[test]
    fn python_plain_gzip_archive_twins_are_read_exactly_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let history = suffix(&path, ".history");
        std::fs::create_dir(&history).unwrap();
        let raw = history.join("0001-history.jsonl");
        let data = b"{\"status\":\"renamed\",\"old\":\"owned\"}\n";
        std::fs::write(&raw, data).unwrap();
        let mut gzip = GzEncoder::new(
            File::create(suffix(&raw, ".gz")).unwrap(),
            Compression::default(),
        );
        gzip.write_all(data).unwrap();
        gzip.finish().unwrap();
        assert_eq!(journal_records(&path).unwrap().len(), 1);
    }
    #[test]
    fn archive_sequence_survives_wall_clock_regression() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        let history = suffix(&path, ".history");
        std::fs::create_dir(&history).unwrap();
        std::fs::write(
            history.join("99999999999999999998-older.jsonl"),
            b"{\"status\":\"renamed\",\"old\":\"first\"}\n",
        )
        .unwrap();
        let mut journal = Journal::new(&path, 1, 1000, 3).unwrap();
        journal
            .emit(&json!({"status":"renamed","old":"second"}))
            .unwrap();
        drop(journal);
        let records = journal_records(&path).unwrap();
        assert_eq!(records[0]["old"], "first");
        assert_eq!(records[1]["old"], "second");
    }
    #[test]
    fn visitor_streams_and_propagates_failure_before_reading_later_records() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("history.jsonl");
        std::fs::write(&path, b"{\"old\":\"first\"}\ninvalid later record\n").unwrap();
        let error = visit_records(&path, |row| {
            assert_eq!(row["old"], "first");
            bail!("visitor-stop")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "visitor-stop");
    }
    #[test]
    fn retry_roundtrip_retains_unresolved_records_and_rejects_invalid_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("retry.json");
        let mut retry = RetryState::new(Some(path.clone()), 1., 60.).unwrap();
        for name in ["first", "second"] {
            let source = temp.path().join(name);
            std::fs::write(&source, b"owned").unwrap();
            retry.failure(
                source.to_str().unwrap(),
                source.with_extension("new").to_str().unwrap(),
                "13",
            );
        }
        retry.save().unwrap();
        let reopened = RetryState::new(Some(path.clone()), 1., 60.).unwrap();
        assert_eq!(reopened.entries.len(), 2);
        std::fs::write(&path,b"{\"version\":1,\"entries\":{\"a\":{\"signature\":[],\"reason\":\"x\",\"count\":0,\"last_failure\":0,\"next_retry\":0}}}").unwrap();
        assert!(RetryState::new(Some(path), 1., 60.).is_err());
    }
    #[test]
    fn python_recovered_journal_records_are_deduplicated_during_revert() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("new");
        std::fs::write(&source, b"owned").unwrap();
        let info = std::fs::metadata(&source).unwrap();
        let path = temp.path().join("history.jsonl");
        let mut journal = Journal::new(&path, 1000, 1000, 3).unwrap();
        let mut record = json!({"operation_id":"python-operation","dir":temp.path(),"old":"old","new":"new","identity":[info.dev(),info.ino()],"status":"error","recovery_required":true,"recovery_path":source});
        journal.emit(&record).unwrap();
        record["status"] = json!("renamed");
        record["recovered"] = json!(true);
        journal.emit(&record).unwrap();
        drop(journal);
        assert_eq!(revert(&path, None).unwrap(), (1, 0));
        assert_eq!(std::fs::read(temp.path().join("old")).unwrap(), b"owned");
    }
}
