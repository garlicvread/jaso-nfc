//! In-memory worker observations. Reading a snapshot never advances progress.
use crate::model::now;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Instant,
};

pub const MAX_EVENTS: usize = 200;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const EVENT_BYTES: usize = 32 * 1024;
const MAX_STRING_BYTES: usize = 768;

fn bounded(value: &str) -> String {
    if value.len() <= MAX_STRING_BYTES {
        return value.into();
    }
    let mut end = MAX_STRING_BYTES - 3;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
#[derive(Default, Serialize)]
struct Counters {
    observed: u64,
    processed: u64,
    renamed: u64,
    deferred: u64,
    errors: u64,
    io_completed: u64,
}
#[derive(Serialize)]
struct Scope {
    id: String,
    observed: u64,
    processed: u64,
    total: Option<u64>,
}
#[derive(Serialize)]
struct State {
    session_id: String,
    state: String,
    phase: String,
    phase_started_at: f64,
    last_progress_at: Option<f64>,
    #[serde(skip)]
    phase_started: Instant,
    #[serde(skip)]
    last_progress: Option<Instant>,
    scope_path: Option<String>,
    item_path: Option<String>,
    issue: Option<Value>,
    scope: Option<Scope>,
    counters: Counters,
    events: VecDeque<Value>,
    dropped_events: u64,
    #[serde(skip)]
    event_bytes: usize,
    #[serde(skip)]
    next_event: u64,
}
impl State {
    fn event(
        &mut self,
        kind: &str,
        phase: &str,
        path: Option<&str>,
        reason: Option<&str>,
        errno: Option<i32>,
    ) {
        let display_path = json!(path.map(bounded));
        let display_reason = json!(reason.map(bounded));
        // Display truncation cannot establish identity. Keep changed causes
        // distinct, and never revive a row whose resolution was already proven.
        let existing = if matches!(kind, "error" | "deferred")
            && path.is_none_or(|path| path.len() <= MAX_STRING_BYTES)
            && phase.len() <= MAX_STRING_BYTES
            && reason.is_none_or(|reason| reason.len() <= MAX_STRING_BYTES)
        {
            self.events.iter().position(|event| {
                event["path"] == display_path
                    && event["path_truncated"] != true
                    && event["phase_truncated"] != true
                    && event["reason_truncated"] != true
                    && event["kind"] == kind
                    && event["phase"] == phase
                    && event["reason"] == display_reason
                    && event["errno"] == json!(errno)
                    && event["resolved_at"].is_null()
            })
        } else {
            None
        };
        let time = now();
        if let Some(index) = existing {
            let event = &mut self.events[index];
            self.event_bytes -= serde_json::to_vec(event).unwrap().len();
            event["at"] = json!(time);
            event["occurrences"] = json!(event["occurrences"].as_u64().unwrap().saturating_add(1));
            event["scope_id"] = json!(self.scope.as_ref().map(|scope| &scope.id));
            self.event_bytes += serde_json::to_vec(event).unwrap().len();
        } else {
            self.next_event += 1;
            let mut event = json!({
                "sequence": self.next_event, "at": time, "first_at": time,
                "kind": kind, "phase": bounded(phase), "path": display_path,
                "scope_id": self.scope.as_ref().map(|scope| &scope.id),
                "reason": display_reason, "errno": errno, "occurrences": 1,
                "resolved_at": null, "resolution": null
            });
            if path.is_some_and(|path| path.len() > MAX_STRING_BYTES) {
                event["path_truncated"] = json!(true);
            }
            if phase.len() > MAX_STRING_BYTES {
                event["phase_truncated"] = json!(true);
            }
            if reason.is_some_and(|reason| reason.len() > MAX_STRING_BYTES) {
                event["reason_truncated"] = json!(true);
            }
            self.event_bytes += serde_json::to_vec(&event).unwrap().len();
            self.events.push_back(event);
        }
        self.trim_events();
    }
    fn resolve(&mut self, phase: &str, path: Option<&str>, resolution: &str) {
        if path.is_some_and(|path| path.len() > MAX_STRING_BYTES)
            || phase.len() > MAX_STRING_BYTES
            || !matches!(
                resolution,
                "checked" | "renamed" | "restored" | "absent" | "no_longer_needed"
            )
        {
            return;
        }
        let time = now();
        let path = json!(path);
        for event in &mut self.events {
            if event["path"] == path
                && event["path_truncated"] != true
                && event["phase_truncated"] != true
                && event["phase"] == phase
                && matches!(event["kind"].as_str(), Some("error" | "deferred"))
                && event["resolved_at"].is_null()
            {
                self.event_bytes -= serde_json::to_vec(event).unwrap().len();
                event["resolved_at"] = json!(time);
                event["resolution"] = json!(resolution);
                self.event_bytes += serde_json::to_vec(event).unwrap().len();
            }
        }
        self.trim_events();
    }
    fn trim_events(&mut self) {
        while self.events.len() > MAX_EVENTS || self.event_bytes > EVENT_BYTES {
            if let Some(old) = self.events.pop_front() {
                self.event_bytes -= serde_json::to_vec(&old).unwrap().len();
                self.dropped_events += 1;
            }
        }
    }
}
#[derive(Clone)]
pub struct Activity(Arc<Mutex<State>>);
impl Default for Activity {
    fn default() -> Self {
        Self::new()
    }
}
impl Activity {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            session_id: uuid::Uuid::new_v4().to_string(),
            state: "starting".into(),
            phase: "starting".into(),
            phase_started_at: now(),
            last_progress_at: None,
            phase_started: Instant::now(),
            last_progress: None,
            scope_path: None,
            item_path: None,
            issue: None,
            scope: None,
            counters: Counters::default(),
            events: VecDeque::new(),
            dropped_events: 0,
            event_bytes: 0,
            next_event: 0,
        })))
    }
    fn update(&self, change: impl FnOnce(&mut State)) {
        // This mutex protects only a bounded in-memory value, never I/O.
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        change(&mut state);
    }
    pub fn snapshot(&self) -> Value {
        let state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let mut value = serde_json::to_value(&*state).unwrap();
        value["snapshot_generated_at"] = json!(now());
        value["phase_elapsed_seconds"] = json!(state.phase_started.elapsed().as_secs_f64());
        value["last_progress_age_seconds"] =
            json!(state.last_progress.map(|time| time.elapsed().as_secs_f64()));
        value
    }
    pub fn renamed_count(&self) -> u64 {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .counters
            .renamed
    }
    pub fn select_scope(
        &self,
        id: &str,
        path: &str,
        observed: u64,
        processed: u64,
        total: Option<u64>,
    ) {
        self.update(|s| {
            s.scope = Some(Scope {
                id: bounded(id),
                observed,
                processed,
                total,
            });
            s.scope_path = Some(bounded(path));
            s.item_path = None;
            s.state = "processing".into();
            s.phase = "opening_directory".into();
            s.phase_started_at = now();
            s.phase_started = Instant::now();
        });
    }
    pub fn scope_counts(&self, observed: u64, processed: u64, total: Option<u64>) {
        self.update(|s| {
            if let Some(scope) = &mut s.scope {
                scope.observed = observed;
                scope.processed = processed;
                scope.total = total;
            }
        });
    }
    pub fn set_state(&self, state: &str, phase: &str) {
        self.update(|s| {
            if state == "idle" && s.issue.is_some() {
                s.state = "blocked".into();
                s.item_path = None;
                s.scope_path = None;
                s.scope = None;
                return;
            }
            if s.state != state || s.phase != phase {
                s.state = bounded(state);
                s.phase = bounded(phase);
                s.phase_started_at = now();
                s.phase_started = Instant::now();
            }
            s.item_path = None;
            s.scope_path = None;
            s.scope = None;
        });
    }
    pub fn set_issue(&self, code: &str, message: &str) {
        self.update(|s| {
            s.issue = Some(json!({"code":bounded(code),"message":bounded(message)}));
            s.state = "blocked".into();
            s.phase = bounded(code);
            s.phase_started_at = now();
            s.phase_started = Instant::now();
        });
    }
    pub fn clear_issue(&self, code: &str) {
        self.update(|s| {
            if s.issue.as_ref().is_some_and(|issue| issue["code"] == code) {
                s.issue = None;
            }
        });
    }
    pub fn begin(&self, phase: &str, item: Option<&str>) {
        self.update(|s| {
            s.state = if matches!(
                phase,
                "reading_metadata" | "opening_directory" | "enumerating"
            ) {
                "waiting_metadata"
            } else {
                "processing"
            }
            .into();
            s.phase = bounded(phase);
            s.phase_started_at = now();
            s.phase_started = Instant::now();
            s.item_path = item.map(bounded);
        });
    }
    pub fn finish(&self, _phase: &str, _item: Option<&str>) {
        self.update(|s| {
            s.last_progress_at = Some(now());
            s.last_progress = Some(Instant::now());
            s.counters.io_completed += 1;
            s.state = "processing".into();
        });
    }
    pub fn failed(&self, phase: &str, item: Option<&str>) {
        self.failed_with_reason(phase, item, None, None);
    }
    pub fn failed_with_reason(
        &self,
        phase: &str,
        item: Option<&str>,
        reason: Option<&str>,
        errno: Option<i32>,
    ) {
        self.update(|s| {
            s.counters.errors += 1;
            s.state = "processing".into();
            s.event("error", phase, item, reason, errno);
        });
    }
    pub fn observed(&self, path: &str) {
        self.update(|s| {
            s.last_progress_at = Some(now());
            s.last_progress = Some(Instant::now());
            s.counters.observed += 1;
            s.item_path = Some(bounded(path));
            if let Some(scope) = &mut s.scope {
                scope.observed += 1;
            }
        });
    }
    pub fn resolve(&self, phase: &str, item: Option<&str>, resolution: &str) {
        self.update(|s| s.resolve(phase, item, resolution));
    }
    pub(crate) fn unresolved_processing_paths(&self) -> Vec<String> {
        // Only exact retained identities can be checked against a completed
        // directory snapshot. This read never changes counters or resolution.
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .events
            .iter()
            .filter(|event| {
                event["phase"] == "processing"
                    && event["phase_truncated"] != true
                    && event["path_truncated"] != true
                    && matches!(event["kind"].as_str(), Some("error" | "deferred"))
                    && event["resolved_at"].is_null()
            })
            .filter_map(|event| event["path"].as_str().map(str::to_owned))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn processed(&self, path: &str, outcome: &str) {
        self.processed_with_reason(path, outcome, None, None);
    }
    pub fn processed_with_reason(
        &self,
        path: &str,
        outcome: &str,
        reason: Option<&str>,
        errno: Option<i32>,
    ) {
        self.update(|s| {
            s.last_progress_at = Some(now());
            s.last_progress = Some(Instant::now());
            s.counters.processed += 1;
            if let Some(scope) = &mut s.scope {
                scope.processed += 1;
            }
            match outcome {
                "renamed" => s.counters.renamed += 1,
                "deferred" => s.counters.deferred += 1,
                "error" => s.counters.errors += 1,
                _ => {}
            }
            if matches!(outcome, "renamed" | "restored") {
                s.resolve("processing", Some(path), outcome);
            }
            // Routine checks update the current path and progress separately.
            // The bounded feed retains only changes and actionable outcomes.
            if matches!(outcome, "renamed" | "restored" | "deferred" | "error") {
                s.event(outcome, "processing", Some(path), reason, errno);
            }
        });
    }
    pub fn bind(&self) -> Binding {
        let previous = CURRENT.with(|current| current.replace(Some(self.clone())));
        Binding {
            previous,
            _thread_bound: std::marker::PhantomData,
        }
    }
}
thread_local! { static CURRENT: RefCell<Option<Activity>> = const { RefCell::new(None) }; }
pub struct Binding {
    previous: Option<Activity>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Drop for Binding {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.replace(self.previous.take());
        });
    }
}
pub(crate) fn current(change: impl FnOnce(&Activity)) {
    CURRENT.with(|current| {
        if let Some(activity) = current.borrow().as_ref() {
            change(activity);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retirement_candidates_are_exact_unresolved_processing_paths_without_side_effects() {
        let activity = Activity::new();
        activity.processed("/needed", "deferred");
        activity.failed_with_reason("processing", Some("/needed"), Some("new cause"), None);
        activity.processed("/resolved", "deferred");
        activity.processed("/resolved", "renamed");
        activity.failed("reading_metadata", Some("/metadata"));
        activity.failed("observation", Some("/folder"));
        activity.failed("processing", None);
        activity.processed(&format!("/{}", "한".repeat(MAX_STRING_BYTES)), "deferred");
        let before = activity.snapshot();
        assert_eq!(activity.unresolved_processing_paths(), ["/needed"]);
        let after = activity.snapshot();
        assert_eq!(before["events"], after["events"]);
        assert_eq!(before["counters"], after["counters"]);
        assert_eq!(before["last_progress_at"], after["last_progress_at"]);
    }
    #[test]
    fn truncated_reasons_and_phases_cannot_claim_an_exact_match() {
        let activity = Activity::new();
        let long = "a".repeat(MAX_STRING_BYTES + 3);
        activity.failed_with_reason("observation", Some("/fixture"), Some(&long), Some(1));
        activity.failed_with_reason(
            "observation",
            Some("/fixture"),
            Some(&bounded(&long)),
            Some(1),
        );
        assert_eq!(activity.snapshot()["events"].as_array().unwrap().len(), 2);
        activity.failed(&long, Some("/fixture"));
        activity.failed(&bounded(&long), Some("/fixture"));
        activity.resolve(&bounded(&long), Some("/fixture"), "checked");
        let snapshot = activity.snapshot();
        assert_eq!(snapshot["events"].as_array().unwrap().len(), 4);
        assert!(snapshot["events"][2]["resolved_at"].is_null());
        assert_eq!(snapshot["events"][3]["resolution"], "checked");
    }
    #[test]
    fn reasons_identify_distinct_failures_and_resolution_never_invents_progress() {
        let activity = Activity::new();
        activity.failed_with_reason("observation", Some("/fixture"), Some("denied"), Some(1));
        activity.failed_with_reason("observation", Some("/fixture"), Some("denied"), Some(1));
        activity.failed_with_reason("observation", Some("/fixture"), Some("timeout"), Some(60));
        activity.failed_with_reason("observation", Some("/fixture"), Some("timeout"), Some(4));
        let before = activity.snapshot();
        assert_eq!(before["events"].as_array().unwrap().len(), 3);
        assert_eq!(before["events"][0]["occurrences"], 2);
        activity.resolve("observation", Some("/fixture"), "checked");
        let resolved = activity.snapshot();
        assert_eq!(resolved["counters"], before["counters"]);
        assert_eq!(resolved["last_progress_at"], before["last_progress_at"]);
        for event in resolved["events"].as_array().unwrap() {
            assert_eq!(event["resolution"], "checked");
            assert!(event["resolved_at"].is_number());
        }
        activity.failed_with_reason("observation", Some("/fixture"), Some("denied"), Some(1));
        assert_eq!(activity.snapshot()["events"][3]["occurrences"], 1);
        assert_eq!(activity.snapshot()["events"][3]["sequence"], 4);
    }

    #[test]
    fn reasons_unicode_and_resolution_reaccount_the_bounded_feed() {
        let activity = Activity::new();
        let reason = "한\"\\\n".repeat(MAX_STRING_BYTES);
        for index in 0..MAX_EVENTS * 2 {
            activity.failed_with_reason(
                "observation",
                Some(&format!("/fixture/{index}")),
                Some(&reason),
                Some(libc::EINTR),
            );
        }
        let before = activity.snapshot();
        let paths: Vec<String> = before["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["path"].as_str().unwrap().into())
            .collect();
        for path in paths {
            activity.resolve("observation", Some(&path), "no_longer_needed");
        }
        let snapshot = activity.snapshot();
        let events = snapshot["events"].as_array().unwrap();
        assert!(events.len() <= MAX_EVENTS);
        let serialized_bytes: usize = events
            .iter()
            .map(|event| serde_json::to_vec(event).unwrap().len())
            .sum();
        assert!(serialized_bytes <= EVENT_BYTES);
        assert_eq!(activity.0.lock().unwrap().event_bytes, serialized_bytes);
        assert!(serde_json::to_vec(&snapshot).unwrap().len() < MAX_RESPONSE_BYTES);
        for event in events {
            assert!(event["reason"].as_str().unwrap().len() <= MAX_STRING_BYTES);
            assert_eq!(event["errno"], libc::EINTR);
        }
        let long_path = format!("/{}", "x".repeat(MAX_STRING_BYTES));
        activity.failed("observation", Some(&long_path));
        activity.resolve("observation", Some(&bounded(&long_path)), "checked");
        activity.resolve("observation", Some(&long_path), "checked");
        assert!(
            activity.snapshot()["events"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["resolved_at"]
                .is_null()
        );
    }

    #[test]
    fn unresolved_failures_keep_occurrences_and_resolve_only_their_phase() {
        let activity = Activity::new();
        activity.failed("observation", Some("/fixture/file"));
        activity.processed("/fixture/file", "deferred");
        let first = activity.snapshot();
        activity.processed("/fixture/file", "deferred");
        let repeated = activity.snapshot();
        assert_eq!(repeated["events"][1]["occurrences"], 2);
        assert_eq!(repeated["events"][1]["first_at"], first["events"][1]["at"]);
        activity.processed("/fixture/file", "renamed");
        let resolved = activity.snapshot();
        assert!(resolved["events"][0]["resolved_at"].is_null());
        assert_eq!(resolved["events"][1]["resolution"], "renamed");
        assert!(resolved["events"][1]["resolved_at"].is_number());
        activity.processed("/fixture/file", "deferred");
        let retried = activity.snapshot();
        assert!(
            retried["events"][3]["sequence"].as_u64() > first["events"][1]["sequence"].as_u64()
        );
        assert_eq!(retried["events"][3]["occurrences"], 1);
    }

    #[test]
    fn shortened_display_paths_do_not_merge_distinct_issues() {
        let activity = Activity::new();
        let long_path = format!("/{}", "a".repeat(MAX_STRING_BYTES + 10));
        let matching_display = bounded(&long_path);
        activity.failed("observation", Some(&long_path));
        activity.failed("observation", Some(&matching_display));
        assert_eq!(activity.snapshot()["events"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn repeated_issues_update_existing_rows_without_evicting_changes() {
        let activity = Activity::new();
        activity.processed("/fixture/completed", "renamed");
        activity.failed("observation", Some("/fixture/folder"));
        activity.processed("/fixture/cloud-file", "deferred");
        let first = activity.snapshot();
        for number in 0..300 {
            activity.select_scope(&number.to_string(), "/fixture", 0, 0, None);
            activity.failed("observation", Some("/fixture/folder"));
            activity.processed("/fixture/cloud-file", "deferred");
        }
        let repeated = activity.snapshot();
        assert_eq!(repeated["counters"]["errors"], 301);
        assert_eq!(repeated["counters"]["deferred"], 301);
        assert_eq!(repeated["events"].as_array().unwrap().len(), 3);
        assert_eq!(repeated["dropped_events"], 0);
        for index in 0..3 {
            assert_eq!(
                repeated["events"][index]["sequence"],
                first["events"][index]["sequence"]
            );
        }
        assert!(repeated["events"][1]["at"].as_f64() >= first["events"][1]["at"].as_f64());
        assert_eq!(repeated["events"][1]["scope_id"], "299");
        // A real change and a different failure phase remain distinct outcomes.
        activity.processed("/fixture/cloud-file", "renamed");
        activity.processed("/fixture/cloud-file", "deferred");
        activity.failed("opening_directory", Some("/fixture/folder"));
        activity.processed("/fixture/completed", "renamed");
        assert_eq!(activity.snapshot()["events"].as_array().unwrap().len(), 7);
        // One failed syscall may be reported at both the I/O and scan layers.
        for _ in 0..300 {
            activity.failed("observation", Some("/fixture/folder"));
            activity.failed("opening_directory", Some("/fixture/folder"));
        }
        assert_eq!(activity.snapshot()["events"].as_array().unwrap().len(), 7);
        assert_eq!(activity.snapshot()["dropped_events"], 0);
    }

    #[test]
    fn unchanged_checks_keep_progress_without_repeating_outcomes() {
        let activity = Activity::new();
        activity.select_scope("scope", "/fixture", 0, 0, Some(3));
        activity.processed("/fixture/file", "renamed");
        let renamed = activity.snapshot();
        for _ in 0..2 {
            activity.begin("reading_metadata", Some("/fixture/file"));
            activity.observed("/fixture/file");
            activity.finish("reading_metadata", Some("/fixture/file"));
            activity.processed("/fixture/file", "unchanged");
        }
        let checked = activity.snapshot();
        assert_eq!(checked["counters"]["processed"], 3);
        assert_eq!(checked["scope"]["processed"], 3);
        assert_eq!(checked["counters"]["renamed"], 1);
        assert_eq!(checked["counters"]["observed"], 2);
        assert_eq!(checked["counters"]["io_completed"], 2);
        assert_eq!(checked["item_path"], "/fixture/file");
        assert!(checked["last_progress_at"].as_f64() >= renamed["last_progress_at"].as_f64());
        assert_eq!(checked["events"], renamed["events"]);
        assert_eq!(checked["dropped_events"], renamed["dropped_events"]);
        assert!(
            checked["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["kind"] == "renamed")
        );
    }

    #[test]
    fn snapshots_keep_actual_progress_and_scope_identity() {
        let activity = Activity::new();
        activity.select_scope("a", "/a", 0, 0, None);
        activity.observed("/a/file");
        activity.begin("reading_metadata", Some("/a/file"));
        let first = activity.snapshot();
        assert_eq!(first["scope"]["id"], "a");
        assert_eq!(first["counters"]["observed"], 1);
        assert_eq!(first["scope"]["total"], Value::Null);
        for _ in 0..10 {
            let next = activity.snapshot();
            for key in [
                "scope",
                "phase_started_at",
                "last_progress_at",
                "counters",
                "events",
            ] {
                assert_eq!(next[key], first[key]);
            }
        }
        activity.select_scope("b", "/b", 9, 2, Some(9));
        let second = activity.snapshot();
        assert_eq!(second["scope_path"], "/b");
        assert!(second["item_path"].is_null());
        assert_eq!(second["scope"]["total"], 9);
        assert_eq!(second["counters"]["observed"], 1);
    }
    #[test]
    fn event_feed_and_serialized_response_are_bounded() {
        let activity = Activity::new();
        activity.select_scope("scope", &"한".repeat(9000), 0, 0, None);
        for _ in 0..400 {
            activity.processed(&"\\\n한".repeat(9000), "renamed");
        }
        let snapshot = activity.snapshot();
        assert!(snapshot["events"].as_array().unwrap().len() <= 200);
        assert!(snapshot["dropped_events"].as_u64().unwrap_or(0) > 0);
        assert!(serde_json::to_vec(&snapshot).unwrap().len() < 64 * 1024);
    }
    #[test]
    fn polling_has_a_separate_generation_time_and_monotonic_phase_age() {
        let activity = Activity::new();
        activity.begin("reading_metadata", Some("/fixture/file"));
        let first = activity.snapshot();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = activity.snapshot();
        assert!(
            second["snapshot_generated_at"].as_f64().unwrap_or(0.0)
                > first["snapshot_generated_at"].as_f64().unwrap_or(0.0)
        );
        assert!(
            second["phase_elapsed_seconds"].as_f64().unwrap_or(0.0)
                > first["phase_elapsed_seconds"].as_f64().unwrap_or(0.0)
        );
        assert_eq!(first["phase_started_at"], second["phase_started_at"]);
        assert_eq!(first["last_progress_at"], second["last_progress_at"]);
        assert_eq!(first["counters"], second["counters"]);
    }
    #[test]
    fn each_runtime_has_a_fresh_session() {
        assert_ne!(
            Activity::new().snapshot()["session_id"],
            Activity::new().snapshot()["session_id"]
        );
    }
}
