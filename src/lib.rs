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

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

mod pane;

const STATE_FILE_NAME: &str = "state.json";
const LOCK_FILE_NAME: &str = "state.lock";
const STATE_VERSION: u32 = 1;

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

// --- event handlers (pure queue transitions; no herdr calls) ---------------

/// `pane.agent_status_changed`: enqueue on `blocked`/`done`, evict on `working`.
/// Other statuses (`idle`, `unknown`) leave the queue untouched.
fn on_status_changed(runtime: &RuntimeEnv) -> Result<(), PluginError> {
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
fn on_focused(runtime: &RuntimeEnv) -> Result<(), PluginError> {
    evict_event_pane(runtime)
}

/// `pane.closed`: the pane is gone, drop any queued entry for it.
fn on_closed(runtime: &RuntimeEnv) -> Result<(), PluginError> {
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

// --- actions ---------------------------------------------------------------

/// Jump to the oldest still-waiting pane, then pop it from the queue. An empty queue is a clean
/// no-op — no error toast.
///
/// Two safety rules keep this from losing the ping it exists to protect:
/// - **Focus first, evict on success only.** The target is kept in the queue while we focus it;
///   only a successful `agent focus` removes it. A focus failure leaves the entry in place.
/// - **Never drop an entry the liveness snapshot couldn't see.** The `pane list` snapshot is
///   taken before the lock, so an entry enqueued after it would look stale. We prune an entry as
///   stale only if it predates the snapshot; newer ones are kept (this window is exactly when you
///   press `next` as an agent blocks).
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
            if entry.enqueued_at_ms >= snapshot_ms {
                kept.push(entry); // too new for the snapshot to judge — keep it
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
            .filter(|entry| is_live(&live, &entry.pane_id) || entry.enqueued_at_ms >= snapshot_ms)
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

// --- queue transitions -----------------------------------------------------

/// Add or refresh an entry for a pane. Deduplicates per pane: if the pane is
/// already queued, its fields and status are updated in place, preserving its
/// FIFO position and original `enqueued_at_ms` (it has been waiting since the
/// first ping). Otherwise it is appended to the back.
fn enqueue(entries: &mut Vec<QueueEntry>, event: &StatusEvent, status: WaitStatus, now_ms: u64) {
    if let Some(existing) = entries.iter_mut().find(|e| e.pane_id == event.pane_id) {
        existing.workspace_id = event.workspace_id.clone();
        existing.agent = event.agent.clone();
        existing.display_agent = event.display_agent.clone();
        existing.title = event.title.clone();
        existing.status = status;
    } else {
        entries.push(QueueEntry {
            pane_id: event.pane_id.clone(),
            workspace_id: event.workspace_id.clone(),
            agent: event.agent.clone(),
            display_agent: event.display_agent.clone(),
            title: event.title.clone(),
            status,
            enqueued_at_ms: now_ms,
        });
    }
}

/// Remove any entry for the given pane.
fn evict(entries: &mut Vec<QueueEntry>, pane_id: &str) {
    entries.retain(|entry| entry.pane_id != pane_id);
}

/// A queued pane is still worth jumping to if it exists and has not resumed
/// working. Missing pane => gone; `working` => the agent picked back up.
fn is_live(live: &HashMap<String, String>, pane_id: &str) -> bool {
    match live.get(pane_id) {
        Some(status) => status != "working",
        None => false,
    }
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

// --- herdr interface -------------------------------------------------------

/// A pane as reported by `herdr pane list` — the subset of `PaneInfo` we seed the queue from.
/// Same fields an event carries, so a scan can build full-fidelity queue entries. We deliberately
/// do NOT carry `focused`: the seed queues every blocked/done pane regardless of which pane herdr
/// mechanically restored focus to on restart (that is not a user "I looked at it" action, so it
/// must not suppress the ping). Focus-based eviction stays where it belongs — the event path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneInfo {
    pane_id: String,
    workspace_id: String,
    agent_status: String,
    agent: Option<String>,
    display_agent: Option<String>,
    title: Option<String>,
}

trait Herdr {
    /// Map of live `pane_id -> agent_status` from `herdr pane list`.
    fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError>;
    /// Full `pane list` info, used by the startup hook to re-seed the queue.
    fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError>;
    /// Bring the agent in the given pane into focus (jumps workspace/tab/pane).
    fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError>;
    /// Show a herdr toast.
    fn show_notification(
        &self,
        title: &str,
        body: Option<&str>,
        sound: &str,
    ) -> Result<(), PluginError>;
}

struct CliHerdr {
    bin_path: PathBuf,
}

impl CliHerdr {
    /// Run `herdr pane list` and return its raw stdout, or an error if the command failed.
    /// Shared by [`pane_status_map`](Self::pane_status_map) and [`pane_infos`](Self::pane_infos)
    /// so both parse the same response without duplicating the spawn/error handling.
    fn pane_list_stdout(&self) -> Result<Vec<u8>, PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("pane")
            .arg("list")
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH pane list ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if !output.status.success() {
            return Err(command_failure("HERDR_BIN_PATH pane list", &output));
        }

        Ok(output.stdout)
    }
}

impl Herdr for CliHerdr {
    fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError> {
        parse_pane_status_map(&self.pane_list_stdout()?)
    }

    fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError> {
        parse_pane_infos(&self.pane_list_stdout()?)
    }

    fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("agent")
            .arg("focus")
            .arg(pane_id)
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH agent focus {pane_id} ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("HERDR_BIN_PATH agent focus", &output))
        }
    }

    fn show_notification(
        &self,
        title: &str,
        body: Option<&str>,
        sound: &str,
    ) -> Result<(), PluginError> {
        let mut command = Command::new(&self.bin_path);
        command.arg("notification").arg("show").arg(title);
        if let Some(body) = body {
            command.arg("--body").arg(body);
        }
        command.arg("--sound").arg(sound);

        let output = command.output().map_err(|error| {
            PluginError::new(format!(
                "failed to run HERDR_BIN_PATH notification show ({}): {error}",
                self.bin_path.display()
            ))
        })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("HERDR_BIN_PATH notification show", &output))
        }
    }
}

fn command_failure(command: &str, output: &Output) -> PluginError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        output.status.to_string()
    };
    PluginError::new(format!("{command} failed: {detail}"))
}

fn parse_pane_status_map(stdout: &[u8]) -> Result<HashMap<String, String>, PluginError> {
    Ok(parse_pane_infos(stdout)?
        .into_iter()
        .map(|pane| (pane.pane_id, pane.agent_status))
        .collect())
}

/// Parse `herdr pane list` into the fields the queue needs. Preserves the panes' returned order
/// (a `Vec`, not a map) so a re-seed is deterministic. Panes without a `pane_id` are skipped;
/// missing `agent_status` falls back to `"unknown"` (which the seed ignores — not a wait status).
fn parse_pane_infos(stdout: &[u8]) -> Result<Vec<PaneInfo>, PluginError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|error| {
        PluginError::new(format!(
            "failed to parse HERDR_BIN_PATH pane list JSON: {error}"
        ))
    })?;

    if let Some(error) = herdr_error_message(&value) {
        return Err(PluginError::new(format!(
            "HERDR_BIN_PATH pane list returned an error: {error}"
        )));
    }

    let panes = value
        .get("result")
        .and_then(|result| result.get("panes"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PluginError::new("HERDR_BIN_PATH pane list returned an unexpected response".to_string())
        })?;

    let mut infos = Vec::with_capacity(panes.len());
    for pane in panes {
        let Some(pane_id) = non_empty_string(pane, "pane_id") else {
            continue;
        };
        infos.push(PaneInfo {
            pane_id,
            workspace_id: non_empty_string(pane, "workspace_id").unwrap_or_default(),
            agent_status: non_empty_string(pane, "agent_status")
                .unwrap_or_else(|| "unknown".to_string()),
            agent: non_empty_string(pane, "agent"),
            display_agent: non_empty_string(pane, "display_agent"),
            title: non_empty_string(pane, "title"),
        });
    }
    Ok(infos)
}

fn herdr_error_message(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    // A present-but-null `error` is a success shape, not a failure.
    if error.is_null() {
        return None;
    }
    let code = error.get("code").and_then(Value::as_str);
    let message = error.get("message").and_then(Value::as_str);

    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (Some(code), None) => Some(code.to_string()),
        (None, Some(message)) => Some(message.to_string()),
        (None, None) => Some(error.to_string()),
    }
}

// --- event parsing ---------------------------------------------------------

struct StatusEvent {
    pane_id: String,
    workspace_id: String,
    agent_status: String,
    agent: Option<String>,
    display_agent: Option<String>,
    title: Option<String>,
}

impl StatusEvent {
    fn wait_status(&self) -> Option<WaitStatus> {
        match self.agent_status.as_str() {
            "blocked" => Some(WaitStatus::Blocked),
            "done" => Some(WaitStatus::Done),
            _ => None,
        }
    }

    fn is_working(&self) -> bool {
        self.agent_status == "working"
    }
}

/// The plugin event JSON is `{ "event": ..., "data": { "type": ..., <fields> } }`.
/// Fields are read from `data`, falling back to the top-level object.
fn parse_status_event(raw: &str) -> Option<StatusEvent> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let data = event_data(&value);
    Some(StatusEvent {
        pane_id: non_empty_string(data, "pane_id")?,
        workspace_id: non_empty_string(data, "workspace_id").unwrap_or_default(),
        agent_status: non_empty_string(data, "agent_status")?,
        agent: non_empty_string(data, "agent"),
        display_agent: non_empty_string(data, "display_agent"),
        title: non_empty_string(data, "title"),
    })
}

fn parse_event_string(raw: &str, key: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    non_empty_string(event_data(&value), key)
}

fn event_data(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

// --- persisted state -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WaitStatus {
    Blocked,
    Done,
}

impl WaitStatus {
    fn as_str(&self) -> &'static str {
        match self {
            WaitStatus::Blocked => "blocked",
            WaitStatus::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QueueEntry {
    pane_id: String,
    workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    status: WaitStatus,
    enqueued_at_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<QueueEntry>,
}

struct LoadedState {
    entries: Vec<QueueEntry>,
    needs_repair: bool,
}

struct StateStore {
    state_dir: PathBuf,
}

impl StateStore {
    fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
        }
    }

    /// Load the queue under an exclusive lock, apply `change`, and persist the
    /// result if it changed (or if the on-disk form needed repair).
    fn update<T>(
        &self,
        change: impl FnOnce(Vec<QueueEntry>) -> (Vec<QueueEntry>, T),
    ) -> Result<T, PluginError> {
        fs::create_dir_all(&self.state_dir).map_err(|error| {
            PluginError::new(format!(
                "failed to create plugin state directory {}: {error}",
                self.state_dir.display()
            ))
        })?;

        let _lock = StateLock::acquire(&self.state_dir.join(LOCK_FILE_NAME))?;
        let loaded = read_state(&self.state_dir.join(STATE_FILE_NAME))?;
        let previous = loaded.entries.clone();
        let (next, result) = change(loaded.entries);

        if loaded.needs_repair || next != previous {
            write_state(&self.state_dir.join(STATE_FILE_NAME), &next)?;
        }

        Ok(result)
    }
}

struct StateLock {
    file: File,
}

impl StateLock {
    fn acquire(path: &Path) -> Result<Self, PluginError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to open plugin state lock {}: {error}",
                    path.display()
                ))
            })?;

        file.lock_exclusive().map_err(|error| {
            PluginError::new(format!(
                "failed to lock plugin state {}: {error}",
                path.display()
            ))
        })?;

        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn read_state(path: &Path) -> Result<LoadedState, PluginError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedState {
                entries: Vec::new(),
                needs_repair: false,
            });
        }
        Err(error) => {
            return Err(PluginError::new(format!(
                "failed to read plugin state {}: {error}",
                path.display()
            )));
        }
    };

    match serde_json::from_str::<PersistedState>(&contents) {
        Ok(state) => Ok(LoadedState {
            // Rewrite (to stamp the current version) only when the file is from an older schema.
            // A forward-version file — written by a newer plugin — is left as-is here rather than
            // silently rewritten down to our version, so we don't strip fields we can't model.
            needs_repair: state.version < STATE_VERSION,
            entries: state.entries,
        }),
        Err(_) => Ok(LoadedState {
            entries: Vec::new(),
            needs_repair: true,
        }),
    }
}

fn write_state(path: &Path, entries: &[QueueEntry]) -> Result<(), PluginError> {
    let parent = path.parent().ok_or_else(|| {
        PluginError::new(format!(
            "plugin state path has no parent directory: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        PluginError::new(format!(
            "failed to create plugin state directory {}: {error}",
            parent.display()
        ))
    })?;

    let temp_path = parent.join(format!(
        ".{STATE_FILE_NAME}.tmp.{}.{}",
        std::process::id(),
        current_unix_ms()
    ));
    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|error| {
            PluginError::new(format!(
                "failed to create temporary plugin state {}: {error}",
                temp_path.display()
            ))
        })?;
    let state = PersistedState {
        version: STATE_VERSION,
        entries: entries.to_vec(),
    };
    // Any failure after the temp file exists must not leave it behind as litter in the state dir.
    serde_json::to_writer_pretty(&mut temp_file, &state).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to serialize plugin state {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.write_all(b"\n").map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to write plugin state {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.sync_all().map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to sync plugin state {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to replace plugin state {}: {error}",
            path.display()
        ))
    })
}

/// Read the current queue for read-only display (the status pane). Reads without the lock —
/// writes are atomic temp+rename, so a reader always sees a complete file — and degrades to an
/// empty queue on any error. Mutations must still go through [`StateStore::update`].
fn load_entries(state_dir: &Path) -> Vec<QueueEntry> {
    read_state(&state_dir.join(STATE_FILE_NAME))
        .map(|loaded| loaded.entries)
        .unwrap_or_default()
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginError {
    message: String,
}

impl PluginError {
    fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeHerdr {
        live: HashMap<String, String>,
        panes: Vec<PaneInfo>,
        focus_fails: bool,
        focused: RefCell<Vec<String>>,
        notifications: RefCell<Vec<(String, Option<String>, String)>>,
    }

    impl FakeHerdr {
        fn new(panes: &[(&str, &str)]) -> Self {
            Self {
                live: panes
                    .iter()
                    .map(|(pane_id, status)| (pane_id.to_string(), status.to_string()))
                    .collect(),
                // Mirror `pane list`: derive full PaneInfos so `pane_infos()` and
                // `pane_status_map()` stay consistent. Workspace is the pane-id prefix (as herdr's
                // `wS:pN` ids encode it); agent/title are absent unless a test overrides `panes`.
                panes: panes
                    .iter()
                    .map(|(pane_id, status)| PaneInfo {
                        pane_id: pane_id.to_string(),
                        workspace_id: pane_id.split(':').next().unwrap_or("").to_string(),
                        agent_status: status.to_string(),
                        agent: None,
                        display_agent: None,
                        title: None,
                    })
                    .collect(),
                focus_fails: false,
                focused: RefCell::new(Vec::new()),
                notifications: RefCell::new(Vec::new()),
            }
        }

        fn with_failing_focus(mut self) -> Self {
            self.focus_fails = true;
            self
        }

        /// Override the `pane list` result with hand-built PaneInfos (for field-fidelity tests).
        fn with_panes(mut self, panes: Vec<PaneInfo>) -> Self {
            self.panes = panes;
            self
        }
    }

    impl Herdr for FakeHerdr {
        fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError> {
            Ok(self.live.clone())
        }

        fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError> {
            Ok(self.panes.clone())
        }

        fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError> {
            if self.focus_fails {
                return Err(PluginError::new(format!("focus refused for {pane_id}")));
            }
            self.focused.borrow_mut().push(pane_id.to_string());
            Ok(())
        }

        fn show_notification(
            &self,
            title: &str,
            body: Option<&str>,
            sound: &str,
        ) -> Result<(), PluginError> {
            self.notifications.borrow_mut().push((
                title.to_string(),
                body.map(str::to_owned),
                sound.to_string(),
            ));
            Ok(())
        }
    }

    fn runtime(state_dir: PathBuf, now_ms: u64) -> RuntimeEnv {
        RuntimeEnv {
            state_dir,
            event_json: None,
            now_ms,
        }
    }

    fn temp_state_dir(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "herdr-checkin-{label}-{}-{}",
            std::process::id(),
            current_unix_ms()
        ));
        fs::create_dir_all(&path).expect("temp state directory should be created");
        path
    }

    fn load(state_dir: &Path) -> Vec<QueueEntry> {
        read_state(&state_dir.join(STATE_FILE_NAME))
            .expect("state should load")
            .entries
    }

    fn status_event_json(pane_id: &str, workspace_id: &str, status: &str, title: &str) -> String {
        format!(
            r#"{{"event":"pane_agent_status_changed","data":{{"type":"pane_agent_status_changed","pane_id":"{pane_id}","workspace_id":"{workspace_id}","agent_status":"{status}","agent":"claude","display_agent":"Claude","title":"{title}"}}}}"#
        )
    }

    fn pane_event_json(kind: &str, pane_id: &str, workspace_id: &str) -> String {
        format!(
            r#"{{"event":"{kind}","data":{{"type":"{kind}","pane_id":"{pane_id}","workspace_id":"{workspace_id}"}}}}"#
        )
    }

    fn feed_status(state_dir: &Path, now_ms: u64, pane: &str, ws: &str, status: &str, title: &str) {
        let mut runtime = runtime(state_dir.to_path_buf(), now_ms);
        runtime.event_json = Some(status_event_json(pane, ws, status, title));
        on_status_changed(&runtime).expect("status-changed should succeed");
    }

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
