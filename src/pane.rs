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
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// How long the input poll blocks each tick before we re-read the queue and repaint, so queue
/// changes and the ticking "waited" column appear promptly without busy-spinning.
const TICK: Duration = Duration::from_millis(250);

const FOOTER_HINTS: &str =
    "j/k move  ·  Enter jump  ·  space reply  ·  d drop  ·  c clear  ·  q quit";

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

    // When launched as a herdr popup (`--placement popup`), dismiss the popup on exit so herdr
    // doesn't keep painting a dead frame until the next keypress. Env-gated (the launcher sets
    // HERDR_CHECKIN_POPUP=1) so a split/overlay launch never closes an unrelated session popup.
    // Best-effort: on failure herdr's own child-exit cleanup still removes it.
    if std::env::var_os("HERDR_CHECKIN_POPUP").is_some() {
        let _ = herdr.popup_close();
    }
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
                            KeyCode::Enter => {
                                // A successful jump closes the popup (run() then fires popup.close).
                                if on_enter(model, runtime, herdr) {
                                    return Ok(());
                                }
                            }
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

/// `Enter`: focus the selected agent, then — only on success — evict it from the queue. Returns
/// `true` when the jump succeeded, signaling the pane should close: as a popup it floats over the
/// whole session, so once we've navigated to the agent it must get out of the way. Returns `false`
/// when nothing is selected or the focus failed (the entry stays and the error shows in the
/// footer), keeping the pane open.
fn on_enter(model: &mut PaneModel, runtime: &RuntimeEnv, herdr: &dyn Herdr) -> bool {
    let Some(pane_id) = model.selected_pane_id().map(str::to_owned) else {
        return false;
    };
    match herdr.focus_agent(&pane_id) {
        Ok(()) => {
            // The jump worked; if the drop can't be persisted, say so rather than imply success.
            model.status = match evict_pane(runtime, &pane_id) {
                Ok(()) => None,
                Err(error) => Some(format!("jumped, but drop failed: {error}")),
            };
            model.sync(load_entries(&runtime.state_dir));
            true
        }
        Err(error) => {
            model.status = Some(format!("focus failed: {error}"));
            false
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
                Row::Header(_) | Row::Spacer => None,
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
/// input first (on-brand `CHECKIN`), then finished ones (`DONE`).
const CHECKIN_HEADER: &str = "CHECKIN";
const DONE_HEADER: &str = "DONE";

/// One rendered line of the grouped agents-view: a blank spacer, a non-selectable section header,
/// or an entry carrying its index into `entries` (the selection source of truth). Built per-frame
/// by [`layout_rows`]. Only `Entry` rows are selectable.
enum Row {
    Spacer,
    Header(&'static str),
    Entry(usize),
}

/// Group the queue into status sections for display — `CHECKIN` (`blocked`) then `DONE` (`done`),
/// FIFO within each — as a pure view transform. Each non-empty section is preceded by a blank
/// spacer row so the groups read as visually distinct blocks (and the first spacer separates them
/// from the count line above). It never reorders `entries`: each `Row::Entry` keeps the entry's
/// original index, and a section (spacer + header) is emitted only when that section has at least
/// one entry (so an all-`done` queue shows no `CHECKIN` heading). This is the only place the
/// on-screen row order diverges from `entries`; `draw` and `row_for_click` both go through it, so
/// the paint and the click hit-testing always agree.
fn layout_rows(entries: &[QueueEntry]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (header, status) in [
        (CHECKIN_HEADER, WaitStatus::Blocked),
        (DONE_HEADER, WaitStatus::Done),
    ] {
        let mut section: Vec<Row> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.status == status)
            .map(|(index, _)| Row::Entry(index))
            .collect();
        if !section.is_empty() {
            rows.push(Row::Spacer);
            rows.push(Row::Header(header));
            rows.append(&mut section);
        }
    }
    rows
}

/// The most input rows the compose strip shows at once; longer replies scroll, keeping the last
/// [`MAX_INPUT_ROWS`] lines (the tail, where the cursor is). Replies are short by design and
/// vertical space in the popup is scarce, so the strip stays small.
const MAX_INPUT_ROWS: usize = 3;

/// Precomputed layout for the inline-reply compose strip: the live draft, its buffer wrapped into
/// display-width lines, and the resulting input height (wrapped line count, capped at
/// [`MAX_INPUT_ROWS`]). Computed up front in [`draw`] because the height drives the vertical split.
struct ComposeLayout<'a> {
    draft: &'a ReplyDraft,
    lines: Vec<String>,
    height: u16,
}

fn draw(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
) {
    let interior = frame.area();

    // In reply mode the single footer line expands into a compose strip docked below the list: a
    // titled rule, a 1..=MAX_INPUT_ROWS input, and a hint row. The input height is the wrapped line
    // count; compute the wrap up front because it drives the vertical split.
    let compose = model.reply.as_ref().map(|draft| {
        let lines = wrap_display(&draft.buffer, input_width(interior.width));
        let height = lines.len().clamp(1, MAX_INPUT_ROWS) as u16;
        ComposeLayout {
            draft,
            lines,
            height,
        }
    });

    let areas = match &compose {
        Some(c) => Layout::vertical([
            Constraint::Length(1),        // count header
            Constraint::Min(0),           // the queue (dimmed while composing)
            Constraint::Length(1),        // titled rule
            Constraint::Length(c.height), // input
            Constraint::Length(1),        // hint
        ])
        .split(interior),
        None => Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(interior),
    };

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
        draw_list(
            frame,
            model,
            now_ms,
            list_state,
            list_area,
            areas[1],
            compose.as_ref(),
        );
    }

    match &compose {
        // Composing: darken the header + queue as one veil so the strip is the only lit surface
        // (focus by receding everything else, not by brightening the input), then draw the strip.
        Some(c) => {
            let veil = Rect {
                height: areas[0].height + areas[1].height,
                ..areas[0]
            };
            dim_area(frame, veil);
            draw_compose(frame, c, areas[2], areas[3], areas[4]);
        }
        // Navigating: the one-line footer carries the clear-confirm, a transient status, or hints.
        None => {
            let footer = if model.confirm_clear {
                confirm_prompt(model.entries.len())
            } else {
                model.status.as_deref().unwrap_or(FOOTER_HINTS).to_string()
            };
            frame.render_widget(Paragraph::new(footer).dim(), areas[2]);
        }
    }
}

/// Render the grouped queue into `area`, recording the painted rect into `list_area` for click
/// hit-testing and drawing a scrollbar when the rows overflow. `compose` decides the highlight:
/// while navigating, the live selection in reversed video; while composing, only a plain `> ` marker
/// on the captured reply target (the whole list is dimmed by the caller, and reversed+dim renders
/// unreliably across terminals).
fn draw_list(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
    area: Rect,
    compose: Option<&ComposeLayout>,
) {
    // The CC-agents-view look: entries grouped into status sections with non-selectable headers.
    // `layout_rows` is a pure view over the FIFO queue — it never reorders `entries`, so `selected`
    // stays an index into `entries` and we translate it to its on-screen row here.
    let rows = layout_rows(&model.entries);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            Row::Spacer => ListItem::new(""),
            Row::Header(title) => ListItem::new(*title).bold(),
            Row::Entry(index) => ListItem::new(describe_entry(&model.entries[*index], now_ms)),
        })
        .collect();

    let (highlight_index, highlight_style) = match compose {
        Some(c) => (
            model
                .entries
                .iter()
                .position(|e| e.pane_id == c.draft.target),
            Style::new(),
        ),
        None => (Some(model.selected), Style::new().reversed()),
    };
    // Highlight the display row that carries the highlighted entry (headers are never selected).
    let selected_row = highlight_index.and_then(|target| {
        rows.iter()
            .position(|row| matches!(row, Row::Entry(index) if *index == target))
    });
    list_state.select(selected_row);

    let list = List::new(items)
        .highlight_style(highlight_style)
        .highlight_symbol("> ");

    // Reserve the right-most column for a scrollbar when the grouped rows overflow the viewport, so
    // the list text never collides with the thumb. `List`+`ListState` already scrolls to keep the
    // selection in view; the scrollbar only makes that off-screen content discoverable. Each
    // `ListItem` is one row, so display-row count == item count == the units `offset()` counts in.
    let viewport = area.height as usize;
    let overflow = rows.len() > viewport;
    let list_rect = if overflow {
        Rect {
            width: area.width.saturating_sub(1),
            ..area
        }
    } else {
        area
    };
    // Record the list's rect for this frame so mouse clicks can be hit-tested against exactly what
    // was painted; `render_stateful_widget` updates `list_state`'s scroll offset in place.
    *list_area = Some(list_rect);
    frame.render_stateful_widget(list, list_rect, list_state);
    if overflow {
        let track = Rect {
            x: area.x + area.width - 1,
            width: 1,
            ..area
        };
        render_list_scrollbar(frame, track, rows.len(), viewport, list_state.offset());
    }
}

/// Draw the inline-reply compose strip: a titled rule, the input, and a hint row. The input renders
/// the last up-to-[`MAX_INPUT_ROWS`] wrapped lines at normal intensity (everything above is dimmed
/// by the caller) and drives the real terminal cursor to the end of the text — no fake caret.
fn draw_compose(
    frame: &mut Frame,
    compose: &ComposeLayout,
    rule_area: Rect,
    input_area: Rect,
    hint_area: Rect,
) {
    // The titled rule announces the mode switch and names the captured target (pinned at arm time,
    // so it stays correct even if the queue re-orders under the dimmed list).
    frame.render_widget(reply_rule(&compose.draft.label, rule_area.width), rule_area);

    // One column of left padding aligns the input under the rule's label; the cursor rides the
    // text's end. An empty buffer shows a dim-italic placeholder with the cursor parked at column 0.
    let pad_x = input_area.x + 1;
    if compose.draft.buffer.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" "),
                "type your reply".dim().italic(),
            ])),
            input_area,
        );
        frame.set_cursor_position((pad_x, input_area.y));
    } else {
        let visible = visible_input_lines(&compose.lines);
        let body: Vec<Line> = visible
            .iter()
            .map(|line| Line::from(format!(" {line}")))
            .collect();
        frame.render_widget(Paragraph::new(body), input_area);
        let last = visible.last().copied().unwrap_or("");
        let cursor_x = pad_x + display_width(last) as u16;
        let cursor_y = input_area.y + (visible.len() as u16).saturating_sub(1);
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    // The affordances: dim (reference, not the work), right-aligned to keep the typing edge clean.
    frame.render_widget(
        Paragraph::new(reply_hint(hint_area.width))
            .dim()
            .alignment(Alignment::Right),
        hint_area,
    );
}

/// Stamp every cell in `area` with the `DIM` modifier over whatever was already drawn there — the
/// same post-hoc veil herdr uses to recede content behind a modal. Dims the queue uniformly while
/// an inline reply is composed, including any scrollbar, so the compose strip reads as active.
fn dim_area(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let cell = &mut buf[(x, y)];
            let dimmed = cell.style().add_modifier(Modifier::DIM);
            cell.set_style(dimmed);
        }
    }
}

/// The content width of the input after its one-column left pad. At least 1 so wrapping and cursor
/// math never divide by or index past zero on a pathologically narrow popup.
fn input_width(interior_width: u16) -> usize {
    interior_width.saturating_sub(1).max(1) as usize
}

/// The last up-to-[`MAX_INPUT_ROWS`] wrapped lines — the tail of the reply, where the cursor sits.
fn visible_input_lines(lines: &[String]) -> Vec<&str> {
    let start = lines.len().saturating_sub(MAX_INPUT_ROWS);
    lines[start..].iter().map(String::as_str).collect()
}

/// The compose strip's titled rule: `─ Reply to <label> ───`, the "Reply to <label>" bold, the
/// leading and trailing dashes dim. The label (never the words "Reply to") is ellipsis-truncated on
/// a narrow popup so the rule keeps a few trailing dashes and always fits one line.
fn reply_rule(label: &str, width: u16) -> Paragraph<'static> {
    const PREFIX: &str = "Reply to ";
    const HEAD: usize = 2; // the leading "─ "
    const GAP: usize = 1; // the space before the trailing dashes
    const MIN_TAIL: usize = 3; // keep at least a few trailing dashes

    let width = width as usize;
    let label_budget = width
        .saturating_sub(HEAD + display_width(PREFIX) + GAP + MIN_TAIL)
        .max(1);
    let title = format!("{PREFIX}{}", truncate_display(label, label_budget));
    let tail = width.saturating_sub(HEAD + display_width(&title) + GAP);
    Paragraph::new(Line::from(vec![
        "─ ".dim(),
        title.bold(),
        Span::raw(" "),
        "─".repeat(tail).dim(),
    ]))
}

/// The full send/cancel hint, degrading to a terse form before it would clip on a narrow popup.
fn reply_hint(width: u16) -> &'static str {
    const FULL: &str = "enter send · esc cancel";
    const SHORT: &str = "enter · esc";
    if (width as usize) >= display_width(FULL) {
        FULL
    } else {
        SHORT
    }
}

/// Wrap `s` into lines no wider than `width` display columns, breaking between characters (the
/// terminal line-editor idiom) so the render and the cursor share one deterministic mapping. Always
/// returns at least one line, and appends a trailing empty line when the text ends exactly on a wrap
/// boundary so the cursor has a row to sit on past a full line.
fn wrap_display(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for ch in s.chars() {
        let ch_width = char_width(ch);
        if current_width + ch_width > width {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }
    let ends_full = current_width == width && !current.is_empty();
    lines.push(current);
    if ends_full {
        lines.push(String::new());
    }
    lines
}

/// Truncate `s` to at most `max` display columns, marking a cut with a trailing `…` (which itself
/// occupies the last column). Returns `s` unchanged when it already fits.
fn truncate_display(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // reserve one column for the ellipsis
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let ch_width = char_width(ch);
        if used + ch_width > budget {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

/// Display width of a string in terminal columns (wide/CJK characters count as 2).
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of a single character in terminal columns; control characters count as 0.
fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// The footer prompt shown while a clear-all confirm is pending. Only armed on a non-empty queue,
/// so `count >= 1`; pluralized so the singular case doesn't read "1 entries".
fn confirm_prompt(count: usize) -> String {
    match count {
        1 => "clear all 1 entry? y/n".to_string(),
        n => format!("clear all {n} entries? y/n"),
    }
}

/// The one-line count shown at the top of the pane. herdr draws the pane name ("Check-in") on the
/// popup's border title, so this line carries only the live count — no redundant "Check-in —" prefix.
fn header_text(count: usize) -> String {
    match count {
        0 => "queue empty".to_string(),
        1 => "1 agent waiting".to_string(),
        n => format!("{n} agents waiting"),
    }
}

/// Geometry of a vertical scrollbar thumb: its top offset within the track and its length, both in
/// cells. Produced by [`scrollbar_thumb`], consumed by [`render_list_scrollbar`].
struct Thumb {
    top: u16,
    len: u16,
}

/// Proportional geometry for a vertical scrollbar thumb — the same shape herdr draws for its own
/// popups (thumb length scaled to the visible fraction, position scaled to the scroll offset),
/// reduced to integer math and kept colorless. Returns `None` when everything fits (no scrollbar).
/// `total` display rows, `viewport` visible rows, `offset` index of the first visible row.
fn scrollbar_thumb(
    total: usize,
    viewport: usize,
    offset: usize,
    track_height: u16,
) -> Option<Thumb> {
    if viewport == 0 || track_height == 0 || total <= viewport {
        return None;
    }
    let track = track_height as usize;
    // total > viewport here, so max_offset >= 1 and the divisions below never hit zero.
    let len = (viewport * track / total).clamp(1, track);
    let max_top = track - len;
    let max_offset = total - viewport;
    let top = if max_top == 0 {
        0
    } else {
        offset.min(max_offset) * max_top / max_offset
    };
    Some(Thumb {
        top: top as u16,
        len: len as u16,
    })
}

/// Draw a 1-column vertical scrollbar in `track` when the grouped rows overflow the viewport: a dim
/// track with a brighter thumb, both colorless to match the pane. A no-op when it all fits.
fn render_list_scrollbar(
    frame: &mut Frame,
    track: Rect,
    total: usize,
    viewport: usize,
    offset: usize,
) {
    let Some(thumb) = scrollbar_thumb(total, viewport, offset, track.height) else {
        return;
    };
    let buf = frame.buffer_mut();
    for y in track.y..track.y.saturating_add(track.height) {
        buf[(track.x, y)]
            .set_symbol("▕")
            .set_style(Style::new().dim());
    }
    let thumb_top = track.y.saturating_add(thumb.top);
    for y in thumb_top..thumb_top.saturating_add(thumb.len) {
        buf[(track.x, y)].set_symbol("▐");
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
        // display order is CHECKIN (0, 2) then DONE (1, 3), so j/k must visit 0 -> 2 -> 1 -> 3
        // — crossing the section boundary monotonically down-screen, not stepping through `entries`.
        let mut m = PaneModel::new(vec![
            entry_with_status("w0:p1", WaitStatus::Blocked),
            entry_with_status("w1:p1", WaitStatus::Done),
            entry_with_status("w2:p1", WaitStatus::Blocked),
            entry_with_status("w3:p1", WaitStatus::Done),
        ]);
        assert_eq!(m.selected, 0); // blocked0, the first CHECKIN row
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
    fn wrap_display_breaks_between_characters_at_the_width() {
        // "abcdefg" through a width of 3 breaks into 3 + 3 + 1.
        assert_eq!(wrap_display("abcdefg", 3), vec!["abc", "def", "g"]);
        // Shorter than the width stays one line.
        assert_eq!(wrap_display("hi", 5), vec!["hi"]);
    }

    #[test]
    fn wrap_display_adds_a_trailing_line_when_the_text_ends_full() {
        // "abc" exactly fills width 3, so the cursor needs an empty row past the full line.
        assert_eq!(wrap_display("abc", 3), vec!["abc", ""]);
        // "abcd" wraps to "abc" + "d" — the tail isn't full, so no extra line.
        assert_eq!(wrap_display("abcd", 3), vec!["abc", "d"]);
    }

    #[test]
    fn wrap_display_always_returns_at_least_one_line() {
        assert_eq!(wrap_display("", 5), vec![""]);
        // A zero width can't fit anything; it degrades to a single empty line, never a panic.
        assert_eq!(wrap_display("abc", 0), vec![""]);
    }

    #[test]
    fn wrap_display_counts_wide_characters_as_two_columns() {
        // Each CJK glyph is 2 columns wide, so only one fits in a width of 3 (2 + 2 > 3).
        assert_eq!(wrap_display("字字", 3), vec!["字", "字"]);
        // A trailing narrow char shares the second glyph's line (2 + 1 <= 3 is false: 2+1=3 fits).
        assert_eq!(wrap_display("字a", 3), vec!["字a", ""]);
    }

    #[test]
    fn visible_input_lines_keeps_the_last_rows() {
        let lines: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(visible_input_lines(&lines), vec!["c", "d", "e"]);
        let short: Vec<String> = ["only"].iter().map(|s| s.to_string()).collect();
        assert_eq!(visible_input_lines(&short), vec!["only"]);
    }

    #[test]
    fn truncate_display_keeps_short_labels_and_ellipsizes_long_ones() {
        assert_eq!(truncate_display("Claude", 10), "Claude");
        // Cut to 5 columns: 4 chars + the ellipsis cell.
        assert_eq!(truncate_display("claude-backend", 5), "clau…");
        assert_eq!(truncate_display("anything", 0), "");
    }

    #[test]
    fn reply_hint_degrades_on_a_narrow_popup() {
        assert_eq!(reply_hint(40), "enter send · esc cancel");
        assert_eq!(reply_hint(12), "enter · esc");
    }

    #[test]
    fn input_width_reserves_the_left_pad_and_never_underflows() {
        assert_eq!(input_width(40), 39);
        assert_eq!(input_width(1), 1);
        assert_eq!(input_width(0), 1);
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

    // All-blocked entries render as [Spacer, Header("CHECKIN"), Entry(0), Entry(1), ...] — a spacer
    // then the section header occupy the first two display rows, so entry N sits at display index N+2.
    fn blocked_rows(count: usize) -> Vec<Row> {
        let entries: Vec<QueueEntry> = (0..count).map(|i| entry(&format!("w{i}:p1"))).collect();
        layout_rows(&entries)
    }

    #[test]
    fn row_for_click_maps_rows_to_entries_past_the_header() {
        // No scroll: display row 0 (area.y, row 1) is the spacer, row 2 the CHECKIN header, so
        // row 3 (display index 2) is entry 0 and row 5 (display index 4) is entry 2.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 3), Some(0));
        assert_eq!(row_for_click(list_area(), 0, &rows, 0, 5), Some(2));
    }

    #[test]
    fn row_for_click_skips_the_spacer_and_section_header() {
        // The leading spacer (row 1) and the CHECKIN header (row 2) select nothing, like a blank row.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 1), None); // spacer
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 2), None); // header
    }

    #[test]
    fn row_for_click_accounts_for_scroll_offset() {
        // Scrolled down by 2: display row at area.y is display index 2 — Entry(0) here
        // ([Spacer, Header, Entry(0), Entry(1), ...]).
        let rows = blocked_rows(5);
        assert_eq!(row_for_click(list_area(), 2, &rows, 10, 1), Some(0));
        assert_eq!(row_for_click(list_area(), 2, &rows, 10, 3), Some(2));
    }

    #[test]
    fn row_for_click_rejects_blank_rows_below_the_last_entry() {
        // Spacer + header + 3 entries = 5 display rows; a click on display row 5 (row 6) is blank.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 6), None);
    }

    #[test]
    fn row_for_click_rejects_clicks_outside_the_area() {
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 0), None); // above the list
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 11), None); // below the list
        assert_eq!(row_for_click(list_area(), 0, &rows, 40, 3), None); // one past the right edge (entry 0 row)
    }

    #[test]
    fn row_for_click_is_safe_on_an_empty_queue() {
        let rows = blocked_rows(0);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 1), None);
    }

    #[test]
    fn layout_rows_groups_by_status_fifo_without_reordering_entries() {
        // A queue interleaving blocked and done: the layout emits a spacer + CHECKIN (blocked, FIFO)
        // then a spacer + DONE (done, FIFO), each Entry keeping its original index into `entries`.
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
                Row::Spacer => "~".to_string(),
                Row::Header(title) => format!("#{title}"),
                Row::Entry(index) => format!("{index}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec!["~", "#CHECKIN", "0", "2", "~", "#DONE", "1", "3"],
            "each section is spacer + header + its entries, FIFO within, indices unchanged"
        );
    }

    #[test]
    fn layout_rows_omits_an_empty_section() {
        // All done: no CHECKIN spacer/heading — the DONE section (spacer + header) leads.
        let entries = vec![
            entry_with_status("w0:p1", WaitStatus::Done),
            entry_with_status("w1:p1", WaitStatus::Done),
        ];
        let rows = layout_rows(&entries);
        assert!(matches!(rows.first(), Some(Row::Spacer)));
        assert!(
            matches!(rows.get(1), Some(Row::Header(DONE_HEADER))),
            "an all-done queue leads with the DONE section, not CHECKIN"
        );
        assert_eq!(rows.len(), 4, "spacer + header + two entries");
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
        // All blocked -> [Spacer, Header, Entry(0), Entry(1), Entry(2)] at terminal rows 1..=5. A
        // click on row 5 lands on the third entry (index 2), past the spacer + header.
        let mut m = model(&["w1:p1", "w2:p1", "w3:p1"]);
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 5),
            Some(list_area()),
            0,
        );
        assert_eq!(m.selected, 2);
    }

    #[test]
    fn on_mouse_left_click_on_a_header_row_selects_nothing() {
        // The CHECKIN header sits at terminal row 2 (row 1 is the spacer); clicking it is a no-op.
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.move_down(); // selection at 1
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 2),
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
            mouse(MouseEventKind::ScrollDown, 5, 3),
            Some(list_area()),
            0,
        );
        assert_eq!(m.selected, 1);
        // A left click on a blank row (past the spacer + header + 2 entries) is a no-op.
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
        assert_eq!(header_text(0), "queue empty");
        assert_eq!(header_text(1), "1 agent waiting");
        assert_eq!(header_text(3), "3 agents waiting");
    }

    #[test]
    fn scrollbar_thumb_is_none_when_everything_fits() {
        // Content shorter than or equal to the viewport needs no scrollbar.
        assert!(scrollbar_thumb(5, 10, 0, 10).is_none());
        assert!(scrollbar_thumb(10, 10, 0, 10).is_none());
        // Degenerate tracks/viewports are also a no-op (never divide by zero).
        assert!(scrollbar_thumb(20, 0, 0, 10).is_none());
        assert!(scrollbar_thumb(20, 10, 0, 0).is_none());
    }

    #[test]
    fn scrollbar_thumb_scales_length_and_slides_with_the_offset() {
        // 20 rows through a 10-row viewport: the thumb spans half the track (10 * 10 / 20 = 5),
        // and its top slides from 0 (top) through the middle to max_top (bottom) as we scroll.
        let at = |offset| scrollbar_thumb(20, 10, offset, 10).expect("overflow => thumb");
        let top = at(0);
        assert_eq!((top.top, top.len), (0, 5), "scrolled to the top");
        let mid = at(5);
        assert_eq!((mid.top, mid.len), (2, 5), "5 * (10-5) / (20-10) = 2");
        let bottom = at(10);
        assert_eq!(
            (bottom.top, bottom.len),
            (5, 5),
            "max offset pins the thumb to the bottom"
        );
        // An offset past the max is clamped, never overruns the track.
        let over = at(99);
        assert_eq!(
            over.top + over.len,
            10,
            "thumb bottom never exceeds the track height"
        );
    }

    #[test]
    fn scrollbar_thumb_length_is_at_least_one_cell() {
        // A huge list through a tiny viewport still shows a visible (>= 1 cell) thumb.
        let thumb = scrollbar_thumb(1_000, 1, 0, 8).expect("overflow => thumb");
        assert!(thumb.len >= 1, "the thumb is never zero-height");
        assert!(thumb.top + thumb.len <= 8, "and stays within the track");
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
    fn on_enter_closes_on_a_successful_jump_and_evicts() {
        let dir = temp_state_dir("enter-ok");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "needs input");
        let mut model = PaneModel::new(load(&dir));
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked")]);

        let close = on_enter(&mut model, &runtime(dir.clone(), 2_000), &herdr);

        assert!(close, "a successful jump signals the popup to close");
        assert_eq!(herdr.focused.into_inner(), vec!["w1:p1".to_string()]);
        assert!(
            load(&dir).is_empty(),
            "the jumped entry is evicted on success"
        );
    }

    #[test]
    fn on_enter_keeps_the_pane_open_when_the_focus_fails() {
        let dir = temp_state_dir("enter-fail");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "needs input");
        let mut model = PaneModel::new(load(&dir));
        let herdr = FakeHerdr::new(&[("w1:p1", "blocked")]).with_failing_focus();

        let close = on_enter(&mut model, &runtime(dir.clone(), 2_000), &herdr);

        assert!(!close, "a failed jump keeps the pane open");
        assert_eq!(
            load(&dir).len(),
            1,
            "a failed jump keeps the entry (invariant #2)"
        );
        assert!(model
            .status
            .as_deref()
            .is_some_and(|s| s.contains("focus failed")));
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
