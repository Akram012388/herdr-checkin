//! Queue transitions and the event handlers that drive them. Pure — no herdr calls, no IO beyond
//! [`crate::state::StateStore::update`]; this module must never depend on the `Herdr` trait.

use crate::herdr::{parse_event_string, parse_status_event, StatusEvent};
use crate::state::{PluginError, QueueEntry, StateStore, WaitStatus};
use crate::RuntimeEnv;
use std::collections::HashMap;

/// `pane.agent_status_changed`: enqueue on `blocked`/`done`, evict on `working`.
/// Other statuses (`idle`, `unknown`) leave the queue untouched.
pub(crate) fn on_status_changed(runtime: &RuntimeEnv) -> Result<(), PluginError> {
    let Some(event) = runtime.event_json.as_deref().and_then(parse_status_event) else {
        return Ok(());
    };

    let now_ms = runtime.now_ms;
    StateStore::new(&runtime.state_dir).update(|mut entries| {
        match event.wait_status() {
            Some(status) => enqueue(&mut entries, &event, status, now_ms),
            None if event.is_working() => evict(&mut entries, &event.pane_id),
            None => {}
        }
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
        existing.agent = event.agent.clone();
        existing.display_agent = event.display_agent.clone();
        existing.title = event.title.clone();
        existing.status = status;
        existing.last_touched_ms = now_ms;
    } else {
        entries.push(QueueEntry {
            pane_id: event.pane_id.clone(),
            workspace_id: event.workspace_id.clone(),
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
