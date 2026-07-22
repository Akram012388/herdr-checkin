# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-07-22

The triage-popup release: the status pane becomes an agents-view console you can **reply into**,
rendered as a centered popup modal. Bundles the previously-unreleased pane features (clear-all,
mouse-select, module split) with the popup, inline reply, and grouped render as one interface
release.

### Added
- **Triage popup — the status pane is now an agents-view console.** `open-pane` opens the pane as a
  centered, session-modal popup (herdr `--placement popup`) — the same class of floating modal as
  herdr's own `prefix+s` settings — sized via `width`/`height` percentages and drawn by herdr with a
  border and a "Check-in" title. Enqueued waiters are grouped into **CHECKIN** (`blocked`) and
  **DONE** (`done`) sections, oldest-first within each, each preceded by a blank spacer line so the
  groups read as distinct blocks. It stays an inbox of what pinged you — only enqueued waiters ever
  appear, never a live roster of all agents. Section headers are non-selectable; `j`/`k` and click
  move in on-screen order across the sections, and selection stays anchored to its entry as the
  queue changes. A popup is a session-level singleton, so the old open/focus/close toggle is gone:
  `q`/`Esc` dismisses it, and a successful `Enter` jump closes it too (the pane calls the
  `popup.close` socket method on exit).
- **Inline reply (`space`).** Reply to the selected waiter without leaving the pane: `space` opens a
  compose strip, you type an answer, and `Enter` routes it into that agent's session via
  `herdr agent prompt <pane_id>`, then drops the entry. Fire-and-forget — the entry leaves the queue
  the instant the send is accepted (reply *is* acknowledgment of the debt); if that agent finishes
  again it re-enqueues at the tail as a fresh waiter. A failed send **keeps** the entry (act, then
  evict on success only — the same discipline as `Enter`/jump); `Esc` cancels; an empty/whitespace
  reply sends nothing and stays in reply mode. The reply target is captured when reply mode is armed,
  so a concurrent queue refresh can't retarget it. Composing darkens the queue as one veil so the
  strip is the only lit surface (and the answered agent keeps a soft grey band so it stays obvious
  which one you're replying to): a titled `Reply to <label>` rule, a single-line input with full
  cursor editing (see Changed), and a right-aligned `enter send · esc cancel` hint — colorless,
  matching herdr's restrained modal look.
- **Scrollbar when the queue overflows the popup.** When the grouped rows exceed the visible height,
  a 1-column scrollbar (dim track, brighter thumb) appears at the right edge so off-screen waiters
  are discoverable; the list already scrolls to keep the selection in view.
- **In-pane `c` = clear-all**, with a confirm. Pressing `c` in the status pane (on a non-empty
  queue) arms a `clear all N entries? y/n` prompt in the footer; `y`/`Y` empties the queue, any
  other key cancels. Reuses the existing `clear` path, so the wipe is a delta under the state lock,
  never a full write-back. No-op on an empty queue, like `d`/`Enter`.
- **Mouse click-to-select** in the status pane. A left click selects the clicked row, exactly like
  `j`/`k` landing on it (other mouse events are ignored); a click on a section header selects
  nothing, and a click while a clear-all confirm or a reply is pending cancels it rather than
  reselecting. Mouse capture is enabled/disabled by hand around the TUI (ratatui's init/restore don't
  touch it) on every exit path, including a chained panic hook so a panic can't leave the shell
  emitting mouse escapes.

### Changed
- **Rows are now location-first, two lines each — mirroring herdr's own `prefix+g` go-to.** A row's
  location used to be just the raw `workspace_id` (e.g. `w1`). Each waiter now renders as a bright
  **destination** line — `{workspace} · {tab} · {pane}` — over a dim **detail** line —
  `{status} · {title} · {waited}` — so *where* the agent is reads first, the way a navigation list
  should. Each segment prefers its human name and falls back to the positional id: workspace label
  (else `workspace_id`), tab label (else `t{N}`), pane manual label (else `pane {N}`). The event
  payload carries none of the names, so the enqueue path resolves them from `pane list` +
  `workspace list` + `tab list` (best-effort — a lookup failure just leaves that segment on its id,
  never dropping the ping); the `startup` re-seed resolves them the same way. New `QueueEntry` fields
  (`tab_id`, `workspace_label`, `tab_label`, `pane_label`) are `serde(default)`, so old `state.json`
  loads unchanged and fills the names in on the next refresh.
- **Reply input rebuilt on `tui-textarea` — real cursor editing.** The compose bar was an
  append/pop buffer with the cursor pinned at the end; it is now a single-line text field with full
  terminal editing: `ctrl+a`/`ctrl+e` (line start/end), `←`/`→`/word-jumps (mid-line cursor),
  `ctrl+w` (delete word), `ctrl+k` (delete to end), correct on wide/combining characters. `Enter`
  still submits and `Esc` cancels (both intercepted before the widget, so it stays single-line). It
  scrolls horizontally instead of wrapping across rows. **Bracketed paste** is now enabled and a
  paste is inserted as one edit with newlines/tabs flattened to spaces — closing a footgun where a
  pasted newline used to fire a half-written reply into the agent.
- **Softer, more legible highlights.** The selection is a soft grey band
  (`Color::DarkGray` background) instead of full reversed video, which read as a harsh white bar on a
  dark theme; the same band marks the agent being replied to while composing. The footer hint bar is
  centered so its margins stay balanced. The `type your reply` placeholder is a faint neutral color
  and no longer italic.
- **`src/lib.rs` split into cohesive modules** (`state`, `herdr`, `queue`, `actions`, `pane`, and a
  test-only `test_support`), each carrying its own tests; `lib.rs` is now the argv-dispatch
  orientation page that re-wires the pieces. The queue transitions no longer depend on the herdr seam
  (enforced by the module boundary, not just a comment). No behavior change.

### Fixed
- **`ctrl+u` in the reply bar now clears the line to the left of the cursor** (readline
  "unix-line-discard"). tui-textarea 0.7 binds `ctrl+u` to *undo* and puts delete-to-line-start on
  `ctrl+j`; the reply bar now intercepts `ctrl+u` and drives the delete itself, matching the
  convention every other cursor key in the bar already follows.

### Docs
- README now documents the popup-modal agents-view console, the grouped **CHECKIN** / **DONE**
  sections, and the `space` inline-reply key, with a refreshed animated demo (`docs/pane-demo.gif`)
  — still regenerable offline with no real agents via `scripts/pane-demo.tape` +
  `scripts/pane-demo-setup.sh` (VHS).

## [0.3.0] - 2026-07-22

### Added
- **Startup queue re-seed** — a `[[startup]]` hook (herdr 0.7.5) that runs once per server process
  (cold start and live-handoff takeover) and re-seeds the queue by scanning `herdr pane list` for
  panes already `blocked`/`done`. Without it, a herdr restart starts the event subscription fresh
  and silently drops the pings for agents that were already waiting. Seeded entries carry the full
  agent/title/workspace fields, identical to event-enqueued ones.

### Fixed
- **Lost-ping race in `next`/`peek`.** A persisted queue entry that a concurrent event refreshed
  during the pre-lock `pane list` snapshot window could be pruned as stale on its original
  `enqueued_at_ms`, dropping a live waiter. Entries now track `last_touched_ms` (bumped on every
  re-enqueue) and the prune guard keeps any entry with
  `max(enqueued_at_ms, last_touched_ms) >= snapshot`. Old `state.json` files without the field load
  unchanged (it defaults to `0`). This race is most reachable in the post-restart window the
  startup hook targets.

### Notes
- The startup seed is additive-only and merges each waiter through the same per-pane upsert the
  event path uses (a delta under the state lock), so it never clobbers a concurrent enqueue and is
  a no-op if it runs twice. Stale-entry pruning is left to the existing `next`/`peek` liveness pass.

## [0.2.0] - 2026-07-22

### Added
- **Status pane** — a persistent, keyboard-driven TUI (ratatui) that lists the live queue in a
  split, as a richer alternative to the `peek` toast. Keys: `j`/`k` (or arrows) to move, `Enter`
  to jump to the selected agent and drop it, `d` to drop without jumping, `q`/`Esc` to close. It
  re-reads the shared queue on a 250ms tick, so the list and waiting-times stay live.
- New `pane` binary subcommand, a `[[panes]]` manifest entry, and an `open-pane` action that
  summons the pane as a split.
- **Idempotent `open-pane` toggle**, scoped to the current tab: opens the pane if absent,
  focuses it if it exists but isn't focused, closes it if it is the focused pane. Backed by a new
  unit-tested `pane-decision` subcommand (reads `pane list`, emits `OPEN`/`FOCUS`/`CLOSE`) and a
  `scripts/open-pane.sh` launcher; degrades to open on any error, and validates target pane ids
  are flag-safe.

### Notes
- `Enter` focuses first and only drops the entry on a successful jump; a failed jump keeps the
  entry and surfaces the error in the footer.
- Pane mutations (`Enter`, `d`) go through the same lockfile-guarded state store as the event
  handlers, so concurrent enqueues are never clobbered.

## [0.1.0] - 2026-07-22

### Added
- Initial release: a durable FIFO attention queue for agent panes.
- Enqueue on `pane.agent_status_changed` (`blocked`/`done`); evict on return to `working`, on
  `pane.focused`, and on `pane.closed`.
- Actions: `next` (jump to the oldest waiter and pop it), `peek` (toast listing the queue),
  `clear`.
- State persisted to `state.json` under the plugin state directory, guarded by a lockfile.
