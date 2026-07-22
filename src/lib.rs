//! herdr-checkin — a durable attention queue for agent panes.
//!
//! Herdr's native `open_notification_target` only jumps to the toast that is
//! currently on screen, so a ping is lost once its toast fades and multiple
//! pings cannot queue. This plugin remembers them: agents that go `blocked`
//! (need input) or `done` (finished) are enqueued FIFO, and the `next` action
//! jumps to the oldest waiter and pops it. Entries are evicted when their pane
//! is focused, returns to `working`, or closes.
//!
//! herdr invokes this binary once per event and once per action; all state
//! lives in `state.json` under `HERDR_PLUGIN_STATE_DIR`, guarded by a lockfile.

use std::env;
use std::path::PathBuf;

mod herdr;
mod pane;
mod queue;
mod state;
#[cfg(test)]
pub(crate) mod test_support;

use herdr::{CliHerdr, StatusEvent};
use queue::{enqueue, is_live, on_closed, on_focused, on_status_changed};

pub(crate) use herdr::Herdr;
pub(crate) use queue::evict;
pub(crate) use state::{current_unix_ms, load_entries, PluginError, QueueEntry, StateStore};
// Only pane.rs's test fixtures name `WaitStatus` directly (production code only ever reaches it
// through `QueueEntry::status`), so the re-export is test-only to keep the release build clean.
#[cfg(test)]
pub(crate) use state::WaitStatus;

/// Entrypoint used by `main`: parse argv, read the herdr-provided environment,
/// dispatch, and translate the result into a process exit code.
pub fn run_from_env() -> i32 {
    let subcommand = match parse_subcommand(env::args().skip(1)) {
        Ok(subcommand) => subcommand,
        Err(ParseCommandError::Usage(message)) => {
            eprintln!("{message}");
            return 2;
        }
    };

    // The launch decision reads `pane list` JSON on stdin and prints OPEN/FOCUS/CLOSE — it needs
    // neither the state dir nor the herdr binary, and must never fail the launcher, so it runs
    // before the environment checks below.
    if subcommand == Subcommand::PaneDecision {
        return pane::decide_from_stdin();
    }

    let state_dir = match env_path("HERDR_PLUGIN_STATE_DIR") {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let herdr_bin_path = match env_path("HERDR_BIN_PATH") {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };

    let runtime = RuntimeEnv {
        state_dir,
        event_json: env::var("HERDR_PLUGIN_EVENT_JSON").ok(),
        now_ms: current_unix_ms(),
    };
    let herdr = CliHerdr {
        bin_path: herdr_bin_path,
    };

    match run(subcommand, &runtime, &herdr) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run(subcommand: Subcommand, runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    match subcommand {
        Subcommand::StatusChanged => on_status_changed(runtime),
        Subcommand::Focused => on_focused(runtime),
        Subcommand::Closed => on_closed(runtime),
        Subcommand::Next => next(runtime, herdr),
        Subcommand::Peek => peek(runtime, herdr),
        Subcommand::Clear => clear(runtime),
        Subcommand::Startup => startup(runtime, herdr),
        Subcommand::Pane => pane::run(runtime, herdr),
        // Handled in `run_from_env` before the environment is read.
        Subcommand::PaneDecision => unreachable!("pane-decision is dispatched before run()"),
    }
}

// --- actions ---------------------------------------------------------------

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
fn next(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
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
fn peek(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
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
fn clear(runtime: &RuntimeEnv) -> Result<(), PluginError> {
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
fn startup(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let panes = herdr.pane_infos()?;
    let now_ms = runtime.now_ms;

    StateStore::new(&runtime.state_dir).update(|mut entries| {
        for pane in &panes {
            let event = StatusEvent {
                pane_id: pane.pane_id.clone(),
                workspace_id: pane.workspace_id.clone(),
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

// --- toast copy ------------------------------------------------------------

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

fn describe_entry(entry: &QueueEntry, now_ms: u64) -> String {
    let who = entry
        .display_agent
        .as_deref()
        .filter(|value| !value.is_empty())
        .or(entry.agent.as_deref().filter(|value| !value.is_empty()))
        .unwrap_or(&entry.pane_id);
    let waited = format_waited(now_ms.saturating_sub(entry.enqueued_at_ms));
    let status = entry.status.as_str();
    // The bracketed suffix carries the workspace and wait time; omit the workspace when it is
    // missing so it never renders as an empty "[, 3m]".
    let meta = if entry.workspace_id.is_empty() {
        waited
    } else {
        format!("{}, {waited}", entry.workspace_id)
    };
    match entry.title.as_deref().filter(|value| !value.is_empty()) {
        Some(title) => format!("{who} — {status} — {title} [{meta}]"),
        None => format!("{who} — {status} [{meta}]"),
    }
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

// --- subcommand parsing ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subcommand {
    StatusChanged,
    Focused,
    Closed,
    Next,
    Peek,
    Clear,
    Startup,
    Pane,
    PaneDecision,
}

enum ParseCommandError {
    Usage(String),
}

fn parse_subcommand<I>(args: I) -> Result<Subcommand, ParseCommandError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let Some(raw_subcommand) = args.next() else {
        return Err(ParseCommandError::Usage(usage()));
    };
    if args.next().is_some() {
        return Err(ParseCommandError::Usage(usage()));
    }

    match raw_subcommand.as_str() {
        "status-changed" => Ok(Subcommand::StatusChanged),
        "focused" => Ok(Subcommand::Focused),
        "closed" => Ok(Subcommand::Closed),
        "next" => Ok(Subcommand::Next),
        "peek" => Ok(Subcommand::Peek),
        "clear" => Ok(Subcommand::Clear),
        "startup" => Ok(Subcommand::Startup),
        "pane" => Ok(Subcommand::Pane),
        "pane-decision" => Ok(Subcommand::PaneDecision),
        "help" | "--help" | "-h" => Err(ParseCommandError::Usage(usage())),
        other => Err(ParseCommandError::Usage(format!(
            "unknown subcommand: {other}\n{}",
            usage()
        ))),
    }
}

fn usage() -> String {
    "usage: herdr-checkin <status-changed|focused|closed|next|peek|clear|startup|pane|pane-decision>"
        .to_string()
}

fn env_path(name: &str) -> Result<PathBuf, PluginError> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            PluginError::new(format!(
                "{name} is not set; herdr should provide this environment variable to plugin commands"
            ))
        })
}

struct RuntimeEnv {
    state_dir: PathBuf,
    event_json: Option<String>,
    now_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use herdr::{herdr_error_message, parse_pane_infos, PaneInfo};
    use serde_json::Value;
    use state::STATE_FILE_NAME;
    use std::fs;
    use test_support::{feed_status, load, pane_event_json, runtime, temp_state_dir, FakeHerdr};

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

    #[test]
    fn state_file_without_last_touched_loads_and_defaults_to_zero() {
        // Backward compatibility: a pre-0.2.x state.json has no last_touched_ms field.
        let dir = temp_state_dir("legacy-state");
        fs::write(
            dir.join(STATE_FILE_NAME),
            r#"{"version":1,"entries":[{"pane_id":"w1:p1","workspace_id":"w1","status":"blocked","enqueued_at_ms":1000}]}"#,
        )
        .expect("legacy state should write");
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].enqueued_at_ms, 1_000);
        assert_eq!(entries[0].last_touched_ms, 0, "missing field defaults to 0");
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
        on_status_changed(&rt).expect("status-changed should parse the flat shape");
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pane_id, "w1:p1");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn describe_entry_omits_a_missing_workspace() {
        let entry = QueueEntry {
            pane_id: "p".to_string(),
            workspace_id: String::new(),
            agent: None,
            display_agent: None,
            title: None,
            status: WaitStatus::Done,
            enqueued_at_ms: 0,
            last_touched_ms: 0,
        };
        let line = describe_entry(&entry, 0);
        assert!(!line.contains("[,"), "line was: {line}");
        assert!(line.ends_with("[just now]"), "line was: {line}");
    }

    #[test]
    fn null_error_field_is_treated_as_success() {
        let value: Value = serde_json::from_str(r#"{"result":{"panes":[]},"error":null}"#).unwrap();
        assert_eq!(herdr_error_message(&value), None);
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
        let herdr = FakeHerdr::new(&[]).with_panes(vec![PaneInfo {
            pane_id: "wA:p1".to_string(),
            workspace_id: "wA".to_string(),
            agent_status: "blocked".to_string(),
            agent: Some("claude".to_string()),
            display_agent: Some("Claude".to_string()),
            title: Some("needs input".to_string()),
        }]);
        let rt = runtime(dir.clone(), 7_000);

        startup(&rt, &herdr).expect("startup should succeed");

        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.workspace_id, "wA");
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
    fn parse_pane_infos_extracts_wait_fields_and_skips_idless_panes() {
        let json = br#"{"result":{"type":"pane_list","panes":[
            {"pane_id":"wA:p1","workspace_id":"wA","agent_status":"blocked","agent":"claude","display_agent":"Claude","title":"needs input","focused":true},
            {"pane_id":"","agent_status":"done"},
            {"agent_status":"done"}
        ]}}"#;
        let infos = parse_pane_infos(json).expect("pane list should parse");
        assert_eq!(infos.len(), 1, "panes without a pane_id are skipped");
        assert_eq!(infos[0].pane_id, "wA:p1");
        assert_eq!(infos[0].workspace_id, "wA");
        assert_eq!(infos[0].agent_status, "blocked");
        assert_eq!(infos[0].title.as_deref(), Some("needs input"));
    }

    #[test]
    fn malformed_state_is_repaired_to_empty() {
        let dir = temp_state_dir("malformed");
        fs::write(dir.join(STATE_FILE_NAME), "not json").expect("write malformed state");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pane_id, "w1:p1");
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
