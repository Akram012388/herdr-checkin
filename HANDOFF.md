# HANDOFF

Orientation for the next session (human or agent). **Read this first, then start on §6.**
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md). The feature's original (overlay-era) design that
later pivoted to a popup: [docs/triage-overlay-design.md](docs/triage-overlay-design.md).

**Version:** **0.4.0 — the triage-popup release — is cut** (version in `Cargo.toml` +
`herdr-plugin.toml` + `Cargo.lock`, CHANGELOG dated 2026-07-22, README + demo gif refreshed). **NOT
tagged** — the maintainer tags on request. · **License:** MIT · **Repo:**
https://github.com/Akram012388/herdr-checkin · **State:** `main` is green (fmt + clippy + test,
**67 lib + 5 CLI tests**) and pushed; latest substantive commit `1d21213` (the overlay→popup switch).
No open branches, no worktrees, working tree clean.

**START HERE — the maintainer has pending edits to land.** Before anything else, the maintainer is
handing you a specific set of edits (they will describe them directly at the start of the session).
Those edits are the immediate task: apply them, keep the CI gate green after each change
(`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`), then commit and push
(pre-approved for this repo). Everything below is orientation and what remains once those edits are in.

**The status pane shipped as a centered popup modal.** It looks and behaves like herdr's own
`prefix+s` settings popup: a session-level `--placement popup` that herdr draws with a border +
"Check-in" title, holding the durable queue grouped into **CHECKIN** (`blocked`) / **DONE** (`done`)
sections, keyboard-navigable and reply-able, dismissed with `q`/`Esc` (the pane calls the
`popup.close` socket method on exit). It began as a 7-slice "triage overlay" build, then pivoted to
`popup` placement once the herdr source revealed it. **Verified live end-to-end in herdr 0.7.5.**

---

## 1. What this is

A herdr plugin: a **durable FIFO attention queue** for agent panes. herdr's native
jump-to-notification only reaches the toast currently on screen, so a ping is lost once the toast
fades, and simultaneous pings can't queue. This plugin remembers them — agents that go `blocked`
(need input) or `done` (finished) are enqueued; you jump to, or reply inline to, the oldest waiter
on demand.

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
- `src/lib.rs` (~160 lines) — the orientation page: argv dispatch (`run_from_env`/`run`), subcommand
  parsing, `RuntimeEnv`, and the `mod`/`pub(crate) use` wiring for everything below.
- `src/state.rs` (~306) — persisted state: `QueueEntry`, `WaitStatus`, `StateStore` (lock + atomic
  temp+rename write), `StateLock`, `read_state`/`write_state`/`load_entries`, `PluginError`. Owns
  the "all mutations via `StateStore::update`" invariant.
- `src/herdr.rs` (~430) — the herdr seam (`Herdr` trait / `CliHerdr`, `PaneInfo`) plus JSON parsing
  for `pane list` responses and plugin event payloads (`StatusEvent`). Trait methods: `pane_status_map`,
  `pane_infos`, `focus_agent`, `prompt_agent`, `show_notification`, **`popup_close`** (socket call).
- `src/queue.rs` (~215) — pure queue transitions (`enqueue`/`evict`/`is_live`) and the event handlers
  (`on_status_changed`/`on_focused`/`on_closed`). Must never depend on the `Herdr` trait (enforced
  by the module boundary now, not just a comment).
- `src/actions.rs` (~540) — the actions (`next`/`peek`/`clear`/`startup`), the toast copy they
  render, and `agent_label` (the display-name helper, shared by the list rows and the reply footer).
- `src/test_support.rs` (~165) — `#[cfg(test)]`-only shared fake `Herdr` + state fixtures. The
  `FakeHerdr` records `focused`, `prompts` (`(pane_id, text)`), and `notifications`, with
  `with_failing_focus`/`with_failing_prompt` toggles; `popup_close` is a no-op.
- `src/pane.rs` (~880) — the ratatui TUI (`PaneModel`, event loop, grouped view, mouse hit-testing),
  the **inline reply mode**, the **grouped CHECKIN/DONE render** (`layout_rows` → `Row::Spacer |
  Header | Entry`), and the **popup lifecycle** (close-on-exit + close-on-jump). Pure model code is
  unit-tested; the terminal loop is thin.
- `src/main.rs` — one-line entry into `lib::run_from_env`.
- `tests/cli.rs` — end-to-end tests that spawn the built binary against a fake `herdr` on
  `HERDR_BIN_PATH`.
- `herdr-plugin.toml` — manifest: `[[actions]]`, `[[events]]`, one `[[panes]]` (popup), `[[build]]`,
  `[[startup]]`.
- `scripts/open-pane.sh` — launcher for `open-pane`: opens the pane as a `popup` with
  `--width`/`--height` and `--env HERDR_CHECKIN_POPUP=1`. No toggle logic (a popup is a singleton).

## 3. Behavior + load-bearing invariants

**Behavior:**
- **Enqueue** on `agent_status` `blocked`/`done`; **evict** on return to `working`, on
  `pane.focused`, and on `pane.closed`. FIFO oldest-first; **deduplicated per pane** (a re-ping
  updates fields in place, keeping the original position + `enqueued_at_ms`).
- **`next`** focuses the oldest still-live waiter (`herdr agent focus <pane_id>`, cross-workspace)
  and evicts it **only after** the focus succeeds. **`peek`** shows the queue as a toast.
  **`clear`** empties it. **`startup`** re-seeds the queue from `pane list` after a herdr restart.
- **Status pane** keys: `j`/`k`/arrows or **left-click** move/select (in on-screen display order;
  headers and spacer rows are non-selectable), `Enter` jump+evict-on-success (**and closes the
  popup**), **`space` reply inline**, `d` drop, `c` clear-all (with a `y`/`n` confirm), `q`/`Esc`
  close (dismissing the popup). `open-pane` just opens it — a popup is a session-level singleton, so
  there is no open/focus/close toggle.
- **Grouped render:** `layout_rows` groups the FIFO queue into **CHECKIN** (`blocked`) then **DONE**
  (`done`) sections, each preceded by a blank `Row::Spacer` for visual separation, FIFO within each.
  Pure view over the ordered `Vec` — it never reorders `entries`. `selected` stays an index into
  `entries`; only `draw`/`row_for_click` learn the spacer+header offsets. The top line is just the
  count (herdr draws "Check-in" on the popup border).
- **Inline reply:** `space` opens a reply line for the selected waiter; you type an answer and
  `Enter` routes it into that agent's session via `herdr agent prompt <pane_id> <text>`, then evicts
  the entry **only on submit success** (a failed submit keeps it). `Esc`/click cancels; the reply's
  **target is captured when reply mode is armed**, so a concurrent queue refresh can't retarget it.
  Empty/whitespace `Enter` sends nothing and stays in reply mode.

**Invariants (do not regress — each has a regression test):**
1. **Mutations are deltas** through `StateStore::update` (read-modify-write under the lock), never a
   full model write-back. The pane polls while event binaries write concurrently; a stale write-back
   would clobber a fresh enqueue.
2. **Act first, evict on success only.** Applies to `next`, pane `Enter` (focus then evict), and
   inline reply (`on_reply_submit`: prompt then evict). A failed action keeps the entry — losing it
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
   when `HERDR_CHECKIN_POPUP` is set (the launcher sets it) — so a non-popup launch (e.g. a manual
   `herdr-checkin pane` invocation, or a future split/overlay launch) never closes an unrelated
   session popup.

## 4. herdr API facts (0.7.5, protocol 17)

- **Event JSON** (in `HERDR_PLUGIN_EVENT_JSON`): `{event, data:{type, pane_id, workspace_id,
  agent_status, agent, display_agent, title}}` — underscore forms. Manifest `on =` uses the dotted
  form (`pane.agent_status_changed`, `pane.focused`, `pane.closed`). Fields also accepted at the top
  level if `data` is absent.
- **Focus an agent pane:** `herdr agent focus <pane_id>` (jumps workspace/tab/pane). The CLI
  `herdr pane focus` is *directional* only; there is no by-id `pane.focus` CLI. **`agent focus` only
  accepts real *agent* panes** — targeting a plain shell returns `{"error":{"code":"agent_not_found"}}`.
- **Reply into an agent (USED by inline reply):** `herdr agent prompt <TARGET> <TEXT>`.
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
  - `herdr agent` also exposes `list`/`get`/`read`/`send-keys`/`wait`/`rename`/`start`.
- **Pane info:** `herdr pane list` → `result.panes[]` of `PaneInfo`. Fields we use: `pane_id`,
  `workspace_id`, `agent_status`, plus optional `agent`, `display_agent`, `title`. (Note: a **popup
  pane is NOT in `pane list`** — see the plugin-pane bullet.)
- **`[[startup]]` hook:** manifest is an array-of-tables with only `command` (required argv) +
  optional `platforms` — no `id`/`on`. Fires **once per server process** (cold start and live-handoff
  takeover). One-shot run-and-exit. Receives the normal plugin env plus `HERDR_PLUGIN_EVENT=startup`;
  **no pane payload** — the hook calls `pane list` itself. Spawned **async and not awaited**, so it
  races the live event loop (see invariant #4).
- **Plugin pane:** declared via `[[panes]]`; opened with `herdr plugin pane open --plugin <id>
  --entrypoint <pane-id> --placement <PLACEMENT> [--width W --height H] [--env K=V] --focus`. No push
  events to a running pane → poll. **`--placement` values: `overlay`, `split`, `tab`, `zoomed`,
  `popup`** — but the CLI `--help` **omits `popup`** (a stale clap value-parser in herdr
  `src/cli/spec.rs`); the real arg parser accepts and runs it. **We use `popup`:** a centered,
  session-modal float (like `prefix+s` settings), sized via `--width`/`--height` — an integer cell
  count or a `"NN%"` percent (herdr's `PopupSize`). On the **CLI** both parse; in the **manifest** a
  cell count must be a bare integer and a percent a `"NN%"` string. herdr centers it
  (`resolve_popup_geometry`, default 50%×50%) and draws the border + the `[[panes]]` `title` itself.
  **Load-bearing popup facts (verified in herdr source + live):** a popup is a **session-level
  singleton** (`AppState.popup_pane`) that is **not** in `pane list` or the API snapshot and has **no
  addressable pane_id**; a second open while one is up errors `plugin_pane_open_failed: "popup already
  open"`; it can only open while herdr's mode is `Terminal`; it **auto-closes when its process dies**
  (`AppEvent::PaneDied`). Dismiss it from inside via the **`popup.close`** socket method (there is no
  `herdr popup close` CLI): connect to `$HERDR_SOCKET_PATH`, write one newline-terminated line
  `{"id":<str>,"method":"popup.close","params":{}}` (herdr's socket protocol is newline-delimited
  JSON). The pane fires this on exit (gated on `HERDR_CHECKIN_POPUP`). **Background dim is
  herdr-core-only** — `render_popup_pane` never calls `dim_background`; only `Mode`-driven native
  modals dim (see §6 note 1). (`--placement overlay`, the pre-popup choice, is tab-scoped and persists
  on blur but is not a global modal — superseded by `popup`.)
- **Env a pane/handler receives:** `HERDR_PLUGIN_STATE_DIR`, `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`,
  `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_ID`.
  **Gotcha:** the id is percent-encoded in the state-dir path (`%41kram012388.checkin`). Always use
  the `HERDR_PLUGIN_STATE_DIR` env var — never construct the path. (This session it resolved to
  `~/.local/state/herdr/plugins/%41kram012388.checkin/state.json`, but that's an implementation
  detail — do not hardcode it in code.)
- **Toast:** `herdr notification show <title> [--body B] --sound none|request|done`.

## 5. Dev loop

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # what CI runs
cargo build --release
herdr plugin link "$PWD"                                   # register the local build (re-run after a manifest edit)
herdr plugin action invoke <next|peek|clear|open-pane> --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin        # inspect event/action/startup runs
```

**Manual E2E — seed the queue.** Seed `state.json` directly (find the path via the
`HERDR_PLUGIN_STATE_DIR` env — running any action once materializes it), then
`herdr plugin action invoke open-pane` to open the popup.

**Popup E2E is different from a normal pane (important gotcha).** A `popup` pane is a session-level
singleton — it is **not** in `pane list` and has **no addressable pane_id**, so you cannot
`herdr pane read`/`send-keys` it by id. To verify:
- **It opened:** the `open-pane` action's stdout is `{"type":"ok"}` (the popup path), not a
  `plugin_pane_opened` object with a pane id (that was the old overlay path). `pgrep -f
  "herdr-checkin pane"` confirms the TUI process is alive (leave it undisturbed — running other herdr
  CLI commands from another pane can steal focus and close it).
- **Test/close the close path yourself over the socket** (exactly what the pane fires on exit):
  connect to `$HERDR_SOCKET_PATH` and write `{"id":"x","method":"popup.close","params":{}}\n`.
  `"result":{"type":"ok"}` = a popup was open and is now closed; `"error":{"code":"popup_not_open"}`
  = none was open. (A tiny Python `AF_UNIX` client does this — see this session's transcript.)
- **The visual (centered/bordered) and keyboard dismissal are user-only** — a popup can't be
  pane-read, so confirming the modal look + `q`/`Esc` behavior needs a human at the terminal.

**Manual E2E of the inline-reply path** (needs a real agent, since `agent prompt` only accepts agent
panes): in a spare pane, launch `claude`; get its `pane_id` from `herdr agent list`; seed a queue
entry for it; open the popup; type your reply and press `Enter` **at the real keyboard** (you can't
`send-keys` a popup by id); confirm the reply landed in the agent and the entry was evicted. The
`startup` path can be exercised with a fake `herdr` on `HERDR_BIN_PATH` (see `tests/cli.rs`).

**Keybinds** live in the user's `~/.config/herdr/config.toml` (NOT the plugin): `prefix+alt+o` next,
`prefix+alt+p` peek, `prefix+alt+c` clear, `prefix+alt+q` open-pane. After editing:
`herdr config check && herdr server reload-config`. (No keybind is needed for `space` reply — that is
a pane-internal key.)

## 6. Next up (START HERE)

### 0. Pending maintainer edits — the immediate task
The maintainer has a specific set of edits to land and will describe them to you directly at the
start of the session. **Apply those first.** Keep the CI gate green after each change
(`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`), then commit/push
(pre-approved for this repo). Only once those are in should you weigh the items below.

### Release status
0.4.0 is **cut but not tagged**. All code/docs/version are in place on `main`. Tagging `v0.4.0` at
HEAD is the maintainer's call — **do NOT tag autonomously.** (Note: the pending edits above may add
commits on top of the current HEAD before any tag.)

### Two open follow-ups the maintainer flagged (both optional)
1. **Background-dim upstream PR (herdr-core, optional).** The popup does not dim the panes behind it
   the way `prefix+s` settings does. This is **not fixable from the plugin** — dimming is herdr-core-
   only (`render_popup_pane` in the herdr source never calls `dim_background`; only its `Mode`-driven
   native modals do, over `frame.area()`). Closing it means a small (~1-line) contribution to
   `ogulcancelik/herdr`: call `dim_background(frame, frame.area())` around `render_popup_pane`
   (`src/ui.rs`, ~the `render_popup_pane` call site), gated on `app.popup_pane.is_some()`. It only
   takes effect after merge + a herdr release + the user updating herdr — a slow external track. The
   maintainer chose to **ship 0.4.0 without the dim** (border + centering + title already read as a
   modal). Draft the PR only if the maintainer asks.
2. **Popup size tuning (plugin-side, trivial).** The popup is sized `width="60%" height="55%"` — set
   in **two places that must stay in sync**: the `[[panes]]` entry in `herdr-plugin.toml` (integer =
   cells, `"NN%"` string = percent) and the `--width`/`--height` flags in `scripts/open-pane.sh`.
   Retuning is a one-line edit in each; re-run `herdr plugin link "$PWD"` to pick up the manifest
   change.

### Parked / optional (all upstream-gated)
- **Global-summon popup** — the popup is summoned by a herdr keybind (`prefix+alt+q`) and is
  session-global, which is the value. A dedicated `prefix+s`-style global-summon binding for it is
  not wired; low priority.
- **Docs note** — herdr 0.7.5 made plugin install/enabled state global-per-user; only relevant if a
  per-session-install section is ever added to the README.

### How we got here (overlay → popup, one paragraph)
The feature was designed and built as a 7-slice "triage overlay" (`--placement overlay`, full-tab;
committed slices 1–7, cut as 0.4.0 at `1a618ae`). The maintainer then pushed back that it should look
like herdr's `prefix+s` settings, and — since herdr is OSS — a source study (clone → Sonnet researcher
→ Fable advisor) proved herdr has a **`popup`** placement giving the centered-modal look with **zero
herdr changes** (the CLI `--help` hides `popup` behind a stale value-parser). The pane was switched to
`popup` (`1d21213`): renamed the first section `AWAITING YOU`→`CHECKIN`, added blank-line group
spacing, slimmed the header (herdr draws "Check-in" on the border), added `popup.close`-on-exit +
close-on-jump, removed the now-dead open/focus/close toggle, bumped `min_herdr_version` to 0.7.5, and
shipped without the background dim. All verified live in herdr 0.7.5. Full per-slice history is in the
git log and `CHANGELOG.md`.

### Suggested skills
- **`/herdr`** — control herdr from inside it (`HERDR_ENV=1`): split panes, spawn/read agents, run
  `herdr agent prompt`/`focus`. The tool for any live E2E.
- **`demo-gif`** — regenerate the README demo gif (VHS; `scripts/pane-demo.tape`, no real agents).
- **Sonnet-5 subagents** for research/exploration and mechanical edits (maintainer preference: delegate
  mechanical work); a **Fable-5 advisor subagent** for a genuine load-bearing call, used sparingly.
- **`/handoff`** — to snapshot again at the end of the next session.

## 7. How we work here (see CLAUDE.md for the short version)

- **Model tiers:** Opus orchestrates (plan/decide/integrate/own correctness); Sonnet subagents do
  research, exploration, scoping, and mechanical implementation; a Fable subagent is the advisor for
  genuine doubt on load-bearing decisions — used sparingly.
- **Design gate before code, adversarial review after.** This keeps paying off: the `[[startup]]`
  race the Fable advisor caught → invariant #3; v0.2.0's clinical review found two ping-loss bugs;
  and the overlay→popup pivot came from taking a maintainer pushback seriously and reading the herdr
  source rather than trusting the CLI `--help`.
- **Verify foundations first.** Confirm an API contract with a throwaway probe or a source read before
  building on it — e.g. `agent prompt`'s target, and the `popup` placement's existence, wire shape,
  and lifecycle were all confirmed against herdr 0.7.5 source before the switch.
- **Tracer-bullet slices, each green.** Small commits that each keep `fmt + clippy -D warnings + test`
  passing. When Rust's dead-code gate would force a lint suppression on a caller-less seam, prefer
  merging the seam with its first real caller over sprinkling `allow(dead_code)`.
