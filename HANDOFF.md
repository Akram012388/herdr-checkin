# HANDOFF

Development status and orientation for the next contributor (human or agent). For user-facing
docs see [README.md](README.md); for the release log see [CHANGELOG.md](CHANGELOG.md).

**Current version:** 0.2.0 · **License:** MIT · **Repo:** https://github.com/Akram012388/herdr-checkin
· **CI:** fmt + clippy + test, green on `main`.

---

## 1. What this is

A herdr plugin: a **durable FIFO attention queue** for agent panes. herdr's native
jump-to-notification only reaches the toast currently on screen, so a ping is lost once the toast
fades and simultaneous pings can't queue. This plugin remembers them — agents that go `blocked`
(need input) or `done` (finished) are enqueued; you jump to the oldest waiter on demand.

- **Manifest id:** `Akram012388.checkin` (GitHub-handle prefix). **Repo/dir name:** `herdr-checkin`
  (the `herdr-` prefix is what ecosystem discovery expects). These deliberately differ — do NOT
  rename the dir to the id.
- **Target:** herdr >= 0.7.0; developed and verified against **herdr 0.7.5, protocol 17**.

## 2. Architecture (read before editing)

Two execution modes share one on-disk queue:

1. **Short-lived per-event / per-action binaries.** herdr spawns one process per event/action.
   Subcommands: `status-changed`, `focused`, `closed` (events); `next`, `peek`, `clear` (actions).
   They mutate `state.json` and exit.
2. **A long-running TUI pane** (`pane` subcommand, ratatui + crossterm). herdr spawns it into a
   split via a `[[panes]]` manifest entry. It has no push channel for events, so it **polls**
   `state.json` on a 250 ms tick. A `pane-decision` subcommand (reads `pane list` on stdin, prints
   `OPEN`/`FOCUS <id>`/`CLOSE <id>`) backs the idempotent open/focus/close launcher.

**State:** `state.json` under `HERDR_PLUGIN_STATE_DIR`, an ordered `Vec<QueueEntry>`
(`{pane_id, workspace_id, agent, display_agent, title, status, enqueued_at_ms}`), guarded by
`state.lock` (`fs2`). Writes are atomic temp+rename; reads outside a mutation take no lock.

**Files:**
- `src/lib.rs` (~1.2k lines) — argv dispatch, queue transitions, `StateStore` (lock + atomic
  write), herdr CLI seam (`Herdr` trait / `CliHerdr`), event parsing, toast copy, all unit tests.
- `src/pane.rs` (~500 lines) — the ratatui TUI (`PaneModel`, event loop, view) and the
  `pane-decision` toggle logic. Pure model/decision code is unit-tested; the terminal loop is thin.
- `src/main.rs` — one-line entry into `lib::run_from_env`.
- `tests/cli.rs` — end-to-end tests that spawn the built binary with a fake `herdr` on
  `HERDR_BIN_PATH`.
- `herdr-plugin.toml` — manifest: `[[actions]]`, `[[events]]`, one `[[panes]]`, `[[build]]`.
- `scripts/open-pane.sh` — idempotent launcher for `open-pane`.

## 3. Behavior reference

- **Enqueue** on `agent_status` `blocked`/`done`; **evict** on return to `working`, on
  `pane.focused`, and on `pane.closed`. FIFO oldest-first; **deduplicated per pane** (a re-ping
  updates fields in place and keeps the original position + `enqueued_at_ms`).
- **`next`** focuses the oldest still-live waiter (`herdr agent focus <pane_id>`, cross-workspace)
  and evicts it **only after** the focus succeeds.
- **`peek`** shows the queue as a toast. **`clear`** empties it.
- **Status pane** keys: `j`/`k`/arrows move, `Enter` jump+evict-on-success, `d` drop, `q`/`Esc`
  close. `open-pane` is a current-tab-scoped toggle (open / focus / close).

### Load-bearing invariants (do not regress — each has a test)
1. **Mutations are deltas** through `StateStore::update` (read-modify-write under the lock), never
   a full model write-back. The pane polls while event binaries write concurrently; writing a
   stale in-memory list back would clobber a fresh enqueue.
2. **Focus first, evict on success only** (`next` and pane `Enter`). A failed jump must keep the
   entry — losing it is the exact failure the plugin exists to prevent.
3. **Never prune an entry the liveness snapshot couldn't see.** `next`/`peek` take the `pane list`
   snapshot before the lock; an entry with `enqueued_at_ms >= snapshot` is kept, not judged stale.
4. **`decide` assumes one globally-focused pane** (verified: herdr reports a single `focused:true`
   across all workspaces). The status pane is identified in `pane list` only by its `label`
   ("Check-in" — the `[[panes]]` title; keep `PANE_LABEL` in sync).

## 4. herdr API facts (0.7.5, protocol 17)

- **Event JSON** (in `HERDR_PLUGIN_EVENT_JSON`): `{event, data:{type, pane_id, workspace_id,
  agent_status, agent, display_agent, title}}` — underscore forms. Manifest `on =` uses the dotted
  form (`pane.agent_status_changed`, `pane.focused`, `pane.closed`). Fields also accepted at the
  top level if `data` is absent.
- **Focus an agent pane:** `herdr agent focus <pane_id>` (jumps workspace/tab/pane). The CLI
  `herdr pane focus` is *directional* only; there is no by-id `pane.focus` CLI.
- **Liveness:** `herdr pane list` → `result.panes[].{pane_id, agent_status, tab_id, focused, label}`.
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
herdr plugin action list --plugin Akram012388.checkin
herdr plugin action invoke next --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin        # inspect event/action runs
```

Real events need a real agent pane going blocked/done (manual `notification show` won't enqueue —
no pane association). To exercise the pane/queue without that, seed `state.json` directly
(`$HERDR_PLUGIN_STATE_DIR/state.json`) and read the pane with
`herdr pane read <pane_id> --source visible`. Drive pane keys with `herdr pane send-keys <id> <key>`.

**Keybinds** live in the user's `~/.config/herdr/config.toml` (NOT the plugin): `prefix+alt+o`
next, `prefix+alt+p` peek, `prefix+alt+c` clear, `prefix+alt+q` open-pane. After editing:
`herdr config check && herdr server reload-config`.

## 6. Pending / next up

1. **`[[startup]]` queue rebuild (top candidate).** herdr 0.7.5 added a one-shot plugin
   `[[startup]]` hook. After a herdr server restart the event subscription starts fresh and misses
   panes already `blocked`/`done`. A startup hook that scans `pane list` and seeds the queue would
   close that gap. **Before building:** read herdr's plugin-manifest docs to confirm the hook's
   exact contract (the v0.7.5 release notes don't specify it).
2. **Idempotent-toggle polish (optional):** the `open-pane` toggle identifies the pane by `label`
   ("Check-in"), which a user could theoretically collide with. If herdr later exposes plugin/
   entrypoint identity in `pane list`, switch to that.
3. **Deferred pane features (v3.0 MVP intentionally excluded these):** mouse click-to-select; a
   `c` clear-all *inside* the pane guarded by a confirm; an optional gif/screenshot in the README.
4. **Docs note (only if relevant):** herdr 0.7.5 made plugin install/enabled state global-per-user
   (was per-session). Our README doesn't describe per-session install, so no change is needed
   unless that section is added.

## 7. Process notes that worked well this session

- **Design gate before code:** the v3 plan was reviewed by an advisor model before implementation;
  its three revisions (focus-then-evict, no mtime guard + time-based redraw, deltas-only rule)
  shaped the initial cut and avoided rework.
- **Clinical independent review after shipping:** a fresh adversarial review of v0.2.0 found two
  real ping-loss bugs (`next` popped before focus; the snapshot-prune race) that the normal test
  pass missed. Both are now fixed and regression-tested. Worth repeating for future features.
- **Env-parity check first.** Before building the pane, a throwaway probe confirmed a pane
  entrypoint actually receives `HERDR_PLUGIN_STATE_DIR`. Verify foundations before building on them.
