use super::*;
use crate::journal::Journal;
use std::os::unix::fs::MetadataExt;

fn fixture() -> (tempfile::TempDir, Config) {
    let temp = tempfile::tempdir().unwrap();
    let config = Config {
        state_dir: temp.path().join("state").to_string_lossy().into(),
        log_dir: Some(temp.path().join("logs").to_string_lossy().into()),
        roots: vec![temp.path().to_string_lossy().into()],
        apply: true,
        ..Config::default()
    };
    crate::control::prepare_directories(&config).unwrap();
    (temp, config)
}
fn emit(config: &Config, id: &str, old: &str, new: &str, ts: &str) {
    let mut log = Journal::standard(&log_path(config)).unwrap();
    log.emit(&json!({"operation_id":id,"dir":config.roots[0],"old":old,"new":new,"ts":ts,"identity":[1,2],"type":"file","status":"renamed"})).unwrap();
}
#[test]
fn history_migration_reclaims_duplicate_payloads_and_preserves_later_undo_checks() {
    let (_temp, config) = fixture();
    let chosen = actual(&config, "chosen", "before", "after");
    let path = config.state_path("history.sqlite3");
    // Recreate the previous cache shape, including its duplicate payload pages.
    fs::remove_file(&path).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE history_events(id TEXT PRIMARY KEY,record TEXT NOT NULL,seq INTEGER NOT NULL); CREATE TABLE history_operations(id TEXT PRIMARY KEY,record TEXT NOT NULL,seq INTEGER NOT NULL,confirmed INTEGER NOT NULL,day TEXT NOT NULL,restored INTEGER NOT NULL DEFAULT 0,undo_request TEXT);").unwrap();
    let padding = "x".repeat(1024 * 1024);
    for table in ["history_events", "history_operations"] {
        db.execute(
            &format!(
                "INSERT INTO {table}(id,record,seq{}) VALUES('obsolete',?1,1{})",
                if table == "history_operations" {
                    ",confirmed,day"
                } else {
                    ""
                },
                if table == "history_operations" {
                    ",1,''"
                } else {
                    ""
                }
            ),
            [&padding],
        )
        .unwrap();
    }
    drop(db);
    let bloated_size = fs::metadata(&path).unwrap().len();
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    log.emit(&json!({"operation_id":"later-undo","restores_operation_id":"other","dir":config.roots[0],"old":"after","new":"third","status":"reverted","identity":[1,2]})).unwrap();
    let page = list(&config, &HistoryQuery::default()).unwrap();
    assert_eq!(
        page.total, 1,
        "undo events stay hidden from operation history"
    );
    assert!(
        !preview(&config, &chosen.id, &page.items[0].revision)
            .unwrap()
            .can_restore
    );
    let db = Connection::open(&path).unwrap();
    let payload_tables: i64 = db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('history_operations','history_events')", [], |r| r.get(0)).unwrap();
    assert_eq!(
        payload_tables, 1,
        "a journal JSON payload has only one cache owner"
    );
    assert!(
        fs::metadata(&path).unwrap().len() < bloated_size / 2,
        "migration must return freed duplicate pages to disk"
    );
}
#[test]
fn confirmed_payload_and_later_recovery_dependency_are_both_preserved() {
    let (_temp, config) = fixture();
    emit(
        &config,
        "other",
        "unrelated-before",
        "unrelated-after",
        "2026-01-01T00:00:00",
    );
    let chosen = actual(&config, "chosen", "before", "after");
    assert!(
        preview(&config, &chosen.id, &chosen.revision)
            .unwrap()
            .can_restore
    );
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    log.emit(&json!({"operation_id":"other","dir":config.roots[0],"old":"after","new":"elsewhere","status":"error","recovery_required":true,"identity":[1,2]})).unwrap();
    assert!(
        !preview(&config, &chosen.id, &chosen.revision)
            .unwrap()
            .can_restore,
        "later recovery dependencies must survive confirmed-payload deduplication"
    );
    let confirmed = list(
        &config,
        &HistoryQuery {
            search: "unrelated-before".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(confirmed.items[0].result, "renamed");
}

#[test]
fn migration_repairs_an_old_operations_table_without_its_event_table() {
    let (_temp, config) = fixture();
    emit(&config, "kept", "before", "after", "2026-01-01T00:00:00");
    let db = Connection::open(config.state_path("history.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE history_operations(id TEXT PRIMARY KEY,record TEXT NOT NULL,seq INTEGER NOT NULL,confirmed INTEGER NOT NULL,day TEXT NOT NULL,restored INTEGER NOT NULL DEFAULT 0,undo_request TEXT);").unwrap();
    drop(db);
    assert_eq!(list(&config, &HistoryQuery::default()).unwrap().total, 1);
}
#[test]
fn maintenance_bounds_completed_archives_and_keeps_pending_and_latest_recovery() {
    let (_temp, config) = fixture();
    let path = log_path(&config);
    let mut log = Journal::new(&path, 1, 1024, 3).unwrap();
    for id in ["expired", "pending-success"] {
        log.emit(&json!({"operation_id":id,"dir":config.roots[0],"old":id,"new":format!("{id}-new"),"status":"renamed"})).unwrap();
    }
    for n in 0..3 {
        log.emit(&json!({"operation_id":"unresolved","dir":config.roots[0],"old":"recover-before","new":"recover-after","status":"error","recovery_required":true,"attempt":n})).unwrap();
    }
    log.emit(&json!({"operation_id":"resolved","dir":config.roots[0],"old":"resolved-before","new":"resolved-after","status":"error","recovery_required":true})).unwrap();
    log.emit(&json!({"operation_id":"resolved","dir":config.roots[0],"old":"resolved-before","new":"resolved-after","status":"renamed","recovery_required":true})).unwrap();
    for id in ["recent-one", "recent-two"] {
        log.emit(&json!({"operation_id":id,"dir":config.roots[0],"old":id,"new":format!("{id}-new"),"status":"renamed"})).unwrap();
    }
    drop(log);
    let pending = b"{\"operation_id\":\"pending-success\"}\n";
    fs::write(config.state_path("pending.json"), pending).unwrap();
    maintain(&config).unwrap();
    let segments = fs::read_dir(journal::suffix(&path, ".history"))
        .unwrap()
        .count();
    assert_eq!(
        segments, 2,
        "only the recent completed archive suffix remains"
    );
    let records = journal::journal_records(&path).unwrap();
    assert_eq!(records.len(), 4);
    assert!(
        !records
            .iter()
            .any(|row| row["operation_id"] == "expired" || row["operation_id"] == "resolved")
    );
    assert_eq!(
        records
            .iter()
            .find(|row| row["operation_id"] == "unresolved")
            .unwrap()["attempt"],
        2
    );
    assert_eq!(
        records
            .iter()
            .find(|row| row["operation_id"] == "pending-success")
            .unwrap()["history_retention_incomplete"],
        true
    );
    assert_eq!(
        fs::read(config.state_path("pending.json")).unwrap(),
        pending
    );
    let before = fs::metadata(config.state_path("history.sqlite3")).unwrap();
    maintain(&config).unwrap();
    let after = fs::metadata(config.state_path("history.sqlite3")).unwrap();
    assert_eq!(
        (before.mtime(), before.mtime_nsec()),
        (after.mtime(), after.mtime_nsec()),
        "unchanged maintenance must reuse the cache"
    );
}

#[test]
fn maintenance_keeps_archives_when_expired_evidence_is_corrupt() {
    let (_temp, config) = fixture();
    let path = log_path(&config);
    let directory = journal::suffix(&path, ".history");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("0001.jsonl.gz"), b"damaged gzip").unwrap();
    fs::write(directory.join("0002.jsonl"), b"{\"status\":\"renamed\"}\n").unwrap();
    fs::write(directory.join("0003.jsonl"), b"{\"status\":\"renamed\"}\n").unwrap();
    assert!(maintain(&config).is_err());
    assert_eq!(fs::read_dir(directory).unwrap().count(), 3);
}

#[test]
fn maintenance_expires_only_completed_mailbox_pairs() {
    let (_temp, config) = fixture();
    let completed = config.state_path("history-requests/completed");
    fs::create_dir_all(&completed).unwrap();
    for n in 0..130 {
        let id = uuid::Uuid::new_v4().to_string();
        let request = RestoreRequest {
            request_id: id.clone(),
            operation_id: format!("op-{n}"),
            revision: "a".repeat(64),
        };
        let result = RestoreResult {
            request_id: id.clone(),
            operation_id: request.operation_id.clone(),
            state: if n == 0 {
                "recovery_required"
            } else {
                "restored"
            }
            .into(),
            message: String::new(),
        };
        fs::write(
            completed.join(format!("{id}.request.json")),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        fs::write(
            completed.join(format!("{id}.result.json")),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
    }
    maintain(&config).unwrap();
    assert_eq!(
        fs::read_dir(&completed).unwrap().count(),
        258,
        "128 terminal pairs plus unresolved evidence"
    );
}
#[test]
fn maintenance_rejects_configured_root_symlinks_before_writing() {
    use std::os::unix::fs::symlink;
    for surface in ["state", "logs", "state-inner"] {
        let (_temp, config) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let path = match surface {
            "state" => PathBuf::from(&config.state_dir),
            "logs" => PathBuf::from(config.logs()),
            _ => config
                .state_path("history.sqlite3")
                .parent()
                .unwrap()
                .to_path_buf(),
        };
        fs::remove_dir_all(&path).unwrap();
        symlink(outside.path(), &path).unwrap();
        assert!(
            maintain(&config).is_err(),
            "{surface}: symlink root was accepted"
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}

#[test]
fn maintenance_rejects_mailbox_symlinks_before_any_retention_deletion() {
    use std::os::unix::fs::symlink;
    for surface in ["history-requests", "history-requests/completed"] {
        let (_temp, config) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let foreign_completed = if surface == "history-requests" {
            outside.path().join("completed")
        } else {
            outside.path().to_path_buf()
        };
        fs::create_dir_all(&foreign_completed).unwrap();
        for _ in 0..129 {
            let id = uuid::Uuid::new_v4().to_string();
            let request = RestoreRequest {
                request_id: id.clone(),
                operation_id: "owned-elsewhere".into(),
                revision: "a".repeat(64),
            };
            let result = RestoreResult {
                request_id: id.clone(),
                operation_id: request.operation_id.clone(),
                state: "restored".into(),
                message: String::new(),
            };
            fs::write(
                foreign_completed.join(format!("{id}.request.json")),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
            fs::write(
                foreign_completed.join(format!("{id}.result.json")),
                serde_json::to_vec(&result).unwrap(),
            )
            .unwrap();
        }
        let linked = config.state_path(surface);
        fs::create_dir_all(linked.parent().unwrap()).unwrap();
        symlink(outside.path(), &linked).unwrap();
        let archives = journal::suffix(&log_path(&config), ".history");
        fs::create_dir(&archives).unwrap();
        for name in ["0001.jsonl", "0002.jsonl", "0003.jsonl"] {
            fs::write(archives.join(name), b"{\"status\":\"renamed\"}\n").unwrap();
        }
        let result = maintain(&config);
        assert_eq!(
            fs::read_dir(&foreign_completed).unwrap().count(),
            258,
            "{surface}: foreign receipts were deleted"
        );
        assert_eq!(
            fs::read_dir(&archives).unwrap().count(),
            3,
            "mailbox preflight must precede archive retention"
        );
        assert!(result.is_err());
    }
}
#[test]
fn confirmed_changes_are_deduplicated_paged_filtered_and_counted_after_restore() {
    let (_temp, config) = fixture();
    let today = local_day();
    for n in 0..130 {
        emit(
            &config,
            &format!("op-{n}"),
            &format!("old-{n}"),
            &format!("new-{n}"),
            &format!("{today}T12:00:00"),
        );
    }
    emit(
        &config,
        "op-0",
        "old-0",
        "new-0",
        &format!("{today}T12:00:00"),
    );
    let page = list(
        &config,
        &HistoryQuery {
            limit: 20,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page.items.len(), 20);
    assert_eq!(page.total, 130);
    assert_eq!(page.today_count, 130);
    let item = page.items[0].clone();
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    log.emit(&json!({"operation_id":"undo-one","restores_operation_id":item.id,"dir":config.roots[0],"old":item.new_name,"new":item.old_name,"ts":format!("{today}T13:00:00"),"status":"reverted","identity":[1,2]})).unwrap();
    let page = list(
        &config,
        &HistoryQuery {
            search: item.old_name.clone(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].restored);
    assert_eq!(page.today_count, 130);
    assert_ne!(page.items[0].revision, item.revision);
    assert_eq!(
        list(
            &config,
            &HistoryQuery {
                result: Some("restored".into()),
                ..Default::default()
            }
        )
        .unwrap()
        .total,
        1
    );
}
#[test]
fn recovery_only_record_is_not_a_confirmed_change_and_success_replaces_it() {
    let (_temp, config) = fixture();
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    let mut record = json!({"operation_id":"recover","dir":config.roots[0],"old":"before","new":"after","identity":[1,2],"ts":format!("{}T10:00:00",local_day()),"status":"error","recovery_required":true});
    log.emit(&record).unwrap();
    let page = list(&config, &HistoryQuery::default()).unwrap();
    assert_eq!(page.today_count, 0);
    assert_eq!(page.items[0].result, "recovery_required");
    record["status"] = json!("renamed");
    record["recovered"] = json!(true);
    log.emit(&record).unwrap();
    let page = list(&config, &HistoryQuery::default()).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.today_count, 1);
    assert_eq!(page.items[0].result, "renamed");
}
fn actual(config: &Config, id: &str, old: &str, new: &str) -> HistoryItem {
    let path = Path::new(&config.roots[0]).join(new);
    fs::write(&path, b"owned content").unwrap();
    let meta = fs::symlink_metadata(&path).unwrap();
    let mut log = Journal::standard(&log_path(config)).unwrap();
    log.emit(&json!({"operation_id":id,"dir":config.roots[0],"old":old,"new":new,"identity":[meta.dev(),meta.ino()],"ts":format!("{}T10:00:00",local_day()),"type":"file","status":"renamed"})).unwrap();
    list(
        config,
        &HistoryQuery {
            search: new.into(),
            ..Default::default()
        },
    )
    .unwrap()
    .items
    .remove(0)
}
#[test]
fn preview_rejects_foreign_inode_collision_stale_revision_and_later_dependencies() {
    let (temp, config) = fixture();
    let item = actual(&config, "chosen", "before", "after");
    assert!(
        preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
    assert!(!preview(&config, &item.id, "stale").unwrap().can_restore);
    fs::write(temp.path().join("before"), b"collision").unwrap();
    assert!(
        !preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
    fs::remove_file(temp.path().join("before")).unwrap();
    fs::rename(temp.path().join("after"), temp.path().join("held")).unwrap();
    fs::write(temp.path().join("after"), b"foreign").unwrap();
    assert!(
        !preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
    fs::remove_file(temp.path().join("after")).unwrap();
    fs::rename(temp.path().join("held"), temp.path().join("after")).unwrap();
    emit(
        &config,
        "later",
        "after",
        "third",
        &format!("{}T11:00:00", local_day()),
    );
    assert!(
        !preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
}
#[test]
fn directory_dependency_rejects_restore_even_when_directory_identity_matches() {
    let (temp, config) = fixture();
    let path = temp.path().join("after-dir");
    fs::create_dir(&path).unwrap();
    let info = fs::metadata(&path).unwrap();
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    log.emit(&json!({"operation_id":"directory","dir":temp.path(),"old":"before-dir","new":"after-dir","identity":[info.dev(),info.ino()],"type":"dir","status":"renamed","ts":format!("{}T10:00:00",local_day())})).unwrap();
    let item = list(&config, &HistoryQuery::default())
        .unwrap()
        .items
        .remove(0);
    assert!(
        preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
    log.emit(&json!({"operation_id":"child","dir":path,"old":"before-child","new":"after-child","identity":[1,2],"status":"renamed","ts":format!("{}T11:00:00",local_day())})).unwrap();
    assert!(
        !preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
}
#[test]
fn queue_restores_only_selected_item_and_is_idempotent_after_double_click() {
    let (temp, config) = fixture();
    let item = actual(&config, "selected", "before", "after");
    let other = actual(&config, "other", "other-before", "other-after");
    let request = RestoreRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        operation_id: item.id.clone(),
        revision: item.revision.clone(),
    };
    assert_eq!(request_restore(&config, &request).unwrap().state, "queued");
    assert_eq!(request_restore(&config, &request).unwrap().state, "queued");
    let mut normalizer = crate::normalizer::Normalizer::new(
        config.policy(),
        Some(log_path(&config)),
        None,
        Some(config.state_path("pending.json")),
        true,
    )
    .unwrap();
    let _lock = crate::control::RuntimeLock::acquire(&config, std::time::Duration::ZERO).unwrap();
    process_requests(&config, &mut normalizer).unwrap();
    assert_eq!(
        restore_result(&config, &request.request_id).unwrap().state,
        "restored"
    );
    assert_eq!(
        fs::read(temp.path().join("before")).unwrap(),
        b"owned content"
    );
    assert_eq!(fs::read(&other.new_path).unwrap(), b"owned content");
    assert!(!Path::new(&other.old_path).exists());
    assert_eq!(
        request_restore(&config, &request).unwrap().state,
        "restored"
    );
    process_requests(&config, &mut normalizer).unwrap();
    let duplicate = RestoreRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        ..request
    };
    request_restore(&config, &duplicate).unwrap();
    process_requests(&config, &mut normalizer).unwrap();
    assert_eq!(
        restore_result(&config, &duplicate.request_id)
            .unwrap()
            .state,
        "rejected"
    );
    let rows = journal::journal_records(&log_path(&config)).unwrap();
    assert_eq!(rows.iter().filter(|r| r["status"] == "reverted").count(), 1);
    assert_eq!(today_count(&config).unwrap(), 2);
    assert!(
        !request_path(&config, &duplicate.request_id, "request")
            .unwrap()
            .exists(),
        "terminal requests must leave the bounded active mailbox"
    );
}
#[test]
fn request_id_cannot_be_rebound_or_escape_mailbox_and_pending_never_reports_done() {
    let (_temp, config) = fixture();
    let item = actual(&config, "chosen", "before", "after");
    let request = RestoreRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        operation_id: item.id,
        revision: item.revision,
    };
    request_restore(&config, &request).unwrap();
    assert!(
        request_restore(
            &config,
            &RestoreRequest {
                operation_id: "different".into(),
                ..request.clone()
            }
        )
        .is_err()
    );
    assert!(restore_result(&config, "../escape").is_err());
    fs::write(
        config.state_path("pending.json"),
        b"incomplete recovery evidence",
    )
    .unwrap();
    let mut normalizer = crate::normalizer::Normalizer::new(
        config.policy(),
        Some(log_path(&config)),
        None,
        Some(config.state_path("pending.json")),
        true,
    )
    .unwrap();
    let _lock = crate::control::RuntimeLock::acquire(&config, std::time::Duration::ZERO).unwrap();
    process_requests(&config, &mut normalizer).unwrap();
    assert_eq!(
        restore_result(&config, &request.request_id).unwrap().state,
        "recovery_required"
    );
    assert!(config.state_path("pending.json").exists());
}
#[test]
fn rotated_plain_gzip_history_is_rebuilt_without_duplicates_and_reads_are_cached() {
    let (_temp, config) = fixture();
    let path = log_path(&config);
    let mut log = Journal::new(&path, 200, 1000, 3).unwrap();
    for n in 0..12 {
        log.emit(&json!({"operation_id":format!("rotation-{n}"),"dir":config.roots[0],"old":format!("old-{n}"),"new":format!("new-{n}"),"status":"renamed","ts":format!("{}T12:00:00",local_day())})).unwrap();
    }
    drop(log);
    assert_eq!(list(&config, &HistoryQuery::default()).unwrap().total, 12);
    let db_path = config.state_path("history.sqlite3");
    let before = fs::metadata(&db_path).unwrap();
    assert_eq!(today_count(&config).unwrap(), 12);
    let after = fs::metadata(&db_path).unwrap();
    assert_eq!(
        (before.mtime(), before.mtime_nsec()),
        (after.mtime(), after.mtime_nsec())
    );
    fs::remove_file(&db_path).unwrap();
    assert_eq!(list(&config, &HistoryQuery::default()).unwrap().total, 12);
}
#[test]
fn cloud_provider_records_and_unconfirmed_pending_are_never_previewed_as_safe() {
    let (_temp, config) = fixture();
    actual(&config, "provider", "before", "after");
    let mut rows = journal::journal_records(&log_path(&config)).unwrap();
    rows[0]["identity_finalization_unavailable"] = json!(true);
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    log.emit(&rows[0]).unwrap();
    let item = list(&config, &HistoryQuery::default())
        .unwrap()
        .items
        .remove(0);
    assert!(
        !preview(&config, &item.id, &item.revision)
            .unwrap()
            .can_restore
    );
}
#[test]
fn timezone_offsets_are_converted_to_local_calendar_day() {
    let epoch: libc::time_t = 0;
    assert_eq!(timestamp_day("1970-01-01T00:00:00Z"), local_date(epoch));
    assert_eq!(
        timestamp_day("1970-01-01T09:00:00+09:00"),
        local_date(epoch)
    );
    assert_eq!(
        timestamp_day("1969-12-31T19:00:00-05:00"),
        local_date(epoch)
    );
    assert_eq!(timestamp_day("1970-01-01T00:00:00.123Z"), local_date(epoch));
}
#[test]
fn completed_journal_request_is_recognized_after_result_write_interruption() {
    let (_temp, config) = fixture();
    let item = actual(&config, "interrupted", "before", "after");
    let request = RestoreRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        operation_id: item.id.clone(),
        revision: item.revision,
    };
    request_restore(&config, &request).unwrap();
    // Simulate a process dying after the normalizer's durable success record,
    // before the mailbox acknowledgement. No second filesystem call is needed.
    fs::rename(&item.new_path, &item.old_path).unwrap();
    let mut log = Journal::standard(&log_path(&config)).unwrap();
    log.emit(&json!({"operation_id":request.request_id,"restores_operation_id":item.id,"dir":config.roots[0],"old":item.new_name,"new":item.old_name,"status":"reverted"})).unwrap();
    let mut normalizer = crate::normalizer::Normalizer::new(
        config.policy(),
        Some(log_path(&config)),
        None,
        Some(config.state_path("pending.json")),
        true,
    )
    .unwrap();
    let _lock = crate::control::RuntimeLock::acquire(&config, std::time::Duration::ZERO).unwrap();
    process_requests(&config, &mut normalizer).unwrap();
    assert_eq!(
        restore_result(&config, &request.request_id).unwrap().state,
        "restored"
    );
    assert_eq!(
        journal::journal_records(&log_path(&config))
            .unwrap()
            .iter()
            .filter(|r| r["status"] == "reverted")
            .count(),
        1
    );
}
#[test]
fn cached_status_count_never_waits_for_the_mutation_journal_lock() {
    let (_temp, config) = fixture();
    assert_eq!(cached_today_count(&config).unwrap(), None);
    emit(
        &config,
        "first",
        "one-before",
        "one-after",
        &format!("{}T10:00:00", local_day()),
    );
    assert_eq!(today_count(&config).unwrap(), 1);
    emit(
        &config,
        "second",
        "two-before",
        "two-after",
        &format!("{}T11:00:00", local_day()),
    );
    let log = log_path(&config);
    let held = JournalLocks::acquire(&[&log]).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let copied = config.clone();
    let thread = std::thread::spawn(move || {
        send.send(cached_today_count(&copied)).unwrap();
    });
    let result = receive.recv_timeout(std::time::Duration::from_secs(1));
    drop(held);
    thread.join().unwrap();
    assert_eq!(
        result
            .expect("status must not wait for the rename journal")
            .unwrap(),
        Some(1)
    );
    assert_eq!(today_count(&config).unwrap(), 2);
    assert_eq!(cached_today_count(&config).unwrap(), Some(2));
}
