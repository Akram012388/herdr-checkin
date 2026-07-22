# HANDOFF

Orientation for the next session (human or agent). Read this first, then start on §6 (Next up).
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md).

**Version:** 0.3.0 (0.4.0 unreleased, pending the maintainer's go) · **License:** MIT · **Repo:**
https://github.com/Akram012388/herdr-checkin · **State:** `main` is green (fmt + clippy + test) and
pushed (HEAD `ba83f02`). No open branches, no worktrees. There is an unshipped `[Unreleased]`
CHANGELOG set (mouse-select, clear-all, README demo, internal module split), and the plugin passed a
full **manual end-to-end test in real herdr** (see §6). The **triage-overlay direction is now
design-gated** in [docs/triage-overlay-design.md](docs/triage-overlay-design.md) — both option-(b)
unknowns were probed against real herdr and passed. Nothing is tagged (maintainer tags on request).
**START AT §6 — a release-timing choice remains: prep 0.4.0 first, or start the 0.5.0 overlay build.**

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

**Files** (`lib.rs` was split into cohesive modules; each holds the `#[cfg(test)]` tests for its own
code, and `lib.rs` re-exports items as `pub(crate)` so `crate::X` paths still resolve):
- `src/lib.rs` (~170 lines) — the orientation page: argv dispatch (`run_from_env`/`run`), subcommand
  parsing, `RuntimeEnv`, and the `mod`/`pub(crate) use` wiring for everything below.
- `src/state.rs` — persisted state: `QueueEntry`, `WaitStatus`, `StateStore` (lock + atomic
  temp+rename write), `StateLock`, `read_state`/`write_state`/`load_entries`, `PluginError`. Owns
  the "all mutations via `StateStore::update`" invariant.
- `src/herdr.rs` — the herdr CLI seam (`Herdr` trait / `CliHerdr`, `PaneInfo`) plus JSON parsing for
  both `pane list` responses and plugin event payloads (`StatusEvent`).
- `src/queue.rs` — pure queue transitions (`enqueue`/`evict`/`is_live`) and the event handlers
  (`on_status_changed`/`on_focused`/`on_closed`). Must never depend on the `Herdr` trait (enforced
  by the module boundary now, not just a comment).
- `src/actions.rs` — the actions (`next`/`peek`/`clear`/`startup`) and the toast copy they render;
  the only non-pane callers that also talk to herdr.
- `src/test_support.rs` — `#[cfg(test)]`-only shared fake `Herdr` + state fixtures.
- `src/pane.rs` (~760 lines) — the ratatui TUI (`PaneModel`, event loop, view, mouse hit-testing)
  and the `pane-decision` toggle logic. Pure model/decision code is unit-tested; the terminal loop
  is thin. Reaches domain/storage/herdr types via `use crate::{...}` (the re-exports above).
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
- **Status pane** keys: `j`/`k`/arrows or **left-click** move/select, `Enter` jump+evict-on-success,
  `d` drop, `c` clear-all (with a `y`/`n` confirm), `q`/`Esc` close. `open-pane` is a
  current-tab-scoped toggle (open / focus / close).

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
  `herdr pane focus` is *directional* only; there is no by-id `pane.focus` CLI. **`agent focus` only
  accepts real *agent* panes** — targeting a plain shell returns
  `{"error":{"code":"agent_not_found"}}`. Irrelevant in production (only agent panes ever enqueue),
  but it surfaced in the E2E test when entries were injected onto non-agent shells; the plugin
  handled it correctly (kept the entry, showed the error — invariant #2).
- **Submit input to an agent** (not yet used; enables the §6 triage-overlay idea): `herdr agent
  prompt <TARGET> <TEXT> [--wait --until <idle|working|blocked|done|unknown>] [--timeout <ms>]` routes
  a reply into an agent's session and can wait for the resulting state. Handles submitting from a
  non-working (blocked/idle/done) start. `herdr agent` also exposes `list`/`get`/`read`/`send-keys`/
  `wait`/`rename`/`start`. `agent list` returns per-agent `agent_status`, `pane_id`, `agent_session`
  (uuid), `tab_id`, `cwd`, `title`.
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

## 6. Next up (START HERE) — probes done + design-gated; a release-timing choice remains

**Update (this session, HEAD `ba83f02`):** the maintainer chose **(b)** — the two triage-overlay
unknowns were **probed against real herdr 0.7.5 and both passed**, and the overlay direction is now
**design-gated** in [docs/triage-overlay-design.md](docs/triage-overlay-design.md) (with a Fable
advisor pass). Probe findings, verbatim, live in that doc §1; the short version:
- **Overlay placement works** — a persistent, keyboard-interactive TUI that survives blur. The enum
  is **`overlay`** (not `popup`), and it's a `plugin pane open --placement` **CLI flag**, so
  offering it is a one-line launcher change, not a manifest rewrite.
- **`agent prompt` routes an inline reply by the `pane_id` we already store** (the `agent_session`
  uuid is rejected). **`blocked` is narrower than "waiting for me"** — a Claude prose-question reads
  as `done`/`idle`, so the queue keys on `done` and uses acknowledgment (jump/reply/drop + evict-on-
  focus) rather than sniffing status. `--wait --until` is flaky from a non-working start → reply
  should be fire-and-poll, not `--wait`-gated.

**The remaining choice (pick up here):**

> (a) **Prep the 0.4.0 release now** (the `[Unreleased]` set), then start the 0.5.0 overlay build; or
> (b) **Start building the 0.5.0 triage overlay** straight from the design doc (§5 has the 6-slice
>     build plan), and fold 0.4.0 in whenever.

Everything is committed and pushed (HEAD `ba83f02`). The `[Unreleased]` CHANGELOG set from the prior
session — **Feature A** (mouse click-to-select), **Feature B** (`c` clear-all with confirm),
**Feature C** (README demo GIF), the `lib.rs` module split — is still unshipped and passed a full
**manual E2E test in real herdr** (pane launch/render/refresh, live enqueue, real event delivery +
auto-eviction, mouse-select, clear-all confirm, `Enter` graceful focus-failure *and* success,
`peek`, durability across a ~2.5 h gap). Details live in the commit history and `CHANGELOG.md`.

### If (a) — prep 0.4.0 (mechanical, ~15 min)
1. In `CHANGELOG.md`, rename `## [Unreleased]` → `## [0.4.0] - <today's date>`.
2. Bump `version = "0.3.0"` → `"0.4.0"` in **`herdr-plugin.toml`** and **`Cargo.toml`** (keep them in
   sync; `Cargo.lock` refreshes on the next build).
3. Update this file's header Version line to 0.4.0.
4. Run the CI gate (§5), then commit + push. **Do NOT tag** — the maintainer tags on request.

(Commit/push at own discretion is pre-approved for this repo — see the memory index.)

### If (b) — start the 0.5.0 triage-overlay build
Follow [docs/triage-overlay-design.md](docs/triage-overlay-design.md) §5 — a 6-slice tracer-bullet
plan (add `Herdr::prompt_agent`; a `PaneModel` reply mode mirroring `confirm_clear`; submit →
evict-on-success; the `space` binding; the launcher `--placement overlay` switch; docs). Each slice
keeps the CI gate (§5) green. The two unknowns are already verified (see the update above), so no
more probing is needed before code.

---

### The triage-overlay idea (the 0.5.0 direction) — designed, not built

The maintainer wants to evolve the status pane from a passive **list + jump** into an active
**triage console**, modeled on **Claude Code's agents view**: agents grouped by status (awaiting
input / working / done), and **per row you reply inline** — type an answer that routes straight into
that agent's session — instead of only jumping to it. Optionally presented as a **popup/overlay**
summoned like herdr's `prefix+s`, rather than a persistent split.

**herdr already has every primitive** (verified via CLI this session — see §4):
- `enter to return` -> `herdr agent focus <target>` (already our `Enter`).
- **`space to reply` -> `herdr agent prompt <target> <text>`** — the key enabler; a robust CLI (not
  raw send-keys), with `--wait --until <state>` that maps onto our "act, then evict on success"
  discipline.
- `delete` -> our existing `evict` (`d`).
- group-by-status <- `herdr agent list`.

**The two load-bearing unknowns are now RESOLVED** (probed against real herdr 0.7.5; full findings in
[docs/triage-overlay-design.md](docs/triage-overlay-design.md) §1):
1. **Overlay placement — YES.** `plugin pane open --placement overlay` runs a persistent,
   keyboard-interactive TUI that survives blur. The enum is **`overlay`** (not `popup`), and it's a
   **CLI flag** (no manifest rewrite / `reload-config` needed). Caveat: it's tab-scoped, not a global
   `prefix+s`-style summon — that flavor is unverified and off the critical path.
2. **`agent prompt` target — the `pane_id` we store** (`w4:p1` form); the `agent_session` uuid is
   rejected. Inline reply routes cleanly and the agent acts. **But `blocked` is narrower than
   "waiting for me"** — a Claude prose-question reads as `done`/`idle`, so key on `done` +
   acknowledgment, not status-sniffing. `--wait --until` is flaky from a non-working start → reply is
   fire-and-poll, not `--wait`-gated.

**Strategic frame (Fable advisor pass, done — see design doc §2/§7):** herdr's *native* `agent list`
renders live status; this plugin's differentiator is the **durable FIFO queue** (memory across
toast-fade / restart / simultaneous blocks). Hold the line: the console is an **inbox** of
unacknowledged pings, **not a roster**. Litmus test for any feature — *does it operate on an enqueued
entry?* The reply feedback loop stays an inbox (not a treadmill) because **reply evicts immediately**;
a later turn-end re-enqueues at the tail as fresh debt.

### Parked / optional (unchanged)
- **Idempotent-toggle identity** — `open-pane` identifies the status pane by `label` ("Check-in");
  switch `PaneInfo::is_status_pane` to plugin/entrypoint identity if herdr ever exposes it in
  `pane list`. Waits on upstream.
- **Docs note** — herdr 0.7.5 made plugin install/enabled state global-per-user; only relevant if a
  per-session-install section is ever added to the README.

### Suggested skills for the next session
- **`/herdr`** — control herdr from inside it (only when `HERDR_ENV=1`): split panes, spawn/read
  agents, run `herdr agent prompt`/`focus`. Handy for manual E2E of the 0.5.0 reply path (seed
  `state.json`, spawn a throwaway agent, prompt it by `pane_id`).
- **A Fable-5 advisor subagent** — for a load-bearing call during the build (used sparingly). The
  strategic queue-vs-native design call is already settled (design doc §2/§7).
- **`/handoff`** — to snapshot again at the end of the next session.

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
