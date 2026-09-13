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

fn history_activity_result<T>(activity: &crate::activity::Activity, result: &Result<T>) {
    match result {
        Ok(_) => {
            activity.finish("updating_history", None);
            activity.resolve("updating_history", None, "checked");
        }
        Err(error) => activity.failed_with_reason(
            "updating_history",
            None,
            Some(&format!("{error:#}")),
            error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::raw_os_error),
        ),
    }
}

fn maintain_history(config: &Config, activity: &crate::activity::Activity) -> Result<Value> {
    activity.begin("updating_history", None);
    let result = crate::history::maintain(config);
    history_activity_result(activity, &result);
    result
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
    let activity = crate::activity::Activity::new();
    let _responder = crate::activity_transport::ActivityResponder::start(config, activity.clone())?;
    let _activity_binding = activity.bind();
    let wake = Arc::new(Wakeup::new(config.state_path("wake.fifo"))?);
    let signals = StopSignals::install(&wake)?;
    let index = Arc::new(Index::new(config.state_path("index.sqlite3"), false)?);
    if let Err(error) = maintain_history(config, &activity) {
        diagnostic(config, &format!("history maintenance: {error:#}"));
    }
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
        let mut history_counted = 0;
        let mut history_checked = std::time::Instant::now()
            .checked_sub(Duration::from_secs(30))
            .unwrap();
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
            // Explicit history requests use this same mutation worker and lock,
            // including while automatic normalization remains paused.
            if let Some(policy) = index.current_policy()? {
                normalizer.policy = policy;
                crate::history::process_requests(config, &mut normalizer)?;
            }
            if crate::control::paused(config)? {
                activity.set_state("paused", "paused");
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
            // Capacity failures pause new work with backoff. Existing pending
            // recovery above and explicit restore handling remain serialized.
            if let Err(error) = crate::storage::ensure_write_capacity(config) {
                activity.set_issue("low_storage", &error.to_string());
                wake.wait(Some(Duration::from_secs(30)))?;
                continue;
            }
            activity.clear_issue("low_storage");
            let retry_checked_at = now();
            match index.work(&mut normalizer) {
                Ok(true) => {
                    if activity.renamed_count() != history_counted
                        && history_checked.elapsed() >= Duration::from_secs(30)
                    {
                        if maintain_history(config, &activity).is_ok() {
                            history_counted = activity.renamed_count();
                        }
                        history_checked = std::time::Instant::now();
                    }
                    continue;
                }
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
            if activity.renamed_count() != history_counted {
                if maintain_history(config, &activity).is_ok() {
                    history_counted = activity.renamed_count();
                }
                history_checked = std::time::Instant::now();
            }
            if index.current_policy()?.is_some() {
                activity.set_state("idle", "waiting_for_events");
            } else {
                activity.set_state("waiting_metadata", "discovering_sources");
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
    activity.set_state("stopping", "stopping");
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
fn saved_retry_metadata(signature: Option<&Value>) -> Option<(Option<u32>, bool)> {
    let parts = signature
        .and_then(Value::as_array)
        .filter(|v| v.len() == 3)?;
    let mut locked = false;
    let mut source_mode = None;
    for (n, part) in parts.iter().enumerate() {
        let values = part.as_array()?;
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
            return None;
        }
        if n == 0 {
            source_mode = Some(values[2].as_u64().unwrap() as u32);
        }
        // macOS sys/stat.h: UF_IMMUTABLE and SF_IMMUTABLE.
        locked |= values[5].as_u64().unwrap() & (0x0000_0002 | 0x0002_0000) != 0;
    }
    Some((source_mode, locked))
}

fn saved_retry_locked(signature: Option<&Value>) -> bool {
    saved_retry_metadata(signature).is_some_and(|(_, locked)| locked)
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
    let activity = crate::activity_transport::snapshot_for(config)?;
    let status = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid index status"))?;
    status.extend(json!({
        "version":env!("CARGO_PKG_VERSION"),"schema_version":2,"activity":activity.get("activity").cloned().unwrap_or(Value::Null),"engine":"rust","running":crate::control::running(config)?,"paused":crate::control::paused(config)?,
        "apply":config.apply,"scope":config.scope,
        "roots":coverage.get("roots").cloned().unwrap_or(json!(config.roots)),
        "active_roots":coverage.get("active_roots").cloned().unwrap_or(json!([])),
        "unavailable_roots":coverage.get("unavailable").cloned().unwrap_or(json!({})),
        "catalog_unavailable":coverage.get("catalog_unavailable").cloned().unwrap_or(json!({})),
        "disconnected_roots":coverage.get("disconnected_roots").cloned().unwrap_or(json!([])),
        "manual_waiting_roots":coverage.get("manual_waiting_roots").cloned().unwrap_or(json!([])),
        "today_renamed":crate::history::cached_today_count(config)?,
        "deferred_renames":entries.len(),
        "rename_retry_items":rename_retry_items(entries),
        "next_rename_retry":entries.values().filter_map(|r|r.get("next_retry").and_then(Value::as_f64)).reduce(f64::min),
        "pending_recovery":config.state_path("pending.json").exists()
    }).as_object().unwrap().clone());
    let disconnected = coverage.get("disconnected_roots").and_then(Value::as_array);
    let desired_roots = coverage.get("roots").and_then(Value::as_array);
    let retry_coverage = crate::coverage::Coverage {
        roots: desired_roots
            .map(|roots| {
                roots
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| config.roots.clone()),
        root_excludes: coverage
            .get("root_excludes")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default(),
        ..Default::default()
    };
    let retry_policy = crate::sources::policy_for(config, &retry_coverage, None);
    let current_entries: serde_json::Map<String, Value> = entries
        .iter()
        .filter(|(path, record)| {
            let directory = saved_retry_metadata(record.get("signature"))
                .and_then(|(mode, _)| mode)
                .is_some_and(|mode| mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32);
            let accepted = if directory {
                retry_policy.accepts_directory_lexically(path)
            } else {
                retry_policy.accepts_lexically(path)
            };
            accepted
                && !disconnected.is_some_and(|roots| {
                    roots
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|root| crate::policy::within(path, root))
                })
        })
        .map(|(path, record)| (path.clone(), record.clone()))
        .collect();
    let current = status
        .entry("current")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid current index status"))?;
    current.extend(json!({
        "deferred_renames":current_entries.len(),
        "rename_retry_items":rename_retry_items(&current_entries),
        "next_rename_retry":current_entries.values().filter_map(|r|r.get("next_retry").and_then(Value::as_f64)).reduce(f64::min)
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

    #[test]
    fn history_activity_keeps_cause_and_success_resolves_without_outcome_counters() {
        let activity = crate::activity::Activity::new();
        let error = anyhow::Error::from(std::io::Error::from_raw_os_error(libc::EACCES))
            .context("updating history");
        history_activity_result(&activity, &Err::<(), _>(error));
        let failed = activity.snapshot();
        assert_eq!(failed["events"][0]["errno"], libc::EACCES);
        assert!(
            failed["events"][0]["reason"]
                .as_str()
                .unwrap()
                .contains("updating history")
        );
        history_activity_result(&activity, &Ok(()));
        let recovered = activity.snapshot();
        assert_eq!(recovered["events"][0]["resolution"], "checked");
        for counter in ["processed", "renamed", "errors", "deferred"] {
            assert_eq!(recovered["counters"][counter], failed["counters"][counter]);
        }
        assert_eq!(recovered["events"].as_array().unwrap().len(), 1);
    }

    fn retry_record(next_retry: f64) -> Value {
        json!({
            "signature": [[1, 2, 33188, 501, 20, 0, 100], ["unavailable", 2], [1, 3, 16877, 501, 20, 32770]],
            "reason": "1", "count": 3, "last_failure": 100.0, "next_retry": next_retry
        })
    }

    #[test]
    fn disconnected_rename_retries_are_dormant_while_recovery_remains_actionable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let config = Config {
            state_dir: temp.path().to_string_lossy().into_owned(),
            roots: vec![
                "/Volumes/Media".into(),
                "/Users/me".into(),
                "/Volumes/Media2".into(),
            ],
            ..Config::default()
        };
        std::fs::create_dir_all(config.state_path("state").parent().unwrap())?;
        let entries = json!({"/Volumes/Media/file": retry_record(10.0), "/Users/me/file": retry_record(20.0), "/Volumes/Media2/file": retry_record(30.0)});
        let skip = serde_json::to_vec(&json!({"entries": entries}))?;
        std::fs::write(config.state_path("skip.json"), &skip)?;
        std::fs::write(
            config.state_path("coverage.json"),
            serde_json::to_vec(&json!({"disconnected_roots":["/Volumes/Media"]}))?,
        )?;
        std::fs::write(config.state_path("pending.json"), b"recovery evidence")?;
        std::fs::write(config.state_path("journal.jsonl"), b"history")?;
        let value = runtime_status(&config)?;
        assert_eq!(value["deferred_renames"], 3);
        assert_eq!(value["current"]["deferred_renames"], 2);
        assert_eq!(value["current"]["next_rename_retry"], 20.0);
        assert_eq!(
            value["current"]["rename_retry_items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(value["disconnected_roots"], json!(["/Volumes/Media"]));
        assert_eq!(value["pending_recovery"], true);
        std::fs::write(
            config.state_path("coverage.json"),
            serde_json::to_vec(&json!({"roots":["/Users/me"], "disconnected_roots":[]}))?,
        )?;
        let removed = runtime_status(&config)?;
        assert_eq!(removed["deferred_renames"], 3);
        assert_eq!(removed["current"]["deferred_renames"], 1);
        assert_eq!(removed["current"]["next_rename_retry"], 20.0);
        assert_eq!(removed["pending_recovery"], true);
        std::fs::write(
            config.state_path("coverage.json"),
            serde_json::to_vec(
                &json!({"roots":["/Volumes/Media", "/Users/me", "/Volumes/Media2"], "disconnected_roots":["/Volumes/Media"]}),
            )?,
        )?;
        let replaced = runtime_status(&config)?;
        assert_eq!(replaced["current"]["deferred_renames"], 2);
        assert_eq!(replaced["deferred_renames"], 3);
        assert_eq!(std::fs::read(config.state_path("skip.json"))?, skip);
        assert_eq!(
            std::fs::read(config.state_path("pending.json"))?,
            b"recovery evidence"
        );
        assert_eq!(
            std::fs::read(config.state_path("journal.jsonl"))?,
            b"history"
        );
        Ok(())
    }

    #[test]
    fn current_rename_retries_follow_hidden_scope_and_saved_coverage_without_writes() -> Result<()>
    {
        for scope in ["configured", "all-user-files"] {
            let temp = tempfile::tempdir()?;
            let config = Config {
                scope: scope.into(),
                roots: if scope == "configured" {
                    vec!["/Users/me/Projects".into()]
                } else {
                    vec![]
                },
                excludes: vec!["/Users/me/Projects/Excluded".into()],
                exclude_names: vec!["ignored".into()],
                skip_hidden_tops: vec![],
                state_dir: temp.path().to_string_lossy().into_owned(),
                ..Config::default()
            };
            let mut directory = retry_record(2.0);
            directory["signature"][0][2] = json!(libc::S_IFDIR | 0o755);
            let entries = json!({
                "/Users/me/Projects/file": retry_record(100.0),
                "/Users/me/Projects/.dotfile": retry_record(110.0),
                "/Users/me/Projects/.hidden-directory": directory,
                "/Users/me/Projects/.cache/file": retry_record(1.0),
                "/Users/me/Projects/.explicit/file": retry_record(120.0),
                "/Users/me/Projects/.explicit/.cache/file": retry_record(3.0),
                "/Users/me/Projects/Excluded/file": retry_record(4.0),
                "/Users/me/Projects/ignored/file": retry_record(5.0),
                "/Volumes/Media/file": retry_record(130.0),
                "/Volumes/Media/Cache/file": retry_record(6.0),
                "/Volumes/Offline/file": retry_record(7.0)
            });
            let skip = serde_json::to_vec(&json!({"version": 1, "entries": entries}))?;
            let coverage = serde_json::to_vec(&json!({
                "roots": ["/Users/me/Projects", "/Users/me/Projects/.explicit", "/Volumes/Media", "/Volumes/Offline"],
                "active_roots": [],
                "disconnected_roots": ["/Volumes/Offline"],
                "root_excludes": {"/Volumes/Media": ["/Volumes/Media/Cache"]}
            }))?;
            std::fs::create_dir_all(config.state_path("skip.json").parent().unwrap())?;
            std::fs::write(config.state_path("skip.json"), &skip)?;
            std::fs::write(config.state_path("coverage.json"), &coverage)?;
            let value = runtime_status(&config)?;
            assert_eq!(value["deferred_renames"], 11);
            assert_eq!(value["current"]["deferred_renames"], 4);
            assert_eq!(value["current"]["next_rename_retry"], 100.0);
            let paths: Vec<_> = value["current"]["rename_retry_items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["path"].as_str().unwrap())
                .collect();
            assert_eq!(
                paths,
                [
                    "/Users/me/Projects/file",
                    "/Users/me/Projects/.dotfile",
                    "/Users/me/Projects/.explicit/file",
                    "/Volumes/Media/file"
                ]
            );
            assert_eq!(std::fs::read(config.state_path("skip.json"))?, skip);
            assert_eq!(std::fs::read(config.state_path("coverage.json"))?, coverage);
            assert!(!config.state_path("index.sqlite3").exists());
        }
        Ok(())
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
