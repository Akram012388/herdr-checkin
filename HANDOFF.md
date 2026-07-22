# HANDOFF

Orientation for the next session (human or agent). Read this first, then start on §6 (Next up).
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md). The active feature's authoritative design:
[docs/triage-overlay-design.md](docs/triage-overlay-design.md).

**Version:** **0.4.0 — the triage-popup release — is cut** (version in `Cargo.toml` +
`herdr-plugin.toml` + `Cargo.lock`, CHANGELOG dated 2026-07-22, README + demo gif refreshed). **NOT
tagged** — the maintainer tags on request. · **License:** MIT · **Repo:**
https://github.com/Akram012388/herdr-checkin · **State:** `main` is green (fmt + clippy + test,
**67 lib + 5 CLI tests**) and pushed. No open branches, no worktrees, working tree clean.

**The status pane is DONE and shipped as a centered popup modal.** It looks and behaves like herdr's
own `prefix+s` settings popup: a session-level `--placement popup` that herdr draws with a border +
"Check-in" title, holding the durable queue grouped into **CHECKIN** (`blocked`) / **DONE** (`done`)
sections, keyboard-navigable and reply-able, dismissed with `q`/`Esc` (the pane calls the
`popup.close` socket method on exit). It began as the 7-slice "triage overlay" build, then pivoted to
`popup` placement once the herdr source revealed it (the CLI `--help` omits `popup` via a stale
value-parser — the real parser accepts it). **Verified live end-to-end** in herdr 0.7.5. **START AT §6
— no in-flight feature; the release just needs the maintainer's tag, and only parked/optional items
remain.**

---

## 1. What this is

A herdr plugin: a **durable FIFO attention queue** for agent panes. herdr's native
jump-to-notification only reaches the toast currently on screen, so a ping is lost once the toast
fades, and simultaneous pings can't queue. This plugin remembers them — agents that go `blocked`
(need input) or `done` (finished) are enqueued; you jump to (or, as of slices 1–4, reply inline to)
the oldest waiter on demand.

- **Manifest id:** `Akram012388.checkin` (GitHub-handle prefix). **Repo/dir name:** `herdr-checkin`
  (the `herdr-` prefix is what ecosystem discovery expects). These deliberately differ — do NOT
  rename the dir to the id.
- **Target:** herdr >= 0.7.5 (the `popup` placement landed in 0.7.5); verified against **herdr 0.7.5,
  protocol 17**.

## 2. Architecture

Two execution modes share one on-disk queue:

1. **Short-lived per-event / per-action binaries.** herdr spawns one process per event/action.
   Subcommands: `status-changed`, `focused`, `closed` (events); `next`, `peek`, `clear` (actions);
   `startup` (the [[startup]] hook). They mutate `state.json` and exit.
2. **A long-running TUI pane** (`pane` subcommand, ratatui + crossterm). herdr spawns it as a
   centered popup (`--placement popup`) via a `[[panes]]` manifest entry. It has no push channel for
   events, so it **polls** `state.json` on a 250 ms tick. A popup is a session-level singleton, so
   there's no open/focus/close decision to make — `scripts/open-pane.sh` just opens it, and the pane
   dismisses its own popup (`popup.close`) on exit.

**State:** `state.json` under `HERDR_PLUGIN_STATE_DIR`, an ordered `Vec<QueueEntry>`
(`{pane_id, workspace_id, agent, display_agent, title, status, enqueued_at_ms, last_touched_ms}`),
guarded by `state.lock` (`fs2`). Writes are atomic temp+rename; reads outside a mutation take no lock.

**Files** (`lib.rs` was split into cohesive modules; each holds the `#[cfg(test)]` tests for its own
code, and `lib.rs` re-exports items as `pub(crate)` so `crate::X` paths still resolve):
- `src/lib.rs` (~170 lines) — the orientation page: argv dispatch (`run_from_env`/`run`), subcommand
  parsing, `RuntimeEnv`, and the `mod`/`pub(crate) use` wiring for everything below.
- `src/state.rs` (~306) — persisted state: `QueueEntry`, `WaitStatus`, `StateStore` (lock + atomic
  temp+rename write), `StateLock`, `read_state`/`write_state`/`load_entries`, `PluginError`. Owns
  the "all mutations via `StateStore::update`" invariant.
- `src/herdr.rs` (~385) — the herdr CLI seam (`Herdr` trait / `CliHerdr`, `PaneInfo`) plus JSON
  parsing for both `pane list` responses and plugin event payloads (`StatusEvent`). The `Herdr`
  trait's methods: `pane_status_map`, `pane_infos`, `focus_agent`, **`prompt_agent`** (slice 1),
  `show_notification`.
- `src/queue.rs` (~215) — pure queue transitions (`enqueue`/`evict`/`is_live`) and the event handlers
  (`on_status_changed`/`on_focused`/`on_closed`). Must never depend on the `Herdr` trait (enforced
  by the module boundary now, not just a comment).
- `src/actions.rs` (~540) — the actions (`next`/`peek`/`clear`/`startup`), the toast copy they
  render, and **`agent_label`** (the display-name helper, shared by the list rows and the reply
  footer). The only non-pane callers that also talk to herdr.
- `src/test_support.rs` (~165) — `#[cfg(test)]`-only shared fake `Herdr` + state fixtures. The
  `FakeHerdr` records `focused`, **`prompts`** (`(pane_id, text)`), and `notifications`, with
  `with_failing_focus`/**`with_failing_prompt`** toggles.
- `src/pane.rs` (~1030) — the ratatui TUI (`PaneModel`, event loop, view, mouse hit-testing) and the
  **inline reply mode** (slices 2–4). Pure model/decision code is unit-tested; the terminal loop is
  thin. Reaches domain/storage/herdr types via `use crate::{…}`.
- `src/main.rs` — one-line entry into `lib::run_from_env`.
- `tests/cli.rs` — end-to-end tests that spawn the built binary against a fake `herdr` on
  `HERDR_BIN_PATH`.
- `herdr-plugin.toml` — manifest: `[[actions]]`, `[[events]]`, one `[[panes]]`, `[[build]]`,
  `[[startup]]`.
- `scripts/open-pane.sh` — launcher for `open-pane` (opens the pane as a popup; no toggle logic).

## 3. Behavior + load-bearing invariants

**Behavior:**
- **Enqueue** on `agent_status` `blocked`/`done`; **evict** on return to `working`, on
  `pane.focused`, and on `pane.closed`. FIFO oldest-first; **deduplicated per pane** (a re-ping
  updates fields in place, keeping the original position + `enqueued_at_ms`).
- **`next`** focuses the oldest still-live waiter (`herdr agent focus <pane_id>`, cross-workspace)
  and evicts it **only after** the focus succeeds. **`peek`** shows the queue as a toast.
  **`clear`** empties it. **`startup`** re-seeds the queue from `pane list` after a herdr restart.
- **Status pane** keys: `j`/`k`/arrows or **left-click** move/select, `Enter` jump+evict-on-success
  (and closes the popup), **`space` reply inline** (slices 2–4), `d` drop, `c` clear-all (with a
  `y`/`n` confirm), `q`/`Esc` close (dismissing the popup). `open-pane` just opens it — a popup is a
  session-level singleton, so there is no open/focus/close toggle.
- **Inline reply** (slices 2–4): `space` opens a reply line for the selected waiter; you type an
  answer and `Enter` routes it into that agent's session via `herdr agent prompt <pane_id> <text>`,
  then evicts the entry **only on submit success** (a failed submit keeps it). `Esc`/click cancels;
  the reply's **target is captured when reply mode is armed**, so a concurrent queue refresh can't
  retarget it. Empty/whitespace `Enter` sends nothing and stays in reply mode.

**Invariants (do not regress — each has a regression test):**
1. **Mutations are deltas** through `StateStore::update` (read-modify-write under the lock), never a
   full model write-back. The pane polls while event binaries write concurrently; a stale write-back
   would clobber a fresh enqueue.
2. **Act first, evict on success only.** Applies to `next`, pane `Enter` (focus then evict), **and
   inline reply** (`on_reply_submit`: prompt then evict). A failed action keeps the entry — losing it
   is the exact failure the plugin exists to prevent.
3. **Never prune an entry the liveness snapshot couldn't see.** `next`/`peek` take the `pane list`
   snapshot before the lock; keep any entry with `max(enqueued_at_ms, last_touched_ms) >= snapshot`.
   `enqueued_at_ms` is the FIFO age; `last_touched_ms` is bumped by every `enqueue` upsert. The
   `max` closes a lost-ping race: a persisted entry that a concurrent event *refreshes* during the
   snapshot→lock window would otherwise be pruned on its old `enqueued_at_ms`.
4. **`startup` is additive-only.** It merges each `blocked`/`done` pane through the same `enqueue`
   upsert events use (a delta under the lock) — never a wholesale `state.json` rewrite, and it never
   evicts. Stale entries are pruned by `next`/`peek`'s liveness pass. The hook is spawned async and
   races the live event loop, so this merge-not-rewrite discipline is what keeps it safe.
5. **The popup dismisses itself only when it opened one.** `run()` calls `popup.close` on exit only
   when `HERDR_CHECKIN_POPUP` is set (the launcher sets it) — so a non-popup launch (e.g. manual
   `herdr-checkin pane` invocation) never closes an unrelated session popup.

## 4. herdr API facts (0.7.5, protocol 17)

- **Event JSON** (in `HERDR_PLUGIN_EVENT_JSON`): `{event, data:{type, pane_id, workspace_id,
  agent_status, agent, display_agent, title}}` — underscore forms. Manifest `on =` uses the dotted
  form (`pane.agent_status_changed`, `pane.focused`, `pane.closed`). Fields also accepted at the top
  level if `data` is absent.
- **Focus an agent pane:** `herdr agent focus <pane_id>` (jumps workspace/tab/pane). The CLI
  `herdr pane focus` is *directional* only; there is no by-id `pane.focus` CLI. **`agent focus` only
  accepts real *agent* panes** — targeting a plain shell returns `{"error":{"code":"agent_not_found"}}`.
- **Reply into an agent (USED by inline reply):** `herdr agent prompt <TARGET> <TEXT>`. **Probed live
  this session** — the load-bearing findings the reply mode is built on:
  - **`<TARGET>` is the `pane_id` we already store** (`w4:p1` form). The `agent_session` uuid from
    `agent list` is **rejected** (`agent_not_found`).
  - The reply routes cleanly into the target's session and the agent acts on it.
  - **`blocked` is narrower than "waiting for me":** a Claude agent that asks a prose question and
    ends its turn shows as **`done`/`idle`**, not `blocked` (`blocked` seems reserved for
    modal/permission prompts). So the queue keys on `done` and relies on **acknowledgment**
    (jump/reply/drop + evict-on-`pane.focused`), never status-/content-sniffing.
  - **`--wait --until <state>` is flaky from a non-working start** (returns `timeout` even when the
    submit succeeded), so `prompt_agent` is **fire-and-forget** — "submit accepted" is the success
    boundary for eviction; the pane never blocks on the agent's next turn.
  - `herdr agent` also exposes `list`/`get`/`read`/`send-keys`/`wait`/`rename`/`start`. `agent list`
    returns per-agent `agent_status`, `pane_id`, `agent_session` (uuid), `tab_id`, `cwd`, `title`.
- **Pane info:** `herdr pane list` → `result.panes[]` of `PaneInfo`. Fields we use: `pane_id`,
  `workspace_id`, `agent_status`, `focused`, plus optional `agent`, `display_agent`, `title`.
- **`[[startup]]` hook:** manifest is an array-of-tables with only `command` (required argv) +
  optional `platforms` — no `id`/`on`. Fires **once per server process** (cold start and live-handoff
  takeover). One-shot run-and-exit. Receives the normal plugin env plus `HERDR_PLUGIN_EVENT=startup`;
  **no pane payload** — the hook calls `pane list` itself. Spawned **async and not awaited**, so it
  races the live event loop (see invariant #4).
- **Plugin pane:** declared via `[[panes]]`; opened with
  `herdr plugin pane open --plugin <id> --entrypoint <pane-id> --placement <PLACEMENT> --focus`. No
  push events to a running pane → poll. **`--placement` values: `overlay`, `split`, `tab`, `zoomed`,
  `popup`** (it's a **CLI flag**, so the launcher can override the manifest's default per-open — no
  manifest rewrite needed). **We use `popup`:** a centered, session-modal floating popup (like
  herdr's own `prefix+s` settings), sized via `--width`/`--height` `"NN%"` percentages of the
  available area (herdr's `PopupSize`); herdr draws the border and the `[[panes]]` `title` on it. A
  popup is a session-level singleton with no separate focus/close-by-id CLI to manage — the pane
  dismisses it from the inside by calling the `popup.close` socket method on exit (gated on the
  launcher-set `HERDR_CHECKIN_POPUP` env var so a non-popup launch never closes an unrelated popup).
  **Probed live:** `--placement overlay` (the pre-popup choice) runs a persistent, keyboard-
  interactive TUI that survives blur but is tab-scoped, not a global `prefix+s`-style summon —
  superseded by `popup` for this plugin.
- **Env a pane/handler receives:** `HERDR_PLUGIN_STATE_DIR`, `HERDR_BIN_PATH`,
  `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_ID`.
  **Gotcha:** the id is percent-encoded in the state-dir path (`%41kram012388.checkin`). Always use
  the `HERDR_PLUGIN_STATE_DIR` env var — never construct the path. (For manual seeding this session it
  resolved to `~/.local/state/herdr/plugins/%41kram012388.checkin/state.json`, but that's an
  implementation detail — do not hardcode it in code.)
- **Toast:** `herdr notification show <title> [--body B] --sound none|request|done`.

## 5. Dev loop

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # what CI runs
cargo build --release
herdr plugin link "$PWD"                                   # register the local build
herdr plugin action invoke <next|peek|clear|open-pane> --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin        # inspect event/action/startup runs
```

**Manual E2E of the pane/queue** (no real agent needed): seed `state.json` directly (find the path
via the `HERDR_PLUGIN_STATE_DIR` env; running any action once materializes it), open the pane, read
it with `herdr pane read <pane_id> --source visible` (or `--ansi` for the rendered TUI), and drive
keys with `herdr pane send-keys <pane_id> <key>`.

**Manual E2E of the inline-reply path** (needs a real agent, since `agent prompt` only accepts agent
panes — verified this session): in a spare pane, launch `claude`; get its `pane_id` from
`herdr agent list`; seed a queue entry for it; open the status pane; `send-keys <pane> ' '` to arm
reply, type text, `send-keys <pane> Enter`; confirm the reply landed in the agent and the entry was
evicted. To exercise placement, open with `herdr plugin pane open … --placement overlay`. The
`startup` path can be exercised with a fake `herdr` on `HERDR_BIN_PATH` that prints a canned
`pane list` (see `tests/cli.rs`).

**Keybinds** live in the user's `~/.config/herdr/config.toml` (NOT the plugin): `prefix+alt+o` next,
`prefix+alt+p` peek, `prefix+alt+c` clear, `prefix+alt+q` open-pane. After editing:
`herdr config check && herdr server reload-config`. (No keybind is needed for `space` reply — that is
a pane-internal key, handled inside the running TUI.)

## 6. Next up (START HERE) — the triage overlay shipped as 0.4.0; only the tag + parked items remain

The **triage overlay was THE chosen interface** (maintainer decision) and is now **built, verified
live, and cut as 0.4.0**: a status pane that **looks and behaves like a Claude Code agents-view TUI
session, rendered via herdr's overlay primitive**. The authoritative design (verified probes + Fable
advisor pass) is [docs/triage-overlay-design.md](docs/triage-overlay-design.md); its §5 was the
7-slice build plan, **all 7 slices done**.

**Immediate next step: none required.** `main` is green and pushed at 0.4.0 but **not tagged** — the
maintainer tags on request (do NOT tag autonomously). If asked to release, the code/docs/version are
all in place; tagging `v0.4.0` at the current HEAD is the only remaining act. Otherwise, only the
**Parked / optional** items below remain, and they all wait on upstream herdr.

**Data-model guardrail (maintainer + advisor confirmed — do not drift):** the agents-view is a
**look**, not a pivot. The console stays an **inbox** — only *enqueued waiters* appear, grouped by
status (CHECKIN = `blocked`, DONE = `done`), FIFO within each. It is **not** a live roster of all
agents (that is herdr's native view, and cloning it loses). Litmus test for any feature: *does it
operate on an enqueued entry?* If no, reject.

### DONE so far (committed, gate green)
- **Slice 1** (`2806f2a`): `Herdr::prompt_agent(pane_id, text)` → `herdr agent prompt <pane_id>
  <text>`, fire-and-forget, mirroring `focus_agent`. `FakeHerdr` records prompts. Command-shaping
  tests live in `src/herdr.rs` (a real `CliHerdr` vs a throwaway fake `herdr` asserting argv + Ok/Err
  — `tests/cli.rs` can't reach `pub(crate) CliHerdr`).
- **Slices 2–4** (`646c285`): the **inline reply input mode** in `src/pane.rs`. Landed as one commit
  because Rust's `-D warnings` dead-code gate flags a model-only slice with no production caller.
  What exists now:
  - `ReplyDraft { target, label, buffer }` and `PaneModel.reply: Option<ReplyDraft>`, mirroring the
    `confirm_clear` modal. Methods: `begin_reply` (arms on the selected entry; captures target+label
    at arm time; no-op on empty queue or while a clear-confirm is pending), `reply_push`,
    `reply_backspace`, `cancel_reply`.
  - `on_reply_submit` (impure handler): route via `herdr.prompt_agent`, then `evict_pane` **only on
    success** (invariant #2); keep-on-failure with a `reply failed: …` status; empty/whitespace
    buffer sends nothing and stays in reply mode.
  - `event_loop`: a **top-priority reply-key guard** hoisted above the `confirm_clear` and normal
    branches (chars → buffer; `Enter` → submit; `Backspace` → edit; `Esc`/click → cancel; `Ctrl-C`
    still quits). A `space` binding in the normal branch calls `begin_reply`.
  - `draw` footer renders the draft via `reply_prompt(label, buffer)`; `FOOTER_HINTS` lists
    `space reply`.
  - Support: `agent_label` extracted to `src/actions.rs` (shared list+footer naming);
    `FakeHerdr::with_failing_prompt` for the keep-on-failure test.
- **Slice 5** (`74220d2`): the **grouped agents-view render** in `src/pane.rs` — the CC-agents-view
  look. Pure view + click-mapping; no new model state, no queue-invariant surface. What exists now:
  - `layout_rows(&[QueueEntry]) -> Vec<Row>` groups the FIFO queue into sections — `AWAITING YOU`
    (`blocked`) then `DONE` (`done`), FIFO within each — as a pure view transform. `Row` is
    `Header(&'static str) | Entry(usize)`; each `Entry` keeps its **original index into `entries`**,
    and a section header is emitted only when that section is non-empty. It never reorders `entries`.
  - `draw` renders through `layout_rows` and highlights the display row carrying `model.selected`
    (headers are never selected). The leading `N.` numbering is gone — section membership + the `>`
    cursor replace it.
  - `selected` stays an **index into `entries`** (design doc §4 option a); only `draw` and
    `row_for_click` learn the header offsets, so `selected_pane_id`/`sync`/reply-target capture are
    untouched. **`j`/`k` traverse in on-screen display order** (`PaneModel::display_order` — the same
    `layout_rows` grouping projected to its entries), so the cursor moves monotonically down-screen
    even when the FIFO queue interleaves blocked/done. The selection *representation* is unchanged (an
    entry index); only the traversal *order* is derived from the layout, so it can't drift from paint.
  - `row_for_click(area, offset, &rows, col, row)` maps a click back to an entry index via the
    grouped `rows`, returning `None` on a header (a header click selects nothing). `on_mouse`
    recomputes `layout_rows` — entries are unchanged between draw and the click, so it reproduces
    exactly what was painted.
  - `WaitStatus` re-export in `lib.rs` is no longer test-only (production `draw`/`layout_rows` name
    it). Tests: `layout_rows` grouping/FIFO/empty-section, header-skip on click, updated click/mouse.
- **Slice 6** (`f29f4dc`): the **overlay launcher** — `scripts/open-pane.sh`'s open now uses
  `--placement overlay` (the split-only `--direction right` dropped; it has no meaning for a centered
  overlay). The open/focus/close toggle is unchanged (it keys off the pane `label`, placement-
  independent), so the decision logic + its tests are untouched. **Verified live in herdr 0.7.5**
  (first full end-to-end run of the console): grouped AWAITING-YOU/DONE render on an interleaved
  queue; `j`/`k` stepped A→C→B→D across the section boundary and clamped; `d`/`c`+`y` evicted through
  the overlay; the overlay persisted on blur and kept rendering; and `space` reply on a **real claude
  agent** routed the typed text into its session via `herdr agent prompt` (the agent began acting) and
  evicted the entry on submit success with a `replied to Claude` footer.

- **Slice 7 — docs + the bundled release (cut as 0.4.0).** `README.md` rewritten for the overlay/
  agents-view console (the `space` reply key, the grouped AWAITING-YOU/DONE sections, headers non-
  selectable, the fire-and-forget reply semantics) with a **refreshed demo gif** (`docs/pane-demo.gif`,
  via the updated `scripts/pane-demo.tape` — the tape now shows the grouped view + a `space` reply,
  rendered offline with the fake herdr; the `*` catch-all covers `agent prompt`). `CHANGELOG.md` has a
  `[0.4.0] - 2026-07-22` section bundling the overlay, inline reply, grouped render, clear-all,
  mouse-select, and the module split. Version bumped to **0.4.0** in `Cargo.toml`, `herdr-plugin.toml`,
  and `Cargo.lock`. **Not tagged** (maintainer tags on request).

### The release is cut — remaining action is the maintainer's tag
Everything for 0.4.0 is committed and pushed on `main`. The only unreleased step is tagging `v0.4.0`
at HEAD, which is the maintainer's call — do NOT tag autonomously. Beyond that, see **Parked /
optional** below (all upstream-gated).

(Commit/push at own discretion is pre-approved for this repo — see the memory index.)

### Parked / optional (unchanged)
- **Idempotent-toggle identity** — `open-pane` identifies the status pane by `label` ("Check-in");
  switch `PaneInfo::is_status_pane` to plugin/entrypoint identity if herdr ever exposes it in
  `pane list`. Waits on upstream.
- **Global-summon overlay** — the tab-scoped overlay is verified; a global `prefix+s`-style summon is
  not, and is off the critical path (design doc §1 caveat).
- **Docs note** — herdr 0.7.5 made plugin install/enabled state global-per-user; only relevant if a
  per-session-install section is ever added to the README.

### Suggested skills for the next session
- **`/herdr`** — control herdr from inside it (only when `HERDR_ENV=1`): split panes, spawn/read
  agents, run `herdr agent prompt`/`focus`. The tool for the slice-6 manual E2E.
- **`demo-gif`** — regenerate the README demo gif in slice 7 (VHS-based).
- **A Fable-5 advisor subagent** — for a genuine load-bearing call during the build, used sparingly.
  The strategic queue-vs-native design call is already settled (design doc §2/§7).
- **`/handoff`** — to snapshot again at the end of the next session.

## 7. How we work here (see CLAUDE.md for the short version)

- **Model tiers:** Opus orchestrates (plan/decide/integrate/own correctness); Sonnet subagents do
  research, exploration, scoping, and mechanical implementation; a Fable subagent is the advisor for
  genuine doubt on load-bearing decisions — used sparingly.
- **Design gate before code, adversarial review after.** This has paid off repeatedly (the
  `[[startup]]` race the Fable advisor caught → invariant #3; v0.2.0's clinical review found two
  ping-loss bugs; this session a probe corrected the design's target-capture and the overlay
  placement assumptions). Keep doing it for anything touching the queue's mutation/prune paths.
- **Verify foundations first.** Confirm an API contract with a throwaway probe/schema check before
  building on it (done this session for `agent prompt`'s target + the `overlay` placement before the
  reply mode was written).
- **Tracer-bullet slices, each green.** Small commits that each keep `fmt + clippy -D warnings + test`
  passing. When Rust's dead-code gate would force a lint suppression on a caller-less seam, prefer
  merging the seam with its first real caller over sprinkling `allow(dead_code)` (see slices 2–4).
