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
    agent_label, clear, current_unix_ms, describe_entry, evict, load_entries, Herdr, PluginError,
    QueueEntry, RuntimeEnv, StateStore, WaitStatus,
};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use serde_json::Value;
use std::io::Read;
use std::time::Duration;

/// How long the input poll blocks each tick before we re-read the queue and repaint, so queue
/// changes and the ticking "waited" column appear promptly without busy-spinning.
const TICK: Duration = Duration::from_millis(250);

const FOOTER_HINTS: &str =
    "j/k move  ·  Enter jump  ·  space reply  ·  d drop  ·  c clear  ·  q quit";

/// The label herdr shows for our status pane in `pane list`, used to recognize our own pane for
/// the idempotent open/focus/close toggle. Must stay in sync with the `[[panes]]` `title` in
/// `herdr-plugin.toml` — that title is the only field identifying the pane in `pane list`.
const PANE_LABEL: &str = "Check-in";

/// Entry point for the `pane` subcommand: run the TUI until the user closes it. The terminal is
/// restored on every exit path (including a panic, via the hook `ratatui::try_init` installs).
///
/// Mouse capture is handled separately: `ratatui::try_init`/`restore` do **not** touch it, so we
/// enable it after init, disable it before restore (covering both the `Ok` and the error return
/// from `event_loop`), and chain a panic hook to disable it on the panic path — ratatui's own
/// panic hook restores the terminal but leaves capture on, which would otherwise flood the shell
/// with mouse escape sequences until the user ran `reset`.
pub fn run(runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    let mut model = PaneModel::new(load_entries(&runtime.state_dir));
    let mut terminal = ratatui::try_init()
        .map_err(|error| PluginError::new(format!("failed to initialize terminal: {error}")))?;

    if let Err(error) = execute!(std::io::stdout(), EnableMouseCapture) {
        ratatui::restore();
        return Err(PluginError::new(format!(
            "failed to enable mouse capture: {error}"
        )));
    }
    install_mouse_panic_hook();

    let result = event_loop(&mut terminal, &mut model, runtime, herdr);

    // Disable capture before restore, and on every non-panic exit path (event_loop's `?` errors
    // land here too). Best-effort: if this fails we're already tearing down.
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Chain a panic hook that disables mouse capture, then defers to the hook `ratatui::try_init`
/// already installed (which restores the terminal). Must be called *after* `try_init` so the hook
/// we capture is ratatui's.
fn install_mouse_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        previous(info);
    }));
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    model: &mut PaneModel,
    runtime: &RuntimeEnv,
    herdr: &dyn Herdr,
) -> Result<(), PluginError> {
    // Persisted across frames so mouse hit-testing can read the list's live scroll offset:
    // `render_stateful_widget` maintains `list_state.offset()` as the selection/size change, and a
    // fresh `ListState` each frame would throw that away. `list_area` is the list's on-screen
    // `Rect`, recorded by `draw` each frame (None while the queue is empty — nothing to click).
    let mut list_state = ListState::default();
    loop {
        let now_ms = current_unix_ms();
        let mut list_area = None;
        terminal
            .draw(|frame| draw(frame, model, now_ms, &mut list_state, &mut list_area))
            .map_err(|error| PluginError::new(format!("failed to draw: {error}")))?;

        if event::poll(TICK)
            .map_err(|error| PluginError::new(format!("failed to poll input: {error}")))?
        {
            let event = event::read()
                .map_err(|error| PluginError::new(format!("failed to read input: {error}")))?;
            // The clear-all confirm guard is hoisted above BOTH the key and mouse branches: while
            // a confirm is pending, any input other than `y`/`Y` cancels it and nothing falls
            // through to the normal bindings (a click, in particular, must not silently reselect).
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Reply mode is the top-priority guard: while composing, keys feed the buffer
                    // and nothing falls through to the normal bindings (so `q`/`d`/`c` are literal
                    // characters, not commands). Ctrl-C stays the universal escape hatch.
                    if model.reply.is_some() {
                        match key.code {
                            KeyCode::Esc => model.cancel_reply(),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }
                            KeyCode::Enter => on_reply_submit(model, runtime, herdr),
                            KeyCode::Backspace => model.reply_backspace(),
                            KeyCode::Char(ch)
                                if !key
                                    .modifiers
                                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                model.reply_push(ch)
                            }
                            _ => {}
                        }
                    } else if model.confirm_clear {
                        on_confirm_clear(model, runtime, key.code);
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }
                            KeyCode::Char('j') | KeyCode::Down => model.move_down(),
                            KeyCode::Char('k') | KeyCode::Up => model.move_up(),
                            KeyCode::Enter => on_enter(model, runtime, herdr),
                            KeyCode::Char('d') | KeyCode::Char('x') => on_drop(model, runtime),
                            KeyCode::Char('c') => model.request_clear(),
                            KeyCode::Char(' ') => model.begin_reply(),
                            _ => {}
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    if model.reply.is_some() {
                        // A click while composing cancels the reply, like any other non-input.
                        model.cancel_reply();
                    } else if model.confirm_clear {
                        // A click while a clear-all confirm is pending cancels it, like any other
                        // non-`y` input — never a reselect.
                        model.confirm_clear = false;
                    } else {
                        on_mouse(model, mouse, list_area, list_state.offset());
                    }
                }
                _ => {}
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

/// `Enter` in reply mode: submit the composed reply to the captured target, then — only on a
/// successful submit — evict it, mirroring `on_enter`'s act-then-evict-on-success discipline
/// (invariant #2). A failed submit keeps the entry: losing an unanswered waiter is the exact
/// failure the plugin exists to prevent. An empty/whitespace buffer sends nothing and stays in
/// reply mode. The reply is fire-and-forget (no `--wait`); "submit accepted" is the success
/// boundary for eviction — the pane never blocks waiting for the agent's next turn.
fn on_reply_submit(model: &mut PaneModel, runtime: &RuntimeEnv, herdr: &dyn Herdr) {
    let has_text = model
        .reply
        .as_ref()
        .is_some_and(|draft| !draft.buffer.trim().is_empty());
    if !has_text {
        return;
    }
    // `take` leaves reply mode now; the target/label were captured when it was armed.
    let draft = model.reply.take().expect("reply draft present");
    let text = draft.buffer.trim();

    match herdr.prompt_agent(&draft.target, text) {
        Ok(()) => {
            model.status = match evict_pane(runtime, &draft.target) {
                Ok(()) => Some(format!("replied to {}", draft.label)),
                Err(error) => Some(format!("replied, but drop failed: {error}")),
            };
            model.sync(load_entries(&runtime.state_dir));
        }
        Err(error) => {
            model.status = Some(format!("reply failed: {error}"));
        }
    }
}

/// Resolve a pending clear-all confirm: `y`/`Y` empties the queue, any other key cancels. Either
/// way the confirm is dismissed. The clear itself is [`crate::clear`] — already a delta through
/// [`StateStore::update`], so invariant #1 holds for free.
fn on_confirm_clear(model: &mut PaneModel, runtime: &RuntimeEnv, code: KeyCode) {
    model.confirm_clear = false;
    if matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) {
        model.status = match clear(runtime) {
            Ok(()) => None,
            Err(error) => Some(format!("clear failed: {error}")),
        };
        model.sync(load_entries(&runtime.state_dir));
    }
}

/// A left mouse-button press selects the clicked row, exactly like `j`/`k` landing on it. Any
/// other mouse event (drags, scrolls, other buttons, releases) is ignored. Safe on an empty queue
/// by construction: `list_area` is `None` then, and even otherwise `row_for_click` returns `None`
/// for an out-of-range row.
fn on_mouse(model: &mut PaneModel, mouse: MouseEvent, list_area: Option<Rect>, offset: usize) {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }
    if let Some(area) = list_area {
        // Recompute the grouped layout the same way `draw` did this frame — the entries are
        // unchanged between draw and this click (the next `sync` runs only after event handling),
        // so it reproduces exactly what was painted. A click on a header row selects nothing.
        let rows = layout_rows(&model.entries);
        if let Some(index) = row_for_click(area, offset, &rows, mouse.column, mouse.row) {
            model.selected = index;
        }
    }
}

/// Map a click at terminal cell `(col, row)` to the queue index it lands on, or `None` if the
/// click is outside the list `area`, on a section header, or on a blank row below the last row.
/// `offset` is the index of the first visible row (the list's scroll position), so the clicked
/// display row is `offset + (row - area.y)`; that row is translated back to an entry index through
/// the grouped `rows` layout (headers map to `None`). Pure and unit-tested.
fn row_for_click(area: Rect, offset: usize, rows: &[Row], col: u16, row: u16) -> Option<usize> {
    let inside_x = col >= area.x && col < area.x.saturating_add(area.width);
    let inside_y = row >= area.y && row < area.y.saturating_add(area.height);
    if !inside_x || !inside_y {
        return None;
    }
    let display_index = offset + (row - area.y) as usize;
    match rows.get(display_index) {
        Some(Row::Entry(index)) => Some(*index),
        _ => None,
    }
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

/// An in-progress inline reply. The target `pane_id` and its display `label` are captured when
/// reply mode is armed — not read from the live selection at submit time — so a concurrent queue
/// `sync` that reorders or evicts entries while the user is still typing can never retarget the
/// reply to a different agent. `buffer` is the text composed so far.
struct ReplyDraft {
    target: String,
    label: String,
    buffer: String,
}

/// The pane's view state: the queue plus a selection cursor and a transient footer message.
/// All transitions are pure and unit-tested; the terminal loop above is the only impure part.
struct PaneModel {
    entries: Vec<QueueEntry>,
    selected: usize,
    status: Option<String>,
    /// True while a clear-all confirm is pending (armed by `c`, resolved by the next key).
    confirm_clear: bool,
    /// `Some` while composing an inline reply (armed by `space`). Distinct from `confirm_clear` so
    /// the two modals never overlap — `begin_reply` refuses to arm while a clear-confirm is pending.
    reply: Option<ReplyDraft>,
}

impl PaneModel {
    fn new(entries: Vec<QueueEntry>) -> Self {
        Self {
            entries,
            selected: 0,
            status: None,
            confirm_clear: false,
            reply: None,
        }
    }

    /// `c`: arm the clear-all confirm, but only when there's something to clear (a no-op on an
    /// empty queue, like `d`/`Enter`).
    fn request_clear(&mut self) {
        if !self.entries.is_empty() {
            self.confirm_clear = true;
        }
    }

    /// `space`: begin an inline reply to the selected agent. No-op on an empty queue (nothing
    /// selected) or while a clear-all confirm is pending, so the two modals never overlap. Captures
    /// the target pane id and display label now (see [`ReplyDraft`]).
    fn begin_reply(&mut self) {
        if self.confirm_clear {
            return;
        }
        if let Some(entry) = self.entries.get(self.selected) {
            self.reply = Some(ReplyDraft {
                target: entry.pane_id.clone(),
                label: agent_label(entry).to_string(),
                buffer: String::new(),
            });
        }
    }

    /// Append a typed character to the reply buffer (no-op outside reply mode).
    fn reply_push(&mut self, ch: char) {
        if let Some(draft) = self.reply.as_mut() {
            draft.buffer.push(ch);
        }
    }

    /// Delete the last character of the reply buffer (no-op outside reply mode or on an empty one).
    fn reply_backspace(&mut self) {
        if let Some(draft) = self.reply.as_mut() {
            draft.buffer.pop();
        }
    }

    /// Leave reply mode, discarding the buffer.
    fn cancel_reply(&mut self) {
        self.reply = None;
    }

    /// The entry indices in on-screen order — the `layout_rows` grouping projected down to just its
    /// entries (headers dropped). `j`/`k` step through *this* order, not raw `entries` order, so the
    /// cursor moves monotonically down the screen even when the FIFO queue interleaves blocked and
    /// done. Derived from the same `layout_rows` the view paints, so traversal and layout can't drift.
    fn display_order(&self) -> Vec<usize> {
        layout_rows(&self.entries)
            .into_iter()
            .filter_map(|row| match row {
                Row::Entry(index) => Some(index),
                Row::Header(_) => None,
            })
            .collect()
    }

    fn move_up(&mut self) {
        let order = self.display_order();
        if let Some(pos) = order.iter().position(|&index| index == self.selected) {
            if pos > 0 {
                self.selected = order[pos - 1];
            }
        }
    }

    fn move_down(&mut self) {
        let order = self.display_order();
        if let Some(pos) = order.iter().position(|&index| index == self.selected) {
            if pos + 1 < order.len() {
                self.selected = order[pos + 1];
            }
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

/// The section header text for each `WaitStatus`, in the order sections are shown: agents that need
/// input first, then finished ones. These are the CC-agents-view group labels.
const AWAITING_HEADER: &str = "AWAITING YOU";
const DONE_HEADER: &str = "DONE";

/// One rendered line of the grouped agents-view: either a non-selectable section header or an
/// entry carrying its index into `entries` (the selection source of truth). Built per-frame by
/// [`layout_rows`].
enum Row {
    Header(&'static str),
    Entry(usize),
}

/// Group the queue into status sections for display — `AWAITING YOU` (`blocked`) then `DONE`
/// (`done`), FIFO within each — as a pure view transform. It never reorders `entries`: each
/// `Row::Entry` keeps the entry's original index, and a section header is emitted only when that
/// section has at least one entry (so an all-`done` queue shows no "AWAITING YOU" heading). This is
/// the only place the on-screen row order diverges from `entries`; `draw` and `row_for_click` both
/// go through it, so the paint and the click hit-testing always agree.
fn layout_rows(entries: &[QueueEntry]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (header, status) in [
        (AWAITING_HEADER, WaitStatus::Blocked),
        (DONE_HEADER, WaitStatus::Done),
    ] {
        let mut section: Vec<Row> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.status == status)
            .map(|(index, _)| Row::Entry(index))
            .collect();
        if !section.is_empty() {
            rows.push(Row::Header(header));
            rows.append(&mut section);
        }
    }
    rows
}

fn draw(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
) {
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
        // The CC-agents-view look: entries grouped into status sections with non-selectable
        // headers. `layout_rows` is a pure view over the FIFO queue — it never reorders `entries`,
        // so `selected` stays an index into `entries` and we translate it to its on-screen row here.
        let rows = layout_rows(&model.entries);
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| match row {
                Row::Header(title) => ListItem::new(*title).bold(),
                Row::Entry(index) => ListItem::new(describe_entry(&model.entries[*index], now_ms)),
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().reversed())
            .highlight_symbol("> ");
        // Highlight the display row that carries the selected entry (headers are never selected).
        let selected_row = rows
            .iter()
            .position(|row| matches!(row, Row::Entry(index) if *index == model.selected));
        list_state.select(selected_row);
        // Record the list's rect for this frame so mouse clicks can be hit-tested against exactly
        // what was painted; `render_stateful_widget` updates `list_state`'s scroll offset in place.
        *list_area = Some(areas[1]);
        frame.render_stateful_widget(list, areas[1], list_state);
    }

    let footer = if let Some(draft) = &model.reply {
        reply_prompt(&draft.label, &draft.buffer)
    } else if model.confirm_clear {
        confirm_prompt(model.entries.len())
    } else {
        model.status.as_deref().unwrap_or(FOOTER_HINTS).to_string()
    };
    frame.render_widget(Paragraph::new(footer).dim(), areas[2]);
}

/// The footer shown while composing an inline reply: who it goes to, the text so far, and the
/// keys. A trailing `_` marks the caret (we don't drive the terminal's own cursor in the pane).
fn reply_prompt(label: &str, buffer: &str) -> String {
    format!("reply to {label}: {buffer}_   (enter send · esc cancel)")
}

/// The footer prompt shown while a clear-all confirm is pending. Only armed on a non-empty queue,
/// so `count >= 1`; pluralized so the singular case doesn't read "1 entries".
fn confirm_prompt(count: usize) -> String {
    match count {
        1 => "clear all 1 entry? y/n".to_string(),
        n => format!("clear all {n} entries? y/n"),
    }
}

fn header_text(count: usize) -> String {
    match count {
        0 => "Check-in — queue empty".to_string(),
        1 => "Check-in — 1 agent waiting".to_string(),
        n => format!("Check-in — {n} agents waiting"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WaitStatus;

    fn entry(pane_id: &str) -> QueueEntry {
        entry_with_status(pane_id, WaitStatus::Blocked)
    }

    fn entry_with_status(pane_id: &str, status: WaitStatus) -> QueueEntry {
        QueueEntry {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("").to_string(),
            agent: Some("claude".to_string()),
            display_agent: Some("Claude".to_string()),
            title: Some("t".to_string()),
            status,
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
    fn move_down_and_up_follow_display_order_across_sections() {
        // FIFO entries interleave blocked/done: [blocked0, done1, blocked2, done3]. The grouped
        // display order is AWAITING YOU (0, 2) then DONE (1, 3), so j/k must visit 0 -> 2 -> 1 -> 3
        // — crossing the section boundary monotonically down-screen, not stepping through `entries`.
        let mut m = PaneModel::new(vec![
            entry_with_status("w0:p1", WaitStatus::Blocked),
            entry_with_status("w1:p1", WaitStatus::Done),
            entry_with_status("w2:p1", WaitStatus::Blocked),
            entry_with_status("w3:p1", WaitStatus::Done),
        ]);
        assert_eq!(m.selected, 0); // blocked0, the first AWAITING YOU row
        m.move_down();
        assert_eq!(
            m.selected, 2,
            "next display row is blocked2, not entries[1]"
        );
        m.move_down();
        assert_eq!(m.selected, 1, "then crosses into DONE: done1");
        m.move_down();
        assert_eq!(m.selected, 3);
        m.move_down();
        assert_eq!(m.selected, 3, "clamps at the last display row");
        m.move_up();
        assert_eq!(m.selected, 1);
        m.move_up();
        assert_eq!(m.selected, 2);
        m.move_up();
        assert_eq!(m.selected, 0);
        m.move_up();
        assert_eq!(m.selected, 0, "clamps at the first display row");
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
    fn request_clear_arms_confirm_on_a_nonempty_queue() {
        let mut m = model(&["w1:p1", "w2:p1"]);
        assert!(!m.confirm_clear);
        m.request_clear();
        assert!(m.confirm_clear);
    }

    #[test]
    fn request_clear_is_a_noop_on_an_empty_queue() {
        let mut m = model(&[]);
        m.request_clear();
        assert!(
            !m.confirm_clear,
            "confirm must not arm with nothing to clear"
        );
    }

    #[test]
    fn confirm_prompt_pluralizes() {
        assert_eq!(confirm_prompt(1), "clear all 1 entry? y/n");
        assert_eq!(confirm_prompt(3), "clear all 3 entries? y/n");
    }

    #[test]
    fn begin_reply_captures_the_selected_target_and_label() {
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.move_down(); // select w2:p1
        m.begin_reply();
        let draft = m.reply.as_ref().expect("reply should be armed");
        assert_eq!(draft.target, "w2:p1");
        assert_eq!(draft.label, "Claude"); // the entry's display_agent
        assert_eq!(draft.buffer, "");
    }

    #[test]
    fn begin_reply_is_a_noop_on_an_empty_queue() {
        let mut m = model(&[]);
        m.begin_reply();
        assert!(m.reply.is_none(), "nothing selected, nothing to reply to");
    }

    #[test]
    fn begin_reply_does_not_arm_while_a_clear_confirm_is_pending() {
        let mut m = model(&["w1:p1"]);
        m.request_clear();
        m.begin_reply();
        assert!(m.reply.is_none(), "the two modals must never overlap");
        assert!(m.confirm_clear);
    }

    #[test]
    fn reply_push_and_backspace_edit_the_buffer() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        m.reply_push('h');
        m.reply_push('i');
        assert_eq!(m.reply.as_ref().unwrap().buffer, "hi");
        m.reply_backspace();
        assert_eq!(m.reply.as_ref().unwrap().buffer, "h");
    }

    #[test]
    fn reply_edits_are_noops_outside_reply_mode() {
        let mut m = model(&["w1:p1"]);
        m.reply_push('x');
        m.reply_backspace();
        assert!(m.reply.is_none());
    }

    #[test]
    fn cancel_reply_discards_the_buffer() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        m.reply_push('x');
        m.cancel_reply();
        assert!(m.reply.is_none());
    }

    #[test]
    fn the_reply_target_is_fixed_at_arm_time_even_if_the_selection_moves() {
        // Arm a reply to w1:p1, then let a concurrent sync evict it. The selection clamps onto the
        // surviving entry, but the reply must stay pointed at the agent it was started for.
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.begin_reply();
        m.sync(vec![entry("w2:p1")]); // w1:p1 evicted; only w2:p1 remains
        assert_eq!(m.selected_pane_id(), Some("w2:p1"), "selection moved");
        assert_eq!(
            m.reply.as_ref().unwrap().target,
            "w1:p1",
            "but the reply stays targeted at the agent it was started for"
        );
    }

    #[test]
    fn reply_prompt_names_the_target_and_shows_the_buffer() {
        let prompt = reply_prompt("Claude", "use option B");
        assert!(prompt.contains("reply to Claude"));
        assert!(prompt.contains("use option B"));
    }

    // The list rect used across the click tests: starts at row 1 (row 0 is the top header line),
    // 10 rows tall, 40 wide.
    fn list_area() -> Rect {
        Rect {
            x: 0,
            y: 1,
            width: 40,
            height: 10,
        }
    }

    // All-blocked entries render as [Header("AWAITING YOU"), Entry(0), Entry(1), ...] — the section
    // header occupies the first display row, so entry N sits one row lower than in a flat list.
    fn blocked_rows(count: usize) -> Vec<Row> {
        let entries: Vec<QueueEntry> = (0..count).map(|i| entry(&format!("w{i}:p1"))).collect();
        layout_rows(&entries)
    }

    #[test]
    fn row_for_click_maps_rows_to_entries_past_the_header() {
        // No scroll: display row 0 (area.y) is the "AWAITING YOU" header, so row 1 is entry 0.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 2), Some(0));
        assert_eq!(row_for_click(list_area(), 0, &rows, 0, 4), Some(2));
    }

    #[test]
    fn row_for_click_skips_section_headers() {
        // The header display row selects nothing, like a blank row.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 1), None);
    }

    #[test]
    fn row_for_click_accounts_for_scroll_offset() {
        // Scrolled down by 2: display row at area.y is display index 2 — which is Entry(1) here
        // ([Header, Entry(0), Entry(1), ...]).
        let rows = blocked_rows(5);
        assert_eq!(row_for_click(list_area(), 2, &rows, 10, 1), Some(1));
        assert_eq!(row_for_click(list_area(), 2, &rows, 10, 3), Some(3));
    }

    #[test]
    fn row_for_click_rejects_blank_rows_below_the_last_entry() {
        // Header + 3 entries = 4 display rows; a click on display row 4 (row 5) is blank.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 5), None);
    }

    #[test]
    fn row_for_click_rejects_clicks_outside_the_area() {
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 0), None); // above the list
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 11), None); // below the list
        assert_eq!(row_for_click(list_area(), 0, &rows, 40, 2), None); // one past the right edge
    }

    #[test]
    fn row_for_click_is_safe_on_an_empty_queue() {
        let rows = blocked_rows(0);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 1), None);
    }

    #[test]
    fn layout_rows_groups_by_status_fifo_without_reordering_entries() {
        // A queue interleaving blocked and done: the layout emits AWAITING YOU (blocked, FIFO) then
        // DONE (done, FIFO), each Entry keeping its original index into `entries`.
        let entries = vec![
            entry_with_status("w0:p1", WaitStatus::Blocked),
            entry_with_status("w1:p1", WaitStatus::Done),
            entry_with_status("w2:p1", WaitStatus::Blocked),
            entry_with_status("w3:p1", WaitStatus::Done),
        ];
        let rows = layout_rows(&entries);
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Header(title) => format!("#{title}"),
                Row::Entry(index) => format!("{index}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec!["#AWAITING YOU", "0", "2", "#DONE", "1", "3"],
            "sections group by status, FIFO within, indices unchanged"
        );
    }

    #[test]
    fn layout_rows_omits_an_empty_section_header() {
        // All done: no AWAITING YOU heading, only DONE.
        let entries = vec![
            entry_with_status("w0:p1", WaitStatus::Done),
            entry_with_status("w1:p1", WaitStatus::Done),
        ];
        let rows = layout_rows(&entries);
        assert!(
            matches!(rows.first(), Some(Row::Header(DONE_HEADER))),
            "an all-done queue leads with the DONE header, not AWAITING YOU"
        );
        assert_eq!(rows.len(), 3, "one header + two entries");
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn on_mouse_left_click_selects_the_clicked_row() {
        // All blocked -> [Header, Entry(0), Entry(1), Entry(2)] at terminal rows 1..=4. A click on
        // row 4 lands on the third entry (index 2), past the section header on row 1.
        let mut m = model(&["w1:p1", "w2:p1", "w3:p1"]);
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 4),
            Some(list_area()),
            0,
        );
        assert_eq!(m.selected, 2);
    }

    #[test]
    fn on_mouse_left_click_on_a_header_row_selects_nothing() {
        // The "AWAITING YOU" header sits at terminal row 1; clicking it leaves the selection put.
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.move_down(); // selection at 1
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 1),
            Some(list_area()),
            0,
        );
        assert_eq!(m.selected, 1, "a header click must not reselect");
    }

    #[test]
    fn on_mouse_ignores_non_left_events_and_out_of_range_clicks() {
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.move_down(); // selection at 1
                       // A scroll event must not move the selection.
        on_mouse(
            &mut m,
            mouse(MouseEventKind::ScrollDown, 5, 2),
            Some(list_area()),
            0,
        );
        assert_eq!(m.selected, 1);
        // A left click on a blank row (past the header + 2 entries) is a no-op.
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 6),
            Some(list_area()),
            0,
        );
        assert_eq!(m.selected, 1);
        // No list area (empty queue was drawn) is a no-op even for a left click.
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 2),
            None,
            0,
        );
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn header_text_pluralizes() {
        assert_eq!(header_text(0), "Check-in — queue empty");
        assert_eq!(header_text(1), "Check-in — 1 agent waiting");
        assert_eq!(header_text(3), "Check-in — 3 agents waiting");
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

    // --- reply submit (impure: reads/writes state.json, talks to a fake herdr) ---------------

    use crate::test_support::{feed_status, load, runtime, temp_state_dir, FakeHerdr};

    // Seed one blocked waiter and return a model over it, already in reply mode with `text` typed.
    fn armed_reply(dir: &std::path::Path, text: &str) -> PaneModel {
        feed_status(dir, 1_000, "w1:p1", "w1", "blocked", "needs input");
        let mut model = PaneModel::new(load(dir));
        model.begin_reply();
        for ch in text.chars() {
            model.reply_push(ch);
        }
        model
    }

    #[test]
    fn on_reply_submit_routes_the_reply_then_evicts_on_success() {
        let dir = temp_state_dir("reply-ok");
        let mut model = armed_reply(&dir, "yes");
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked")]);

        on_reply_submit(&mut model, &runtime(dir.clone(), 2_000), &herdr);

        assert_eq!(
            herdr.prompts.into_inner(),
            vec![("w1:p1".to_string(), "yes".to_string())],
            "the reply is routed to the captured target by pane_id"
        );
        assert!(load(&dir).is_empty(), "a submitted reply evicts the waiter");
        assert!(model.reply.is_none(), "reply mode ends after submit");
        assert_eq!(model.status.as_deref(), Some("replied to Claude"));
    }

    #[test]
    fn on_reply_submit_keeps_the_entry_when_the_prompt_fails() {
        let dir = temp_state_dir("reply-fail");
        let mut model = armed_reply(&dir, "yes");
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked")]).with_failing_prompt();

        on_reply_submit(&mut model, &runtime(dir.clone(), 2_000), &herdr);

        assert_eq!(
            load(&dir).len(),
            1,
            "a failed reply must NOT lose the waiter (invariant #2)"
        );
        assert!(model.reply.is_none(), "reply mode still ends");
        assert!(model
            .status
            .as_deref()
            .is_some_and(|s| s.contains("reply failed")));
    }

    #[test]
    fn on_reply_submit_sends_nothing_for_an_empty_buffer_and_stays_in_reply_mode() {
        let dir = temp_state_dir("reply-empty");
        let mut model = armed_reply(&dir, "   "); // whitespace only
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked")]);

        on_reply_submit(&mut model, &runtime(dir.clone(), 2_000), &herdr);

        assert!(
            herdr.prompts.into_inner().is_empty(),
            "an empty reply sends nothing"
        );
        assert!(
            model.reply.is_some(),
            "and stays in reply mode to keep typing"
        );
        assert_eq!(load(&dir).len(), 1, "the waiter is untouched");
    }
}
