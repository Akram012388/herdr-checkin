# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- **`roster.json` no longer grows without bound.** The time-in-state registry had no removal path:
  `pane.closed` evicts the durable queue but never touched the registry, so closed-pane entries
  accumulated forever and every event stamp paid a full-file parse and rewrite that scaled with
  them. The `startup` seed's existing locked update now also sweeps entries for departed panes —
  panes absent from the live `pane list` whose last observation predates the startup snapshot —
  capping the registry at the live roster plus panes closed since the last server start. Live
  entries are never touched (seeding stays additive and idempotent), entries recorded at or after
  the snapshot are kept as too new to judge (the same guard `next`/`peek` use), and the sweep
  removes only observation-cache data, never a queued ping.

## [0.4.0] - 2026-07-22

The triage-popup release: the status pane becomes a two-tab attention console you can **reply into**,
rendered as a centered popup modal. A durable Queue sits beside a live Agents roster, combining the
previously-unreleased pane features (clear-all, mouse-select, module split) with the popup, inline
reply, and grouped render as one interface release.

### Added
- **Triage popup — the status pane is now an agents-view console.** `open-pane` opens the pane as a
  centered, session-modal popup (herdr `--placement popup`) — the same class of floating modal as
  herdr's own `prefix+s` settings — sized via `width`/`height` percentages and drawn by herdr with a
  border and a "Check-in" title. Enqueued waiters are grouped into **CHECKIN** (`blocked`) and
  **DONE** (`done`) sections, oldest-first within each, each preceded by a blank spacer line so the
  groups read as distinct blocks. This durable inbox is the **Queue** tab; the adjacent **Agents**
  tab shows every running agent. Section headers are non-selectable; `j`/`k` and click move in
  on-screen order across the sections, and selection stays anchored to its entry as the queue
  changes. A popup is a session-level singleton, so the old open/focus/close toggle is gone:
  `q`/`Esc` dismisses it, and a successful `Enter` jump closes it too (the pane calls the
  `popup.close` socket method on exit).
- **Live Agents roster beside the durable Queue.** `Tab` or `Ctrl+S` switches between two persistent
  tabs in the same popup. Agents are grouped by workspace; each primary row identifies the agent
  type plus its human tab and pane destination, omitting a tab label only when it repeats the agent
  type. The popup opens on Agents when the Queue is empty and on Queue when waiters exist; each tab
  preserves its own selection. The roster refreshes in the background without running herdr CLI
  calls on the render tick, and selection remains anchored by `pane_id` across refreshes. `j`/`k`,
  arrows, and click select an agent; `space` replies and `Enter` jumps through the same act-first,
  evict-on-success handlers as the Queue. Queue-only mutations (`d` and `c`) remain unavailable on
  the roster.
- **Live status context for every agent.** Agent rows include time in the current state and the last
  meaningful terminal output line. Transition times are event-stamped in a separate, prunable
  `roster.json` observation cache; unknown times are shown honestly as `~`, and deleting the cache
  cannot lose a queued ping. Terminal tails are sampled on the worker with a bounded, status-first
  round-robin budget and skip rendered agent UI chrome; read failures retain the last known line and
  the terminal title remains the fallback.
- **Inline reply (`space`).** Reply to the selected agent without leaving the pane: `space` opens a
  compose strip from either tab, you type an answer, and `Enter` routes it into that agent's session
  via `herdr agent prompt <pane_id>`, then drops any queued entry. Fire-and-forget — the entry leaves
  the queue the instant the send is accepted (reply *is* acknowledgment of the debt); if that agent
  finishes again it re-enqueues at the tail as a fresh waiter. A failed send **keeps** the entry
  (act, then evict on success only — the same discipline as `Enter`/jump); `Esc` cancels; an empty/whitespace
  reply sends nothing and stays in reply mode. The reply target is captured when reply mode is armed,
  so a concurrent queue refresh can't retarget it. Composing darkens the queue as one veil so the
  strip is the only lit surface (and the answered agent keeps a dim theme-derived band so it stays
  obvious which one you're replying to): a titled `Reply to <label>` rule, a single-line input with
  full cursor editing (see Changed), and a right-aligned `enter send · esc cancel` hint,
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
  soft-wraps across three display rows, with `Up`/`Down` moving between wrapped rows while preserving
  the visual column; this does not add multiline send semantics. **Bracketed paste** is now enabled
  and a paste is inserted as one edit with newlines/tabs flattened to spaces — closing a footgun
  where a pasted newline used to fire a half-written reply into the agent.
- **Agents view now paints with the popup and samples cheaply.** On an Agents-first open, the pane
  waits at most 200ms for the worker's immediate first snapshot before entering the event loop, so
  rows appear with the popup instead of a tick later. Human-name maps are cached on the sampler
  thread and refreshed when membership changes or approximately every 15 seconds, reducing the
  steady-state roster path from four herdr subprocesses per second to one.
- **Popup styling now inherits Herdr's resolved theme.** Herdr snapshots its effective pane
  presentation palette when launching the plugin, and Check-in maps those existing semantic tokens
  onto the entire popup interior: panel and input surfaces, primary/secondary text, dim selection
  bands, accent active tabs, placeholders, hints, compose rule, confirmation, and scrollbar. Reset, ANSI,
  indexed, and RGB colors stay lossless, so the `terminal` theme continues to defer to the user's
  terminal palette while built-in light/dark and custom themes match Herdr exactly. The snapshot is
  parsed once before terminal initialization and never queried on a render tick. The first producing
  build is the `0.7.5-akram.1` downstream candidate; stock Herdr 0.7.5 remains supported and retains
  the popup's established terminal-native styling because it does not provide the snapshot.
- **`src/lib.rs` split into cohesive modules** (`state`, `herdr`, `queue`, `actions`, `pane`, and a
  test-only `test_support`), each carrying its own tests; `lib.rs` is now the argv-dispatch
  orientation page that re-wires the pieces. The queue transitions no longer depend on the herdr seam
  (enforced by the module boundary, not just a comment). No behavior change.

### Fixed
- **Agents rows now show the last settled output above the input prompt.** Codex's non-bare
  composer prompt and pinned model/context footer are treated as a structural boundary rather than
  terminal output, while Claude Code and amp chrome filtering remains intact. Terminal-tail text now
  uses the readable secondary-text palette role without an extra dim modifier. Status and
  time-in-state now sit beside the pane on the first row, leaving the second row's full width for
  terminal context. The popup's first sample now immediately enriches blocked, done, and idle agents
  with their settled terminal context instead of making every row wait for the one-second periodic
  sweep; a baseline roster still paints within the existing bound if a terminal read is slow.
- **Valid alphabetic Herdr pane IDs no longer leak into rows.** Herdr encodes stable public
  allocation numbers with a bijective base-32 alphabet, so `pA` means pane allocation 10. Queue and
  Agents rows now render `pane 10` instead of exposing `wT:pA` or silently omitting the pane. Unknown
  future ID syntax degrades to neutral `pane`; raw IDs remain internal focus/reply targets.
- **`ctrl+u` in the reply bar now clears the line to the left of the cursor** (readline
  "unix-line-discard"). tui-textarea 0.7 binds `ctrl+u` to *undo* and puts delete-to-line-start on
  `ctrl+j`; the reply bar now intercepts `ctrl+u` and drives the delete itself, matching the
  convention every other cursor key in the bar already follows.

### Docs
- README now documents the popup-modal console, its Queue and Agents tabs, the grouped **CHECKIN** /
  **DONE** queue sections, and the `space` inline-reply key. The animated demo remains regenerable
  offline with no real agents via `scripts/pane-demo.tape` + `scripts/pane-demo-setup.sh` (VHS).

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
