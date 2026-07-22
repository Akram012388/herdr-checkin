# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
