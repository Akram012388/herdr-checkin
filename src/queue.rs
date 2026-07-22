//! Queue transitions and the event handlers that drive them. Pure — no herdr calls, no IO beyond
//! [`crate::state::StateStore::update`]; this module must never depend on the `Herdr` trait.

use crate::herdr::{parse_event_string, parse_status_event, StatusEvent};
use crate::state::{PluginError, QueueEntry, StateStore, WaitStatus};
use crate::RuntimeEnv;
use std::collections::HashMap;

/// `pane.agent_status_changed`: enqueue on `blocked`/`done`, evict on `working`.
/// Other statuses (`idle`, `unknown`) leave the queue untouched.
///
/// `enrich` fills the location fields the event omits (`tab_id`, `workspace_label`). It is an
/// injected closure — not a `Herdr` call — so this module stays free of the `Herdr` trait; the
/// dispatch layer supplies one backed by `herdr::enrich_location`. It runs **before the lock** and
/// **only when we will actually enqueue**, so an ordinary `working` eviction pays for no lookups.
pub(crate) fn on_status_changed(
    runtime: &RuntimeEnv,
    enrich: impl FnOnce(&mut StatusEvent),
) -> Result<(), PluginError> {
    let Some(mut event) = runtime.event_json.as_deref().and_then(parse_status_event) else {
        return Ok(());
    };

    let Some(status) = event.wait_status() else {
        // Not a wait status: evict on `working`, ignore the rest. No location lookup needed.
        if event.is_working() {
            return StateStore::new(&runtime.state_dir).update(|mut entries| {
                evict(&mut entries, &event.pane_id);
                (entries, ())
            });
        }
        return Ok(());
    };

    enrich(&mut event);

    let now_ms = runtime.now_ms;
    StateStore::new(&runtime.state_dir).update(|mut entries| {
        enqueue(&mut entries, &event, status, now_ms);
        (entries, ())
    })
}

/// `pane.focused`: you looked at the pane, so it no longer needs your attention.
pub(crate) fn on_focused(runtime: &RuntimeEnv) -> Result<(), PluginError> {
    evict_event_pane(runtime)
}

/// `pane.closed`: the pane is gone, drop any queued entry for it.
pub(crate) fn on_closed(runtime: &RuntimeEnv) -> Result<(), PluginError> {
    evict_event_pane(runtime)
}

fn evict_event_pane(runtime: &RuntimeEnv) -> Result<(), PluginError> {
    let Some(pane_id) = runtime
        .event_json
        .as_deref()
        .and_then(|raw| parse_event_string(raw, "pane_id"))
    else {
        return Ok(());
    };

    StateStore::new(&runtime.state_dir).update(|mut entries| {
        evict(&mut entries, &pane_id);
        (entries, ())
    })
}

/// Add or refresh an entry for a pane. Deduplicates per pane: if the pane is
/// already queued, its fields and status are updated in place, preserving its
/// FIFO position and original `enqueued_at_ms` (it has been waiting since the
/// first ping). Otherwise it is appended to the back.
///
/// Either way it stamps `last_touched_ms = now_ms`. `enqueued_at_ms` drives FIFO order and the
/// "waited" display; `last_touched_ms` records this refresh so `next`/`peek`'s prune guard can tell
/// a just-refreshed persisted entry from a genuinely stale one (see those functions).
pub(crate) fn enqueue(
    entries: &mut Vec<QueueEntry>,
    event: &StatusEvent,
    status: WaitStatus,
    now_ms: u64,
) {
    if let Some(existing) = entries.iter_mut().find(|e| e.pane_id == event.pane_id) {
        existing.workspace_id = event.workspace_id.clone();
        existing.tab_id = event.tab_id.clone();
        existing.workspace_label = event.workspace_label.clone();
        existing.agent = event.agent.clone();
        existing.display_agent = event.display_agent.clone();
        existing.title = event.title.clone();
        existing.status = status;
        existing.last_touched_ms = now_ms;
    } else {
        entries.push(QueueEntry {
            pane_id: event.pane_id.clone(),
            workspace_id: event.workspace_id.clone(),
            tab_id: event.tab_id.clone(),
            workspace_label: event.workspace_label.clone(),
            agent: event.agent.clone(),
            display_agent: event.display_agent.clone(),
            title: event.title.clone(),
            status,
            enqueued_at_ms: now_ms,
            last_touched_ms: now_ms,
        });
    }
}

/// Remove any entry for the given pane.
pub(crate) fn evict(entries: &mut Vec<QueueEntry>, pane_id: &str) {
    entries.retain(|entry| entry.pane_id != pane_id);
}

/// A queued pane is still worth jumping to if it exists and has not resumed
/// working. Missing pane => gone; `working` => the agent picked back up.
pub(crate) fn is_live(live: &HashMap<String, String>, pane_id: &str) -> bool {
    match live.get(pane_id) {
        Some(status) => status != "working",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WaitStatus;
    use crate::test_support::{feed_status, load, pane_event_json, runtime, temp_state_dir};
    use std::fs;

    #[test]
    fn blocked_and_done_enqueue_in_fifo_order() {
        let dir = temp_state_dir("fifo");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "needs input");
        feed_status(&dir, 2_000, "w2:p1", "w2", "done", "finished");

        let entries = load(&dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pane_id, "w1:p1");
        assert_eq!(entries[0].status, WaitStatus::Blocked);
        assert_eq!(entries[0].enqueued_at_ms, 1_000);
        assert_eq!(entries[1].pane_id, "w2:p1");
        assert_eq!(entries[1].status, WaitStatus::Done);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn re_enqueue_dedups_and_keeps_position_and_enqueued_at() {
        let dir = temp_state_dir("dedup");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "first");
        feed_status(&dir, 2_000, "w2:p1", "w2", "blocked", "second");
        // w1:p1 goes from blocked to done — same waiter, updated in place.
        feed_status(&dir, 3_000, "w1:p1", "w1", "done", "now done");

        let entries = load(&dir);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pane_id, "w1:p1");
        assert_eq!(entries[0].status, WaitStatus::Done);
        assert_eq!(entries[0].title.as_deref(), Some("now done"));
        // Position and original wait time are preserved.
        assert_eq!(entries[0].enqueued_at_ms, 1_000);
        assert_eq!(entries[1].pane_id, "w2:p1");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn returning_to_working_evicts() {
        let dir = temp_state_dir("working-evict");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        feed_status(&dir, 2_000, "w1:p1", "w1", "working", "x");
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn idle_and_unknown_leave_queue_untouched() {
        let dir = temp_state_dir("idle-noop");
        feed_status(&dir, 1_000, "w1:p1", "w1", "done", "x");
        feed_status(&dir, 2_000, "w1:p1", "w1", "idle", "x");
        feed_status(&dir, 3_000, "w1:p1", "w1", "unknown", "x");
        assert_eq!(load(&dir).len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn focused_event_evicts() {
        let dir = temp_state_dir("focused-evict");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        let mut rt = runtime(dir.clone(), 2_000);
        rt.event_json = Some(pane_event_json("pane_focused", "w1:p1", "w1"));
        on_focused(&rt).expect("focused should succeed");
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn closed_event_evicts() {
        let dir = temp_state_dir("closed-evict");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        let mut rt = runtime(dir.clone(), 2_000);
        rt.event_json = Some(pane_event_json("pane_closed", "w1:p1", "w1"));
        on_closed(&rt).expect("closed should succeed");
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn re_enqueue_bumps_last_touched_but_keeps_enqueued_at() {
        let dir = temp_state_dir("last-touched");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        // A later re-ping refreshes the same waiter.
        feed_status(&dir, 5_000, "w1:p1", "w1", "done", "x");
        let entries = load(&dir);
        assert_eq!(entries[0].enqueued_at_ms, 1_000, "FIFO age is preserved");
        assert_eq!(
            entries[0].last_touched_ms, 5_000,
            "refresh bumps last_touched"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn status_event_parses_the_top_level_shape_without_a_data_wrapper() {
        let dir = temp_state_dir("toplevel-shape");
        let mut rt = runtime(dir.clone(), 1_000);
        rt.event_json = Some(
            r#"{"pane_id":"w1:p1","workspace_id":"w1","agent_status":"blocked","title":"x"}"#
                .to_string(),
        );
        on_status_changed(&rt, |_| {}).expect("status-changed should parse the flat shape");
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pane_id, "w1:p1");
        let _ = fs::remove_dir_all(dir);
    }
}
