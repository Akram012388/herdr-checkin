# HANDOFF

Orientation for the next session (human or agent). Read this first, then start on §6 (Next up).
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md).

**Version:** 0.3.0 · **License:** MIT · **Repo:** https://github.com/Akram012388/herdr-checkin
· **State:** `main` is green (fmt + clippy + test) and pushed at the 0.3.0 release. No open branches,
no worktrees. 0.3.0 is **not tagged** (the maintainer tags on request).

---

## 1. What this is

A herdr plugin: a **durable FIFO attention queue** for agent panes. herdr's native
jump-to-notification only reaches the toast currently on screen, so a ping is lost once the toast
fades, and simultaneous pings can't queue. This plugin remembers them — agents that go `blocked`
(need input) or `done` (finished) are enqueued; you jump to the oldest waiter on demand.

- **Manifest id:** `Akram012388.checkin` (GitHub-handle prefix). **Repo/dir name:** `herdr-checkin`
  (the `herdr-` prefix is what ecosystem discovery expects). These deliberately differ — do NOT
  rename the dir to the id.
- **Target:** herdr >= 0.7.0; developed and verified against **herdr 0.7.5, protocol 17**.

## 2. Architecture

Two execution modes share one on-disk queue:

1. **Short-lived per-event / per-action binaries.** herdr spawns one process per event/action.
   Subcommands: `status-changed`, `focused`, `closed` (events); `next`, `peek`, `clear` (actions);
   `startup` (the [[startup]] hook). They mutate `state.json` and exit.
2. **A long-running TUI pane** (`pane` subcommand, ratatui + crossterm). herdr spawns it into a
   split via a `[[panes]]` manifest entry. It has no push channel for events, so it **polls**
   `state.json` on a 250 ms tick. A `pane-decision` subcommand (reads `pane list` on stdin, prints
   `OPEN`/`FOCUS <id>`/`CLOSE <id>`) backs the idempotent open/focus/close launcher.

**State:** `state.json` under `HERDR_PLUGIN_STATE_DIR`, an ordered `Vec<QueueEntry>`
(`{pane_id, workspace_id, agent, display_agent, title, status, enqueued_at_ms, last_touched_ms}`),
guarded by `state.lock` (`fs2`). Writes are atomic temp+rename; reads outside a mutation take no lock.

**Files:**
- `src/lib.rs` (~1.4k lines) — argv dispatch, queue transitions, `StateStore` (lock + atomic write),
  herdr CLI seam (`Herdr` trait / `CliHerdr`), event/pane-list parsing, toast copy, all unit tests.
- `src/pane.rs` (~500 lines) — the ratatui TUI (`PaneModel`, event loop, view) and the
  `pane-decision` toggle logic. Pure model/decision code is unit-tested; the terminal loop is thin.
- `src/main.rs` — one-line entry into `lib::run_from_env`.
- `tests/cli.rs` — end-to-end tests that spawn the built binary against a fake `herdr` on
  `HERDR_BIN_PATH`.
- `herdr-plugin.toml` — manifest: `[[actions]]`, `[[events]]`, one `[[panes]]`, `[[build]]`,
  `[[startup]]`.
- `scripts/open-pane.sh` — idempotent launcher for `open-pane`.

## 3. Behavior + load-bearing invariants

**Behavior:**
- **Enqueue** on `agent_status` `blocked`/`done`; **evict** on return to `working`, on
  `pane.focused`, and on `pane.closed`. FIFO oldest-first; **deduplicated per pane** (a re-ping
  updates fields in place, keeping the original position + `enqueued_at_ms`).
- **`next`** focuses the oldest still-live waiter (`herdr agent focus <pane_id>`, cross-workspace)
  and evicts it **only after** the focus succeeds. **`peek`** shows the queue as a toast.
  **`clear`** empties it. **`startup`** re-seeds the queue from `pane list` after a herdr restart.
- **Status pane** keys: `j`/`k`/arrows move, `Enter` jump+evict-on-success, `d` drop, `q`/`Esc`
  close. `open-pane` is a current-tab-scoped toggle (open / focus / close).

**Invariants (do not regress — each has a regression test):**
1. **Mutations are deltas** through `StateStore::update` (read-modify-write under the lock), never a
   full model write-back. The pane polls while event binaries write concurrently; a stale write-back
   would clobber a fresh enqueue.
2. **Focus first, evict on success only** (`next` and pane `Enter`). A failed jump keeps the entry —
   losing it is the exact failure the plugin exists to prevent.
3. **Never prune an entry the liveness snapshot couldn't see.** `next`/`peek` take the `pane list`
   snapshot before the lock; keep any entry with `max(enqueued_at_ms, last_touched_ms) >= snapshot`.
   `enqueued_at_ms` is the FIFO age; `last_touched_ms` is bumped by every `enqueue` upsert. The
   `max` closes a lost-ping race: a persisted entry that a concurrent event *refreshes* during the
   snapshot→lock window would otherwise be pruned on its old `enqueued_at_ms`.
4. **`startup` is additive-only.** It merges each `blocked`/`done` pane through the same `enqueue`
   upsert events use (a delta under the lock) — never a wholesale `state.json` rewrite, and it never
   evicts. Stale entries are pruned by `next`/`peek`'s liveness pass. The hook is spawned async and
   races the live event loop, so this merge-not-rewrite discipline is what keeps it safe.
5. **`decide` assumes one globally-focused pane** (verified: herdr reports a single `focused:true`
   across all workspaces). The status pane is identified in `pane list` only by its `label`
   ("Check-in" — the `[[panes]]` title; keep `PANE_LABEL` in sync).

## 4. herdr API facts (0.7.5, protocol 17)

- **Event JSON** (in `HERDR_PLUGIN_EVENT_JSON`): `{event, data:{type, pane_id, workspace_id,
  agent_status, agent, display_agent, title}}` — underscore forms. Manifest `on =` uses the dotted
  form (`pane.agent_status_changed`, `pane.focused`, `pane.closed`). Fields also accepted at the top
  level if `data` is absent.
- **Focus an agent pane:** `herdr agent focus <pane_id>` (jumps workspace/tab/pane). The CLI
  `herdr pane focus` is *directional* only; there is no by-id `pane.focus` CLI.
- **Pane info:** `herdr pane list` → `result.panes[]` of `PaneInfo`. Fields we use: `pane_id`,
  `workspace_id`, `agent_status`, `focused`, plus optional `agent`, `display_agent`, `title` — the
  same fields an event carries, so a scan seeds full-fidelity entries. Verified against
  `herdr api schema --json` (`success_response.$defs.PaneInfo`).
- **`[[startup]]` hook** (used by the `startup` subcommand): manifest is an array-of-tables with only
  `command` (required argv) + optional `platforms` — no `id`/`on`. Fires **once per server process**
  (cold start and live-handoff takeover), not per session/enable. One-shot run-and-exit. Receives
  the normal plugin env plus `HERDR_PLUGIN_EVENT=startup`; **no pane payload** — the hook calls
  `pane list` itself. Spawned **async and not awaited**, so it races the live event loop (see
  invariant #4). Failure is logged (`plugin log list`) and does not stop the server.
- **Plugin pane:** declared via `[[panes]]`; opened/focused/closed with
  `herdr plugin pane open --plugin <id> --entrypoint <pane-id> --placement split --focus` /
  `plugin pane focus <PANE_ID>` / `plugin pane close <PANE_ID>`. No push events to a running pane
  (`herdr api` only has `snapshot`/`schema`) → poll.
- **Env a pane/handler receives:** `HERDR_PLUGIN_STATE_DIR`, `HERDR_BIN_PATH`,
  `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_ID`.
  **Gotcha:** the id is percent-encoded in the state-dir path (`%41kram012388.checkin`). Always use
  the `HERDR_PLUGIN_STATE_DIR` env var — never construct the path.
- **Toast:** `herdr notification show <title> [--body B] --sound none|request|done`.

## 5. Dev loop

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # what CI runs
cargo build --release
herdr plugin link "$PWD"                                   # register the local build
herdr plugin action invoke <next|peek|clear|open-pane> --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin        # inspect event/action/startup runs
```

Real events need a real agent pane going blocked/done (manual `notification show` won't enqueue — no
pane association). To exercise the pane/queue without that, seed `state.json` directly
(`$HERDR_PLUGIN_STATE_DIR/state.json`), read the pane with `herdr pane read <pane_id> --source
visible`, and drive keys with `herdr pane send-keys <pane_id> <key>`. For the `startup` path, point
`HERDR_BIN_PATH` at a fake `herdr` that prints a canned `pane list` (see `tests/cli.rs`).

**Keybinds** live in the user's `~/.config/herdr/config.toml` (NOT the plugin): `prefix+alt+o` next,
`prefix+alt+p` peek, `prefix+alt+c` clear, `prefix+alt+q` open-pane. After editing:
`herdr config check && herdr server reload-config`.

## 6. Next up (start here)

### A. Deferred pane features — the ready-to-build lane (`src/pane.rs`)

This lane is already scoped in detail. All work is in `src/pane.rs` (+ `README.md` for the demo);
it is file-disjoint from the queue/manifest code. **Implement B before A** (B is small and validates
the reuse pattern; A restructures the loop's frame state and terminal init).

**Feature B — in-pane `c` = clear-all, with a confirm.**
- Add `confirm_clear: bool` to `PaneModel` (default false).
- In `event_loop`, before the normal key `match`, intercept when `confirm_clear`: `y`/`Y` confirms,
  any other key cancels. Add a plain `Char('c')` arm (after the existing ctrl-`c` arm) that sets
  `confirm_clear = true` only when the queue is non-empty (no-op on empty, like `d`/`Enter`).
- Confirm handler calls the existing `crate::clear` (already a delta through `StateStore::update` —
  invariant #1 satisfied for free; just add it to the `use crate::{...}` list, no `lib.rs` change).
- Footer shows `clear all N entries? y/n` while pending (precedence: confirm > status > hints);
  update `FOOTER_HINTS` to mention `c`.
- Tests: enter-confirm on non-empty, no-confirm on empty, and a pure `confirm_prompt(count)` string.

**Feature A — mouse click-to-select.**
- Harder than it sounds. `ratatui::try_init()`/`restore()` do NOT touch mouse capture — enable/
  disable it by hand (`EnableMouseCapture`/`DisableMouseCapture` via `ratatui::crossterm`) on every
  exit path. Note the panic-path gap: ratatui's panic hook won't disable capture, so a panic leaves
  the shell emitting mouse escapes until `reset` — add a chained panic hook or at least a comment.
- Hit-testing needs the list's live scroll offset, which the loop currently throws away (a fresh
  `ListState::default()` is built inside `draw` each frame). Persist `ListState` + the list `Rect`
  in `event_loop` and thread them into `draw`; read `list_state.offset()` after each render.
- New pure fn `row_for_click(area, offset, entry_count, col, row) -> Option<usize>` (unit-testable):
  bounds-check, map `offset + (row - area.y)`, return `None` if outside or `>= entry_count`.
- Handle only `MouseEventKind::Down(Left)` (select, like `j`/`k`); ignore the rest. Empty queue is
  safe by construction (`entry_count == 0` → always `None`).
- **Cross-feature trap:** B's confirm intercept wraps only the `Event::Key` branch. When A adds an
  `Event::Mouse` branch, hoist the confirm guard above BOTH branches, or a click during a pending
  confirm silently reselects instead of cancelling.

**Feature C — README demo (scope only).** Record with `vhs` (charmbracelet): a checked-in
`scripts/pane-demo.tape` drives seeded state through `j`/`k`/`Enter`/`d`/`c`/`q`; output
`docs/pane-demo.gif` embedded in README's "status pane" section, keeping the existing ASCII fence as
a fallback. GitHub renders GIFs inline (asciinema's player JS is stripped). Keep it < 2 MB.

### B. Parked (needs upstream herdr, do not schedule)

**Idempotent-toggle identity.** The `open-pane` toggle identifies the status pane by `label`
("Check-in"), which a user could theoretically collide with. If herdr later exposes plugin/
entrypoint identity in `pane list`, switch `PaneInfo::is_status_pane` to that. Nothing in this repo
unblocks it — it waits on an upstream feature.

### C. Optional

**Docs note (only if relevant).** herdr 0.7.5 made plugin install/enabled state global-per-user (was
per-session). The README doesn't describe per-session install, so no change is needed unless that
section is added.

## 7. How we work here (see CLAUDE.md for the short version)

- **Model tiers:** Opus orchestrates (plan/decide/integrate/own correctness); Sonnet subagents do
  research, exploration, scoping, and mechanical implementation; a Fable subagent is the advisor for
  genuine doubt on load-bearing decisions — used sparingly.
- **Design gate before code, adversarial review after.** This session's `[[startup]]` sprint:
  a Sonnet spike confirmed the hook contract against herdr source before any code; a Fable advisor
  then caught a real lost-ping race (invariant #3's `last_touched_ms` fix) that the normal test pass
  missed. Both patterns have now paid off repeatedly (v0.2.0's clinical review found two similar
  ping-loss bugs). Keep doing them for anything touching the queue's mutation/prune paths.
- **Verify foundations first.** Confirm an API contract or env-parity fact with a throwaway
  probe/schema check before building on it (done for the pane env, the startup contract, and the
  `pane list` field set).
