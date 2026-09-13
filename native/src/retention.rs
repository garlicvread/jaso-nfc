//! Retention of completed installation and settings transactions.
use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

fn directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "retention directory is not a regular directory: {}",
                path.display()
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub fn mark_completed(backup: &Path) -> Result<()> {
    ensure!(directory(backup)?, "backup directory is missing");
    crate::config::atomic_json(
        backup.join("completed.json"),
        &serde_json::json!({"completed_at":crate::model::now()}),
    )
}

fn children(path: &Path) -> Result<Vec<(SystemTime, PathBuf)>> {
    if !directory(path)? {
        return Ok(vec![]);
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let m = fs::symlink_metadata(entry.path())?;
        if m.is_dir() && !m.file_type().is_symlink() {
            paths.push((m.modified()?, entry.path()));
        }
    }
    paths.sort_by(|a, b| b.cmp(a));
    Ok(paths)
}

/// Incomplete transactions remain available for recovery. Only backups marked
/// after a successful transaction participate in the retention limit.
pub fn prune_backups(root: &Path) -> Result<usize> {
    ensure!(directory(root)?, "application data directory is missing");
    let entries = children(&root.join("backups"))?;
    let mut removed = 0;
    for (prefix, keep) in [("native-install-", 1), ("setup-", 2)] {
        let mut completed = 0;
        for (_, path) in &entries {
            if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(prefix)
            {
                continue;
            }
            let marker = path.join("completed.json");
            let Ok(metadata) = fs::symlink_metadata(&marker) else {
                continue;
            };
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&fs::read(marker)?)?;
            if !value["completed_at"].is_number() {
                continue;
            }
            completed += 1;
            if completed > keep {
                fs::remove_dir_all(path)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn referenced_releases(value: &serde_json::Value, releases: &Path, keep: &mut BTreeSet<PathBuf>) {
    match value {
        serde_json::Value::String(s) => {
            let path = Path::new(s);
            if let Ok(relative) = path.strip_prefix(releases)
                && let Some(std::path::Component::Normal(name)) = relative.components().next()
            {
                keep.insert(releases.join(name));
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                referenced_releases(value, releases, keep);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                referenced_releases(value, releases, keep);
            }
        }
        _ => {}
    }
}

/// Called only after successful installation, while the lifecycle lock is held.
/// Keep the installed release, one rollback release and all recovery references.
pub fn prune_releases(root: &Path, installed: &Path) -> Result<usize> {
    ensure!(directory(root)?, "application data directory is missing");
    let releases = root.join("releases");
    let mut keep = BTreeSet::new();
    referenced_releases(&serde_json::json!(installed), &releases, &mut keep);
    ensure!(
        !keep.is_empty(),
        "installed release is outside the managed directory"
    );
    let entries = children(&releases)?;
    for (_, previous) in &entries {
        let path = previous;
        let name = path.file_name().unwrap().to_string_lossy();
        if !keep.contains(path)
            && name.contains("-native-")
            && !name.starts_with('.')
            && directory(&path.join("Jaso NFC.app"))?
        {
            keep.insert(previous.clone());
            break;
        }
    }
    for (_, backup) in children(&root.join("backups"))? {
        let manifest = backup.join("installation.json");
        if let Ok(m) = fs::symlink_metadata(&manifest) {
            ensure!(
                m.is_file() && !m.file_type().is_symlink(),
                "invalid installation recovery manifest"
            );
            let value = serde_json::from_slice(&fs::read(manifest)?)?;
            referenced_releases(&value, &releases, &mut keep);
        }
    }
    let mut removed = 0;
    for (_, path) in entries {
        let name = path.file_name().unwrap().to_string_lossy();
        if !name.contains("-native-") || name.starts_with('.') {
            continue;
        }
        if !keep.contains(&path) && directory(&path.join("Jaso NFC.app"))? {
            fs::remove_dir_all(path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn completed(root: &Path, name: &str) {
        let path = root.join("backups").join(name);
        fs::create_dir_all(&path).unwrap();
        mark_completed(&path).unwrap();
    }

    #[test]
    fn keeps_latest_completed_backups_and_all_unfinished_recovery() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..5 {
            completed(temp.path(), &format!("native-install-{n}"));
        }
        for n in 0..5 {
            completed(temp.path(), &format!("setup-{n}"));
        }
        let pending = temp.path().join("backups/native-install-pending");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("config.json"), b"recovery").unwrap();
        prune_backups(temp.path()).unwrap();
        let entries: Vec<_> = fs::read_dir(temp.path().join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 4, "one install, two setup, one unfinished");
        assert!(pending.join("config.json").exists());
        assert_eq!(prune_backups(temp.path()).unwrap(), 0);
    }

    #[test]
    fn cleanup_never_follows_backup_or_release_symlinks() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        symlink(outside.path(), temp.path().join("backups")).unwrap();
        assert!(prune_backups(temp.path()).is_err());
        assert!(outside.path().join("keep").exists());
    }

    #[test]
    fn old_releases_retire_while_current_and_unfinished_recovery_survive() {
        let temp = tempfile::tempdir().unwrap();
        let releases = temp.path().join("releases");
        for name in [
            "0.1-native-old",
            "0.2-native-recovery",
            "0.2-native-current",
        ] {
            fs::create_dir_all(releases.join(name).join("Jaso NFC.app")).unwrap();
        }
        let recovery = temp.path().join("backups/native-install-pending");
        fs::create_dir_all(&recovery).unwrap();
        let old = releases.join("0.2-native-recovery/Jaso NFC.app");
        fs::write(
            recovery.join("installation.json"),
            serde_json::to_vec(&serde_json::json!({"new_app":old})).unwrap(),
        )
        .unwrap();
        let current = releases.join("0.2-native-current/Jaso NFC.app");
        assert_eq!(prune_releases(temp.path(), &current).unwrap(), 1);
        assert!(old.exists());
        assert!(current.exists());
        assert_eq!(prune_releases(temp.path(), &current).unwrap(), 0);
    }

    #[test]
    fn incomplete_recovery_does_not_replace_the_latest_rollback_release() {
        let temp = tempfile::tempdir().unwrap();
        let releases = temp.path().join("releases");
        for (index, name) in [
            "0.1-native-obsolete",
            "0.1-native-recovery",
            "0.2-native-rollback",
            "0.2-native-current",
        ]
        .into_iter()
        .enumerate()
        {
            let release = releases.join(name);
            fs::create_dir_all(release.join("Jaso NFC.app")).unwrap();
            fs::File::open(&release)
                .unwrap()
                .set_modified(
                    SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(index as u64 + 1),
                )
                .unwrap();
        }
        let recovery = releases.join("0.1-native-recovery/Jaso NFC.app");
        let current = releases.join("0.2-native-current/Jaso NFC.app");
        let pending = temp.path().join("backups/native-install-pending");
        fs::create_dir_all(&pending).unwrap();
        fs::write(
            pending.join("installation.json"),
            serde_json::to_vec(&serde_json::json!({"new_app": recovery})).unwrap(),
        )
        .unwrap();
        // Reinstalling the same current bundle records current -> current,
        // while the separately retained rollback version is still needed.
        let reinstalled = temp.path().join("backups/native-install-reinstalled");
        fs::create_dir_all(&reinstalled).unwrap();
        fs::write(
            reinstalled.join("installation.json"),
            serde_json::to_vec(&serde_json::json!({"new_app": current, "previous_app": current}))
                .unwrap(),
        )
        .unwrap();
        mark_completed(&reinstalled).unwrap();

        prune_releases(temp.path(), &current).unwrap();

        assert!(current.exists());
        assert!(recovery.exists());
        assert!(
            releases.join("0.2-native-rollback/Jaso NFC.app").exists(),
            "an incomplete recovery reference must not consume the rollback slot"
        );
        assert!(!releases.join("0.1-native-obsolete").exists());
    }

    #[test]
    fn incomplete_release_directory_cannot_consume_the_rollback_slot() {
        let temp = tempfile::tempdir().unwrap();
        let releases = temp.path().join("releases");
        let current = releases.join("0.2-native-current/Jaso NFC.app");
        let rollback = releases.join("0.1-native-rollback/Jaso NFC.app");
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&rollback).unwrap();
        let incomplete = releases.join("0.3-native-incomplete");
        fs::create_dir_all(&incomplete).unwrap();
        fs::File::open(&incomplete)
            .unwrap()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        prune_releases(temp.path(), &current).unwrap();
        assert!(
            rollback.exists(),
            "a directory without an app is not a rollback release"
        );
        assert!(current.exists());
        assert!(incomplete.exists());
    }
}
