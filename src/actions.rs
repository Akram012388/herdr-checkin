//! The subcommand actions (`next`, `peek`, `clear`, `startup`) and the toast copy they render.
//! Each mutation still goes through [`crate::state::StateStore::update`] — these are the only
//! callers that also talk to herdr (focus, notifications, pane list).

use crate::herdr::{Herdr, StatusEvent};
use crate::queue::{enqueue, evict, is_live};
use crate::roster::{render_roster_text, RosterSnapshot};
use crate::state::{current_unix_ms, PluginError, QueueEntry, StateStore};
use crate::RuntimeEnv;

/// Jump to the oldest still-waiting pane, then pop it from the queue. An empty queue is a clean
/// no-op — no error toast.
///
/// Two safety rules keep this from losing the ping it exists to protect:
/// - **Focus first, evict on success only.** The target is kept in the queue while we focus it;
///   only a successful `agent focus` removes it. A focus failure leaves the entry in place.
/// - **Never drop an entry the liveness snapshot couldn't see.** The `pane list` snapshot is
///   taken before the lock, so an entry enqueued — or *refreshed* — after it would look stale. We
///   prune an entry as stale only if both its enqueue and its last refresh predate the snapshot
///   (`max(enqueued_at_ms, last_touched_ms) < snapshot_ms`); newer ones are kept. This window is
///   exactly when you press `next` as an agent blocks, or as a post-restart event re-pings a pane
///   the seed persisted with an older `enqueued_at_ms`.
pub(crate) fn next(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let snapshot_ms = current_unix_ms();
    let live = herdr.pane_status_map()?;

    let target = StateStore::new(&runtime.state_dir).update(|entries| {
        let mut kept: Vec<QueueEntry> = Vec::new();
        let mut target = None;
        let mut remaining = entries.into_iter();
        for entry in remaining.by_ref() {
            if is_live(&live, &entry.pane_id) {
                target = Some(entry.pane_id.clone());
                kept.push(entry); // keep the target until the focus is confirmed
                break;
            }
            if entry.last_touched_ms.max(entry.enqueued_at_ms) >= snapshot_ms {
                kept.push(entry); // enqueued or refreshed after the snapshot — too new to judge
            }
            // else: stale and older than the snapshot — drop it.
        }
        kept.extend(remaining); // everything past the target stays in order
        (kept, target)
    })?;

    if let Some(pane_id) = target {
        // Focus first; a failure here returns Err with the entry still queued.
        herdr.focus_agent(&pane_id)?;
        // The jump succeeded — now evict the entry as a delta under the lock.
        StateStore::new(&runtime.state_dir).update(|mut entries| {
            evict(&mut entries, &pane_id);
            (entries, ())
        })?;
    }

    Ok(())
}

/// Show the current queue as a herdr toast. Read-oriented, but prunes stale entries so the count
/// and list stay honest — keeping any entry the pre-lock snapshot was too early to judge (see
/// [`next`] for why).
pub(crate) fn peek(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let snapshot_ms = current_unix_ms();
    let live = herdr.pane_status_map()?;

    let entries = StateStore::new(&runtime.state_dir).update(|entries| {
        let kept: Vec<QueueEntry> = entries
            .into_iter()
            .filter(|entry| {
                is_live(&live, &entry.pane_id)
                    || entry.last_touched_ms.max(entry.enqueued_at_ms) >= snapshot_ms
            })
            .collect();
        (kept.clone(), kept)
    })?;

    let title = peek_title(entries.len());
    let body = peek_body(&entries, runtime.now_ms);
    let sound = if entries.is_empty() {
        "none"
    } else {
        "request"
    };
    herdr.show_notification(&title, body.as_deref(), sound)
}

/// Empty the queue. Silent success.
pub(crate) fn clear(runtime: &RuntimeEnv) -> Result<(), PluginError> {
    StateStore::new(&runtime.state_dir).update(|_| (Vec::new(), ()))
}

/// `[[startup]]` hook: re-seed the queue after a herdr server (re)start. herdr runs this once per
/// server process (cold start and live-handoff takeover); the event subscription starts fresh on
/// restart and misses panes that were already `blocked`/`done`, so we scan the live `pane list`
/// and enqueue them.
///
/// Two properties keep this safe:
/// - **Additive-only, through the same upsert events use.** For each `blocked`/`done` pane we call
///   [`enqueue`] under the lock — a delta, never a wholesale `state.json` rewrite (invariant #1).
///   The hook is spawned async and races the now-live event loop, so a `status-changed` event may
///   upsert the same pane concurrently; both merge, neither clobbers. A pane already queued
///   (persisted across the restart) keeps its FIFO position and original `enqueued_at_ms`; a new
///   waiter is appended stamped `now_ms`. Running twice (e.g. cold start then takeover) is a no-op.
/// - **No eviction.** Stale persisted entries (panes that closed or resumed `working` during the
///   downtime) are left for `next`/`peek`'s existing liveness pass to prune — eviction is the only
///   operation that can lose a ping, so the seed never performs it. The currently-focused pane is
///   seeded like any other (restart focus is not a user action).
pub(crate) fn startup(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let panes = herdr.pane_infos()?;
    // Human workspace + tab labels for the rows. Best-effort: a `workspace list`/`tab list` failure
    // just leaves those rows on their raw ids — it must not abort re-seeding the queue after a
    // herdr restart.
    let workspace_labels = herdr.workspace_labels().unwrap_or_default();
    let tab_labels = herdr.tab_labels().unwrap_or_default();
    let now_ms = runtime.now_ms;

    StateStore::new(&runtime.state_dir).update(|mut entries| {
        for pane in &panes {
            let event = StatusEvent {
                pane_id: pane.pane_id.clone(),
                workspace_id: pane.workspace_id.clone(),
                // `pane list` carries the tab id + pane label directly; the workspace/tab names come
                // from their respective label maps (tab keyed by the pane's tab id).
                tab_id: pane.tab_id.clone(),
                workspace_label: workspace_labels.get(&pane.workspace_id).cloned(),
                tab_label: pane
                    .tab_id
                    .as_deref()
                    .and_then(|tab_id| tab_labels.get(tab_id).cloned()),
                pane_label: pane.label.clone(),
                agent_status: pane.agent_status.clone(),
                agent: pane.agent.clone(),
                display_agent: pane.display_agent.clone(),
                title: pane.title.clone(),
            };
            if let Some(status) = event.wait_status() {
                enqueue(&mut entries, &event, status, now_ms);
            }
        }
        (entries, ())
    })
}

fn peek_title(count: usize) -> String {
    match count {
        0 => "Check-in: queue empty".to_string(),
        1 => "Check-in: 1 agent waiting".to_string(),
        n => format!("Check-in: {n} agents waiting"),
    }
}

fn peek_body(entries: &[QueueEntry], now_ms: u64) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        lines.push(format!("{}. {}", index + 1, describe_entry(entry, now_ms)));
    }
    Some(lines.join("\n"))
}

/// The short display name for an entry: the display agent, else the agent, else the pane id
/// (skipping empty strings). Shared by the list rows and the reply footer so they always name an
/// agent the same way.
pub(crate) fn agent_label(entry: &QueueEntry) -> &str {
    entry
        .display_agent
        .as_deref()
        .filter(|value| !value.is_empty())
        .or(entry.agent.as_deref().filter(|value| !value.is_empty()))
        .unwrap_or(&entry.pane_id)
}

/// A queue row as one line — the destination followed by its detail, joined by " · ". Used by the
/// `peek` toast (plain text, one line per entry). The status pane renders the same two pieces as two
/// lines instead (destination bright, [`entry_detail`] dim); see `pane::draw_list`.
pub(crate) fn describe_entry(entry: &QueueEntry, now_ms: u64) -> String {
    match entry_destination(entry) {
        Some(destination) => format!("{destination} · {}", entry_detail(entry, now_ms)),
        None => entry_detail(entry, now_ms),
    }
}

/// The row's **destination** — where the waiter is, in herdr's own go-to vocabulary and order:
/// `{workspace} · {tab} · {pane}`. Each segment prefers its human label and falls back to the
/// positional id, and is dropped entirely when neither is known so there is never a dangling
/// separator:
/// - workspace: `workspace_label` else the raw `workspace_id`
/// - tab: `tab_label` (program) else `t{N}` (from `tab_id`)
/// - pane: `pane_label` (manual) else `pane {N}` (from `pane_id`)
///
/// Returns `None` only when not even a pane number is known (a malformed/hand-seeded row).
pub(crate) fn entry_destination(entry: &QueueEntry) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    if let Some(workspace) =
        non_empty(entry.workspace_label.as_deref()).or(non_empty(Some(entry.workspace_id.as_str())))
    {
        parts.push(workspace.to_string());
    }
    if let Some(tab) = non_empty(entry.tab_label.as_deref())
        .or_else(|| entry.tab_id.as_deref().and_then(id_segment))
    {
        parts.push(tab.to_string());
    }
    if let Some(pane) = non_empty(entry.pane_label.as_deref()) {
        parts.push(pane.to_string());
    } else if let Some(number) = id_segment(&entry.pane_id).and_then(pane_number) {
        parts.push(format!("pane {number}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// The row's **detail** — the agent's status, what it said, and how long it has waited:
/// `{status} · {title} · {waited}` (the title is dropped when empty). This is the dim second line in
/// the pane; the go-to destination is what leads.
pub(crate) fn entry_detail(entry: &QueueEntry, now_ms: u64) -> String {
    let status = entry.status.as_str();
    let waited = format_waited(now_ms.saturating_sub(entry.enqueued_at_ms));
    match entry.title.as_deref().filter(|value| !value.is_empty()) {
        Some(title) => format!("{status} · {title} · {waited}"),
        None => format!("{status} · {waited}"),
    }
}

/// `Some(trimmed)` if the option holds a non-empty string, else `None`.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// The trailing positional segment of a herdr id — the part after its `:` (`wS:t2` -> `t2`). An id
/// with no `:` (or an empty tail) has no segment we can trust, so this returns None.
fn id_segment(id: &str) -> Option<&str> {
    id.rsplit_once(':')
        .map(|(_, segment)| segment)
        .filter(|segment| !segment.is_empty())
}

/// The number in a pane segment (`p3` -> `3`), for the `pane {N}` fallback that mirrors herdr's
/// go-to picker. None if the segment is not `p<digits>`.
fn pane_number(segment: &str) -> Option<&str> {
    let number = segment.strip_prefix('p')?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
}

fn format_waited(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    let remainder = mins % 60;
    if remainder == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{remainder}m")
    }
}

/// Hidden `roster` subcommand: sample `herdr agent list` and print the grouped roster as text.
/// Dev-only visibility into the Agents-view data path (Slice 1) — no queue mutation, no UI. The
/// grouping and formatting live in the Herdr-free `roster.rs`; this only bridges the CLI sample into
/// a [`RosterSnapshot`] stamped with the run's sample time and prints the dump.
pub(crate) fn roster(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let agents = herdr.agent_list()?;
    let snapshot = RosterSnapshot {
        sampled_at_ms: runtime.now_ms,
        agents,
    };
    print!("{}", render_roster_text(&snapshot));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::PaneInfo;
    use crate::state::WaitStatus;
    use crate::test_support::{feed_status, load, runtime, temp_state_dir, FakeHerdr};
    use std::fs;

    #[test]
    fn next_focuses_oldest_and_pops_it() {
        let dir = temp_state_dir("next");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        feed_status(&dir, 2_000, "w2:p1", "w2", "done", "y");
        let rt = runtime(dir.clone(), 3_000);
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked"), ("w2:p1", "done")]);

        next(&rt, &herdr).expect("next should succeed");

        assert_eq!(herdr.focused.into_inner(), vec!["w1:p1".to_string()]);
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pane_id, "w2:p1");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn next_skips_and_drops_stale_entries() {
        let dir = temp_state_dir("next-stale");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "gone");
        feed_status(&dir, 2_000, "w2:p1", "w2", "blocked", "resumed");
        feed_status(&dir, 3_000, "w3:p1", "w3", "done", "real");
        // w1:p1 no longer exists; w2:p1 resumed to working; w3:p1 still waiting.
        let herdr = FakeHerdr::new(&[("w2:p1", "working"), ("w3:p1", "done")]);
        let rt = runtime(dir.clone(), 4_000);

        next(&rt, &herdr).expect("next should succeed");

        assert_eq!(herdr.focused.into_inner(), vec!["w3:p1".to_string()]);
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn next_on_empty_queue_is_a_clean_noop() {
        let dir = temp_state_dir("next-empty");
        let herdr = FakeHerdr::new(&[]);
        let rt = runtime(dir.clone(), 1_000);
        next(&rt, &herdr).expect("next should be a no-op");
        assert!(herdr.focused.into_inner().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn next_keeps_the_entry_when_the_focus_fails() {
        let dir = temp_state_dir("next-focus-fail");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked")]).with_failing_focus();
        let rt = runtime(dir.clone(), 2_000);

        // A failed jump must not lose the ping.
        assert!(next(&rt, &herdr).is_err());
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pane_id, "w1:p1");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn next_does_not_drop_an_entry_enqueued_after_the_liveness_snapshot() {
        let dir = temp_state_dir("next-fresh");
        // Enqueued far in the future so it postdates next()'s snapshot; its pane is absent from
        // the snapshot, so the old code would have judged it stale and dropped it.
        feed_status(&dir, u64::MAX, "wZ:p9", "wZ", "blocked", "fresh");
        let herdr = FakeHerdr::new(&[]);
        let rt = runtime(dir.clone(), 1_000);

        next(&rt, &herdr).expect("next should succeed");
        assert!(herdr.focused.into_inner().is_empty());
        assert_eq!(load(&dir).len(), 1, "the fresh entry must survive");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn peek_does_not_drop_an_entry_enqueued_after_the_liveness_snapshot() {
        let dir = temp_state_dir("peek-fresh");
        feed_status(&dir, u64::MAX, "wZ:p9", "wZ", "blocked", "fresh");
        let herdr = FakeHerdr::new(&[]);
        let rt = runtime(dir.clone(), 1_000);

        peek(&rt, &herdr).expect("peek should succeed");
        assert_eq!(load(&dir).len(), 1, "the fresh entry must survive");
        assert_eq!(
            herdr.notifications.into_inner()[0].0,
            "Check-in: 1 agent waiting"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn next_keeps_a_refreshed_entry_whose_enqueue_predates_the_snapshot() {
        // The post-restart race: a persisted entry (old enqueued_at) is refreshed to blocked by a
        // concurrent event (last_touched postdates any snapshot) while its pane is absent from the
        // pre-lock liveness snapshot. Pruning on enqueued_at alone would lose this live ping;
        // max(enqueued_at, last_touched) must keep it.
        let dir = temp_state_dir("next-refreshed");
        StateStore::new(&dir)
            .update(|mut entries| {
                entries.push(QueueEntry {
                    pane_id: "wR:p1".to_string(),
                    workspace_id: "wR".to_string(),
                    tab_id: None,
                    workspace_label: None,
                    tab_label: None,
                    pane_label: None,
                    agent: None,
                    display_agent: None,
                    title: None,
                    status: WaitStatus::Blocked,
                    enqueued_at_ms: 1_000,     // old — predates the snapshot
                    last_touched_ms: u64::MAX, // fresh — a concurrent refresh
                });
                (entries, ())
            })
            .expect("seed should succeed");
        let herdr = FakeHerdr::new(&[]); // pane absent from the live snapshot
        let rt = runtime(dir.clone(), 2_000);

        next(&rt, &herdr).expect("next should succeed");

        assert!(
            herdr.focused.into_inner().is_empty(),
            "the pane is not live, so nothing is focused"
        );
        assert_eq!(
            load(&dir).len(),
            1,
            "a concurrently-refreshed entry must not be pruned as stale"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn peek_keeps_a_refreshed_entry_whose_enqueue_predates_the_snapshot() {
        let dir = temp_state_dir("peek-refreshed");
        StateStore::new(&dir)
            .update(|mut entries| {
                entries.push(QueueEntry {
                    pane_id: "wR:p1".to_string(),
                    workspace_id: "wR".to_string(),
                    tab_id: None,
                    workspace_label: None,
                    tab_label: None,
                    pane_label: None,
                    agent: None,
                    display_agent: None,
                    title: None,
                    status: WaitStatus::Blocked,
                    enqueued_at_ms: 1_000,
                    last_touched_ms: u64::MAX,
                });
                (entries, ())
            })
            .expect("seed should succeed");
        let herdr = FakeHerdr::new(&[]);
        let rt = runtime(dir.clone(), 2_000);

        peek(&rt, &herdr).expect("peek should succeed");

        assert_eq!(
            load(&dir).len(),
            1,
            "a concurrently-refreshed entry must survive peek's prune"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// Build a QueueEntry from its identity fields for the render tests. Keeps the many literals out
    /// of each test so they read as the (fields) -> rendered-string mapping they are.
    #[allow(clippy::too_many_arguments)] // a test fixture spanning the identity fields
    fn render_entry(
        pane_id: &str,
        workspace_id: &str,
        workspace_label: Option<&str>,
        tab_id: Option<&str>,
        tab_label: Option<&str>,
        pane_label: Option<&str>,
        title: Option<&str>,
        status: WaitStatus,
    ) -> QueueEntry {
        QueueEntry {
            pane_id: pane_id.to_string(),
            workspace_id: workspace_id.to_string(),
            tab_id: tab_id.map(str::to_string),
            workspace_label: workspace_label.map(str::to_string),
            tab_label: tab_label.map(str::to_string),
            pane_label: pane_label.map(str::to_string),
            agent: None,
            display_agent: None,
            title: title.map(str::to_string),
            status,
            enqueued_at_ms: 0,
            last_touched_ms: 0,
        }
    }

    #[test]
    fn entry_destination_uses_human_names_in_go_to_order() {
        // Full identity resolved: workspace label, tab label, and a manual pane label.
        let entry = render_entry(
            "wM:p1",
            "wM",
            Some("claude-feedback"),
            Some("wM:t1"),
            Some("claude"),
            Some("orchestrator"),
            None,
            WaitStatus::Blocked,
        );
        assert_eq!(
            entry_destination(&entry).as_deref(),
            Some("claude-feedback · claude · orchestrator")
        );
    }

    #[test]
    fn entry_destination_falls_back_to_ids_and_pane_number() {
        // An un-enriched event row: no labels at all -> raw workspace id, no tab, `pane {N}`.
        let entry = render_entry(
            "wN:p2",
            "wN",
            None,
            None,
            None,
            None,
            None,
            WaitStatus::Done,
        );
        assert_eq!(entry_destination(&entry).as_deref(), Some("wN · pane 2"));
    }

    #[test]
    fn entry_destination_uses_positional_tab_when_only_the_tab_id_is_known() {
        // tab_id present but no tab label yet -> positional `t{N}`; workspace label present.
        let entry = render_entry(
            "wT:p1",
            "wT",
            Some("herdr-checkin"),
            Some("wT:t2"),
            None,
            None,
            None,
            WaitStatus::Blocked,
        );
        assert_eq!(
            entry_destination(&entry).as_deref(),
            Some("herdr-checkin · t2 · pane 1")
        );
    }

    #[test]
    fn entry_destination_is_none_without_a_workspace_or_pane_number() {
        // pane_id has no positional segment and there is no workspace at all.
        let entry = render_entry("p", "", None, None, None, None, None, WaitStatus::Done);
        assert_eq!(entry_destination(&entry), None);
    }

    #[test]
    fn entry_detail_reads_status_title_then_waited() {
        let entry = render_entry(
            "wN:p2",
            "wN",
            None,
            None,
            None,
            None,
            Some("ran the suite"),
            WaitStatus::Done,
        );
        assert_eq!(entry_detail(&entry, 180_000), "done · ran the suite · 3m");
    }

    #[test]
    fn entry_detail_omits_an_empty_title() {
        let entry = render_entry(
            "wN:p2",
            "wN",
            None,
            None,
            None,
            None,
            None,
            WaitStatus::Blocked,
        );
        assert_eq!(entry_detail(&entry, 60_000), "blocked · 1m");
    }

    #[test]
    fn describe_entry_joins_destination_and_detail_for_the_toast() {
        let entry = render_entry(
            "wM:p1",
            "wM",
            Some("claude-feedback"),
            Some("wM:t1"),
            Some("claude"),
            Some("orchestrator"),
            Some("approve the migration?"),
            WaitStatus::Blocked,
        );
        assert_eq!(
            describe_entry(&entry, 300_000),
            "claude-feedback · claude · orchestrator · blocked · approve the migration? · 5m"
        );
    }

    #[test]
    fn peek_reports_the_queue_and_prunes_stale() {
        let dir = temp_state_dir("peek");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "needs input");
        feed_status(&dir, 2_000, "w2:p1", "w2", "done", "finished");
        // w1:p1 is gone; only w2:p1 remains live.
        let herdr = FakeHerdr::new(&[("w2:p1", "done")]);
        let rt = runtime(dir.clone(), 62_000);

        peek(&rt, &herdr).expect("peek should succeed");

        let notifications = herdr.notifications.into_inner();
        assert_eq!(notifications.len(), 1);
        let (title, body, sound) = &notifications[0];
        assert_eq!(title, "Check-in: 1 agent waiting");
        assert_eq!(sound, "request");
        let body = body.as_deref().expect("body should be present");
        assert!(body.contains("finished"), "body was: {body}");
        assert!(body.contains("done"), "body was: {body}");
        // stale entry pruned from disk
        assert_eq!(load(&dir).len(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn peek_on_empty_queue_says_empty() {
        let dir = temp_state_dir("peek-empty");
        let herdr = FakeHerdr::new(&[]);
        let rt = runtime(dir.clone(), 1_000);
        peek(&rt, &herdr).expect("peek should succeed");
        let notifications = herdr.notifications.into_inner();
        assert_eq!(notifications[0].0, "Check-in: queue empty");
        assert_eq!(notifications[0].1, None);
        assert_eq!(notifications[0].2, "none");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn clear_empties_the_queue() {
        let dir = temp_state_dir("clear");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        feed_status(&dir, 2_000, "w2:p1", "w2", "done", "y");
        let rt = runtime(dir.clone(), 3_000);
        clear(&rt).expect("clear should succeed");
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_seeds_blocked_and_done_and_ignores_others() {
        let dir = temp_state_dir("startup-seed");
        let herdr = FakeHerdr::new(&[
            ("w1:p1", "blocked"),
            ("w2:p1", "working"),
            ("w3:p1", "done"),
            ("w4:p1", "idle"),
        ]);
        let rt = runtime(dir.clone(), 5_000);

        startup(&rt, &herdr).expect("startup should succeed");

        let entries = load(&dir);
        assert_eq!(entries.len(), 2, "only blocked/done panes are seeded");
        assert_eq!(entries[0].pane_id, "w1:p1");
        assert_eq!(entries[0].status, WaitStatus::Blocked);
        assert_eq!(entries[0].enqueued_at_ms, 5_000);
        assert_eq!(entries[1].pane_id, "w3:p1");
        assert_eq!(entries[1].status, WaitStatus::Done);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_upsert_preserves_position_and_enqueued_at() {
        let dir = temp_state_dir("startup-upsert");
        // Persisted before the restart: w1:p1 has been waiting since t=1000.
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "old");
        // After the restart the pane list still shows w1:p1 blocked, plus a fresh waiter.
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked"), ("w9:p1", "done")]);
        let rt = runtime(dir.clone(), 9_000);

        startup(&rt, &herdr).expect("startup should succeed");

        let entries = load(&dir);
        assert_eq!(entries.len(), 2);
        // The persisted waiter keeps its slot and original wait time (upsert, not re-append).
        assert_eq!(entries[0].pane_id, "w1:p1");
        assert_eq!(entries[0].enqueued_at_ms, 1_000);
        // The new waiter is appended, stamped at seed time.
        assert_eq!(entries[1].pane_id, "w9:p1");
        assert_eq!(entries[1].enqueued_at_ms, 9_000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_is_idempotent_when_run_twice() {
        // A cold start immediately followed by a live-handoff takeover can fire the hook twice;
        // the upsert must make the second run a no-op on positions and timestamps.
        let dir = temp_state_dir("startup-twice");
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked"), ("w2:p1", "done")]);
        let rt = runtime(dir.clone(), 5_000);

        startup(&rt, &herdr).expect("first startup should succeed");
        let once = load(&dir);
        startup(&rt, &herdr).expect("second startup should succeed");
        let twice = load(&dir);

        assert_eq!(
            once, twice,
            "running the startup hook twice must be a no-op"
        );
        assert_eq!(twice.len(), 2);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_carries_full_pane_fields() {
        let dir = temp_state_dir("startup-fields");
        let herdr = FakeHerdr::new(&[])
            .with_panes(vec![PaneInfo {
                pane_id: "wA:p1".to_string(),
                workspace_id: "wA".to_string(),
                tab_id: Some("wA:t2".to_string()),
                label: Some("orchestrator".to_string()),
                agent_status: "blocked".to_string(),
                agent: Some("claude".to_string()),
                display_agent: Some("Claude".to_string()),
                title: Some("needs input".to_string()),
            }])
            .with_workspace_labels(&[("wA", "home")])
            .with_tab_labels(&[("wA:t2", "claude")]);
        let rt = runtime(dir.clone(), 7_000);

        startup(&rt, &herdr).expect("startup should succeed");

        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.workspace_id, "wA");
        // Startup resolves the tab id + pane label from `pane list`, the workspace name from
        // `workspace list`, and the tab name from `tab list` (keyed by the tab id).
        assert_eq!(entry.tab_id.as_deref(), Some("wA:t2"));
        assert_eq!(entry.workspace_label.as_deref(), Some("home"));
        assert_eq!(entry.tab_label.as_deref(), Some("claude"));
        assert_eq!(entry.pane_label.as_deref(), Some("orchestrator"));
        assert_eq!(entry.agent.as_deref(), Some("claude"));
        assert_eq!(entry.display_agent.as_deref(), Some("Claude"));
        assert_eq!(entry.title.as_deref(), Some("needs input"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_on_empty_pane_list_is_a_noop() {
        let dir = temp_state_dir("startup-empty");
        let herdr = FakeHerdr::new(&[]);
        let rt = runtime(dir.clone(), 1_000);
        startup(&rt, &herdr).expect("startup should succeed");
        assert!(load(&dir).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn format_waited_reads_naturally() {
        assert_eq!(format_waited(0), "just now");
        assert_eq!(format_waited(59_000), "just now");
        assert_eq!(format_waited(60_000), "1m");
        assert_eq!(format_waited(3_600_000), "1h");
        assert_eq!(format_waited(3_720_000), "1h2m");
    }
}
