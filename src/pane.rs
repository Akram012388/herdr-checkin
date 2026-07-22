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
use std::time::Duration;

/// How long the input poll blocks each tick before we re-read the queue and repaint, so queue
/// changes and the ticking "waited" column appear promptly without busy-spinning.
const TICK: Duration = Duration::from_millis(250);

const FOOTER_HINTS: &str = "j/k move  ·  Enter jump  ·  d drop  ·  q quit";

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
            evict_pane(runtime, &pane_id);
            model.sync(load_entries(&runtime.state_dir));
            model.status = None;
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
    evict_pane(runtime, &pane_id);
    model.sync(load_entries(&runtime.state_dir));
    model.status = None;
}

/// Remove a pane from the queue as a delta under the lock (never a full model write-back).
fn evict_pane(runtime: &RuntimeEnv, pane_id: &str) {
    let _ = StateStore::new(&runtime.state_dir).update(|mut entries| {
        evict(&mut entries, pane_id);
        (entries, ())
    });
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
}
