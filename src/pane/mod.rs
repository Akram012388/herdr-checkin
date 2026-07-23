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
//!
//! This module is the **shell**: terminal setup, the event loop and tick, the pure [`PaneModel`],
//! and the top-level [`draw`] layout. The two render surfaces live in sibling modules — the durable
//! queue in [`queue_view`], the inline-reply compose strip in [`compose`] — so the coming Agents
//! view slots in as a third sibling without growing the loop.

mod agents_view;
mod compose;
mod queue_view;

use crate::herdr::{sample_roster, CliHerdr, LabelCache};
use crate::roster::{agents_in_display_order, roster_reply_label, RosterAgent, RosterSnapshot};
use crate::{
    agent_label, clear, current_unix_ms, evict, load_entries, Herdr, PluginError, QueueEntry,
    RuntimeEnv, StateStore,
};
use compose::{dim_area, draw_compose};
use queue_view::{confirm_prompt, draw_list, header_text, layout_rows, row_for_click, Row};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tui_textarea::{CursorMove, TextArea};

/// How long the input poll blocks each tick before we re-read the queue and repaint, so queue
/// changes and the ticking "waited" column appear promptly without busy-spinning.
const TICK: Duration = Duration::from_millis(250);

/// How often the roster sampler thread polls `herdr agent list`. The render tick (`TICK`) only
/// drains the delivered snapshots, so a status flip surfaces within about one cadence plus one tick.
const ROSTER_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How long the pane blocks once at open, waiting for the sampler's first snapshot, so the Agents
/// view paints rows with the popup instead of a "Sampling agents..." placeholder that lingers a
/// render tick (or ~1s under load). Bounded: a slow or dead sampler delays the popup no longer than
/// this, then the normal async path takes over (see [`RosterSampler::recv_latest_within`]).
const FIRST_SAMPLE_WAIT: Duration = Duration::from_millis(200);

const FOOTER_HINTS: &str =
    "j/k move  ·  enter jump  ·  space reply  ·  d drop  ·  c clear  ·  q quit";

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
    if let Err(error) = execute!(std::io::stdout(), EnableBracketedPaste) {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        return Err(PluginError::new(format!(
            "failed to enable bracketed paste: {error}"
        )));
    }
    install_mouse_panic_hook();

    let result = event_loop(&mut terminal, &mut model, runtime, herdr);

    // Disable capture/paste before restore, and on every non-panic exit path (event_loop's `?`
    // errors land here too). Best-effort: if this fails we're already tearing down.
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
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

    // The Agents view is fed by a background thread polling `herdr agent list` (never the render
    // tick — invariant that the durable Queue view can't jank). It ships snapshots over an mpsc the
    // tick drains without blocking; it is joined on drop, so every exit path (including a `?` below)
    // tears it down cleanly.
    let sampler = RosterSampler::spawn(runtime.herdr_bin_path.clone());

    // Frame 1: paint the popup shell immediately (it appears instantly, never hostage to a slow
    // `agent list`). Then, when the Agents view is what's showing, block briefly for the sampler's
    // immediate first snapshot (it samples once on spawn, ~20ms) so rows appear almost at once
    // instead of after a render-tick round-trip. Bounded by `FIRST_SAMPLE_WAIT`; on the Queue tab the
    // roster is off-screen, so skip the wait and go straight to input handling.
    {
        let mut list_area = None;
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    model,
                    current_unix_ms(),
                    &mut list_state,
                    &mut list_area,
                )
            })
            .map_err(|error| PluginError::new(format!("failed to draw: {error}")))?;
    }
    if model.tab == ActiveTab::Agents {
        if let Some(snapshot) = sampler.recv_latest_within(FIRST_SAMPLE_WAIT) {
            model.apply_roster(snapshot);
        }
    }

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
                            // Enter (and ctrl+m) submit — never insert a newline (single-line input).
                            KeyCode::Enter => on_reply_submit(model, runtime, herdr),
                            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                on_reply_submit(model, runtime, herdr)
                            }
                            // ctrl+u clears the line to the left of the cursor (readline). We remap
                            // it explicitly: tui-textarea 0.7 binds ctrl+u to `undo` and puts
                            // delete-to-line-start on ctrl+j, not the convention the maintainer expects.
                            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                model.reply_delete_to_line_start()
                            }
                            // Up/Down walk the soft-wrapped rows of the single logical line rather
                            // than feeding the TextArea (which has only one line to move within).
                            KeyCode::Up => model.reply_cursor_vertical(false),
                            KeyCode::Down => model.reply_cursor_vertical(true),
                            _ => model.reply_input(key),
                        }
                    } else if model.confirm_clear {
                        on_confirm_clear(model, runtime, key.code);
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                return Ok(());
                            }
                            // Tab / Ctrl+S toggle Queue <-> Agents in either view. Purely a view
                            // switch: it never touches the popup lifecycle (invariant #5) — `Enter`
                            // from either view is the one shared close path.
                            KeyCode::Tab => model.toggle_tab(),
                            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                model.toggle_tab()
                            }
                            // Selection, reply, and jump work in BOTH views — each dispatches on the
                            // active tab (queue entry vs roster agent). `Enter` is the one shared
                            // close path: a successful jump closes the popup (run() fires popup.close).
                            KeyCode::Char('j') | KeyCode::Down => model.move_down(),
                            KeyCode::Char('k') | KeyCode::Up => model.move_up(),
                            KeyCode::Char(' ') => model.begin_reply(),
                            KeyCode::Enter => {
                                if on_enter(model, runtime, herdr) {
                                    return Ok(());
                                }
                            }
                            // Drop and clear are durable-queue operations — Queue view only.
                            _ if model.tab == ActiveTab::Queue => match key.code {
                                KeyCode::Char('d') | KeyCode::Char('x') => on_drop(model, runtime),
                                KeyCode::Char('c') => model.request_clear(),
                                _ => {}
                            },
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
                        // A left click selects a row in whichever view is active; `on_mouse`
                        // dispatches on the tab and is safe when there's nothing to click.
                        on_mouse(model, mouse, list_area, list_state.offset());
                    }
                }
                // A terminal-native paste (bracketed paste, enabled in `run`) is only meaningful
                // while composing a reply; ignored otherwise.
                Event::Paste(text) if model.reply.is_some() => {
                    model.reply_paste(&text);
                }
                _ => {}
            }
        }

        // Drain any roster snapshots the sampler delivered since the last tick, keeping the newest.
        // Non-blocking (`try_recv`): the tick never waits on the CLI, and a tick with no new sample
        // leaves the cached roster in place so a row never blanks (design §5).
        if let Some(snapshot) = sampler.drain_latest() {
            model.apply_roster(snapshot);
        }

        // Refresh from the shared file every tick. Parse-and-compare (no mtime guard: mtime has
        // ~1s granularity and would miss two writes in the same second); the file is tiny.
        let fresh = load_entries(&runtime.state_dir);
        if fresh != model.entries {
            model.sync(fresh);
        }
    }
}

/// `Enter`: jump to the focused agent in whichever view is active — one shared close path. Both
/// variants act-then-evict-on-success (invariant #2): focus first, and only on success evict the
/// pane from the durable queue. Returns `true` when the jump succeeded, signaling the pane should
/// close (as a popup it floats over the whole session, so once navigated it must get out of the way).
fn on_enter(model: &mut PaneModel, runtime: &RuntimeEnv, herdr: &dyn Herdr) -> bool {
    let target = match model.tab {
        ActiveTab::Queue => model.selected_pane_id().map(str::to_owned),
        ActiveTab::Agents => model.roster_selected_agent().map(|a| a.pane_id.clone()),
    };
    let Some(pane_id) = target else {
        return false;
    };
    match herdr.focus_agent(&pane_id) {
        Ok(()) => {
            // The jump worked; if the drop can't be persisted, say so rather than imply success.
            // Evicting is idempotent — a no-op when the agent (an Agents-view row) wasn't queued.
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
        .is_some_and(|draft| !draft.input.lines().join(" ").trim().is_empty());
    if !has_text {
        return;
    }
    // `take` leaves reply mode now; the target/label were captured when it was armed.
    let draft = model.reply.take().expect("reply draft present");
    let joined = draft.input.lines().join(" "); // single line in practice; join guards against any stray newline
    let text = joined.trim();

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
    let Some(area) = list_area else {
        return;
    };
    // Recompute the same grouped layout `draw` painted this frame (entries/roster are unchanged
    // between draw and this click — the next sync/drain runs only after event handling), so the
    // hit-test reproduces exactly what was on screen. A click on a header/spacer selects nothing.
    match model.tab {
        ActiveTab::Queue => {
            let rows = layout_rows(&model.entries);
            if let Some(index) = row_for_click(area, offset, &rows, mouse.column, mouse.row) {
                model.selected = index;
            }
        }
        ActiveTab::Agents => {
            let index = {
                let agents = model.roster_display_agents();
                let rows = agents_view::layout_rows(&agents);
                agents_view::row_for_click(area, offset, &rows, mouse.column, mouse.row)
            };
            if let Some(index) = index {
                model.roster_selected = index;
            }
        }
    }
}

/// Remove a pane from the queue as a delta under the lock (never a full model write-back).
fn evict_pane(runtime: &RuntimeEnv, pane_id: &str) -> Result<(), PluginError> {
    StateStore::new(&runtime.state_dir).update(|mut entries| {
        evict(&mut entries, pane_id);
        (entries, ())
    })
}

// --- roster sampler (the Agents view's live feed) --------------------------

/// A background thread that polls `herdr agent list` on a fixed cadence and ships each
/// [`RosterSnapshot`] to the render tick over an mpsc. It exists so the CLI never runs on the render
/// tick — the durable Queue view can't be janked by a slow `agent list`, and a dropped sample just
/// leaves the last roster on screen.
///
/// Owned by [`event_loop`]; dropping it stops and joins the thread (see the [`Drop`] impl), so every
/// pane-exit path tears the worker down deterministically rather than detaching it.
struct RosterSampler {
    /// Newest-wins queue of snapshots from the worker; [`drain_latest`](Self::drain_latest) empties
    /// it each tick without blocking.
    rx: Receiver<RosterSnapshot>,
    /// Signals the worker to stop: dropping it disconnects the worker's `recv_timeout`, waking it out
    /// of its sleep immediately. `Option` so [`Drop`] can drop it *before* the join.
    stop_tx: Option<Sender<()>>,
    /// The worker handle, joined on drop. `Option` so [`Drop`] can `take` it out to join by value.
    handle: Option<JoinHandle<()>>,
}

impl RosterSampler {
    /// Spawn the sampler over a fresh [`CliHerdr`] built from `bin_path` (the worker owns its own
    /// herdr handle — the borrowed `&dyn Herdr` the pane runs on is neither `Send` nor `'static`).
    /// Samples once immediately so the Agents view has data on its first open, then every
    /// [`ROSTER_SAMPLE_INTERVAL`].
    fn spawn(bin_path: PathBuf) -> Self {
        let (snapshot_tx, rx) = mpsc::channel::<RosterSnapshot>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let handle = thread::Builder::new()
            .name("checkin-roster-sampler".to_string())
            .spawn(move || sampler_loop(&CliHerdr { bin_path }, &snapshot_tx, &stop_rx))
            .expect("spawning the roster sampler thread should not fail");
        Self {
            rx,
            stop_tx: Some(stop_tx),
            handle: Some(handle),
        }
    }

    /// Drain every snapshot delivered since the last call and return the most recent, or `None` if
    /// the worker delivered nothing (keep showing the cached roster). Never blocks.
    fn drain_latest(&self) -> Option<RosterSnapshot> {
        let mut latest = None;
        while let Ok(snapshot) = self.rx.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }

    /// Block up to `bound` for the first snapshot, then sweep up any already queued behind it
    /// (newest wins). Returns `None` on timeout or if the worker is gone — the caller keeps the
    /// placeholder and the async path recovers. Used once at open to seed the Agents view promptly
    /// without ever running the CLI on the render thread (invariant: the worker still samples).
    fn recv_latest_within(&self, bound: Duration) -> Option<RosterSnapshot> {
        match self.rx.recv_timeout(bound) {
            Ok(first) => {
                let mut latest = first;
                while let Ok(snapshot) = self.rx.try_recv() {
                    latest = snapshot;
                }
                Some(latest)
            }
            // Timeout (no sample yet) or Disconnected (worker died at spawn): keep the placeholder.
            Err(_) => None,
        }
    }
}

impl Drop for RosterSampler {
    fn drop(&mut self) {
        // Drop the stop sender first so the worker's `recv_timeout` returns `Disconnected` at once,
        // then join — so shutdown waits on at most an in-flight `agent list`, not the full cadence.
        drop(self.stop_tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The sampler thread body: sample, deliver, then sleep on `stop_rx` for one cadence. Sampling on an
/// interruptible `recv_timeout` (rather than `thread::sleep`) means a stop wakes it immediately.
/// Exits when the shell drops the receiver (the pane is closing) or signals stop; an `agent list`
/// error is skipped so a transient failure never blanks the view — the next sample recovers.
///
/// The thread owns a [`LabelCache`], so the steady-state sample is a single `agent list` spawn: the
/// workspace/tab/pane label maps are refetched only when a new agent pane appears or on a slow
/// periodic refresh (see [`sample_roster`]), not every second.
fn sampler_loop(herdr: &CliHerdr, snapshot_tx: &Sender<RosterSnapshot>, stop_rx: &Receiver<()>) {
    let mut labels = LabelCache::default();
    loop {
        if let Ok(agents) = sample_roster(herdr, &mut labels) {
            let snapshot = RosterSnapshot {
                sampled_at_ms: current_unix_ms(),
                agents,
            };
            // The receiver is gone -> the pane is closing; stop.
            if snapshot_tx.send(snapshot).is_err() {
                return;
            }
        }
        match stop_rx.recv_timeout(ROSTER_SAMPLE_INTERVAL) {
            // Told to stop, or the shell dropped its stop sender: exit.
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            // The cadence elapsed with no stop: take the next sample.
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

// --- model (pure) ----------------------------------------------------------

/// Which view the popup is showing. `Tab`/`Ctrl+S` flip between them: the durable [`Queue`] is the
/// attention inbox, the read-only [`Agents`] roster is the live view the sampler feeds. A fresh
/// popup opens on [`Queue`] when there's a waiter to show, else on [`Agents`] — an empty inbox
/// shouldn't greet the user with "you're all caught up" when there's a live roster to see instead.
///
/// [`Queue`]: ActiveTab::Queue
/// [`Agents`]: ActiveTab::Agents
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTab {
    Queue,
    Agents,
}

/// An in-progress inline reply. The target `pane_id` and its display `label` are captured when
/// reply mode is armed — not read from the live selection at submit time — so a concurrent queue
/// `sync` that reorders or evicts entries while the user is still typing can never retarget the
/// reply to a different agent. `input` is the single-line editor holding the text composed so far.
struct ReplyDraft {
    target: String,
    label: String,
    input: TextArea<'static>,
    /// The width the input last rendered at, recorded by `draw_compose` each frame. The Up/Down
    /// handlers read it so they wrap the logical line exactly as the render did — otherwise the
    /// caret could jump to a wrapped row that isn't on screen.
    wrap_width: Cell<u16>,
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
    /// Which view is showing. Each view keeps its own selection across a toggle, so flipping to the
    /// Agents view and back is lossless.
    tab: ActiveTab,
    /// The latest roster the sampler delivered, or `None` until its first sample lands. Replaced
    /// wholesale each delivery (re-anchoring [`roster_selected`](Self::roster_selected) by pane id)
    /// and never persisted (design §5) — the Agents view renders it, the Queue view ignores it.
    roster: Option<RosterSnapshot>,
    /// The Agents view's selection cursor: an index into the display-order agent list
    /// (`roster::agents_in_display_order`), independent of the Queue's `selected`.
    roster_selected: usize,
}

impl PaneModel {
    fn new(entries: Vec<QueueEntry>) -> Self {
        // One-time decision at construction only: an empty queue opens on Agents instead of Queue
        // (see the ActiveTab doc comment). This never re-evaluates as the queue empties/fills later
        // — that would yank the user's tab around — `tab` is otherwise only changed by `toggle_tab`.
        let tab = if entries.is_empty() {
            ActiveTab::Agents
        } else {
            ActiveTab::Queue
        };
        Self {
            entries,
            selected: 0,
            status: None,
            confirm_clear: false,
            reply: None,
            tab,
            roster: None,
            roster_selected: 0,
        }
    }

    /// Replace the roster with a fresh sample, keeping the Agents-view selection anchored to the same
    /// pane where possible (so the 1s refresh never yanks the cursor), else clamping it into range.
    /// The roster is never persisted (design §5) — this is the Agents-view twin of [`sync`](Self::sync).
    fn apply_roster(&mut self, snapshot: RosterSnapshot) {
        let anchor = self
            .roster_selected_agent()
            .map(|agent| agent.pane_id.clone());
        self.roster = Some(snapshot);
        let (len, anchored) = {
            let order = self.roster_display_agents();
            let anchored = anchor.and_then(|id| order.iter().position(|a| a.pane_id == id));
            (order.len(), anchored)
        };
        self.roster_selected = match anchored {
            Some(position) => position,
            None => self.roster_selected.min(len.saturating_sub(1)),
        };
    }

    /// The Agents-view agents in on-screen (grouped-by-workspace) order — the sequence `j`/`k`,
    /// clicks, and the render all index into. Empty until the first sample lands.
    fn roster_display_agents(&self) -> Vec<&RosterAgent> {
        match &self.roster {
            Some(snapshot) => agents_in_display_order(&snapshot.agents),
            None => Vec::new(),
        }
    }

    /// The agent the Agents-view cursor is on, or `None` when the roster is empty.
    fn roster_selected_agent(&self) -> Option<&RosterAgent> {
        self.roster_display_agents()
            .get(self.roster_selected)
            .copied()
    }

    /// `Tab`/`Ctrl+S`: switch between the Queue and Agents views. A pure view flip — it leaves the
    /// queue's selection, any armed reply, and a pending clear-confirm untouched, and never touches
    /// the popup lifecycle (invariant #5).
    fn toggle_tab(&mut self) {
        self.tab = match self.tab {
            ActiveTab::Queue => ActiveTab::Agents,
            ActiveTab::Agents => ActiveTab::Queue,
        };
    }

    /// `c`: arm the clear-all confirm, but only when there's something to clear (a no-op on an
    /// empty queue, like `d`/`Enter`).
    fn request_clear(&mut self) {
        if !self.entries.is_empty() {
            self.confirm_clear = true;
        }
    }

    /// `space`: begin an inline reply to the selected agent in whichever view is active — the Queue's
    /// selected entry or the Agents view's selected roster agent. No-op when nothing is selected, so
    /// the shared compose target (a `pane_id` + display `label`) is captured now (see [`ReplyDraft`]),
    /// never re-read from the live selection at submit time.
    fn begin_reply(&mut self) {
        let target = match self.tab {
            ActiveTab::Queue => self
                .entries
                .get(self.selected)
                .map(|entry| (entry.pane_id.clone(), agent_label(entry).to_string())),
            ActiveTab::Agents => self
                .roster_selected_agent()
                .map(|agent| (agent.pane_id.clone(), roster_reply_label(agent))),
        };
        if let Some((target, label)) = target {
            self.arm_reply(target, label);
        }
    }

    /// Arm reply mode against an explicit target, the shared path both views funnel through. No-op
    /// while a clear-all confirm is pending, so the two modals never overlap.
    fn arm_reply(&mut self, target: String, label: String) {
        if self.confirm_clear {
            return;
        }
        // The `TextArea` is only the text model now — `compose::draw_input` paints the soft-wrapped
        // rows, placeholder, and block caret itself, so the widget's own render styling is unused.
        let input = TextArea::default();
        self.reply = Some(ReplyDraft {
            target,
            label,
            input,
            wrap_width: Cell::new(0),
        });
    }

    /// Feed a key event to the reply input (no-op outside reply mode). The TextArea's default
    /// bindings handle cursor movement, word/line deletion, etc.
    fn reply_input(&mut self, key: KeyEvent) {
        if let Some(draft) = self.reply.as_mut() {
            draft.input.input(key);
        }
    }

    /// Up/Down in reply mode: move the caret one wrapped display row within the single logical line,
    /// keeping its visual column. Wraps at the width `draw_input` last rendered at (recorded on the
    /// draft) so navigation matches the screen exactly. No-op before the first render or outside
    /// reply mode.
    fn reply_cursor_vertical(&mut self, down: bool) {
        if let Some(draft) = self.reply.as_mut() {
            let width = draft.wrap_width.get() as usize;
            if width == 0 {
                return;
            }
            let line = draft.input.lines()[0].clone();
            let (_, col) = draft.input.cursor();
            let target = compose::cursor_move_vertical(&line, width, col, down);
            draft.input.move_cursor(CursorMove::Jump(0, target as u16));
        }
    }

    /// `ctrl+u`: delete from the cursor to the start of the line (readline "unix-line-discard").
    /// tui-textarea 0.7 binds `ctrl+u` to `undo` and puts delete-to-head on `ctrl+j`, so we drive
    /// the delete ourselves rather than relying on its default bindings. No-op outside reply mode.
    fn reply_delete_to_line_start(&mut self) {
        if let Some(draft) = self.reply.as_mut() {
            draft.input.delete_line_by_head();
        }
    }

    /// Insert pasted text as one edit, flattened to a single line (newlines/tabs/other control
    /// chars -> a space) so a multi-line paste can't inject a newline the agent's own input would
    /// read as Enter, nor break the single-line strip. No-op outside reply mode.
    fn reply_paste(&mut self, text: &str) {
        if let Some(draft) = self.reply.as_mut() {
            let flat: String = text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            draft.input.insert_str(flat);
        }
    }

    /// Leave reply mode, discarding the draft.
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
                Row::Header(_) | Row::Spacer | Row::Detail(_) => None,
            })
            .collect()
    }

    /// `k`/up: move the active view's cursor one row up the screen. Dispatches on the tab — the Queue
    /// steps through its grouped display order, the Agents view down its display-order agent list.
    fn move_up(&mut self) {
        match self.tab {
            ActiveTab::Queue => {
                let order = self.display_order();
                if let Some(pos) = order.iter().position(|&index| index == self.selected) {
                    if pos > 0 {
                        self.selected = order[pos - 1];
                    }
                }
            }
            ActiveTab::Agents => {
                self.roster_selected = self.roster_selected.saturating_sub(1);
            }
        }
    }

    /// `j`/down: move the active view's cursor one row down the screen (see [`move_up`](Self::move_up)).
    fn move_down(&mut self) {
        match self.tab {
            ActiveTab::Queue => {
                let order = self.display_order();
                if let Some(pos) = order.iter().position(|&index| index == self.selected) {
                    if pos + 1 < order.len() {
                        self.selected = order[pos + 1];
                    }
                }
            }
            ActiveTab::Agents => {
                let len = self.roster_display_agents().len();
                if len > 0 && self.roster_selected + 1 < len {
                    self.roster_selected += 1;
                }
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

/// Paint the active view. Dispatches on the tab so the Queue render stays exactly as it was (the
/// durable view is unaffected by the Agents-view work); the Agents view is a sibling render module.
fn draw(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
) {
    match model.tab {
        ActiveTab::Queue => draw_queue(frame, model, now_ms, list_state, list_area),
        ActiveTab::Agents => agents_view::draw_agents(frame, model, list_state, list_area),
    }
}

/// The tab bar shown as the top row of both views: the two tabs side by side (the active one carrying
/// the same soft grey band the selected row uses, bold and undimmed; the inactive one dim), followed
/// by a dim `tab · switch` tooltip. It is the consistent, always-visible affordance for the
/// `Tab`/`Ctrl+S` toggle — identical on both surfaces.
fn draw_tab_bar(frame: &mut Frame, area: Rect, active: ActiveTab) {
    use ratatui::text::{Line, Span};

    let band = Style::new().bold().bg(queue_view::SELECTION_BG);
    let idle = Style::new().dim();
    let (queue_style, agents_style) = match active {
        ActiveTab::Queue => (band, idle),
        ActiveTab::Agents => (idle, band),
    };
    let line = Line::from(vec![
        Span::styled(" Queue ", queue_style),
        Span::raw("   "),
        Span::styled(" Agents ", agents_style),
        Span::styled("     tab · switch", idle),
    ]);
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), area);
}

fn draw_queue(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
) {
    let interior = frame.area();

    // In reply mode the single footer line expands into a compose strip docked below the list: a
    // titled rule, a soft-wrapping input (`compose::INPUT_ROWS` rows; one logical line wrapped for
    // display), and a hint row.
    let compose = model.reply.as_ref();

    let areas = match &compose {
        Some(_) => Layout::vertical([
            Constraint::Length(1),                   // tab bar
            Constraint::Length(1),                   // count header
            Constraint::Min(0),                      // the queue (dimmed while composing)
            Constraint::Length(1),                   // titled rule
            Constraint::Length(compose::INPUT_ROWS), // input
            Constraint::Length(1),                   // hint
        ])
        .split(interior),
        None => Layout::vertical([
            Constraint::Length(1), // tab bar
            Constraint::Length(1), // count header
            Constraint::Min(0),    // the queue
            Constraint::Length(1), // footer
        ])
        .split(interior),
    };

    draw_tab_bar(frame, areas[0], ActiveTab::Queue);

    frame.render_widget(
        Paragraph::new(header_text(model.entries.len())).bold(),
        areas[1],
    );

    if model.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("No agents waiting — you're all caught up.")
                .dim()
                .alignment(Alignment::Center),
            areas[2],
        );
    } else {
        draw_list(
            frame, model, now_ms, list_state, list_area, areas[2], compose,
        );
    }

    match compose {
        // Composing: darken the tab bar + header + queue as one veil so the strip is the only lit
        // surface (focus by receding everything else, not by brightening the input), then draw it.
        Some(draft) => {
            let veil = Rect {
                height: areas[0].height + areas[1].height + areas[2].height,
                ..areas[0]
            };
            dim_area(frame, veil);
            draw_compose(frame, draft, areas[3], areas[4], areas[5]);
        }
        // Navigating: the one-line footer carries the clear-confirm, a transient status, or hints.
        None => {
            let footer = if model.confirm_clear {
                confirm_prompt(model.entries.len())
            } else {
                model.status.as_deref().unwrap_or(FOOTER_HINTS).to_string()
            };
            // Centered so the left/right margins stay balanced regardless of the hint string's width.
            frame.render_widget(
                Paragraph::new(footer).dim().alignment(Alignment::Center),
                areas[3],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WaitStatus;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn entry(pane_id: &str) -> QueueEntry {
        entry_with_status(pane_id, WaitStatus::Blocked)
    }

    fn entry_with_status(pane_id: &str, status: WaitStatus) -> QueueEntry {
        QueueEntry {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("").to_string(),
            tab_id: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
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
    fn new_opens_on_agents_when_the_queue_is_empty_else_on_queue() {
        assert_eq!(
            model(&[]).tab,
            ActiveTab::Agents,
            "an empty queue shouldn't greet the user with an empty inbox"
        );
        assert_eq!(
            model(&["w1:p1"]).tab,
            ActiveTab::Queue,
            "a nonempty queue still opens on the attention inbox"
        );
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
    fn begin_reply_captures_the_selected_target_and_label() {
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.move_down(); // select w2:p1
        m.begin_reply();
        let draft = m.reply.as_ref().expect("reply should be armed");
        assert_eq!(draft.target, "w2:p1");
        assert_eq!(draft.label, "Claude"); // the entry's display_agent
        assert_eq!(draft.input.lines(), [String::new()]); // a fresh TextArea has one empty line
    }

    #[test]
    fn begin_reply_is_a_noop_on_an_empty_queue() {
        let mut m = model(&[]);
        // An empty queue now defaults to the Agents tab; force Queue so this still exercises the
        // queue-tab's begin_reply no-op, as intended.
        m.tab = ActiveTab::Queue;
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

    // A plain, unmodified character key, for feeding `reply_input` in tests.
    fn char_key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    #[test]
    fn reply_input_edits_the_textarea() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        m.reply_input(char_key('h'));
        m.reply_input(char_key('i'));
        assert_eq!(m.reply.as_ref().unwrap().input.lines(), ["hi"]);
        m.reply_input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(m.reply.as_ref().unwrap().input.lines(), ["h"]);
    }

    #[test]
    fn reply_delete_to_line_start_clears_left_of_cursor() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        for ch in "hello".chars() {
            m.reply_input(char_key(ch));
        }
        // Park the cursor between "hel" and "lo" (two lefts from the end).
        m.reply_input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        m.reply_input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        m.reply_delete_to_line_start();
        // Only the left of the cursor is gone; the right ("lo") survives.
        assert_eq!(m.reply.as_ref().unwrap().input.lines(), ["lo"]);
    }

    #[test]
    fn reply_input_is_a_noop_outside_reply_mode() {
        let mut m = model(&["w1:p1"]);
        m.reply_input(char_key('x'));
        assert!(m.reply.is_none());
    }

    #[test]
    fn reply_paste_flattens_newlines_and_tabs_into_a_single_line() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        m.reply_paste("line1\nline2\tx");
        assert_eq!(
            m.reply.as_ref().unwrap().input.lines().join("\n"),
            "line1 line2 x"
        );
    }

    #[test]
    fn cancel_reply_discards_the_draft() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        m.reply_input(char_key('x'));
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

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    // The list rect used across the mouse tests: starts at row 1 (row 0 is the top header line),
    // 10 rows tall, 40 wide — matching what `draw` records for a queue this size.
    fn list_area() -> Rect {
        Rect {
            x: 0,
            y: 1,
            width: 40,
            height: 10,
        }
    }

    #[test]
    fn on_mouse_left_click_selects_the_clicked_row() {
        // All blocked -> [Spacer, Header, E0, D0, E1, D1, E2, D2] at terminal rows 1..=8. The third
        // entry's primary is row 7 and its detail is row 8; a click on either selects index 2.
        let mut m = model(&["w1:p1", "w2:p1", "w3:p1"]);
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 7),
            Some(list_area()),
            0,
        );
        assert_eq!(
            m.selected, 2,
            "clicking the entry's primary line selects it"
        );

        m.selected = 0;
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 8),
            Some(list_area()),
            0,
        );
        assert_eq!(
            m.selected, 2,
            "clicking the entry's detail line selects it too"
        );
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
        // A left click on a blank row (past spacer + header + 2 entries × 2 rows = terminal row 6)
        // is a no-op; row 7 is the first blank line.
        on_mouse(
            &mut m,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 7),
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

    // --- render snapshots (TestBackend) ------------------------------------
    //
    // These lock the pane's rendered *content* and vertical layout in CI with no herdr: they draw
    // the real `draw` into an off-screen `TestBackend` and compare the text of each row. Horizontal
    // styling (centering, the grey selection band, dim/bold) is deliberately not asserted here —
    // it stays under live tuning (HANDOFF §6) and the maintainer confirms it at the terminal — so
    // `content_lines` trims each row to its text. One extra test locks the `> ` selection marker.

    fn render_buffer(model: &PaneModel, width: u16, height: u16) -> Buffer {
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).expect("test terminal builds");
        let mut list_state = ListState::default();
        let mut list_area = None;
        terminal
            .draw(|frame| draw(frame, model, 1_000, &mut list_state, &mut list_area))
            .expect("draw succeeds");
        terminal.backend().buffer().clone()
    }

    // The text of each rendered row, trimmed of the leading/trailing padding the widgets add
    // (center/right alignment, the list's highlight-symbol column). Blank rows become "".
    fn content_lines(buffer: &Buffer) -> Vec<String> {
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                let mut line = String::new();
                for x in 0..area.width {
                    line.push_str(buffer[(x, y)].symbol());
                }
                line.trim().to_string()
            })
            .collect()
    }

    #[test]
    fn snapshot_empty_queue_shows_the_caught_up_message() {
        let mut m = model(&[]);
        // An empty queue now defaults to the Agents tab; force Queue so this still renders (and
        // tests) the queue's own empty-state message.
        m.tab = ActiveTab::Queue;
        assert_eq!(
            content_lines(&render_buffer(&m, 80, 6)),
            vec![
                "Queue     Agents      tab · switch", // the tab bar tops both views (active tab banded)
                "queue empty",
                "No agents waiting — you're all caught up.",
                "",
                "",
                "j/k move  ·  enter jump  ·  space reply  ·  d drop  ·  c clear  ·  q quit",
            ]
        );
    }

    #[test]
    fn snapshot_queue_groups_checkin_then_done_sections() {
        let m = PaneModel::new(vec![
            entry_with_status("w1:p1", WaitStatus::Blocked),
            entry_with_status("w2:p1", WaitStatus::Blocked),
            entry_with_status("w3:p1", WaitStatus::Done),
        ]);
        assert_eq!(
            content_lines(&render_buffer(&m, 80, 13)),
            vec![
                "Queue     Agents      tab · switch",
                "3 agents waiting",
                "",
                "CHECKIN",
                "> w1 · pane 1", // the selection cursor sits on the first entry
                "blocked · t · just now",
                "w2 · pane 1",
                "blocked · t · just now",
                "",
                "DONE",
                "w3 · pane 1",
                "done · t · just now",
                "j/k move  ·  enter jump  ·  space reply  ·  d drop  ·  c clear  ·  q quit",
            ]
        );
    }

    #[test]
    fn snapshot_compose_strip_renders_the_rule_input_and_hint() {
        let mut m = model(&["w1:p1"]);
        m.begin_reply();
        m.reply.as_mut().unwrap().input.insert_str("hi");
        // The input is now `compose::INPUT_ROWS` tall (a single logical line wrapped for display), so
        // "hi" sits on the first input row with two blank rows beneath it before the hint.
        assert_eq!(
            content_lines(&render_buffer(&m, 80, 11)),
            vec![
                "Queue     Agents      tab · switch".to_string(),
                "1 agent waiting".to_string(),
                "".to_string(),
                "CHECKIN".to_string(),
                "> w1 · pane 1".to_string(), // the reply target keeps the cursor while composing
                "blocked · t · just now".to_string(),
                format!("─ Reply to Claude {}", "─".repeat(62)),
                "hi".to_string(),
                "".to_string(),
                "".to_string(),
                "enter send · esc cancel".to_string(),
            ]
        );
    }

    #[test]
    fn snapshot_marks_the_selected_entry_with_the_cursor() {
        // Rows: [tab bar][count][spacer][CHECKIN][E0][D0][E1][D1]... The selection starts on entry 0,
        // so its destination row (terminal row 4) carries the "> " marker while the unselected
        // entry's row (row 6) does not. Both are one row lower than before the tab bar was added.
        let m = model(&["w1:p1", "w2:p1"]);
        let buffer = render_buffer(&m, 80, 10);
        assert_eq!(
            buffer[(0, 4)].symbol(),
            ">",
            "selected entry gets the cursor"
        );
        assert_eq!(
            buffer[(0, 6)].symbol(),
            " ",
            "the unselected entry has no marker"
        );
    }

    // --- Agents view: tab toggle + roster render ---------------------------

    use crate::roster::{AgentStatus, RosterAgent, RosterSnapshot};

    fn roster_agent(pane_id: &str, workspace_id: &str, status: AgentStatus) -> RosterAgent {
        RosterAgent {
            pane_id: pane_id.to_string(),
            workspace_id: workspace_id.to_string(),
            tab_id: Some(format!("{workspace_id}:t1")),
            agent: Some("claude".to_string()),
            agent_status: status,
            agent_session: Some("uuid-1".to_string()),
            cwd: Some("/tmp".to_string()),
            focused: false,
            terminal_title: Some("herdr-checkin".to_string()),
            workspace_label: None,
            tab_label: None,
            pane_label: None,
        }
    }

    #[test]
    fn toggle_tab_flips_queue_and_agents_and_is_lossless() {
        let mut m = model(&["w1:p1", "w2:p1"]);
        m.move_down(); // selection at 1
        assert_eq!(m.tab, ActiveTab::Queue);
        m.toggle_tab();
        assert_eq!(m.tab, ActiveTab::Agents);
        m.toggle_tab();
        assert_eq!(m.tab, ActiveTab::Queue);
        assert_eq!(
            m.selected, 1,
            "the queue selection survives a round-trip toggle"
        );
    }

    fn agents_model(ids: &[(&str, &str)]) -> PaneModel {
        let mut m = model(&[]);
        m.tab = ActiveTab::Agents;
        m.roster = Some(RosterSnapshot {
            sampled_at_ms: 1_000,
            agents: ids
                .iter()
                .map(|(pane, ws)| roster_agent(pane, ws, AgentStatus::Working))
                .collect(),
        });
        m
    }

    #[test]
    fn agents_move_down_and_up_clamp_at_the_ends() {
        let mut m = agents_model(&[("w4:p1", "w4"), ("w4:p2", "w4"), ("wN:p1", "wN")]);
        assert_eq!(m.roster_selected, 0);
        m.move_up(); // already at top
        assert_eq!(m.roster_selected, 0);
        m.move_down();
        m.move_down();
        assert_eq!(m.roster_selected, 2);
        m.move_down(); // already at the last agent
        assert_eq!(m.roster_selected, 2);
        m.move_up();
        assert_eq!(m.roster_selected, 1);
    }

    #[test]
    fn agents_selection_follows_workspace_grouping_not_snapshot_order() {
        // Snapshot interleaves workspaces; the cursor steps through grouped display order
        // (w4's agents contiguously, then wN), so move_down lands on w4:p2, not the snapshot's next.
        let mut m = agents_model(&[("w4:p1", "w4"), ("wN:p1", "wN"), ("w4:p2", "w4")]);
        m.move_down();
        assert_eq!(
            m.roster_selected_agent().map(|a| a.pane_id.as_str()),
            Some("w4:p2"),
            "grouped order keeps w4 together"
        );
    }

    #[test]
    fn apply_roster_keeps_the_agents_selection_on_the_same_pane() {
        let mut m = agents_model(&[("w4:p1", "w4"), ("w4:p2", "w4"), ("wN:p1", "wN")]);
        m.move_down(); // select w4:p2
        assert_eq!(
            m.roster_selected_agent().map(|a| a.pane_id.as_str()),
            Some("w4:p2")
        );
        // A fresh sample drops w4:p1; the selection should follow w4:p2 to its new index.
        m.apply_roster(RosterSnapshot {
            sampled_at_ms: 2_000,
            agents: vec![
                roster_agent("w4:p2", "w4", AgentStatus::Blocked),
                roster_agent("wN:p1", "wN", AgentStatus::Working),
            ],
        });
        assert_eq!(m.roster_selected, 0);
        assert_eq!(
            m.roster_selected_agent().map(|a| a.pane_id.as_str()),
            Some("w4:p2")
        );
    }

    #[test]
    fn apply_roster_clamps_when_the_selected_agent_vanishes() {
        let mut m = agents_model(&[("w4:p1", "w4"), ("w4:p2", "w4"), ("wN:p1", "wN")]);
        m.move_down();
        m.move_down(); // select wN:p1 (index 2)
        m.apply_roster(RosterSnapshot {
            sampled_at_ms: 2_000,
            agents: vec![roster_agent("w4:p1", "w4", AgentStatus::Working)],
        });
        assert_eq!(m.roster_selected, 0, "clamped into the shrunken roster");
        assert_eq!(
            m.roster_selected_agent().map(|a| a.pane_id.as_str()),
            Some("w4:p1")
        );
    }

    #[test]
    fn begin_reply_in_the_agents_view_targets_the_selected_roster_agent() {
        let mut m = agents_model(&[("w4:p1", "w4"), ("wN:p2", "wN")]);
        m.move_down(); // select wN:p2
        m.begin_reply();
        let draft = m.reply.as_ref().expect("reply armed from the agents view");
        assert_eq!(draft.target, "wN:p2");
        assert_eq!(draft.label, "Claude", "the roster agent name, capitalized");
    }

    #[test]
    fn snapshot_agents_view_groups_rows_by_workspace() {
        let mut m = model(&[]);
        m.tab = ActiveTab::Agents;
        m.roster = Some(RosterSnapshot {
            sampled_at_ms: 1_000,
            agents: vec![
                roster_agent("w4:p1", "w4", AgentStatus::Idle),
                roster_agent("w4:p2", "w4", AgentStatus::Blocked),
                roster_agent("wN:p1", "wN", AgentStatus::Working),
            ],
        });
        assert_eq!(
            content_lines(&render_buffer(&m, 80, 13)),
            vec![
                "Queue     Agents      tab · switch", // the tab bar, Agents active
                "3 agents",
                "",
                "w4",            // workspace group header
                "> t1 · pane 1", // the selection cursor sits on the first agent
                "idle · herdr-checkin",
                "t1 · pane 2",
                "blocked · herdr-checkin",
                "",
                "wN",
                "t1 · pane 1",
                "working · herdr-checkin",
                "j/k move  ·  enter jump  ·  space reply  ·  q quit",
            ]
        );
    }

    #[test]
    fn snapshot_agents_view_shows_the_sampling_placeholder_before_the_first_sample() {
        // `roster` is None until the sampler delivers: the view says it is sampling, not "no agents".
        let mut m = model(&[]);
        m.tab = ActiveTab::Agents;
        assert_eq!(
            content_lines(&render_buffer(&m, 80, 5)),
            vec![
                "Queue     Agents      tab · switch",
                "no agents",
                "Sampling agents...",
                "",
                "j/k move  ·  enter jump  ·  space reply  ·  q quit",
            ]
        );
    }

    #[test]
    fn snapshot_agents_view_distinguishes_an_empty_roster_from_no_sample_yet() {
        // A delivered-but-empty snapshot is a real "herdr reports no agents" reading, worded apart
        // from the pre-first-sample placeholder above.
        let mut m = model(&[]);
        m.tab = ActiveTab::Agents;
        m.roster = Some(RosterSnapshot {
            sampled_at_ms: 1_000,
            agents: Vec::new(),
        });
        assert_eq!(
            content_lines(&render_buffer(&m, 80, 5)),
            vec![
                "Queue     Agents      tab · switch",
                "no agents",
                "No agents running.",
                "",
                "j/k move  ·  enter jump  ·  space reply  ·  q quit",
            ]
        );
    }

    #[test]
    fn the_toggle_switches_which_view_renders() {
        // The same model renders the Queue, then the Agents view, purely off `tab` — proving the
        // toggle picks the surface with no other state change.
        let mut m = model(&["w1:p1"]);
        m.roster = Some(RosterSnapshot {
            sampled_at_ms: 1_000,
            agents: vec![roster_agent("w9:p1", "w9", AgentStatus::Done)],
        });
        // Row 0 is the shared tab bar on both views; the count header is row 1.
        let queue = content_lines(&render_buffer(&m, 80, 7));
        assert_eq!(queue[0], "Queue     Agents      tab · switch");
        assert_eq!(
            queue[1], "1 agent waiting",
            "Queue tab shows the queue header"
        );

        m.toggle_tab();
        let agents = content_lines(&render_buffer(&m, 80, 7));
        assert_eq!(agents[0], "Queue     Agents      tab · switch");
        assert_eq!(agents[1], "1 agent", "Agents tab shows the roster header");
        assert!(
            agents.iter().any(|line| line == "w9"),
            "and the roster's workspace group, not the queue"
        );
    }

    // --- reply submit (impure: reads/writes state.json, talks to a fake herdr) ---------------

    use crate::test_support::{feed_status, load, runtime, temp_state_dir, FakeHerdr};

    // Seed one blocked waiter and return a model over it, already in reply mode with `text` typed.
    fn armed_reply(dir: &std::path::Path, text: &str) -> PaneModel {
        feed_status(dir, 1_000, "w1:p1", "w1", "blocked", "needs input");
        let mut model = PaneModel::new(load(dir));
        model.begin_reply();
        model.reply.as_mut().unwrap().input.insert_str(text);
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
    fn on_enter_in_the_agents_view_focuses_the_selected_agent_and_closes() {
        // A blocked agent is both queued and in the roster. Jumping from the Agents view focuses it
        // (via the shared on_enter dispatch) and evicts it from the durable queue, same as the Queue.
        let dir = temp_state_dir("agents-enter-ok");
        feed_status(&dir, 1_000, "wN:p2", "wN", "blocked", "needs input");
        let mut model = PaneModel::new(load(&dir));
        model.tab = ActiveTab::Agents;
        model.roster = Some(RosterSnapshot {
            sampled_at_ms: 1_000,
            agents: vec![roster_agent("wN:p2", "wN", AgentStatus::Blocked)],
        });
        let herdr = FakeHerdr::new(&[("wN:p2", "blocked")]);

        let close = on_enter(&mut model, &runtime(dir.clone(), 2_000), &herdr);

        assert!(close, "a successful jump signals the popup to close");
        assert_eq!(herdr.focused.into_inner(), vec!["wN:p2".to_string()]);
        assert!(
            load(&dir).is_empty(),
            "the jumped agent is evicted from the queue on success"
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
