//! Python-compatible journals, retained archives, retries, and inverse operations.
use crate::model::now;
use anyhow::{Context, Result, bail};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
pub fn visit_records(path: &Path, mut visitor: impl FnMut(Value) -> Result<()>) -> Result<()> {
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
    let mut sources: Vec<PathBuf> = segments.into_values().collect();
    if path.exists() {
        sources.push(path.to_path_buf());
    } else if sources.is_empty() {
        return Err(io::Error::from_raw_os_error(libc::ENOENT).into());
    }
    for source in sources {
        let file = File::open(&source)?;
        let stream: Box<dyn Read> = if source.extension().is_some_and(|e| e == "gz") {
            Box::new(GzDecoder::new(file))
        } else {
            Box::new(file)
        };
        for line in BufReader::new(stream).lines() {
            let line = line?;
            if !line.trim().is_empty() {
                visitor(
                    serde_json::from_str(&line).with_context(|| {
                        format!("invalid journal record in {}", source.display())
                    })?,
                )?;
            }
        }
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
