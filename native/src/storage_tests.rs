use super::*;
#[test]
fn allocation_categories_explain_index_history_and_retained_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    let config = Config {
        state_dir: temp.path().to_string_lossy().into(),
        ..Config::default()
    };
    for name in [
        "state/index.sqlite3",
        "state/history.sqlite3",
        "logs/renames.jsonl",
        "backups/old",
        "releases/app",
    ] {
        let path = temp.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; 8192]).unwrap();
    }
    let value = snapshot(&config).unwrap();
    let categories = value["categories"]
        .as_array()
        .expect("storage category breakdown");
    for role in ["index", "history", "logs", "backups", "releases"] {
        assert!(
            categories
                .iter()
                .any(|c| c["role"] == role && c["allocated_bytes"].as_u64().unwrap() >= 8192),
            "missing {role}"
        );
    }
    assert_eq!(
        categories
            .iter()
            .map(|c| c["allocated_bytes"].as_u64().unwrap())
            .sum::<u64>(),
        value["allocated_bytes"].as_u64().unwrap()
    );
}
#[test]
fn category_database_paths_do_not_follow_an_intermediate_state_symlink() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("index.sqlite3"), vec![0; 65536]).unwrap();
    symlink(outside.path(), temp.path().join("state")).unwrap();
    let config = Config {
        state_dir: temp.path().to_string_lossy().into(),
        ..Config::default()
    };
    let value = snapshot(&config).unwrap();
    let categories = value["categories"].as_array().unwrap();
    assert_eq!(
        categories.iter().find(|c| c["role"] == "index").unwrap()["allocated_bytes"],
        0
    );
    assert_eq!(
        categories
            .iter()
            .map(|c| c["allocated_bytes"].as_u64().unwrap())
            .sum::<u64>(),
        value["allocated_bytes"].as_u64().unwrap()
    );
}
fn reading(id: u64, total: u64, available: u64) -> VolumeReading {
    VolumeReading {
        id: Some(id),
        total: Some(total),
        available: Some(available),
        error: None,
    }
}
#[test]
fn capacity_status_has_exact_critical_and_warning_boundaries() {
    assert_eq!(evaluate(&reading(1, 10000 * MIB, 255 * MIB)), "critical");
    assert_eq!(evaluate(&reading(1, 10000 * MIB, 256 * MIB)), "warning");
    assert_eq!(evaluate(&reading(1, 10000 * MIB, 1023 * MIB)), "warning");
    assert_eq!(evaluate(&reading(1, 10000 * MIB, 1024 * MIB)), "ok");
    assert_eq!(evaluate(&reading(1, 100000 * MIB, 4000 * MIB)), "warning");
    assert_eq!(evaluate(&reading(1, 100000 * MIB, 5000 * MIB)), "ok");
}
#[test]
fn paths_on_same_volume_are_counted_once_and_unknown_is_explicit() {
    let value = aggregate(vec![
        ("state".into(), reading(1, 10000 * MIB, 4000 * MIB)),
        ("logs".into(), reading(1, 10000 * MIB, 4000 * MIB)),
    ]);
    assert_eq!(value["volumes"].as_array().unwrap().len(), 1);
    assert_eq!(value["volumes"][0]["paths"].as_array().unwrap().len(), 2);
    assert_eq!(value["status"], "ok");
    let unknown = VolumeReading {
        id: Some(2),
        total: None,
        available: None,
        error: Some("volume unavailable".into()),
    };
    assert_eq!(evaluate(&unknown), "unknown");
    let value = aggregate(vec![("external".into(), unknown)]);
    assert_eq!(value["status"], "unknown");
    assert!(value["volumes"][0]["available_bytes"].is_null());
}
#[test]
fn critical_volume_controls_aggregate_even_when_other_volume_is_unknown() {
    let value = aggregate(vec![
        ("logs".into(), reading(2, 10000 * MIB, 128 * MIB)),
        (
            "state".into(),
            VolumeReading {
                id: Some(1),
                total: None,
                available: None,
                error: Some("unavailable".into()),
            },
        ),
    ]);
    assert_eq!(value["status"], "critical");
}
#[test]
fn allocated_usage_deduplicates_overlapping_roots_and_never_follows_symlinks() {
    use std::os::unix::fs::{MetadataExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let logs = state.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let journal = logs.join("journal");
    std::fs::write(&journal, vec![b'x'; 8192]).unwrap();
    std::fs::hard_link(&journal, state.join("journal-link")).unwrap();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, vec![b'x'; 65536]).unwrap();
    symlink(&outside, logs.join("outside-link")).unwrap();
    let config = Config {
        state_dir: state.to_string_lossy().into(),
        log_dir: Some(logs.to_string_lossy().into()),
        ..Config::default()
    };
    let value = snapshot(&config).unwrap();
    let expected = std::fs::symlink_metadata(&state).unwrap().blocks() * 512
        + std::fs::symlink_metadata(&logs).unwrap().blocks() * 512
        + std::fs::symlink_metadata(&journal).unwrap().blocks() * 512
        + std::fs::symlink_metadata(logs.join("outside-link"))
            .unwrap()
            .blocks()
            * 512;
    assert_eq!(value["allocated_bytes"].as_u64(), Some(expected));
    assert_eq!(value["volumes"].as_array().unwrap().len(), 1);
}
#[test]
fn write_guard_blocks_only_critical_known_capacity() {
    assert!(ensure_readings(vec![("state".into(), reading(1, 10000 * MIB, 255 * MIB))]).is_err());
    assert!(ensure_readings(vec![("state".into(), reading(1, 10000 * MIB, 256 * MIB))]).is_ok());
    assert!(
        ensure_readings(vec![(
            "state".into(),
            VolumeReading {
                id: Some(1),
                total: None,
                available: None,
                error: Some("unavailable".into())
            }
        )])
        .is_ok()
    );
}
#[test]
fn an_unavailable_second_path_cannot_erase_a_known_critical_shared_volume() {
    let value = aggregate(vec![
        ("logs".into(), reading(1, 10000 * MIB, 128 * MIB)),
        (
            "state".into(),
            VolumeReading {
                id: Some(1),
                total: None,
                available: None,
                error: Some("unavailable".into()),
            },
        ),
    ]);
    assert_eq!(value["volumes"].as_array().unwrap().len(), 1);
    assert_eq!(value["status"], "critical");
}
#[test]
fn symlink_directory_root_is_reported_unknown_without_enumerating_its_target() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    let target = temp.path().join("outside");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("foreign"), vec![b'x'; 8192]).unwrap();
    let logs = state.join("logs");
    symlink(&target, &logs).unwrap();
    let config = Config {
        state_dir: state.to_string_lossy().into(),
        log_dir: Some(logs.to_string_lossy().into()),
        ..Config::default()
    };
    let value = snapshot(&config).unwrap();
    assert_eq!(value["status"], "unknown");
    assert!(
        value["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["status"] == "unknown" && v["available_bytes"].is_null())
    );
}
