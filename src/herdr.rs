//! The herdr CLI seam (the `Herdr` trait and its `herdr pane list`/`agent focus`/`notification
//! show` implementation) and the JSON parsing for both `pane list` responses and plugin event
//! payloads.

use crate::roster::{AgentStatus, RosterAgent};
use crate::state::{PluginError, WaitStatus};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::{Command, Output};

/// A pane as reported by `herdr pane list` — the subset of `PaneInfo` we seed the queue from.
/// Same fields an event carries, so a scan can build full-fidelity queue entries. We deliberately
/// do NOT carry `focused`: the seed queues every blocked/done pane regardless of which pane herdr
/// mechanically restored focus to on restart (that is not a user "I looked at it" action, so it
/// must not suppress the ping). Focus-based eviction stays where it belongs — the event path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneInfo {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    /// The pane's tab id (`wS:t2`). Present in `pane list` but NOT in the event payload — the reason
    /// the enqueue path must look a pane up here to learn its tab.
    pub(crate) tab_id: Option<String>,
    /// The pane's manual label (`terminal.manual_label`, e.g. `orchestrator`) — usually null. When
    /// set, herdr's own go-to picker shows it in place of `pane {N}`, so the row mirrors that.
    pub(crate) label: Option<String>,
    pub(crate) agent_status: String,
    pub(crate) agent: Option<String>,
    pub(crate) display_agent: Option<String>,
    pub(crate) title: Option<String>,
}

pub(crate) trait Herdr {
    /// Map of live `pane_id -> agent_status` from `herdr pane list`.
    fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError>;
    /// Full `pane list` info, used by the startup hook to re-seed the queue.
    fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError>;
    /// Map of `workspace_id -> human label` from `herdr workspace list` (e.g. `w4 -> "home"`). Used
    /// to render a readable workspace name on each row instead of the raw positional id.
    fn workspace_labels(&self) -> Result<HashMap<String, String>, PluginError>;
    /// Map of `tab_id -> label` from `herdr tab list` (e.g. `w4:t2 -> "claude"`). The tab label is
    /// usually the running program, exactly what herdr's go-to picker shows for the tab.
    fn tab_labels(&self) -> Result<HashMap<String, String>, PluginError>;
    /// The raw roster from `herdr agent list` (one spawn, no enrichment): every detected agent pane,
    /// all states, carrying the `agent_session` uuid and `focused` flag the queue path ignores. The
    /// location labels (`workspace_label`/`tab_label`/`pane_label`) are left `None` — callers fill
    /// them via [`enrich_roster_labels`] or the cached [`sample_roster`].
    fn agent_roster(&self) -> Result<Vec<RosterAgent>, PluginError>;
    /// The live roster, enriched with herdr's human names in one shot (four spawns: `agent list` plus
    /// `workspace`/`tab`/`pane list`). For a one-off dump; the sampler uses [`sample_roster`], which
    /// caches the label maps instead of refetching them every sample.
    fn agent_list(&self) -> Result<Vec<RosterAgent>, PluginError>;
    /// Read a pane's terminal snapshot (`herdr agent read <pane_id> --source recent --format text`) —
    /// the recent on-screen output, from which [`crate::roster::last_terminal_line`] extracts the
    /// agent's last line for the Agents view (Slice 4). Errs when the target is not a readable agent
    /// pane (herdr rejects some panes); the tail sweep treats that as "no reading this time" and keeps
    /// the pane's cached line, so a failure never blanks a row.
    fn read_terminal_tail(&self, pane_id: &str) -> Result<String, PluginError>;
    /// Bring the agent in the given pane into focus (jumps workspace/tab/pane).
    fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError>;
    /// Submit `text` as a reply into the agent in the given pane (routes into its session).
    /// The target is the stored `pane_id` (verified: the `agent_session` uuid is not accepted).
    /// Fire-and-forget: no `--wait` (its settled-state gate is unreliable from a non-working start).
    fn prompt_agent(&self, pane_id: &str, text: &str) -> Result<(), PluginError>;
    /// Show a herdr toast.
    fn show_notification(
        &self,
        title: &str,
        body: Option<&str>,
        sound: &str,
    ) -> Result<(), PluginError>;
    /// Close herdr's session-modal popup (the `popup.close` socket method). The status pane, when
    /// launched via `--placement popup`, calls this on exit so herdr does not keep painting a dead
    /// popup frame until the next keypress. There is no `herdr popup close` CLI verb, so this talks
    /// the newline-delimited-JSON socket at `HERDR_SOCKET_PATH` directly. Best-effort at the call
    /// sites — a failure just falls back to herdr's own child-exit cleanup.
    fn popup_close(&self) -> Result<(), PluginError>;
}

pub(crate) struct CliHerdr {
    pub(crate) bin_path: PathBuf,
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

    /// Run `herdr workspace list` and return its raw stdout, or an error if the command failed.
    fn workspace_list_stdout(&self) -> Result<Vec<u8>, PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("workspace")
            .arg("list")
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH workspace list ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if !output.status.success() {
            return Err(command_failure("HERDR_BIN_PATH workspace list", &output));
        }

        Ok(output.stdout)
    }

    /// Run `herdr tab list` and return its raw stdout, or an error if the command failed.
    fn tab_list_stdout(&self) -> Result<Vec<u8>, PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("tab")
            .arg("list")
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH tab list ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if !output.status.success() {
            return Err(command_failure("HERDR_BIN_PATH tab list", &output));
        }

        Ok(output.stdout)
    }

    /// Run `herdr agent list` and return its raw stdout, or an error if the command failed.
    fn agent_list_stdout(&self) -> Result<Vec<u8>, PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("agent")
            .arg("list")
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH agent list ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if !output.status.success() {
            return Err(command_failure("HERDR_BIN_PATH agent list", &output));
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

    fn workspace_labels(&self) -> Result<HashMap<String, String>, PluginError> {
        parse_workspace_labels(&self.workspace_list_stdout()?)
    }

    fn tab_labels(&self) -> Result<HashMap<String, String>, PluginError> {
        parse_tab_labels(&self.tab_list_stdout()?)
    }

    fn agent_roster(&self) -> Result<Vec<RosterAgent>, PluginError> {
        parse_agent_list(&self.agent_list_stdout()?)
    }

    fn agent_list(&self) -> Result<Vec<RosterAgent>, PluginError> {
        let mut roster = self.agent_roster()?;
        enrich_roster_labels(self, &mut roster);
        Ok(roster)
    }

    fn read_terminal_tail(&self, pane_id: &str) -> Result<String, PluginError> {
        // `recent` (herdr's default source) returns the recent on-screen rows — enough history to
        // reach the agent's last output above its pinned input box. `text` strips ANSI (we only want
        // the characters). We do NOT cap `--lines`: a small cap returns only the bottom chrome rows.
        let output = Command::new(&self.bin_path)
            .arg("agent")
            .arg("read")
            .arg(pane_id)
            .arg("--source")
            .arg("recent")
            .arg("--format")
            .arg("text")
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH agent read {pane_id} ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if !output.status.success() {
            return Err(command_failure("HERDR_BIN_PATH agent read", &output));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

    fn prompt_agent(&self, pane_id: &str, text: &str) -> Result<(), PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("agent")
            .arg("prompt")
            .arg(pane_id)
            .arg(text)
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH agent prompt {pane_id} ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("HERDR_BIN_PATH agent prompt", &output))
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

    fn popup_close(&self) -> Result<(), PluginError> {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let socket_path = std::env::var("HERDR_SOCKET_PATH").map_err(|_| {
            PluginError::new("HERDR_SOCKET_PATH is not set (not running inside herdr)".to_string())
        })?;
        let mut stream = UnixStream::connect(&socket_path).map_err(|error| {
            PluginError::new(format!(
                "failed to connect to herdr socket ({socket_path}): {error}"
            ))
        })?;
        // Newline-delimited JSON, one request per line (herdr's socket protocol).
        let mut line = popup_close_request_line();
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|error| PluginError::new(format!("failed to send popup.close: {error}")))
    }
}

/// The `popup.close` request as a single JSON line (no trailing newline). The wire shape is
/// verified against herdr 0.7.5: a flattened `{ "id", "method": "popup.close", "params": {} }`.
fn popup_close_request_line() -> String {
    serde_json::json!({
        "id": "herdr-checkin-popup-close",
        "method": "popup.close",
        "params": {},
    })
    .to_string()
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
            tab_id: non_empty_string(pane, "tab_id"),
            label: non_empty_string(pane, "label"),
            agent_status: non_empty_string(pane, "agent_status")
                .unwrap_or_else(|| "unknown".to_string()),
            agent: non_empty_string(pane, "agent"),
            display_agent: non_empty_string(pane, "display_agent"),
            title: non_empty_string(pane, "title"),
        });
    }
    Ok(infos)
}

/// Parse `herdr agent list` into [`RosterAgent`]s, preserving the returned order (grouping happens
/// later, in `roster.rs`). Agents without a `pane_id` are skipped; a missing/empty `agent_status`
/// folds to [`AgentStatus::Unknown`]; the session uuid is read from the nested `agent_session.value`
/// (absent for some agents, so `Option`). A herdr error or an unexpected shape surfaces as `Err`.
fn parse_agent_list(stdout: &[u8]) -> Result<Vec<RosterAgent>, PluginError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|error| {
        PluginError::new(format!(
            "failed to parse HERDR_BIN_PATH agent list JSON: {error}"
        ))
    })?;

    if let Some(error) = herdr_error_message(&value) {
        return Err(PluginError::new(format!(
            "HERDR_BIN_PATH agent list returned an error: {error}"
        )));
    }

    let agents = value
        .get("result")
        .and_then(|result| result.get("agents"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PluginError::new(
                "HERDR_BIN_PATH agent list returned an unexpected response".to_string(),
            )
        })?;

    let mut roster = Vec::with_capacity(agents.len());
    for agent in agents {
        let Some(pane_id) = non_empty_string(agent, "pane_id") else {
            continue;
        };
        let status = agent
            .get("agent_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        roster.push(RosterAgent {
            pane_id,
            workspace_id: non_empty_string(agent, "workspace_id").unwrap_or_default(),
            tab_id: non_empty_string(agent, "tab_id"),
            agent: non_empty_string(agent, "agent"),
            agent_status: AgentStatus::parse(status),
            agent_session: agent_session_value(agent),
            cwd: non_empty_string(agent, "cwd"),
            focused: agent
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            terminal_title: non_empty_string(agent, "terminal_title"),
            // Not in `agent list` (it carries no timestamps, design §4); the roster sampler fills it
            // from the `roster.json` registry after parse (see `roster_state::reconcile_roster`).
            status_since_ms: None,
            // Labels are not in `agent list`; the CliHerdr seam enriches them (see `agent_list`).
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            // Terminal contents are not in `agent list`; the sampler's tail sweep fills this from
            // `agent read` after parse (see `TailCache::refresh`).
            last_line: None,
        });
    }
    Ok(roster)
}

/// Enrich a parsed roster with herdr's human names for each pane's workspace/tab/pane. `agent list`
/// carries only positional ids (`w4`, `w4:t1`), so we resolve labels from `workspace list`/`tab
/// list`/`pane list` — the same sources the Queue enriches from — so the Agents view reads like
/// herdr's own sidebar (`home · ~`) rather than raw ids. **Best-effort:** a lookup that fails leaves
/// that label `None` and the view falls back to the id; cosmetic enrichment must never fail the
/// roster (losing a name degrades the display, never a waiter).
fn enrich_roster_labels(herdr: &dyn Herdr, roster: &mut [RosterAgent]) {
    let workspaces = herdr.workspace_labels().unwrap_or_default();
    let tabs = herdr.tab_labels().unwrap_or_default();
    let pane_labels = pane_label_map(herdr);
    apply_labels(&workspaces, &tabs, &pane_labels, roster);
}

/// Fill each roster agent's human names from the given label maps (`workspace_id -> label`,
/// `tab_id -> label`, `pane_id -> manual pane label`). Pure: a missing entry leaves that label `None`
/// and the row falls back to the id. Shared by the one-shot [`enrich_roster_labels`] and the cached
/// [`sample_roster`], so both render identical names from identical maps.
fn apply_labels(
    workspaces: &HashMap<String, String>,
    tabs: &HashMap<String, String>,
    pane_labels: &HashMap<String, String>,
    roster: &mut [RosterAgent],
) {
    for agent in roster.iter_mut() {
        agent.workspace_label = workspaces.get(&agent.workspace_id).cloned();
        agent.tab_label = agent
            .tab_id
            .as_deref()
            .and_then(|tab_id| tabs.get(tab_id).cloned());
        agent.pane_label = pane_labels.get(&agent.pane_id).cloned();
    }
}

/// The `pane_id -> manual pane label` map from `pane list` (only panes carrying a manual label; most
/// have none). Best-effort: a failed `pane list` yields an empty map and the rows fall back to ids.
fn pane_label_map(herdr: &dyn Herdr) -> HashMap<String, String> {
    herdr
        .pane_infos()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|pane| pane.label.map(|label| (pane.pane_id, label)))
        .collect()
}

/// Refetch the label maps at least this often (in samples) regardless of roster membership, so a
/// workspace/tab/pane *rename* (which doesn't change the pane-id set) still surfaces. At the ~1s
/// sample cadence that is ~15s of worst-case rename staleness for a cosmetic label — acceptable.
const LABEL_REFRESH_EVERY: u32 = 15;

/// The workspace/tab/pane label maps the Agents roster enriches from, cached across samples so the
/// sampler's hot loop spawns a single `herdr agent list` instead of four processes every second.
/// The maps change only on a rename or a new workspace/tab/pane, so they are refetched lazily — when
/// a new agent pane appears (it must get its human name in the sample that first shows it) or on the
/// [`LABEL_REFRESH_EVERY`] periodic refresh (for renames). Lives on the sampler thread; never shared.
#[derive(Default)]
pub(crate) struct LabelCache {
    workspaces: HashMap<String, String>,
    tabs: HashMap<String, String>,
    pane_labels: HashMap<String, String>,
    /// The agent pane-id set at the last map fetch; a pane outside it means a new agent appeared.
    known_pane_ids: HashSet<String>,
    /// Samples enriched since the last fetch, for the periodic refresh.
    samples_since_refresh: u32,
}

impl LabelCache {
    /// Whether the cached maps are stale for `pane_ids`: a pane appeared that wasn't present at the
    /// last fetch (a new agent needs its name now), or the periodic interval elapsed (to catch
    /// renames). A pane merely *vanishing* needs no refetch — the remaining names are unchanged.
    fn needs_refresh(&self, pane_ids: &HashSet<&str>) -> bool {
        self.samples_since_refresh >= LABEL_REFRESH_EVERY
            || pane_ids.iter().any(|id| !self.known_pane_ids.contains(*id))
    }
}

/// Sample the live roster for the Agents view, enriching from `cache` and refetching the label maps
/// only when [`LabelCache::needs_refresh`] says so. Steady state is one `agent list` spawn per call;
/// the three label-map spawns happen only on a membership change or the periodic refresh. Best-effort
/// enrichment, exactly like [`enrich_roster_labels`] — a failed map fetch degrades names, never the
/// roster.
pub(crate) fn sample_roster(
    herdr: &dyn Herdr,
    cache: &mut LabelCache,
) -> Result<Vec<RosterAgent>, PluginError> {
    let mut roster = herdr.agent_roster()?;
    let pane_ids: HashSet<&str> = roster.iter().map(|agent| agent.pane_id.as_str()).collect();

    if cache.needs_refresh(&pane_ids) {
        cache.workspaces = herdr.workspace_labels().unwrap_or_default();
        cache.tabs = herdr.tab_labels().unwrap_or_default();
        cache.pane_labels = pane_label_map(herdr);
        cache.known_pane_ids = pane_ids.iter().map(|id| id.to_string()).collect();
        cache.samples_since_refresh = 0;
    } else {
        cache.samples_since_refresh += 1;
    }

    apply_labels(
        &cache.workspaces,
        &cache.tabs,
        &cache.pane_labels,
        &mut roster,
    );
    Ok(roster)
}

/// Read at most this many panes' terminals per sample sweep, so a large fleet can't stall the sampler
/// worker: the `agent read` spawns are spread round-robin across sweeps (a status-changed pane jumps
/// the queue). At the ~1s sample cadence a fleet of this size or smaller is fully refreshed every
/// sweep; a larger one refreshes each pane every `ceil(n / budget)` seconds — a few seconds of lag on
/// a line that only feeds the glanceable view (invariant #7). Tunable by eyeball with a large fleet.
pub(crate) const TAIL_READ_BUDGET: usize = 10;

/// One pane's cached terminal tail: the last content line we extracted and the agent status when we
/// read it (to notice a transition and re-read promptly).
struct CachedTail {
    /// The last *non-empty* content line extracted from this pane; kept across sweeps so a pane we did
    /// not read this sweep — or whose latest read showed only chrome — never blanks. Replaced only by
    /// a fresh non-empty read.
    line: Option<String>,
    /// The agent status at the last read of this pane.
    status: AgentStatus,
}

/// The Agents view's last-line cache (Slice 4 / issue #5): `pane_id -> CachedTail`, filled by a
/// budgeted `herdr agent read` sweep on the sampler thread — the second cache on that thread beside
/// [`LabelCache`]. A **prunable observation cache** (invariant #7): every read is best-effort, a
/// failure keeps the prior line, and it feeds only the live view, never a ping. Lives on the sampler
/// thread; never shared.
#[derive(Default)]
pub(crate) struct TailCache {
    lines: HashMap<String, CachedTail>,
    /// Round-robin cursor into the roster, so successive sweeps read different panes when the fleet
    /// exceeds the budget.
    cursor: usize,
}

impl TailCache {
    /// Fill each agent's [`RosterAgent::last_line`] from the cache, first refreshing up to `budget`
    /// panes this sweep: panes whose status changed since their last read (their output likely changed
    /// too), or that were never read, go first; the rest fill round-robin. Un-refreshed panes keep
    /// their cached line (**never-blank**). Best-effort throughout — a read error or a chrome-only
    /// screen leaves the prior line intact (invariant #7).
    pub(crate) fn refresh(&mut self, herdr: &dyn Herdr, agents: &mut [RosterAgent], budget: usize) {
        // Drop cache entries for panes no longer present, so the map tracks the live roster.
        let live: HashSet<&str> = agents.iter().map(|agent| agent.pane_id.as_str()).collect();
        self.lines
            .retain(|pane_id, _| live.contains(pane_id.as_str()));

        for index in self.select_to_read(agents, budget) {
            let pane_id = agents[index].pane_id.clone();
            let status = agents[index].agent_status;
            let line = herdr
                .read_terminal_tail(&pane_id)
                .ok()
                .and_then(|snapshot| crate::roster::last_terminal_line(&snapshot));
            let entry = self
                .lines
                .entry(pane_id)
                .or_insert(CachedTail { line: None, status });
            // Never-blank: only a fresh non-empty line replaces the cached one; a read miss or a
            // chrome-only screen keeps what we had. Always record the status so a pane that stays put
            // is not re-prioritized every sweep.
            if line.is_some() {
                entry.line = line;
            }
            entry.status = status;
        }

        // Fill every row from the cache (un-read panes included -> never-blank).
        for agent in agents.iter_mut() {
            agent.last_line = self
                .lines
                .get(&agent.pane_id)
                .and_then(|cached| cached.line.clone());
        }
    }

    /// The pane indices to read this sweep: every pane whose status differs from its cached status (or
    /// that has no cache entry) first, then round-robin fill from `cursor` up to `budget`. Advances
    /// `cursor` past the panes the round-robin pass scanned, so the next sweep continues onward.
    fn select_to_read(&mut self, agents: &[RosterAgent], budget: usize) -> Vec<usize> {
        let n = agents.len();
        if n == 0 || budget == 0 {
            return Vec::new();
        }
        let mut selected: Vec<usize> = Vec::new();
        let mut chosen = vec![false; n];
        // Priority: status-changed or never-read panes (their output most likely just changed).
        for (index, agent) in agents.iter().enumerate() {
            let changed = match self.lines.get(&agent.pane_id) {
                Some(cached) => cached.status != agent.agent_status,
                None => true,
            };
            if changed {
                selected.push(index);
                chosen[index] = true;
            }
        }
        selected.truncate(budget);
        if selected.len() >= budget {
            return selected;
        }
        // Round-robin fill the rest of the budget, starting at the cursor and skipping the already
        // chosen. Track how far we scanned so the cursor advances even past priority-chosen panes.
        let mut scanned = 0usize;
        for step in 0..n {
            if selected.len() >= budget {
                break;
            }
            scanned = step + 1;
            let index = (self.cursor + step) % n;
            if !chosen[index] {
                selected.push(index);
                chosen[index] = true;
            }
        }
        self.cursor = (self.cursor + scanned) % n;
        selected
    }
}

/// The session uuid from an agent's nested `agent_session.value`, or `None` when the object (or the
/// value) is absent — herdr lists some agents without a session, and pins key on this later (§6).
fn agent_session_value(agent: &Value) -> Option<String> {
    non_empty_string(agent.get("agent_session")?, "value")
}

/// Parse `herdr workspace list` into a `workspace_id -> label` map. Workspaces without an id or with
/// an empty label are skipped (the row then falls back to the raw id). A herdr error surfaces as
/// `Err`; callers treat identity resolution as best-effort and keep enqueueing without it.
fn parse_workspace_labels(stdout: &[u8]) -> Result<HashMap<String, String>, PluginError> {
    parse_id_label_map(stdout, "workspace list", "workspaces", "workspace_id")
}

/// Parse `herdr tab list` into a `tab_id -> label` map (same shape as the workspace map).
fn parse_tab_labels(stdout: &[u8]) -> Result<HashMap<String, String>, PluginError> {
    parse_id_label_map(stdout, "tab list", "tabs", "tab_id")
}

/// Shared parser for herdr's `{workspace,tab} list` responses: read `result.<array_key>[]` and
/// collect `<id_key> -> label`, skipping any element missing the id or with an empty label. `command`
/// names the source in error messages. A herdr error or an unexpected shape surfaces as `Err`.
fn parse_id_label_map(
    stdout: &[u8],
    command: &str,
    array_key: &str,
    id_key: &str,
) -> Result<HashMap<String, String>, PluginError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|error| {
        PluginError::new(format!(
            "failed to parse HERDR_BIN_PATH {command} JSON: {error}"
        ))
    })?;

    if let Some(error) = herdr_error_message(&value) {
        return Err(PluginError::new(format!(
            "HERDR_BIN_PATH {command} returned an error: {error}"
        )));
    }

    let items = value
        .get("result")
        .and_then(|result| result.get(array_key))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PluginError::new(format!(
                "HERDR_BIN_PATH {command} returned an unexpected response"
            ))
        })?;

    let mut labels = HashMap::with_capacity(items.len());
    for item in items {
        if let (Some(id), Some(label)) = (
            non_empty_string(item, id_key),
            non_empty_string(item, "label"),
        ) {
            labels.insert(id, label);
        }
    }
    Ok(labels)
}

/// Fill in the location fields the event payload omits — the pane's `tab_id` + manual `label` (from
/// `pane list`), its workspace's human `label` (from `workspace list`), and its tab's `label` (from
/// `tab list`, keyed by the tab id we just learned). Best-effort by design: identity is cosmetic, so
/// a failed lookup leaves the field `None` and the enqueue proceeds unchanged — losing a ping over a
/// missing label would defeat the plugin's whole purpose. Called only from the dispatch layer (which
/// owns the `Herdr` handle) so `queue.rs` stays trait-free.
pub(crate) fn enrich_location(herdr: &dyn Herdr, event: &mut StatusEvent) {
    if let Ok(panes) = herdr.pane_infos() {
        if let Some(info) = panes.iter().find(|pane| pane.pane_id == event.pane_id) {
            event.tab_id = info.tab_id.clone();
            event.pane_label = info.label.clone();
        }
    }
    if let Ok(labels) = herdr.workspace_labels() {
        event.workspace_label = labels.get(&event.workspace_id).cloned();
    }
    if let Some(tab_id) = event.tab_id.as_deref() {
        if let Ok(labels) = herdr.tab_labels() {
            event.tab_label = labels.get(tab_id).cloned();
        }
    }
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

pub(crate) struct StatusEvent {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    /// The pane's tab id. Absent from the event payload, so this is `None` off the wire and filled by
    /// [`enrich_location`] before enqueue; the `startup` seed sets it straight from `pane list`.
    pub(crate) tab_id: Option<String>,
    /// The workspace's human label. Never on the event; resolved the same way as `tab_id`.
    pub(crate) workspace_label: Option<String>,
    /// The tab's label (program name). Resolved from `tab list`, keyed by `tab_id`.
    pub(crate) tab_label: Option<String>,
    /// The pane's manual label. Resolved from `pane list` alongside `tab_id`.
    pub(crate) pane_label: Option<String>,
    pub(crate) agent_status: String,
    pub(crate) agent: Option<String>,
    pub(crate) display_agent: Option<String>,
    pub(crate) title: Option<String>,
}

impl StatusEvent {
    pub(crate) fn wait_status(&self) -> Option<WaitStatus> {
        match self.agent_status.as_str() {
            "blocked" => Some(WaitStatus::Blocked),
            "done" => Some(WaitStatus::Done),
            _ => None,
        }
    }

    pub(crate) fn is_working(&self) -> bool {
        self.agent_status == "working"
    }
}

/// The plugin event JSON is `{ "event": ..., "data": { "type": ..., <fields> } }`.
/// Fields are read from `data`, falling back to the top-level object.
pub(crate) fn parse_status_event(raw: &str) -> Option<StatusEvent> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let data = event_data(&value);
    Some(StatusEvent {
        pane_id: non_empty_string(data, "pane_id")?,
        workspace_id: non_empty_string(data, "workspace_id").unwrap_or_default(),
        // The event omits these; `enrich_location` fills them before enqueue. Parse `tab_id`
        // defensively in case a future herdr adds it to the payload — then the lookup is a no-op
        // refresh. The labels are never on the wire.
        tab_id: non_empty_string(data, "tab_id"),
        workspace_label: None,
        tab_label: None,
        pane_label: None,
        agent_status: non_empty_string(data, "agent_status")?,
        agent: non_empty_string(data, "agent"),
        display_agent: non_empty_string(data, "display_agent"),
        title: non_empty_string(data, "title"),
    })
}

pub(crate) fn parse_event_string(raw: &str, key: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pane_infos_extracts_wait_fields_and_skips_idless_panes() {
        let json = br#"{"result":{"type":"pane_list","panes":[
            {"pane_id":"wA:p1","workspace_id":"wA","tab_id":"wA:t2","agent_status":"blocked","agent":"claude","display_agent":"Claude","title":"needs input","focused":true},
            {"pane_id":"wB:p3","workspace_id":"wB","agent_status":"done"},
            {"pane_id":"","agent_status":"done"},
            {"agent_status":"done"}
        ]}}"#;
        let infos = parse_pane_infos(json).expect("pane list should parse");
        assert_eq!(infos.len(), 2, "panes without a pane_id are skipped");
        assert_eq!(infos[0].pane_id, "wA:p1");
        assert_eq!(infos[0].workspace_id, "wA");
        assert_eq!(infos[0].tab_id.as_deref(), Some("wA:t2"));
        assert_eq!(infos[0].agent_status, "blocked");
        assert_eq!(infos[0].title.as_deref(), Some("needs input"));
        // A pane list may omit tab_id (it did before we needed it); parse it as absent, not empty.
        assert_eq!(infos[1].tab_id, None);
    }

    #[test]
    fn parse_workspace_labels_maps_id_to_label_and_skips_the_unlabeled() {
        let json = br#"{"result":{"type":"workspace_list","workspaces":[
            {"workspace_id":"w4","label":"home","number":1},
            {"workspace_id":"wT","label":"herdr-checkin","number":10},
            {"workspace_id":"wX","label":"","number":11},
            {"label":"orphan"}
        ]}}"#;
        let labels = parse_workspace_labels(json).expect("workspace list should parse");
        assert_eq!(
            labels.len(),
            2,
            "empty-label and id-less workspaces are skipped"
        );
        assert_eq!(labels.get("w4").map(String::as_str), Some("home"));
        assert_eq!(labels.get("wT").map(String::as_str), Some("herdr-checkin"));
        assert_eq!(labels.get("wX"), None);
    }

    #[test]
    fn parse_agent_list_reads_a_captured_live_response() {
        // A pristine capture of real `herdr agent list` output (herdr 0.7.5) — NOT hand-written, so
        // the parser is proven against the true schema (extra fields like revision/terminal_id, the
        // nested agent_session, agents across three workspaces). The middle agent (amp) was listed
        // live with NO agent_session and NO terminal_title — the missing-session case, for free.
        let json = include_str!("fixtures/agent_list.json");
        let agents = parse_agent_list(json.as_bytes()).expect("live agent list should parse");
        assert_eq!(agents.len(), 3);

        assert_eq!(agents[0].pane_id, "w4:p1");
        assert_eq!(agents[0].agent.as_deref(), Some("codex"));
        assert_eq!(agents[0].agent_status, AgentStatus::Idle);
        assert_eq!(agents[0].tab_id.as_deref(), Some("w4:t1"));
        assert_eq!(agents[0].workspace_id, "w4");
        assert!(!agents[0].focused);
        assert_eq!(
            agents[0].agent_session.as_deref(),
            Some("019f8b57-77d7-7353-9062-35f49261a20d"),
            "the session uuid is read from the nested agent_session.value"
        );
        assert_eq!(agents[0].cwd.as_deref(), Some("/Users/akram"));
        assert!(agents[0].terminal_title.is_some());

        // amp: listed live without a session or a terminal_title, and it holds focus.
        assert_eq!(agents[1].pane_id, "wN:p2");
        assert_eq!(agents[1].agent.as_deref(), Some("amp"));
        assert_eq!(
            agents[1].agent_session, None,
            "no agent_session on this pane"
        );
        assert_eq!(agents[1].terminal_title, None);
        assert!(agents[1].focused);

        assert_eq!(agents[2].pane_id, "wT:p1");
        assert_eq!(agents[2].agent_status, AgentStatus::Working);
        assert_eq!(
            agents[2].agent_session.as_deref(),
            Some("c157c523-3984-4c0a-bbb5-afd9b7fad361")
        );
    }

    #[test]
    fn parse_agent_list_folds_unknown_status_and_skips_idless_agents() {
        // Schema mirrors the live capture, crafted to exercise cases the live sample lacked: an
        // explicit `unknown` status, an unrecognized future status, and a pane_id-less agent.
        let json = br#"{"result":{"type":"agent_list","agents":[
            {"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","agent":"claude","agent_status":"unknown","focused":false},
            {"pane_id":"w2:p1","workspace_id":"w2","agent":"codex","agent_status":"reticulating","focused":false},
            {"workspace_id":"w3","agent_status":"idle"}
        ]}}"#;
        let agents = parse_agent_list(json).expect("edge agent list should parse");
        assert_eq!(agents.len(), 2, "the pane_id-less agent is skipped");
        assert_eq!(agents[0].agent_status, AgentStatus::Unknown);
        assert_eq!(
            agents[1].agent_status,
            AgentStatus::Unknown,
            "an unrecognized status folds to Unknown, never dropped"
        );
        assert_eq!(agents[0].agent_session, None);
    }

    #[test]
    fn parse_agent_list_surfaces_a_herdr_error() {
        let json = br#"{"error":{"code":"no_session","message":"not attached"}}"#;
        assert!(parse_agent_list(json).is_err(), "a herdr error maps to Err");
    }

    #[test]
    fn enrich_roster_labels_fills_names_from_the_label_sources() {
        use crate::test_support::FakeHerdr;

        // A parsed roster carries only ids; enrichment resolves herdr's human names from the
        // workspace/tab/pane label maps, so the Agents view reads `home · ~ · editor`, not ids.
        let mut roster = vec![RosterAgent {
            pane_id: "w4:p1".to_string(),
            workspace_id: "w4".to_string(),
            tab_id: Some("w4:t1".to_string()),
            agent: Some("codex".to_string()),
            agent_status: AgentStatus::Idle,
            agent_session: None,
            cwd: None,
            focused: false,
            terminal_title: None,
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
        }];
        let herdr = FakeHerdr::new(&[])
            .with_workspace_labels(&[("w4", "home")])
            .with_tab_labels(&[("w4:t1", "~")])
            .with_panes(vec![PaneInfo {
                pane_id: "w4:p1".to_string(),
                workspace_id: "w4".to_string(),
                tab_id: Some("w4:t1".to_string()),
                label: Some("editor".to_string()),
                agent_status: "idle".to_string(),
                agent: Some("codex".to_string()),
                display_agent: None,
                title: None,
            }]);

        enrich_roster_labels(&herdr, &mut roster);

        assert_eq!(roster[0].workspace_label.as_deref(), Some("home"));
        assert_eq!(roster[0].tab_label.as_deref(), Some("~"));
        assert_eq!(roster[0].pane_label.as_deref(), Some("editor"));
    }

    #[test]
    fn enrich_roster_labels_leaves_ids_when_a_lookup_misses() {
        use crate::test_support::FakeHerdr;

        // No label maps seeded: enrichment is a no-op (labels stay None) and the view falls back to
        // ids — cosmetic enrichment must never fail or blank the roster.
        let mut roster = vec![RosterAgent {
            pane_id: "wZ:p9".to_string(),
            workspace_id: "wZ".to_string(),
            tab_id: Some("wZ:t9".to_string()),
            agent: None,
            agent_status: AgentStatus::Working,
            agent_session: None,
            cwd: None,
            focused: false,
            terminal_title: None,
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
        }];

        enrich_roster_labels(&FakeHerdr::new(&[]), &mut roster);

        assert_eq!(roster[0].workspace_label, None);
        assert_eq!(roster[0].tab_label, None);
        assert_eq!(roster[0].pane_label, None);
    }

    #[test]
    fn sample_roster_enriches_from_the_cache_and_only_refetches_on_a_membership_change() {
        use crate::test_support::FakeHerdr;

        let herdr = FakeHerdr::new(&[])
            .with_agents(vec![RosterAgent {
                pane_id: "w4:p1".to_string(),
                workspace_id: "w4".to_string(),
                tab_id: Some("w4:t1".to_string()),
                agent: Some("codex".to_string()),
                agent_status: AgentStatus::Idle,
                agent_session: None,
                cwd: None,
                focused: false,
                terminal_title: None,
                status_since_ms: None,
                workspace_label: None,
                tab_label: None,
                pane_label: None,
                last_line: None,
            }])
            .with_workspace_labels(&[("w4", "home")])
            .with_tab_labels(&[("w4:t1", "~")])
            .with_panes(vec![PaneInfo {
                pane_id: "w4:p1".to_string(),
                workspace_id: "w4".to_string(),
                tab_id: Some("w4:t1".to_string()),
                label: Some("editor".to_string()),
                agent_status: "idle".to_string(),
                agent: Some("codex".to_string()),
                display_agent: None,
                title: None,
            }]);

        let mut cache = LabelCache::default();
        // First sample: the fresh cache is a miss, so it fetches the maps and enriches human names.
        let roster = sample_roster(&herdr, &mut cache).expect("first sample succeeds");
        assert_eq!(roster[0].workspace_label.as_deref(), Some("home"));
        assert_eq!(roster[0].tab_label.as_deref(), Some("~"));
        assert_eq!(roster[0].pane_label.as_deref(), Some("editor"));
        assert!(cache.known_pane_ids.contains("w4:p1"));
        assert_eq!(cache.samples_since_refresh, 0);

        // Second sample, identical roster: no membership change -> cache-only, still enriched.
        let roster = sample_roster(&herdr, &mut cache).expect("second sample succeeds");
        assert_eq!(roster[0].workspace_label.as_deref(), Some("home"));
        assert_eq!(
            cache.samples_since_refresh, 1,
            "a stable roster does not refetch the label maps"
        );
    }

    /// A minimal roster agent for the tail-sweep tests (only pane id + status matter here).
    fn tail_agent(pane_id: &str, status: AgentStatus) -> RosterAgent {
        RosterAgent {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("w1").to_string(),
            tab_id: None,
            agent: Some("claude".to_string()),
            agent_status: status,
            agent_session: None,
            cwd: None,
            focused: false,
            terminal_title: None,
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
        }
    }

    #[test]
    fn tail_sweep_extracts_and_fills_each_rows_last_line() {
        use crate::test_support::FakeHerdr;
        let herdr = FakeHerdr::new(&[]).with_tails(&[("w1:p1", "⏺ done\n  the answer is 42\n❯\n")]);
        let mut cache = TailCache::default();
        let mut agents = vec![tail_agent("w1:p1", AgentStatus::Working)];
        cache.refresh(&herdr, &mut agents, TAIL_READ_BUDGET);
        assert_eq!(agents[0].last_line.as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn tail_sweep_keeps_the_prior_line_when_a_later_read_is_chrome_only() {
        // never-blank (invariant #7): once a pane has a line, a later read that finds only chrome
        // (its output scrolled off) or fails must not erase it — the row holds its last known line.
        let good = "⏺ done\n  the answer is 42\n❯\n";
        let chrome = "──────────── x ────────────\n❯\n  -- INSERT -- auto mode on\n";
        let mut cache = TailCache::default();

        let mut first = vec![tail_agent("w1:p1", AgentStatus::Working)];
        cache.refresh(
            &crate::test_support::FakeHerdr::new(&[]).with_tails(&[("w1:p1", good)]),
            &mut first,
            TAIL_READ_BUDGET,
        );
        assert_eq!(first[0].last_line.as_deref(), Some("the answer is 42"));

        let mut second = vec![tail_agent("w1:p1", AgentStatus::Working)];
        cache.refresh(
            &crate::test_support::FakeHerdr::new(&[]).with_tails(&[("w1:p1", chrome)]),
            &mut second,
            TAIL_READ_BUDGET,
        );
        assert_eq!(
            second[0].last_line.as_deref(),
            Some("the answer is 42"),
            "a chrome-only read keeps the last known line, never blanks it"
        );
    }

    #[test]
    fn tail_sweep_reads_a_status_changed_pane_before_the_round_robin() {
        use crate::test_support::FakeHerdr;
        // Warm two working panes so neither is a never-read priority anymore.
        let warm = FakeHerdr::new(&[])
            .with_tails(&[("w1:p1", "⏺\n  line A\n❯\n"), ("w1:p2", "⏺\n  line B\n❯\n")]);
        let mut cache = TailCache::default();
        let mut agents = vec![
            tail_agent("w1:p1", AgentStatus::Working),
            tail_agent("w1:p2", AgentStatus::Working),
        ];
        cache.refresh(&warm, &mut agents, TAIL_READ_BUDGET);

        // p2 transitions to blocked with new output. A budget-1 sweep must spend its single read on
        // p2 (status-changed), not p1, so the new line surfaces without waiting for the round-robin.
        let changed = FakeHerdr::new(&[]).with_tails(&[
            ("w1:p1", "⏺\n  line A\n❯\n"),
            ("w1:p2", "⏺\n  now blocked: proceed?\n❯\n"),
        ]);
        let mut next = vec![
            tail_agent("w1:p1", AgentStatus::Working),
            tail_agent("w1:p2", AgentStatus::Blocked),
        ];
        cache.refresh(&changed, &mut next, 1);
        assert_eq!(
            changed.reads.borrow().as_slice(),
            &["w1:p2".to_string()],
            "the status-changed pane is the one read"
        );
        assert_eq!(next[1].last_line.as_deref(), Some("now blocked: proceed?"));
    }

    #[test]
    fn tail_sweep_round_robins_across_sweeps_when_over_budget() {
        use crate::test_support::FakeHerdr;
        let herdr = FakeHerdr::new(&[]).with_tails(&[
            ("w1:p1", "⏺\n  A\n❯\n"),
            ("w1:p2", "⏺\n  B\n❯\n"),
            ("w1:p3", "⏺\n  C\n❯\n"),
        ]);
        let mut cache = TailCache::default();
        let roster = || {
            vec![
                tail_agent("w1:p1", AgentStatus::Working),
                tail_agent("w1:p2", AgentStatus::Working),
                tail_agent("w1:p3", AgentStatus::Working),
            ]
        };
        // Warm all three, then clear the log so we only observe the round-robin sweeps below.
        cache.refresh(&herdr, &mut roster(), 3);
        herdr.reads.borrow_mut().clear();

        // With none status-changed, three budget-1 sweeps must read three *distinct* panes.
        for _ in 0..3 {
            cache.refresh(&herdr, &mut roster(), 1);
        }
        let mut distinct = herdr.reads.borrow().clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            3,
            "round-robin covers every pane across sweeps; got {:?}",
            herdr.reads.borrow()
        );
    }

    #[test]
    fn label_cache_refreshes_on_a_new_pane_and_periodically_but_not_on_a_stable_or_shrinking_roster(
    ) {
        let mut cache = LabelCache::default();

        // A fresh cache has seen nothing, so any pane is new -> refresh.
        let one: HashSet<&str> = ["w1:p1"].into_iter().collect();
        assert!(cache.needs_refresh(&one));

        // Record that membership as fetched; the same roster is now cache-only.
        cache.known_pane_ids = ["w1:p1"].into_iter().map(String::from).collect();
        cache.samples_since_refresh = 0;
        assert!(!cache.needs_refresh(&one));

        // A newly appeared pane forces a refetch (it must get its human name at once)...
        let grown: HashSet<&str> = ["w1:p1", "w2:p1"].into_iter().collect();
        assert!(cache.needs_refresh(&grown));

        // ...but a pane merely vanishing does not (the remaining names are unchanged).
        assert!(!cache.needs_refresh(&one));

        // The periodic refresh fires regardless of membership, to catch renames.
        cache.samples_since_refresh = LABEL_REFRESH_EVERY;
        assert!(cache.needs_refresh(&one));
    }

    #[test]
    fn null_error_field_is_treated_as_success() {
        let value: Value = serde_json::from_str(r#"{"result":{"panes":[]},"error":null}"#).unwrap();
        assert_eq!(herdr_error_message(&value), None);
    }

    // A throwaway `herdr` that logs each argv entry on its own line (so a multi-word arg proves it
    // was passed as ONE argument, not shell-split) and exits with `exit_code`.
    #[cfg(unix)]
    fn fake_agent_prompt_herdr(
        dir: &std::path::Path,
        log: &std::path::Path,
        exit_code: i32,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("fake-herdr-{exit_code}.sh"));
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{log}\"\nprintf 'prompt refused\\n' >&2\nexit {exit_code}\n",
            log = log.display()
        );
        std::fs::write(&path, script).expect("fake herdr should write");
        let mut permissions = std::fs::metadata(&path)
            .expect("fake herdr metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("fake herdr should be executable");
        path
    }

    #[test]
    #[cfg(unix)]
    fn cli_prompt_agent_shapes_the_command() {
        let dir = crate::test_support::temp_state_dir("prompt-ok");
        let log = dir.join("argv.log");
        let herdr = CliHerdr {
            bin_path: fake_agent_prompt_herdr(&dir, &log, 0),
        };

        herdr
            .prompt_agent("wA:p1", "use option B")
            .expect("prompt should succeed");

        let argv = std::fs::read_to_string(&log).expect("argv log should exist");
        let lines: Vec<&str> = argv.lines().collect();
        // The reply text arrives as a single argv entry — not split on its spaces.
        assert_eq!(lines, ["agent", "prompt", "wA:p1", "use option B"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn cli_prompt_agent_maps_a_nonzero_exit_to_err() {
        let dir = crate::test_support::temp_state_dir("prompt-err");
        let log = dir.join("argv.log");
        let herdr = CliHerdr {
            bin_path: fake_agent_prompt_herdr(&dir, &log, 1),
        };

        let error = herdr
            .prompt_agent("wA:p1", "hi")
            .expect_err("a nonzero exit should map to an error");
        assert!(
            error.to_string().contains("agent prompt"),
            "error was: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn popup_close_request_line_matches_the_herdr_wire_shape() {
        // Must round-trip to herdr's `popup.close` request (schema.rs: flattened id/method/params).
        let value: Value = serde_json::from_str(&popup_close_request_line())
            .expect("the popup.close line must be valid JSON");
        assert_eq!(value["method"], "popup.close");
        assert_eq!(value["params"], serde_json::json!({}));
        assert!(value["id"].is_string(), "the request carries a string id");
    }

    // The shared fake records prompt calls in order — the contract the reply-mode slices assert on.
    #[test]
    fn fake_herdr_records_prompt_calls() {
        let herdr = crate::test_support::FakeHerdr::new(&[("wA:p1", "blocked")]);
        herdr
            .prompt_agent("wA:p1", "yes")
            .expect("fake prompt should succeed");
        assert_eq!(
            herdr.prompts.into_inner(),
            vec![("wA:p1".to_string(), "yes".to_string())]
        );
    }
}
