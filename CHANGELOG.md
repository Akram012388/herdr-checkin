# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-07-22

### Added
- **Status pane** — a persistent, keyboard-driven TUI (ratatui) that lists the live queue in a
  split, as a richer alternative to the `peek` toast. Keys: `j`/`k` (or arrows) to move, `Enter`
  to jump to the selected agent and drop it, `d` to drop without jumping, `q`/`Esc` to close. It
  re-reads the shared queue on a 250ms tick, so the list and waiting-times stay live.
- New `pane` binary subcommand, a `[[panes]]` manifest entry, and an `open-pane` action that
  summons the pane as a split.

### Notes
- `Enter` focuses first and only drops the entry on a successful jump; a failed jump keeps the
  entry and surfaces the error in the footer.
- Pane mutations (`Enter`, `d`) go through the same lockfile-guarded state store as the event
  handlers, so concurrent enqueues are never clobbered.
- Opening the pane again opens a second split rather than toggling; both share one queue safely.
  An idempotent open-or-focus toggle is planned.

## [0.1.0] - 2026-07-22

### Added
- Initial release: a durable FIFO attention queue for agent panes.
- Enqueue on `pane.agent_status_changed` (`blocked`/`done`); evict on return to `working`, on
  `pane.focused`, and on `pane.closed`.
- Actions: `next` (jump to the oldest waiter and pop it), `peek` (toast listing the queue),
  `clear`.
- State persisted to `state.json` under the plugin state directory, guarded by a lockfile.
