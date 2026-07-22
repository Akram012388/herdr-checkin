# HANDOFF

Orientation for the next session (human or agent). **Read this first, then start on §6.**
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md). The feature's original (overlay-era) design that
later pivoted to a popup: [docs/triage-overlay-design.md](docs/triage-overlay-design.md).

**Version:** **0.4.0 — the triage-popup release — is cut** (version in `Cargo.toml` +
`herdr-plugin.toml` + `Cargo.lock`, CHANGELOG dated 2026-07-22). **NOT tagged** — the maintainer tags
on request. **The Agents-view work below is post-0.4.0 internal feature work — NOT in the CHANGELOG.**
· **License:** MIT · **Repo:** https://github.com/Akram012388/herdr-checkin · **State:** `main` is
green (`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` = **118 lib + 5
CLI tests**), pushed, tip **`1e8de9e`**. Working tree clean, no open branches/worktrees.

**START HERE (§6): the popup is now TWO tabs — the durable Queue + a live Agents roster, `Tab`/`Ctrl+S`
to toggle. Slices 0-3 of the Agents view shipped this session.** Next:
- **(a) Quick HITL confirm** — the Agents-view jump+reply E2E (#3/#4): open the popup, `Tab` to
  Agents, `Enter` to jump to a real agent and `space` to reply. Built + CI-green + visually confirmed,
  but that last live drive wasn't explicitly signed off. Confirm, then close #3/#4.
- **(b) Build Slice 4 / issue #5** — the last-line status column. Full plan in §6.

**What shipped this session (all on `main`; post-0.4.0, not in the CHANGELOG):**
- **Slice 2 (#3, `9bb25ce`)** — `Tab`/`Ctrl+S` toggle + a live Agents roster fed by a **`RosterSampler`
  worker thread**: samples `herdr agent list` once immediately then every ~1s over an **mpsc**; the
  250ms render tick drains it non-blocking (`try_recv`, newest-wins) so the CLI never runs on the tick;
  interruptible `recv_timeout` shutdown, **joined on `Drop`** (every exit path tears it down).
  `RuntimeEnv` gained **`herdr_bin_path`** so the worker builds its own `CliHerdr` (a borrowed
  `&dyn Herdr` is neither `Send` nor `'static`).
- **Slice 3 (#4, `f59ecc4`)** — full interaction parity in the Agents view: `j`/`k`/click selection
  (**anchored by `pane_id`** across the 1s refresh so the cursor never jumps), `space` reply via a
  shared **`arm_reply(pane_id,label)`** target, `Enter` jump via one shared `on_enter` (focus then
  evict-on-success, idempotent — a no-op when the agent wasn't queued). A **persistent tab bar** now
  tops BOTH views (active tab banded) + a dim **`tab · switch`** tooltip. This reversed the earlier
  "keep the Queue byte-identical" stance (maintainer wanted a clear, consistent indicator).
- **Names (`91a68f8`)** — the roster is **enriched** (`herdr::enrich_roster_labels`, best-effort) from
  `workspace list`/`tab list`/`pane list`, so rows read `home / ~ · pane 1` like herdr's own sidebar
  instead of raw ids. `RosterAgent` gained `workspace_label`/`tab_label`/`pane_label`.
- **Empty-queue default (`1e8de9e`)** — the popup opens on **Agents when the queue is empty**, else
  Queue (decided once in `PaneModel::new`; never re-evaluates — `tab` only moves via `toggle_tab`).
- **Architectural fork considered + parked** — "do we even need the Queue now the Agents view is
  dominant?" Fable-5 consulted; decision = **keep both tabs** + the empty-queue default. See §6.
- Maintainer live-confirmed the view/names/tooltip/empty-queue behavior at the terminal.

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
2. **A long-running TUI pane** (`pane` subcommand, ratatui + crossterm + tui-textarea). herdr spawns
   it as a centered popup (`--placement popup`) via a `[[panes]]` manifest entry. It has no push
   channel for events, so it **polls** `state.json` on a 250 ms tick. A popup is a session-level
   singleton, so `scripts/open-pane.sh` just opens it, and the pane dismisses its own popup
   (`popup.close`) on exit.

**State:** `state.json` under `HERDR_PLUGIN_STATE_DIR`, an ordered `Vec<QueueEntry>`, guarded by
`state.lock` (`fs2`). Writes are atomic temp+rename; reads outside a mutation take no lock. A
`QueueEntry` now carries the full identity for the location render:
`{pane_id, workspace_id, tab_id, workspace_label, tab_label, pane_label, agent, display_agent,
title, status, enqueued_at_ms, last_touched_ms}`. The four label/id location fields
(`tab_id`/`workspace_label`/`tab_label`/`pane_label`) are `#[serde(default)]` and `Option`, so old
`state.json` loads unchanged and fills them on the next refresh.

**Files** (`lib.rs` was split into cohesive modules; each holds its own `#[cfg(test)]` tests; `lib.rs`
re-exports items as `pub(crate)` so `crate::X` paths still resolve):
- `src/lib.rs` (~170) — argv dispatch (`run_from_env`/`run`), subcommand parsing, `RuntimeEnv` (now
  carries **`herdr_bin_path`** so the pane's roster sampler thread can build its own `CliHerdr`), the
  `mod`/`pub(crate) use` wiring. **The `status-changed` dispatch injects `herdr::enrich_location` as a
  closure** into `on_status_changed`, so the enqueue path can resolve identity from herdr while
  `queue.rs` stays free of the `Herdr` trait.
- `src/state.rs` (~320) — persisted state: `QueueEntry` (+ the four identity fields), `WaitStatus`,
  `StateStore` (lock + atomic write), `read_state`/`write_state`/`load_entries`, `PluginError`. Owns
  the "all mutations via `StateStore::update`" invariant.
- `src/herdr.rs` (~560) — the herdr seam. `Herdr` trait / `CliHerdr` / `PaneInfo` (+ `tab_id`,
  `label`); parsers `parse_pane_infos`, `parse_workspace_labels`/`parse_tab_labels` (shared
  `parse_id_label_map`), `parse_status_event`. Trait methods: `pane_status_map`, `pane_infos`,
  **`workspace_labels`**, **`tab_labels`**, **`agent_list`** (`parse_agent_list` over `herdr agent
  list`, then **`enrich_roster_labels`** fills each `RosterAgent`'s `workspace_label`/`tab_label`/
  `pane_label` from `workspace list`/`tab list`/`pane list` — best-effort, so the Agents view reads
  human names; a missed lookup degrades to ids), `focus_agent`, `prompt_agent`, `show_notification`,
  `popup_close`. **`enrich_location(&dyn Herdr, &mut StatusEvent)`** is the Queue's analogue for the
  enqueue path (fills `tab_id`+`pane_label`/`workspace_label`/`tab_label`), best-effort, never fails
  the enqueue. **NB:** the Agents-view sampler calls `agent_list` ~1s, so that's ~4 short herdr CLI
  spawns/sec off the render tick (agent+workspace+tab+pane list) — fine today; cache the label maps if
  it ever janks.
- `src/queue.rs` (~230) — pure queue transitions (`enqueue`/`evict`/`is_live`) and the event handlers
  (`on_status_changed`/`on_focused`/`on_closed`). **Never depends on the `Herdr` trait.**
  `on_status_changed(runtime, enrich: impl FnOnce(&mut StatusEvent))` runs `enrich` **before the
  lock** and **only when it will enqueue** (a `working` eviction pays for no lookups).
- `src/roster.rs` (~420) — the Agents-view roster's **pure core**. **Herdr-free like `queue.rs`**
  (invariant #6): types `AgentStatus` (idle/working/blocked/done/**unknown catch-all**), `RosterAgent`
  (now carries `workspace_label`/`tab_label`/`pane_label` — the enriched human names), `RosterSnapshot`,
  `WorkspaceGroup`. Functions: `group_by_workspace` + **`agents_in_display_order`** (the flattened
  grouped order the live view's selection cursor + click hit-testing both index into — they can't
  drift); the per-row formatters **`agent_destination`** (`{tab} · {pane}`, name-with-id fallback),
  **`agent_detail`** (`{status} · {title}`), **`workspace_display_label`** (the group header name),
  **`roster_reply_label`** (capitalized agent name for the reply footer); and `render_roster_text` (the
  hidden `roster` debug dump). `herdr.rs` parses + enriches into these; this module never touches the
  `Herdr` trait. `src/fixtures/agent_list.json` is a **pristine live capture** the `parse_agent_list`
  test `include_str!`s (one agent has no `agent_session`/`terminal_title` — the missing-session path,
  and the no-`agent_session` case that matters for the parked overlay's identity join, §6).
- `src/actions.rs` (~640) — the actions (`next`/`peek`/`clear`/`startup`), the toast copy, and the
  **row-render helpers**: `agent_label`, **`entry_destination`** (`{workspace} · {tab} · {pane}`,
  human-name-with-id-fallback), **`entry_detail`** (`{status} · {title} · {waited}`), and
  `describe_entry` (the one-line `destination · detail` join used by the `peek` toast). `startup`
  resolves all four identity fields from `pane list` + `workspace list` + `tab list`.
- `src/pane/` — the ratatui TUI, **a shell + three render surfaces**. Two tabs (`ActiveTab::{Queue,
  Agents}`) share one popup; `draw` dispatches on the active tab.
  - `pane/mod.rs` (~1700, mostly tests) — the **shell**: `run`/`event_loop`/tick, the pure `PaneModel`
    (now holds `tab`, `roster: Option<RosterSnapshot>`, `roster_selected`) + `ReplyDraft`, the
    `on_enter`/`on_drop`/`on_reply_submit`/`on_confirm_clear`/`on_mouse` handlers (all **tab-aware** —
    they dispatch on `model.tab`), `draw` + `draw_queue` + `draw_tab_bar`, and the **`RosterSampler`**
    (the worker thread + mpsc + `Drop`-join described in the header). `PaneModel::new` picks the opening
    tab (Agents if the queue is empty). `begin_reply` funnels both views through `arm_reply(pane_id,
    label)`; `move_up`/`move_down`/`on_enter`/`on_mouse` each branch Queue vs. Agents. The event loop
    handles `Tab`/`Ctrl+S` (toggle), `j`/`k`/`space`/`Enter` (both tabs), and `d`/`c` (Queue only).
  - `pane/queue_view.rs` (~470) — the durable Queue render (unchanged in substance). Two-line rows
    `Row::{Spacer,Header,Entry(i),Detail(i)}`, `entry_destination` bright + `entry_detail` dim, the
    **selection band** `SELECTION_BG` (`Color::DarkGray`, now `pub(super)`), `row_for_click`,
    `header_text`, `confirm_prompt`, and the scrollbar (`scrollbar_thumb`/`render_list_scrollbar`, now
    `pub(super)` so the Agents view reuses them).
  - `pane/agents_view.rs` (~300) — the **live Agents render** (sibling of `queue_view`). Its own
    `Row`/`layout_rows`/`row_for_click`/`draw_roster` mirror the queue's idiom (selection band, `> `
    cursor, scrollbar) but group by **workspace** with herdr's human names; rows = `agent_destination`
    over `agent_detail`; a placeholder distinguishes "Sampling agents..." (pre-first-sample) from "No
    agents running." Read-only never — reply/jump are wired through `mod.rs`.
  - `pane/compose.rs` (~150) — the inline-reply strip, shared by both views: `draw_compose` (`Reply to
    <label>` rule + `tui-textarea` field + `enter send · esc cancel` hint) and `dim_area` (the veil),
    both `pub(super)`.
  - **Testing foundation:** ratatui `TestBackend` snapshot tests in `pane/mod.rs` lock rendered
    *content* (empty/grouped queue, the tab bar row `Queue     Agents      tab · switch`, the compose
    strip, the grouped roster + cursor, the placeholders, the toggle) in CI with no herdr, plus model
    tests for tab-aware selection/reply/jump/anchoring. They trim horizontal styling on purpose (stays
    under live tuning; the maintainer confirms the pixel look at the terminal).
- `src/main.rs` — one-line entry into `lib::run_from_env`.
- `tests/cli.rs` — end-to-end tests that spawn the built binary against a fake `herdr` on
  `HERDR_BIN_PATH`.
- `herdr-plugin.toml` — manifest: `[[actions]]`, `[[events]]`, one `[[panes]]` (popup), `[[build]]`,
  `[[startup]]`.
- `scripts/open-pane.sh` — opens the pane as a `popup` with `--width 50% --height 50%` and
  `--env HERDR_CHECKIN_POPUP=1`. Keep `--width`/`--height` in sync with the `[[panes]]` manifest entry.

## 3. Behavior + load-bearing invariants

**Behavior:**
- **Enqueue** on `agent_status` `blocked`/`done`; **evict** on return to `working`, on `pane.focused`,
  and on `pane.closed`. FIFO oldest-first; **deduplicated per pane** (a re-ping updates fields in
  place, keeping position + `enqueued_at_ms`).
- **`next`** focuses the oldest still-live waiter (`herdr agent focus <pane_id>`) and evicts it **only
  after** the focus succeeds. **`peek`** shows the queue as a toast. **`clear`** empties it.
  **`startup`** re-seeds from `pane list` after a herdr restart.
- **Two tabs (`Tab`/`Ctrl+S` toggle), one popup.** A persistent **tab bar** tops both views (`Queue |
  Agents`, active tab banded, dim `tab · switch` tooltip). The popup **opens on Queue when there are
  waiters, else on Agents** (decided once in `PaneModel::new`; never re-evaluates). The toggle is a
  pure view switch — it never touches the popup lifecycle (invariant #5); each view keeps its own
  selection across a toggle.
- **Queue-view keys:** `j`/`k`/arrows or **left-click** move/select (headers/spacers non-selectable),
  `Enter` jump+evict-on-success (**and closes the popup**), **`space` reply inline**, `d` drop, `c`
  clear-all (`y`/`n` confirm), `q`/`Esc` close.
- **Agents-view keys (full parity, Slice 3):** live roster of **every** agent pane (all states),
  grouped by workspace with herdr's **human names** (`home / ~ · pane 1`), refreshed ~1s by the
  sampler thread. `j`/`k`/click select (**selection anchored by `pane_id`** so the 1s refresh never
  yanks the cursor); `space` reply and `Enter` jump target the selected roster agent through the SAME
  handlers as the Queue (`arm_reply`, `on_enter` — focus then evict-on-success, idempotent since the
  agent may not be queued). `d`/`c` are Queue-only (durable-queue ops). No time-in-state / last-line
  yet (Slices 4-5).
- **Row copy (the render — this session's redesign):** each waiter is **two lines** — the bright
  destination `{workspace} · {tab} · {pane}` then the dim detail `{status} · {title} · {waited}` —
  location-first, mirroring herdr's `prefix+g` go-to breadcrumb. Every segment prefers its human name
  and falls back to a positional id: workspace label → `workspace_id`; tab label → `t{N}` (from
  `tab_id`); pane manual label → `pane {N}` (from `pane_id`). Built once in `entry_destination` +
  `entry_detail` (actions.rs) so the pane rows, the `peek` toast (`describe_entry` joins them one-
  line), and the reply footer (`agent_label`) stay consistent. Colorless except the grey selection
  band.
- **Inline reply:** `space` opens the compose strip for the selected waiter; the reply's **target is
  captured when reply mode is armed** (a concurrent refresh can't retarget it). You type into a
  **`tui-textarea` single-line field** (full cursor editing); `Enter` routes the text into that
  agent's session via `herdr agent prompt <pane_id> <text>`, then evicts the entry **only on submit
  success**. Empty/whitespace `Enter` sends nothing and stays in reply mode. `Esc`/click cancels. A
  **paste** is inserted as one edit with newlines/tabs flattened to spaces. The strip dims the header
  + queue as one veil and keeps the grey band on the target.

**Invariants (do not regress — each has a regression test):**
1. **Mutations are deltas** through `StateStore::update`, never a full write-back (the pane polls
   while event binaries write concurrently).
2. **Act first, evict on success only.** `next`, pane `Enter`, and inline reply (`on_reply_submit`)
   all act then evict; a failure keeps the entry — losing it is the exact failure this plugin exists
   to prevent.
3. **Never prune an entry the liveness snapshot couldn't see.** `next`/`peek` keep any entry with
   `max(enqueued_at_ms, last_touched_ms) >= snapshot`.
4. **`startup` is additive-only** — merges each `blocked`/`done` pane through the same `enqueue`
   upsert (a delta under the lock), never a wholesale rewrite, never evicts.
5. **The popup dismisses itself only when it opened one** — `run()` calls `popup.close` on exit only
   when `HERDR_CHECKIN_POPUP` is set.
6. **`queue.rs` never depends on the `Herdr` trait** — identity resolution reaches it as an injected
   closure (`enrich`), not a `Herdr` call. The module boundary enforces it.

## 4. herdr API facts (0.7.5, protocol 17)

- **Event JSON** (`HERDR_PLUGIN_EVENT_JSON`): `{event, data:{type, pane_id, workspace_id,
  agent_status, agent, display_agent, title}}`. Manifest `on =` uses the dotted form
  (`pane.agent_status_changed`, `pane.focused`, `pane.closed`).
- **PANE IDENTITY (load-bearing for the row render; verified in herdr 0.7.5 source `c234f22`):**
  - IDs are positional: `workspace_id` = `w1`; `tab_id` = `w1:t2`; `pane_id` = `w1:p3`. **`pane_id`
    does NOT encode the tab** — the tab is only in `tab_id`.
  - **`herdr pane list`** per pane: `pane_id`, `workspace_id`, **`tab_id`**, **`label`** (manual pane
    label, usually null), `agent`, `title`, `display_agent`, `agent_status`, … We parse `pane_id`,
    `workspace_id`, `tab_id`, `label`, `agent_status`, `agent`, `display_agent`, `title`.
  - **`herdr workspace list`** per workspace: `workspace_id`, **`label`** (human name, e.g. `home`,
    `herdr-checkin`), `number`, … → `workspace_labels()`.
  - **`herdr tab list`** per tab: `tab_id`, **`label`** (usually the running program: `claude`,
    `codex`, `zsh`), `number`, … → `tab_labels()`.
  - **The event payload is LEANER than `pane list`** — it has NO `tab_id`, NO pane `label`, and the
    workspace/tab **names** live only in `workspace list`/`tab list`. So the everyday enqueue path
    resolves the four location fields via **three CLI lookups** (`enrich_location`), best-effort;
    `startup` and `next`/`peek` already call `pane list`.
- **Focus:** `herdr agent focus <pane_id>` (cross-workspace). Only accepts real *agent* panes.
- **Reply:** `herdr agent prompt <pane_id> <text>` — `<pane_id>` is what we store (`w4:p1` form; the
  `agent_session` uuid is rejected). Fire-and-forget ("submit accepted" is the eviction boundary).
  `blocked` is narrower than "waiting for me" (a Claude prose question ends its turn as `done`/idle),
  so the queue keys on `done` and relies on acknowledgment, never content-sniffing.
- **`[[startup]]` hook:** array-of-tables, `command` (+ optional `platforms`). Fires once per server
  process; run-and-exit; no pane payload (calls `pane list` itself); spawned async, races the live
  event loop (invariant #4).
- **Plugin pane / popup:** `herdr plugin pane open --plugin <id> --entrypoint <pane-id> --placement
  popup [--width W --height H] [--env K=V]`. A popup is a **session-level singleton**, not in
  `pane list`, no addressable `pane_id`; a second open errors `popup already open`; auto-closes when
  its process dies. Dismiss from inside via the **`popup.close`** socket method (newline-delimited
  JSON on `$HERDR_SOCKET_PATH`; there is no CLI verb). The pane fires it on exit (gated on
  `HERDR_CHECKIN_POPUP`). **Popup geometry is herdr-internal** — see §6 "Upstream-only".
- **Env a pane/handler receives:** `HERDR_PLUGIN_STATE_DIR`, `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`,
  `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_ID`. **Gotcha:** the
  id is percent-encoded in the state-dir path (`%41kram012388.checkin`). Always use the env var.
- **Toast:** `herdr notification show <title> [--body B] --sound none|request|done`.

## 5. Dev loop

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # what CI runs
cargo build --release
herdr plugin link "$PWD"                                   # re-run after a manifest edit
herdr plugin action invoke <next|peek|clear|open-pane> --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin        # inspect event/action/startup runs
```

**Probe the real pane identity** (from inside herdr, agents open across workspaces/tabs):
```sh
herdr pane list        # pane_id, workspace_id, tab_id, label (manual pane label)
herdr workspace list   # workspace_id -> label (human name)
herdr tab list         # tab_id -> label (program name)
```

**Verify the identity render end-to-end without the popup** (the data path is inspectable even though
the popup can't be pane-read): run the built binary's `status-changed` with a real event and real
`HERDR_BIN_PATH`, then read `state.json` — all four location fields should be resolved. Example that
resolved `tab_id: wN:t2`, `workspace_label: exp-v1`, `tab_label: codex` live:
```sh
HERDR_BIN_PATH=$(command -v herdr) HERDR_PLUGIN_STATE_DIR=<tmp> \
HERDR_PLUGIN_EVENT_JSON='{"data":{"pane_id":"wN:p2","workspace_id":"wN","agent_status":"done","agent":"codex","display_agent":"Codex","title":"ran the suite"}}' \
  ./target/release/herdr-checkin status-changed
```

**Seed the real queue to eyeball the popup.** The state dir is
`/Users/akram/.local/state/herdr/plugins/%41kram012388.checkin/` (find via the `HERDR_PLUGIN_STATE_DIR`
env; never build the path). Seed a few waiters through the binary as above (best: real `blocked`/`done`
panes), then open the popup with the `prefix+alt+q` keybind and eyeball. **`clear` needs
`HERDR_BIN_PATH` set** even though it doesn't use herdr (the CLI wires `CliHerdr` up front), so a bare
`clear` invocation errors — clear from inside the pane (`c`) or `prefix+alt+c` instead.

**Popup E2E is visual-only** — a popup is not in `pane list` and has no addressable `pane_id`, so you
cannot `pane read`/`send-keys` it. The modal look, the grey selection band, the two-line rows, the
placeholder shade, and the reply input's cursor keys / paste **must be eyeballed at the terminal**.
The close path can be tested over the socket (`{"id":"x","method":"popup.close","params":{}}\n` to
`$HERDR_SOCKET_PATH`).

**Keybinds** live in `~/.config/herdr/config.toml` (NOT the plugin): `prefix+alt+o` next,
`prefix+alt+p` peek, `prefix+alt+c` clear, `prefix+alt+q` open-pane. `space` reply is a pane-internal
key. After editing: `herdr config check && herdr server reload-config`.

## 6. Next up (START HERE)

### The Agents view — Slices 0-3 DONE, next is Slice 4. See [docs/agents-view-design.md](docs/agents-view-design.md).
The live **Agents view** roster now sits beside the durable Queue in the popup (`Tab`/`Ctrl+S`).
Slices 0-3 shipped this session (see the header for the commit-by-commit summary and `agents-view-
design.md` §8 for the full slice table). **Remaining work, in order:**

1. **HITL confirm (#3/#4), then close them.** The Agents-view jump+reply was built + CI-green +
   visually confirmed (names/tab bar/cursor/empty-queue default), but the live drive — `Enter` to jump
   to a real agent, `space` to reply — wasn't explicitly signed off. Open the popup, `Tab` to Agents,
   do both against a real agent (use `/herdr` to spawn/prompt one). If good, close #3 and #4. **The
   live pane runs `target/release/herdr-checkin`** — `cargo test` (debug) does NOT update it; `cargo
   build --release` before eyeballing.
2. **Slice 4 / issue [#5](https://github.com/Akram012388/herdr-checkin/issues/5)** — the last-line
   status column: a **2s** visible-rows `herdr agent read` sweep on the sampler thread, budgeted
   round-robin (~15 panes/sweep), invalidate-immediately-on-status-change, **never-blank cache**
   (a row keeps its last known line while a fresh read is in flight). Row becomes `{status} · {title}`
   → `{status} · {last terminal line}`. HITL: smooth with 5+ agents, lines track real output.
3. **Slice 5 / issue [#6](https://github.com/Akram012388/herdr-checkin/issues/6)** — `roster.json` +
   `RosterStore` (a **separate, prunable** store from `state.json` = **new invariant #7**: deleting it
   only degrades timers/pins, never a ping). Time-in-state is **stamped by the `status-changed` event
   binary** into the store (the pane isn't running to observe transitions — poll-loop tracking would
   fabricate `0s`); rows show `blocked 4m` / an honest `~` when unknown. Startup seeds additively
   (idempotence test). AFK-ish + a data-path test.
4. **Slice 6 / issue [#7](https://github.com/Akram012388/herdr-checkin/issues/7)** — pin-to-top,
   persisted by **`agent_session` uuid, not `pane_id`** (positional/reusable), with tombstone GC. Pins
   float to the top **of their workspace group**. Survives popup reopen + pane-slot reuse.

- **Tracker: GitHub issues [#1–#7](https://github.com/Akram012388/herdr-checkin/issues)** (Slice 0→#1
  … Slice 6→#7). **#1 (Slice 0)** stays open pending the maintainer's pixel-identical popup eyeball.
  #2 done. #3/#4 built (see step 1). **#5 is the next build.** This doc + the design doc are the
  durable in-repo tracker; the issues are the work queue.

### PARKED architectural fork — "dissolve the Queue view into the roster" (do NOT start without a re-decision)
The maintainer mused whether the Queue is still needed now the Agents view is dominant. Explored +
**Fable-5 consulted**; decision: **keep both tabs** + the empty-queue→Agents default (both shipped).
The unification idea is **deferred, not dead**, and slices 4-6 build its machinery anyway. If it's
ever revisited, the Fable guidance to honor (do not re-derive):
- **The backbone stays.** A 1s foreground poll CANNOT replace the durable event path: `herdr agent
  list` carries **no timestamps** (only `revision`/`state_change_seq`), and the plugin **has no
  daemon** — the event binaries are the only code running while the popup is closed (so wait-time,
  FIFO, "you never saw this `done`", and notifications are not reconstructible from polling). Kill the
  Queue *view* if anything, never the ledger + notifications.
- **#1 risk = identity join.** Key the durable ledger by **`agent_session` uuid, NOT `pane_id`**
  (positional + reusable → a new agent in a reused pane inherits a stale badge). Needs a fallback:
  our own fixture's `amp` agent has **no `agent_session`**. This is where "never lose a ping" dies
  silently — write the join rule as code with a fixture test in the very first slice.
- **Ordering = partition, don't pick a winner:** a pinned "Needs you (N)" FIFO section on top, the
  normal stable workspace-grouped roster below. And **freeze row re-ordering while a selection is
  active** (else the FIFO head moves between glance and keystroke → wrong-agent jump under load).
- **Acknowledgment asymmetry:** `done` → ack clears the badge; `blocked` → a dismissed badge on a
  still-blocked agent is a lie, so it clears only by *answering* (act-then-clear-badge-on-success — the
  same shape as today's act-then-evict, keep the tests). Keep ONE `Enter` (always jumps).
- **Reversible first step:** a READ-ONLY overlay on the Agents view (badge + `blocked 8m` wait-time +
  pinned needs-you section, `state.json`-sourced, **zero writes**, both tabs alive). Live a week; delete
  the Queue view only if you stop reaching for `Tab`. The ack/eviction rewiring lands BEFORE the view
  deletion, and port the invariant tests first.

### Continue polishing (maintainer's standing intent)
Small, tasteful, terminal-first QoL — the same bar as this session. Candidates, low-priority, pick by
feel and eyeball live: fine-tune the `SELECTION_BG` grey shade if it reads too dark/light; the reply
placeholder shade; the two-line indent. **Refresh `docs/pane-demo.gif`** — now badly stale: it
predates the two-line rows/grey band AND the entire Agents view (no tab bar, no roster, no `tab ·
switch`); a good refresh should show the `Tab` toggle and the live roster. Regenerate with the
**`demo-gif`** skill (VHS; `scripts/pane-demo.tape` + `scripts/pane-demo-setup.sh`, no real agents)
once the feature settles. **Deferred by the maintainer.** Two findings from a first pass, apply when
regenerating:
  1. **Seed needs the four identity fields.** `pane-demo-setup.sh`'s `state.json` predates the
     redesign — add `tab_id`/`workspace_label`/`tab_label` (leave `pane_label:null`) to each entry
     so the two-line destination renders human names (`api-server · claude · pane 1`) instead of
     sparse id fallbacks.
  2. **The tape's mid-sequence `Enter` now EXITS the pane.** A successful jump returns `Ok(())` and
     closes the pane regardless of popup mode (`on_enter` in `pane/mod.rs`), and the demo's fake `agent focus`
     always succeeds — so the current tape's `Enter` (between reply and `d`) drops to the shell and
     the following `d`/`c`/`y`/`q` leak there. Reorder so the jump is the FINAL action (end on it
     instead of `q`), or drop the jump step.

### Decisions — settled, do NOT relitigate
- **Popup modal is KEPT.** A dedicated split/tab pane (like herdr-file-viewer) was considered and
  **rejected** — it takes persistent screen space for too little, wrong for a summon-and-glance queue.
  The centered popup float is the intended design.
- **The reply input ceiling is single-line cursor editing** (`tui-textarea`). **No** `shift+enter`/
  multiline, **no** edit-in-`$EDITOR`, **no** reply history/draft-preservation. Rationale (Fable +
  maintainer): replies are short by design; anything substantial is composed by pressing **`Enter` to
  jump to the agent's real pane**, which is a full terminal with every editing binding for free. If
  richer editing is ever truly needed, we already have tui-textarea — enable more of it there; do not
  bolt on an external editor.

### Upstream-only (herdr core) — parked
- **Popup full-screen centering.** The maintainer wants the popup centered to the full terminal
  window; today it centers against `terminal_area` = the active tab's content region **minus the
  sidebar and tab-bar** (herdr `src/app/popup.rs:158`, `src/popup_size.rs:71`). **Not plugin-fixable**
  — `PluginPaneOpenParams` exposes only `placement`/`width`/`height`, no anchor/reference-rect. The
  popup's size is a % of that same rect, so it does **not** vary with pane splits (the "shifts with
  the pane" impression is sidebar/tab-bar variance across workspaces). Minimal upstream fix: thread a
  true full-frame rect into `spawn_popup_command` + `popup_pane_rects` (`src/ui/panes.rs`). Parked
  unless it bothers in daily use; if pursued, follow herdr's contributor gate (Discussion → approve →
  PR) like the background-dim item below.
- **Background dim behind the popup** — still **proposed as GitHub Discussion #1733** (Ideas):
  https://github.com/ogulcancelik/herdr/discussions/1733, awaiting a maintainer. `render_popup_pane`
  (herdr `src/ui/panes.rs`, ~L401-429) never calls the private `dim_background`; one-line fix is to
  add `super::dim_background(frame, area);` after the early-return guards, mirroring `settings.rs:44`.
  **herdr gates first-time external contributors** — a UI/feel change starts as a Discussion; an agent
  must NOT open an issue/PR on the maintainer's behalf until `/approve @Akram012388` lands. Their commit
  style: lowercase conventional, no emoji, no AI co-author line; don't touch root README/CHANGELOG in
  the PR; `just ci` = `cargo fmt --check` + `cargo nextest run`.

### Release status
0.4.0 is **cut but not tagged**. This session's polish (two-line rows, tui-textarea input, softer
highlights) is folded into the unreleased 0.4.0 CHANGELOG entry. Tagging `v0.4.0` is the maintainer's
call — **do NOT tag autonomously.**

### Suggested skills / helpers
- **`/herdr`** — control herdr from inside it (`HERDR_ENV=1`): split panes, spawn/read agents, run
  `herdr pane/workspace/tab list`, `agent prompt`/`focus`. The tool for any live probe or E2E.
- **`demo-gif`** — regenerate the README demo gif (VHS; no real agents).
- **Sonnet-5 subagents** for research/exploration and mechanical implementation (maintainer preference:
  delegate mechanical work; this session's empty-queue-default was a clean Sonnet implementation from a
  tightly-scoped spec). A **Fable-5 advisor** for genuine load-bearing calls, used sparingly (this
  session: the "do we still need the Queue?" architecture consult — see §6's parked fork).
- **`/handoff`** — to snapshot again at the end of the next session.

## 7. How we work here (see CLAUDE.md for the short version)

- **Model tiers:** Opus orchestrates (plan/decide/integrate/own correctness); Sonnet subagents do
  research, exploration, scoping, and mechanical implementation (hand them a tightly-scoped recorded
  spec — see this session's `tui-textarea` swap); a Fable subagent is the advisor for genuine doubt on
  load-bearing decisions — used sparingly.
- **Design gate before code, adversarial review after.** Keeps paying off: the row redesign went
  probe → maintainer-aligned format (via previews) → build; the input scope went to a Fable advisory
  before a line was written; the popup-vs-pane and popup-centering questions were resolved by reading
  herdr source, not guessing.
- **Verify foundations first.** Confirm an API contract with a throwaway probe or a source read before
  building on it — the identity fields, the popup geometry, and tui-textarea's ratatui-0.29 fit were
  all confirmed before use.
- **Tracer-bullet slices, each green.** Small changes that each keep `fmt + clippy -D warnings + test`
  passing.
- **Eyeball the popup live.** It can't be pane-read — the two-line rows, the grey band, the input
  cursor/paste all need a human at the terminal. The data path (state.json) is inspectable; the visual
  is not.
- **Respect third-party contribution norms.** For upstream herdr work, its `CONTRIBUTING.md` +
  `AGENTS.md` govern (external-contributor gate; agents don't open issues/PRs on the human's behalf; no
  emoji, no AI co-author lines). See the background-dim + popup-centering notes.
