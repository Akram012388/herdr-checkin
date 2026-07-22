//! The status pane — a persistent TUI listing the live queue with select-and-jump.
//!
//! herdr spawns this as a long-running terminal pane (via a `[[panes]]` manifest entry), so
//! unlike the per-event handlers it runs a ratatui + crossterm loop until the user closes it.
//! There is no push delivery of events to a running pane, so it stays live by re-reading the
//! shared `state.json` on a tick — the same file the per-event binaries keep current.
//!
//! Two rules from the design review are load-bearing:
//! - **Mutations are deltas.** `Enter` and `d` change the queue through
//!   [`crate::StateStore::update`] (read-modify-write under the lock), never by writing the
//!   pane's in-memory list back — that would clobber an enqueue that landed since the last tick.
//! - **`Enter` evicts only after a successful focus.** We do not rely on herdr emitting a
//!   `pane.focused` event to auto-evict; eviction is idempotent, so an explicit evict is safe
//!   whether or not that event also fires.

use crate::{
    current_unix_ms, describe_entry, evict, load_entries, Herdr, PluginError, QueueEntry,
    RuntimeEnv, StateStore,
};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

/// How long the input poll blocks each tick before we re-read the queue and repaint, so queue
/// changes and the ticking "waited" column appear promptly without busy-spinning.
const TICK: Duration = Duration::from_millis(250);

const FOOTER_HINTS: &str = "j/k move  ·  Enter jump  ·  d drop  ·  q quit";

/// The label herdr shows for our status pane in `pane list`, used to recognize our own pane for
/// the idempotent open/focus/close toggle. Must stay in sync with the `[[panes]]` `title` in
/// `herdr-plugin.toml` — that title is the only field identifying the pane in `pane list`.
const PANE_LABEL: &str = "Check-in";

/// Entry point for the `pane` subcommand: run the TUI until the user closes it. The terminal is
/// restored on every exit path (including a panic, via the hook `ratatui::try_init` installs).
pub fn run(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let mut model = PaneModel::new(load_entries(&runtime.state_dir));
    let mut terminal = ratatui::try_init()
        .map_err(|error| PluginError::new(format!("failed to initialize terminal: {error}")))?;
    let result = event_loop(&mut terminal, &mut model, runtime, herdr);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    model: &mut PaneModel,
    runtime: &RuntimeEnv,
    herdr: &dyn Herdr,
) -> Result<(), PluginError> {
    loop {
        let now_ms = current_unix_ms();
        terminal
            .draw(|frame| draw(frame, model, now_ms))
            .map_err(|error| PluginError::new(format!("failed to draw: {error}")))?;

        if event::poll(TICK)
            .map_err(|error| PluginError::new(format!("failed to poll input: {error}")))?
        {
            let event = event::read()
                .map_err(|error| PluginError::new(format!("failed to read input: {error}")))?;
            if let Event::Key(key) = event {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('j') | KeyCode::Down => model.move_down(),
                    KeyCode::Char('k') | KeyCode::Up => model.move_up(),
                    KeyCode::Enter => on_enter(model, runtime, herdr),
                    KeyCode::Char('d') | KeyCode::Char('x') => on_drop(model, runtime),
                    _ => {}
                }
            }
        }

        // Refresh from the shared file every tick. Parse-and-compare (no mtime guard: mtime has
        // ~1s granularity and would miss two writes in the same second); the file is tiny.
        let fresh = load_entries(&runtime.state_dir);
        if fresh != model.entries {
            model.sync(fresh);
        }
    }
}

/// `Enter`: focus the selected agent, then — only on success — evict it from the queue.
fn on_enter(model: &mut PaneModel, runtime: &RuntimeEnv, herdr: &dyn Herdr) {
    let Some(pane_id) = model.selected_pane_id().map(str::to_owned) else {
        return;
    };
    match herdr.focus_agent(&pane_id) {
        Ok(()) => {
            // The jump worked; if the drop can't be persisted, say so rather than imply success.
            model.status = match evict_pane(runtime, &pane_id) {
                Ok(()) => None,
                Err(error) => Some(format!("jumped, but drop failed: {error}")),
            };
            model.sync(load_entries(&runtime.state_dir));
        }
        Err(error) => {
            model.status = Some(format!("focus failed: {error}"));
        }
    }
}

/// `d`: drop the selected entry from the queue without jumping to it.
fn on_drop(model: &mut PaneModel, runtime: &RuntimeEnv) {
    let Some(pane_id) = model.selected_pane_id().map(str::to_owned) else {
        return;
    };
    model.status = match evict_pane(runtime, &pane_id) {
        Ok(()) => None,
        Err(error) => Some(format!("drop failed: {error}")),
    };
    model.sync(load_entries(&runtime.state_dir));
}

/// Remove a pane from the queue as a delta under the lock (never a full model write-back).
fn evict_pane(runtime: &RuntimeEnv, pane_id: &str) -> Result<(), PluginError> {
    StateStore::new(&runtime.state_dir).update(|mut entries| {
        evict(&mut entries, pane_id);
        (entries, ())
    })
}

// --- launch decision (idempotent open / focus / close toggle) --------------

/// The `pane-decision` subcommand: read `pane list` JSON on stdin, decide whether the launcher
/// should open, focus, or close the status pane, and print that decision. Never fails — on any
/// read/parse trouble it prints `OPEN`, preserving an always-open fallback for the launcher.
pub fn decide_from_stdin() -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    println!("{}", decide(&parse_panes(&input)).into_line());
    0
}

/// What the launcher should do. `Focus`/`Close` carry the target pane id.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Open,
    Focus(String),
    Close(String),
}

impl Decision {
    fn into_line(self) -> String {
        match self {
            Decision::Open => "OPEN".to_string(),
            Decision::Focus(pane_id) => format!("FOCUS {pane_id}"),
            Decision::Close(pane_id) => format!("CLOSE {pane_id}"),
        }
    }
}

/// The subset of a `pane list` entry the decision needs.
struct PaneInfo {
    pane_id: String,
    tab_id: Option<String>,
    focused: bool,
    label: Option<String>,
}

impl PaneInfo {
    fn is_status_pane(&self) -> bool {
        self.label.as_deref() == Some(PANE_LABEL)
    }
}

/// Idempotent, current-tab-scoped toggle:
/// - the focused pane is our status pane  -> CLOSE it (a repeat press hides it);
/// - a status pane exists in the current tab but isn't focused -> FOCUS it;
/// - otherwise -> OPEN a new one.
///
/// Scoping to the current tab keeps a focus from yanking the user into another workspace, and
/// matches herdr's own plugin-pane conventions. All targeted ids are checked flag-safe so a
/// crafted id can never be read as a CLI option by the launcher.
///
/// This relies on herdr reporting exactly one globally-focused pane in `pane list` (verified
/// against herdr 0.7.5: one `focused: true` across all workspaces), so `find` returns the pane
/// the user is actually on.
fn decide(panes: &[PaneInfo]) -> Decision {
    let focused = panes.iter().find(|pane| pane.focused);

    if let Some(focused) = focused {
        if focused.is_status_pane() && is_flag_safe(&focused.pane_id) {
            return Decision::Close(focused.pane_id.clone());
        }
    }

    if let Some(current_tab) = focused.and_then(|pane| pane.tab_id.as_deref()) {
        if let Some(pane) = panes.iter().find(|pane| {
            pane.is_status_pane()
                && pane.tab_id.as_deref() == Some(current_tab)
                && is_flag_safe(&pane.pane_id)
        }) {
            return Decision::Focus(pane.pane_id.clone());
        }
    }

    Decision::Open
}

/// A pane id is safe to pass as a positional CLI argument only if it can't be mistaken for an
/// option. herdr ids never start with `-`, so anything that does is rejected (degrades to OPEN).
fn is_flag_safe(pane_id: &str) -> bool {
    !pane_id.is_empty() && !pane_id.starts_with('-')
}

fn parse_panes(json: &str) -> Vec<PaneInfo> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(panes) = value
        .get("result")
        .and_then(|result| result.get("panes"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    panes
        .iter()
        .filter_map(|pane| {
            Some(PaneInfo {
                pane_id: pane.get("pane_id").and_then(Value::as_str)?.to_string(),
                tab_id: pane
                    .get("tab_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                focused: pane
                    .get("focused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                label: pane.get("label").and_then(Value::as_str).map(str::to_owned),
            })
        })
        .collect()
}

// --- model (pure) ----------------------------------------------------------

/// The pane's view state: the queue plus a selection cursor and a transient footer message.
/// All transitions are pure and unit-tested; the terminal loop above is the only impure part.
struct PaneModel {
    entries: Vec<QueueEntry>,
    selected: usize,
    status: Option<String>,
}

impl PaneModel {
    fn new(entries: Vec<QueueEntry>) -> Self {
        Self {
            entries,
            selected: 0,
            status: None,
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    fn selected_pane_id(&self) -> Option<&str> {
        self.entries
            .get(self.selected)
            .map(|entry| entry.pane_id.as_str())
    }

    /// Replace the entries with a fresh read, keeping the selection anchored to the same pane
    /// where possible; otherwise clamp it into range.
    fn sync(&mut self, entries: Vec<QueueEntry>) {
        let anchor = self
            .entries
            .get(self.selected)
            .map(|entry| entry.pane_id.clone());
        self.entries = entries;
        if let Some(anchor) = anchor {
            if let Some(position) = self.entries.iter().position(|e| e.pane_id == anchor) {
                self.selected = position;
            }
        }
        let max_index = self.entries.len().saturating_sub(1);
        if self.selected > max_index {
            self.selected = max_index;
        }
    }
}

// --- view ------------------------------------------------------------------

fn draw(frame: &mut Frame, model: &PaneModel, now_ms: u64) {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new(header_text(model.entries.len())).bold(),
        areas[0],
    );

    if model.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("No agents waiting — you're all caught up.")
                .dim()
                .alignment(Alignment::Center),
            areas[1],
        );
    } else {
        let items: Vec<ListItem> = model
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| ListItem::new(row_text(index, entry, now_ms)))
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().reversed())
            .highlight_symbol("> ");
        let mut state = ListState::default();
        state.select(Some(model.selected));
        frame.render_stateful_widget(list, areas[1], &mut state);
    }

    let footer = model.status.as_deref().unwrap_or(FOOTER_HINTS);
    frame.render_widget(Paragraph::new(footer).dim(), areas[2]);
}

fn header_text(count: usize) -> String {
    match count {
        0 => "Check-in — queue empty".to_string(),
        1 => "Check-in — 1 agent waiting".to_string(),
        n => format!("Check-in — {n} agents waiting"),
    }
}

fn row_text(index: usize, entry: &QueueEntry, now_ms: u64) -> String {
    format!("{}. {}", index + 1, describe_entry(entry, now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WaitStatus;

    fn entry(pane_id: &str) -> QueueEntry {
        QueueEntry {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("").to_string(),
            agent: Some("claude".to_string()),
            display_agent: Some("Claude".to_string()),
            title: Some("t".to_string()),
            status: WaitStatus::Blocked,
            enqueued_at_ms: 1_000,
            last_touched_ms: 1_000,
        }
    }

    fn model(ids: &[&str]) -> PaneModel {
        PaneModel::new(ids.iter().map(|id| entry(id)).collect())
    }

    #[test]
    fn move_down_and_up_clamp_at_the_ends() {
        let mut m = model(&["w1:p1", "w2:p1"]);
        assert_eq!(m.selected, 0);
        m.move_up(); // already at top
        assert_eq!(m.selected, 0);
        m.move_down();
        assert_eq!(m.selected, 1);
        m.move_down(); // already at bottom
        assert_eq!(m.selected, 1);
        m.move_up();
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn sync_keeps_selection_on_the_same_pane() {
        let mut m = model(&["w1:p1", "w2:p1", "w3:p1"]);
        m.move_down(); // select w2:p1
        assert_eq!(m.selected_pane_id(), Some("w2:p1"));
        // w1:p1 drops out; selection should follow w2:p1 to its new index 0.
        m.sync(vec![entry("w2:p1"), entry("w3:p1")]);
        assert_eq!(m.selected, 0);
        assert_eq!(m.selected_pane_id(), Some("w2:p1"));
    }

    #[test]
    fn sync_clamps_when_selected_pane_is_gone() {
        let mut m = model(&["w1:p1", "w2:p1", "w3:p1"]);
        m.move_down();
        m.move_down(); // select w3:p1 (index 2)
        assert_eq!(m.selected_pane_id(), Some("w3:p1"));
        // The selected pane is evicted; index 2 no longer exists, so clamp to the last row.
        m.sync(vec![entry("w1:p1"), entry("w2:p1")]);
        assert_eq!(m.selected, 1);
        assert_eq!(m.selected_pane_id(), Some("w2:p1"));
    }

    #[test]
    fn sync_to_empty_leaves_no_selection() {
        let mut m = model(&["w1:p1"]);
        m.sync(Vec::new());
        assert_eq!(m.selected, 0);
        assert_eq!(m.selected_pane_id(), None);
    }

    #[test]
    fn header_text_pluralizes() {
        assert_eq!(header_text(0), "Check-in — queue empty");
        assert_eq!(header_text(1), "Check-in — 1 agent waiting");
        assert_eq!(header_text(3), "Check-in — 3 agents waiting");
    }

    #[test]
    fn row_text_is_one_indexed_and_describes_the_entry() {
        let row = row_text(0, &entry("w7:p2"), 1_000);
        assert!(row.starts_with("1. "), "row was: {row}");
        assert!(row.contains("blocked"), "row was: {row}");
    }

    fn pane_info(pane_id: &str, tab: &str, focused: bool, label: Option<&str>) -> PaneInfo {
        PaneInfo {
            pane_id: pane_id.to_string(),
            tab_id: Some(tab.to_string()),
            focused,
            label: label.map(str::to_owned),
        }
    }

    #[test]
    fn decide_opens_when_no_status_pane_exists() {
        let panes = [pane_info("wA:p1", "wA:t1", true, Some("editor"))];
        assert_eq!(decide(&panes), Decision::Open);
    }

    #[test]
    fn decide_focuses_status_pane_in_current_tab() {
        let panes = [
            pane_info("wA:p1", "wA:t1", true, None),
            pane_info("wA:p2", "wA:t1", false, Some(PANE_LABEL)),
        ];
        assert_eq!(decide(&panes), Decision::Focus("wA:p2".to_string()));
    }

    #[test]
    fn decide_closes_when_focused_pane_is_the_status_pane() {
        let panes = [
            pane_info("wA:p1", "wA:t1", false, None),
            pane_info("wA:p2", "wA:t1", true, Some(PANE_LABEL)),
        ];
        assert_eq!(decide(&panes), Decision::Close("wA:p2".to_string()));
    }

    #[test]
    fn decide_opens_when_status_pane_is_in_another_tab() {
        // A status pane exists, but in a different tab — focusing it would jump workspaces, so we
        // open a fresh one in the current tab instead.
        let panes = [
            pane_info("wA:p1", "wA:t1", true, None),
            pane_info("wB:p1", "wB:t1", false, Some(PANE_LABEL)),
        ];
        assert_eq!(decide(&panes), Decision::Open);
    }

    #[test]
    fn decide_rejects_option_like_pane_ids() {
        // A crafted id that looks like a flag must never be forwarded to the launcher.
        let panes = [pane_info("-rf", "wA:t1", true, Some(PANE_LABEL))];
        assert_eq!(decide(&panes), Decision::Open);
    }

    #[test]
    fn parse_panes_reads_ids_and_labels() {
        let json = r#"{"result":{"panes":[
            {"pane_id":"wA:p1","tab_id":"wA:t1","focused":true,"label":"Check-in"},
            {"pane_id":"wA:p2","tab_id":"wA:t1","focused":false}
        ]}}"#;
        let panes = parse_panes(json);
        assert_eq!(panes.len(), 2);
        assert!(panes[0].is_status_pane());
        assert!(!panes[1].is_status_pane());
    }

    #[test]
    fn parse_panes_degrades_on_garbage() {
        assert!(parse_panes("not json").is_empty());
        assert!(parse_panes("{}").is_empty());
    }
}
