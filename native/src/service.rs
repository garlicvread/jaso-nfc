//! A single worker sleeps on a FIFO. FSEvents callbacks only enqueue and wake it.
use crate::{
    config::Config,
    control::{RuntimeLock, StopSignals, Wakeup},
    index::Index,
    model::now,
    normalizer::Normalizer,
    sources::SourceWorker,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{io::Write, path::Path, sync::Arc, time::Duration};

pub const LABEL: &str = crate::app_bundle::IDENTIFIER;

pub fn make_normalizer(config: &Config) -> Result<Normalizer> {
    Normalizer::new(
        config.policy(),
        Some(Path::new(&config.logs()).join("renames.jsonl")),
        Some(config.state_path("skip.json")),
        Some(config.state_path("pending.json")),
        config.apply,
    )
}

pub fn watch(config: &Config) -> Result<()> {
    watch_with_menu(config, None, false)
}

pub fn watch_at(config: &Config, config_path: &Path) -> Result<()> {
    watch_with_menu(config, Some(config_path), false)
}

pub fn run(config: &Config, config_path: &Path) -> Result<()> {
    watch_with_menu(config, Some(config_path), true)
}

#[cfg(target_os = "macos")]
fn start_menu(config: &Config, config_path: &Path) -> Result<()> {
    use anyhow::Context;
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    let executable = std::env::current_exe()?;
    let macos = executable.parent().context("cannot locate native menu")?;
    let bundle = macos
        .parent()
        .and_then(Path::parent)
        .context("cannot locate application bundle")?;
    crate::app_bundle::validate_metadata(bundle)?;
    let menu = macos.join(crate::app_bundle::GUI_EXECUTABLE);
    crate::app_bundle::validate_native_gui(&menu)?;
    let mut command = Command::new(menu);
    command
        .args([std::ffi::OsStr::new("--config"), config_path.as_os_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // The menu handles Stop, Restart and Quit confirmation after this worker
    // exits. Give it its own process group so worker teardown cannot kill it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let config = config.clone();
    // Start the reaper before the child exists, so a thread creation failure
    // cannot leave an unreaped process. The GUI's per-user lock makes repeated
    // worker starts harmless; an exited menu is never automatically relaunched.
    std::thread::Builder::new()
        .name("jaso-menu".into())
        .spawn(move || {
            let result = command.spawn().and_then(|mut child| child.wait());
            match result {
                Err(error) => diagnostic(&config, &format!("menu startup failed: {error}")),
                Ok(status) if !status.success() => {
                    diagnostic(&config, &format!("native menu exited: {status}"))
                }
                _ => {}
            }
        })?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn start_menu(_: &Config, _: &Path) -> Result<()> {
    anyhow::bail!("the native menu requires macOS")
}

fn watch_with_menu(config: &Config, config_path: Option<&Path>, open_menu: bool) -> Result<()> {
    let _lock = RuntimeLock::acquire(config, Duration::ZERO)?;
    let wake = Arc::new(Wakeup::new(config.state_path("wake.fifo"))?);
    let signals = StopSignals::install(&wake)?;
    let index = Arc::new(Index::new(config.state_path("index.sqlite3"), false)?);
    let mut normalizer = make_normalizer(config)?;
    let notify = wake.clone();
    let mut sources = SourceWorker::new(
        config.clone(),
        index.clone(),
        Arc::new(move || notify.set()),
    )?;
    let _ready = crate::setup::RuntimeReady::publish(config, config_path)?;
    if open_menu
        && let Some(path) = config_path
        && let Err(error) = start_menu(config, path)
    {
        diagnostic(config, &format!("menu startup failed: {error:#}"));
    }
    let result = (|| -> Result<()> {
        let mut recovery_checked = false;
        diagnostic(
            config,
            &format!(
                "native watch started; source discovery pending; apply={}",
                config.apply
            ),
        );
        while !signals.requested() {
            wake.clear()?;
            if signals.requested() {
                break;
            }
            sources.check()?;
            if crate::control::paused(config)? {
                // Continue recording filesystem events while mutations are paused.
                // A due work queue must not turn a paused worker into a busy loop.
                wake.wait(None)?;
                continue;
            }
            if !recovery_checked && let Some(policy) = index.current_policy()? {
                normalizer.policy = policy;
                normalizer.recover()?;
                recovery_checked = true;
            }
            let retry_checked_at = now();
            match index.work(&mut normalizer) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    if error
                        .downcast_ref::<crate::model::PendingRecoveryError>()
                        .is_some()
                    {
                        return Err(error);
                    }
                    if index
                        .status()?
                        .get("needs_revalidation")
                        .and_then(Value::as_bool)
                        == Some(true)
                    {
                        sources.request(true);
                        wake.wait(None)?;
                        continue;
                    }
                    return Err(error);
                }
            }
            wake.wait(timeout(
                [
                    index.next_wakeup()?,
                    normalizer.next_retry_time(Some(retry_checked_at)),
                ]
                .into_iter()
                .flatten(),
            ))?;
        }
        Ok(())
    })();
    let closed = sources.close(); // Drain callbacks before releasing the database.
    let message = match (&result, &closed) {
        (Err(error), _) => format!("native watch failed: {error:#}"),
        (_, Err(error)) => format!("native watch shutdown failed: {error:#}"),
        _ => "native watch stopped".to_owned(),
    };
    diagnostic(config, &message);
    result.and(closed)
}

fn timeout(deadlines: impl Iterator<Item = f64>) -> Option<Duration> {
    deadlines
        .filter(|v| v.is_finite())
        .reduce(f64::min)
        .map(|t| Duration::from_secs_f64((t - now()).max(0.0)))
}

fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read(path) {
        Ok(b) => Ok(serde_json::from_slice(&b)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

// Interpret only the layout written by journal::candidate_signature. A saved
// immutable flag is evidence of a lock at the last failure, not a fresh stat.
fn saved_retry_locked(signature: Option<&Value>) -> bool {
    let Some(parts) = signature.and_then(Value::as_array).filter(|v| v.len() == 3) else {
        return false;
    };
    let mut locked = false;
    for (n, part) in parts.iter().enumerate() {
        let Some(values) = part.as_array() else {
            return false;
        };
        if values.len() == 2
            && values[0] == "unavailable"
            && (values[1].is_null() || values[1].as_i64().is_some())
        {
            continue;
        }
        if values.len() != if n == 2 { 6 } else { 7 }
            || values[..2].iter().any(|v| v.as_u64().is_none())
            || values[2..6]
                .iter()
                .any(|v| !v.as_u64().is_some_and(|v| v <= u32::MAX as u64))
            || (n != 2 && values[6].as_i64().is_none())
        {
            return false;
        }
        // macOS sys/stat.h: UF_IMMUTABLE and SF_IMMUTABLE.
        locked |= values[5].as_u64().unwrap() & (0x0000_0002 | 0x0002_0000) != 0;
    }
    locked
}

fn rename_retry_items(entries: &serde_json::Map<String, Value>) -> Vec<Value> {
    let mut sample: Vec<(f64, &str, Value)> = Vec::new();
    for (path, record) in entries {
        let Some(reason) = record.get("reason").and_then(Value::as_str) else {
            continue;
        };
        let Some(attempts) = record
            .get("count")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
        else {
            continue;
        };
        let Some(next_retry) = record
            .get("next_retry")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
        else {
            continue;
        };
        let Some(last_failure) = record
            .get("last_failure")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite())
        else {
            continue;
        };
        let position = sample
            .binary_search_by(|(due, existing_path, _)| {
                due.total_cmp(&next_retry)
                    .then_with(|| existing_path.cmp(&path.as_str()))
            })
            .unwrap_or_else(|position| position);
        if position < 8 {
            sample.insert(
                position,
                (
                    next_retry,
                    path,
                    json!({
                        "path": path, "reason": reason, "attempts": attempts,
                        "next_retry": next_retry, "last_failure": last_failure,
                        "locked": saved_retry_locked(record.get("signature"))
                    }),
                ),
            );
            sample.truncate(8);
        }
    }
    sample.into_iter().map(|(_, _, item)| item).collect()
}

pub fn runtime_status(config: &Config) -> Result<Value> {
    let coverage = read_json(&config.state_path("coverage.json"))?;
    let retries = read_json(&config.state_path("skip.json"))?;
    let empty = serde_json::Map::new();
    let entries = retries
        .get("entries")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut value = if config.state_path("index.sqlite3").exists() {
        Index::new(config.state_path("index.sqlite3"), true)?.status()?
    } else {
        json!({"indexed":false})
    };
    let status = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid index status"))?;
    status.extend(json!({
        "version":env!("CARGO_PKG_VERSION"),"engine":"rust","running":crate::control::running(config)?,"paused":crate::control::paused(config)?,
        "apply":config.apply,"scope":config.scope,
        "roots":coverage.get("roots").cloned().unwrap_or(json!(config.roots)),
        "active_roots":coverage.get("active_roots").cloned().unwrap_or(json!([])),
        "unavailable_roots":coverage.get("unavailable").cloned().unwrap_or(json!({})),
        "catalog_unavailable":coverage.get("catalog_unavailable").cloned().unwrap_or(json!({})),
        "deferred_renames":entries.len(),
        "rename_retry_items":rename_retry_items(entries),
        "next_rename_retry":entries.values().filter_map(|r|r.get("next_retry").and_then(Value::as_f64)).reduce(f64::min),
        "pending_recovery":config.state_path("pending.json").exists()
    }).as_object().unwrap().clone());
    Ok(value)
}

pub fn diagnostic(config: &Config, message: &str) {
    let path = Path::new(&config.logs()).join("service.log");
    let _ = (|| -> Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        if path.metadata().is_ok_and(|m| m.len() >= 1024 * 1024) {
            for n in (1..3).rev() {
                let from = path.with_extension(format!("log.{n}"));
                if from.exists() {
                    std::fs::rename(from, path.with_extension(format!("log.{}", n + 1)))?;
                }
            }
            std::fs::rename(&path, path.with_extension("log.1"))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{:.3} {message}", now())?;
        Ok(())
    })();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn retry_record(next_retry: f64) -> Value {
        json!({
            "signature": [[1, 2, 33188, 501, 20, 0, 100], ["unavailable", 2], [1, 3, 16877, 501, 20, 32770]],
            "reason": "1", "count": 3, "last_failure": 100.0, "next_retry": next_retry
        })
    }

    #[test]
    fn runtime_status_samples_rename_retries_by_deadline_then_path_without_writes() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = Config {
            state_dir: temp.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let mut entries = serde_json::Map::new();
        for n in (0..12).rev() {
            entries.insert(
                format!("/missing/{n:02}"),
                retry_record(200.0 - (n / 3) as f64),
            );
        }
        let path = config.state_path("skip.json");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let before = serde_json::to_vec(&json!({"version": 1, "entries": entries}))?;
        std::fs::write(&path, &before)?;
        let status = runtime_status(&config)?;
        assert_eq!(status["deferred_renames"], 12);
        let items = status["rename_retry_items"]
            .as_array()
            .expect("retry sample");
        assert_eq!(items.len(), 8);
        for (n, item) in [9, 10, 11, 6, 7, 8, 3, 4].into_iter().zip(items) {
            assert_eq!(
                item,
                &json!({
                    "path": format!("/missing/{n:02}"), "reason": "1", "attempts": 3,
                    "next_retry": 200.0 - (n / 3) as f64, "last_failure": 100.0, "locked": true
                })
            );
        }
        assert_eq!(status["next_rename_retry"], 197.0);
        assert_eq!(std::fs::read(path)?, before);
        assert!(!config.state_path("index.sqlite3").exists());
        Ok(())
    }

    #[test]
    fn runtime_status_does_not_infer_locks_from_errors_or_malformed_signatures() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = Config {
            state_dir: temp.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let signatures = [
            json!([]),
            json!([
                [1, 2, 33188, 501, 20, 0, 100],
                ["unavailable", 2],
                [1, 3, 16877, 501, 20, "32770"]
            ]),
            json!([
                [1, 2, 33188, 501, 20, 2],
                ["unavailable", 2],
                [1, 3, 16877, 501, 20, 0]
            ]),
            json!([
                [1, 2, 33188, 501, 20, 0, 100],
                ["unavailable", 2],
                [1, 3, 16877, 501, 20, 0]
            ]),
            json!([[1, 2, 33188, 501, 20, 2, 100], ["unavailable", 2], null]),
            json!([
                [1, 2, 33188, 501, 20, 131072, 100],
                ["unavailable", 2],
                [1, 3, 16877, 501, 20, 0]
            ]),
        ];
        let mut entries = serde_json::Map::new();
        for (n, signature) in signatures.into_iter().enumerate() {
            let mut record = retry_record(200.0);
            record["signature"] = signature;
            entries.insert(format!("/missing/{n}"), record);
        }
        for (field, bad) in [
            ("count", json!(0)),
            ("next_retry", json!(null)),
            ("reason", json!(1)),
            ("last_failure", json!("100")),
        ] {
            let mut record = retry_record(1.0);
            record[field] = bad;
            entries.insert(format!("/invalid/{field}"), record);
        }
        std::fs::create_dir_all(config.state_path("skip.json").parent().unwrap())?;
        std::fs::write(
            config.state_path("skip.json"),
            serde_json::to_vec(&json!({"version": 1, "entries": entries}))?,
        )?;
        let status = runtime_status(&config)?;
        assert_eq!(status["deferred_renames"], 10);
        let items = status["rename_retry_items"]
            .as_array()
            .expect("retry sample");
        assert_eq!(items.len(), 6);
        for (n, item) in items.iter().enumerate() {
            assert_eq!(item["locked"], n == 5, "item {n}");
        }
        Ok(())
    }

    #[test]
    fn runtime_status_reports_worker_while_index_schema_is_initializing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = Config {
            state_dir: temp.path().to_string_lossy().into_owned(),
            ..Config::default()
        };
        let path = config.state_path("index.sqlite3");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let writer = rusqlite::Connection::open(&path)?;
        writer.execute_batch("PRAGMA journal_mode=WAL; BEGIN IMMEDIATE; CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
        let status = runtime_status(&config)?;
        assert_eq!(status["indexed"], false);
        assert_eq!(status["running"], false);
        assert_eq!(status["apply"], config.apply);
        assert!(status.get("indexed_entries").is_none());
        assert!(status.get("directory_retry_count").is_none());
        assert!(status.get("directory_retry_items").is_none());
        assert_eq!(status["rename_retry_items"], json!([]));
        writer.execute_batch("ROLLBACK;")?;
        let index = Index::new(&path, false)?;
        let status = runtime_status(&config)?;
        assert_eq!(status["pending_jobs"], 0);
        assert_eq!(status["directory_retry_count"], 0);
        assert_eq!(status["directory_retry_items"], json!([]));
        drop(index);
        Ok(())
    }

    #[test]
    fn idle_has_no_polling_deadline() {
        assert_eq!(timeout(std::iter::empty()), None);
    }
    #[test]
    fn due_work_has_zero_wait() {
        assert_eq!(timeout([now() - 1.0].into_iter()), Some(Duration::ZERO));
    }
}
