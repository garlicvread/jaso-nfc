//! App-owned allocation and filesystem capacity. Reports only; never deletes data.
use crate::config::Config;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};
const MIB: u64 = 1024 * 1024;
const CRITICAL_BYTES: u64 = 256 * MIB;
const WARNING_BYTES: u64 = 1024 * MIB;
#[derive(Clone, Debug)]
struct VolumeReading {
    id: Option<u64>,
    total: Option<u64>,
    available: Option<u64>,
    error: Option<String>,
}
#[cfg(test)]
thread_local! { static TEST_AVAILABLE:std::cell::Cell<Option<u64>>=const {std::cell::Cell::new(None)}; }
#[cfg(test)]
pub(crate) fn test_capacity<T>(available_bytes: u64, body: impl FnOnce() -> T) -> T {
    struct Reset(Option<u64>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_AVAILABLE.set(self.0);
        }
    }
    let _reset = Reset(TEST_AVAILABLE.replace(Some(available_bytes)));
    body()
}
fn evaluate(reading: &VolumeReading) -> &'static str {
    let Some(available) = reading.available else {
        return "unknown";
    };
    if available < CRITICAL_BYTES {
        return "critical";
    }
    if available < WARNING_BYTES {
        return "warning";
    }
    let Some(total) = reading.total.filter(|total| *total > 0) else {
        return "unknown";
    };
    if u128::from(available) * 100 < u128::from(total) * 5 {
        "warning"
    } else if reading.error.is_some() {
        "unknown"
    } else {
        "ok"
    }
}
fn rank(status: &str) -> u8 {
    match status {
        "critical" => 3,
        "warning" => 2,
        "unknown" => 1,
        _ => 0,
    }
}
fn message(status: &str) -> &'static str {
    match status {
        "critical" => {
            "Free up space on the app's storage volume. New filename changes are paused; pending recovery can still finish."
        }
        "warning" => {
            "Storage space is running low. Move or remove files you no longer need before the volume fills up."
        }
        "unknown" => "Some storage information is unavailable. No files were removed.",
        _ => "There is enough free space for filename changes.",
    }
}
fn aggregate(readings: Vec<(String, VolumeReading)>) -> Value {
    let mut volumes: BTreeMap<String, (Vec<String>, VolumeReading)> = BTreeMap::new();
    for (path, reading) in readings {
        let key = reading
            .id
            .map_or_else(|| format!("unknown:{path}"), |id| format!("dev:{id}"));
        if let Some((paths, existing)) = volumes.get_mut(&key) {
            if !paths.contains(&path) {
                paths.push(path);
            }
            // Concurrent readings can differ slightly; retain the lower known
            // free-space reading and never erase an observed unavailable state.
            if existing.error.is_none() && reading.error.is_some() {
                existing.error = reading.error.clone();
            }
            existing.available = match (existing.available, reading.available) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            existing.total = match (existing.total, reading.total) {
                (Some(a), Some(b)) if a != b => None,
                (a, b) => a.or(b),
            };
        } else {
            volumes.insert(key, (vec![path], reading));
        }
    }
    let mut status = "ok";
    let volumes:Vec<Value>=volumes.into_iter().map(|(id,(paths,reading))| {
        let state=evaluate(&reading);if rank(state)>rank(status){status=state;}
        let percent=reading.total.filter(|total|*total>0).zip(reading.available).map(|(total,available)|available as f64/total as f64*100.);
        json!({"id":if reading.id.is_some(){Some(id)}else{None},"paths":paths,"total_bytes":reading.total,"available_bytes":reading.available,"available_percent":percent,"status":state,"error":reading.error})
    }).collect();
    if volumes.is_empty() {
        status = "unknown";
    }
    json!({"status":status,"volumes":volumes,"message":message(status),"critical_below_bytes":CRITICAL_BYTES,"warning_below_bytes":WARNING_BYTES,"warning_below_percent":5})
}
fn nearest_directory(path: &Path) -> Result<(PathBuf, fs::Metadata)> {
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    loop {
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "storage path is a symlink and was not followed"
                );
                if metadata.is_dir() {
                    return Ok((current, metadata));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        ensure!(
            current.pop(),
            "no accessible directory identifies the storage volume"
        );
    }
}
fn volume(path: &Path) -> VolumeReading {
    let mut id = None;
    let measured = (|| -> Result<(u64, u64)> {
        let (directory, metadata) = nearest_directory(path)?;
        id = Some(metadata.dev());
        let raw = CString::new(directory.as_os_str().as_bytes())?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(raw.as_ptr(), stats.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let stats = unsafe { stats.assume_init() };
        let block = if stats.f_frsize == 0 {
            stats.f_bsize
        } else {
            stats.f_frsize
        } as u128;
        ensure!(block > 0, "filesystem returned an unknown block size");
        let total = u64::try_from(stats.f_blocks as u128 * block)
            .context("volume size exceeds the supported range")?;
        let available = u64::try_from(stats.f_bavail as u128 * block)
            .context("volume free space exceeds the supported range")?;
        ensure!(total > 0, "filesystem returned an unknown capacity");
        #[cfg(test)]
        let available = TEST_AVAILABLE.get().unwrap_or(available);
        Ok((total, available))
    })();
    match measured {
        Ok((total, available)) => VolumeReading {
            id,
            total: Some(total),
            available: Some(available),
            error: None,
        },
        Err(error) => VolumeReading {
            id,
            total: None,
            available: None,
            error: Some(format!("{error:#}")),
        },
    }
}
/// Capacity only: local metadata plus statvfs, never scans app files or journals.
pub fn capacity_snapshot(paths: &[&Path]) -> Value {
    aggregate(
        paths
            .iter()
            .map(|path| (path.to_string_lossy().into_owned(), volume(path)))
            .collect(),
    )
}
fn ensure_readings(readings: Vec<(String, VolumeReading)>) -> Result<()> {
    let status = aggregate(readings);
    if status["status"] == "critical" {
        let paths: Vec<String> = status["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["status"] == "critical")
            .flat_map(|v| {
                v["paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        bail!(
            "Critical storage: fewer than 256 MiB available for {}. Free up space before starting new filename changes; pending recovery remains available.",
            paths.join(", ")
        );
    }
    Ok(())
}
/// Call immediately before the first pending write of a NEW operation. Recovery
/// deliberately bypasses this guard so an interrupted rename can finish safely.
pub fn ensure_paths_capacity(paths: &[&Path]) -> Result<()> {
    let _materialization = crate::directory_io::DirectoryMaterialization::deny()?;
    ensure_readings(
        paths
            .iter()
            .map(|path| (path.to_string_lossy().into_owned(), volume(path)))
            .collect(),
    )
}
pub fn ensure_write_capacity(config: &Config) -> Result<()> {
    ensure_paths_capacity(&[Path::new(&config.state_dir), Path::new(&config.logs())])
}
fn allocated(path: &Path, seen: &mut HashSet<(u64, u64)>) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    // Individual database files and enclosing directories are measured in
    // separate categories. Count every app-owned inode only once.
    if !seen.insert((metadata.dev(), metadata.ino())) {
        return Ok(0);
    }
    let mut bytes = metadata
        .blocks()
        .checked_mul(512)
        .context("allocated byte count overflow")?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        for entry in fs::read_dir(path)? {
            bytes = bytes
                .checked_add(allocated(&entry?.path(), seen)?)
                .context("allocated byte count overflow")?;
        }
    }
    Ok(bytes)
}
/// Scans app-owned state/log metadata without opening file contents. Symlinks
/// themselves may consume blocks, but their targets are never enumerated.
pub fn snapshot(config: &Config) -> Result<Value> {
    let _materialization = crate::directory_io::DirectoryMaterialization::deny()?;
    let state = PathBuf::from(&config.state_dir);
    let logs = PathBuf::from(config.logs());
    let mut value = capacity_snapshot(&[&state, &logs]);
    let mut owned = vec![("state", state), ("logs", logs)];
    // Give nested log directories their own contribution before the state walk.
    owned.sort_by_key(|(_, path)| std::cmp::Reverse(path.components().count()));
    let mut seen = HashSet::new();
    let mut total = Some(0_u64);
    let mut paths = Vec::new();
    for (role, path) in owned {
        let measured = allocated(&path, &mut seen);
        let (bytes, error) = match measured {
            Ok(bytes) => (Some(bytes), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        total = total
            .zip(bytes)
            .and_then(|(total, bytes)| total.checked_add(bytes));
        paths.push(json!({"role":role,"path":path,"allocated_bytes":bytes,"error":error}));
    }
    if total.is_none() && value["status"] == "ok" {
        value["status"] = json!("unknown");
        value["message"] = json!(message("unknown"));
    }
    value["allocated_bytes"] = json!(total);
    value["paths"] = json!(paths);
    let mut seen = HashSet::new();
    let mut categories = Vec::new();
    for (role, names) in [
        (
            "index",
            vec![
                config.state_path("index.sqlite3"),
                config.state_path("index.sqlite3-wal"),
                config.state_path("index.sqlite3-shm"),
            ],
        ),
        (
            "history",
            vec![
                config.state_path("history.sqlite3"),
                config.state_path("history.sqlite3-wal"),
                config.state_path("history.sqlite3-shm"),
            ],
        ),
        ("logs", vec![PathBuf::from(config.logs())]),
        (
            "backups",
            vec![Path::new(&config.state_dir).join("backups")],
        ),
        (
            "releases",
            vec![Path::new(&config.state_dir).join("releases")],
        ),
        ("other", vec![PathBuf::from(&config.state_dir)]),
    ] {
        let mut bytes = Some(0u64);
        for path in names {
            // Direct database categories must not traverse an intermediate
            // state symlink that the enclosing allocation walk skips.
            if path.starts_with(&config.state_dir) {
                let mut linked_parent = false;
                for parent in path.ancestors().skip(1) {
                    if fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_symlink()) {
                        linked_parent = true;
                        break;
                    }
                    if parent == Path::new(&config.state_dir) {
                        break;
                    }
                }
                if linked_parent {
                    continue;
                }
            }
            bytes = bytes
                .zip(allocated(&path, &mut seen).ok())
                .and_then(|(a, b)| a.checked_add(b));
        }
        categories.push(json!({"role":role,"allocated_bytes":bytes}));
    }
    value["categories"] = json!(categories);
    value["measured_at"] = json!(crate::model::now());
    Ok(value)
}
#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
