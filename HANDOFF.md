# HANDOFF

Orientation for the next session (human or agent). **Read this first, then start on §6.**
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md). The feature's original (overlay-era) design that
later pivoted to a popup: [docs/triage-overlay-design.md](docs/triage-overlay-design.md).

**Version:** **0.4.0 — the triage-popup release — is cut** (version in `Cargo.toml` +
`herdr-plugin.toml` + `Cargo.lock`, CHANGELOG dated 2026-07-22). **NOT tagged** — the maintainer tags
on request. This session's polish lands on top of the untagged 0.4.0 (folded into its CHANGELOG
entry). · **License:** MIT · **Repo:** https://github.com/Akram012388/herdr-checkin · **State:**
`main` is green (`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` =
**79 lib + 5 CLI tests**) and pushed. No open branches/worktrees, working tree clean.

**START HERE (§6): one known bug to fix, then continue polishing.**
- **`ctrl+u` in the reply bar does not clear the whole line to the left of the cursor** (it should be
  delete-to-line-start). Noted live by the maintainer; **deferred to this session on purpose.** Start
  from §6 Task A.

**What shipped this session (all folded into the untagged 0.4.0).** The status pane stays a centered
**popup modal** (herdr `--placement popup`) — that direction is settled (see §6 "Decisions"). On top
of it this session:
- **Rows are location-first and two lines each**, mirroring herdr's own `prefix+g` go-to picker: a
  bright **destination** line `{workspace} · {tab} · {pane}` over a dim **detail** line
  `{status} · {title} · {waited}`. Human names with positional-id fallback at every segment.
- **The reply input is now `tui-textarea`** (single-line) — real cursor editing (`ctrl+a`/`ctrl+e`,
  arrows, `ctrl+w`/`ctrl+k`, correct unicode) + **bracketed paste** with newline-flattening.
- **Softer highlights**: selection is a soft grey band (not reversed video); the reply target keeps
  the band while composing; the footer hint bar is centered; the placeholder is faint + non-italic.
- Verified live against herdr 0.7.5: the identity fields resolve correctly (`state.json` inspected);
  the visuals were eyeballed by the maintainer at the terminal.

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
- `src/lib.rs` (~160) — argv dispatch (`run_from_env`/`run`), subcommand parsing, `RuntimeEnv`, the
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
  list` into `roster::RosterAgent` — the Agents view), `focus_agent`, `prompt_agent`, `show_notification`,
  `popup_close`. **`enrich_location(&dyn Herdr, &mut StatusEvent)`** fills `tab_id`+`pane_label` (from
  `pane list`), `workspace_label` (from `workspace list`), `tab_label` (from `tab list`) — best-effort,
  never fails the enqueue.
- `src/queue.rs` (~230) — pure queue transitions (`enqueue`/`evict`/`is_live`) and the event handlers
  (`on_status_changed`/`on_focused`/`on_closed`). **Never depends on the `Herdr` trait.**
  `on_status_changed(runtime, enrich: impl FnOnce(&mut StatusEvent))` runs `enrich` **before the
  lock** and **only when it will enqueue** (a `working` eviction pays for no lookups).
- `src/roster.rs` (~230) — the Agents-view roster (Slice 1). **Herdr-free like `queue.rs`** (invariant
  #6): pure types `AgentStatus` (idle/working/blocked/done/**unknown catch-all**), `RosterAgent`,
  `RosterSnapshot`, `WorkspaceGroup`; `group_by_workspace` (encounter order; the pins hook for Slice 6)
  and `render_roster_text` (the hidden `roster` debug dump). `herdr.rs` parses into these; this module
  never touches the `Herdr` trait. `src/fixtures/agent_list.json` is a **pristine live capture** of
  `herdr agent list` that the `parse_agent_list` test `include_str!`s (one agent listed live with no
  `agent_session`/`terminal_title`, so the missing-session path is covered by real data).
- `src/actions.rs` (~640) — the actions (`next`/`peek`/`clear`/`startup`), the toast copy, and the
  **row-render helpers**: `agent_label`, **`entry_destination`** (`{workspace} · {tab} · {pane}`,
  human-name-with-id-fallback), **`entry_detail`** (`{status} · {title} · {waited}`), and
  `describe_entry` (the one-line `destination · detail` join used by the `peek` toast). `startup`
  resolves all four identity fields from `pane list` + `workspace list` + `tab list`.
- `src/pane/` — the ratatui TUI, **split into a shell + two render surfaces (Slice 0)** so the coming
  Agents view is a sibling module, not more weight in the loop:
  - `pane/mod.rs` (~1100, mostly tests) — the **shell**: `run`/`event_loop`/tick, the pure
    `PaneModel` + `ReplyDraft`, the `on_enter`/`on_drop`/`on_reply_submit`/`on_confirm_clear`/
    `on_mouse` handlers, and the top-level `draw` layout. The event loop intercepts
    `Esc`/`Enter`(+`ctrl+m`)/`ctrl+u`/`ctrl+c` and feeds everything else to `reply_input`;
    `Event::Paste` → `reply_paste` (flatten control chars). Bracketed paste is enabled/disabled in
    `run()` alongside mouse capture.
  - `pane/queue_view.rs` (~470) — the durable Queue render. **Two-line rows:**
    `Row::{Spacer,Header,Entry(i),Detail(i)}` — `layout_rows` emits `Entry` then `Detail` per waiter
    (one `Row` per painted line, so the scrollbar/click math is unchanged); `draw_list` renders
    `entry_destination` bright on the `Entry` line and `entry_detail` dim+indented on the `Detail`
    line, with the **selection band** `SELECTION_BG` (`Color::DarkGray`) on both lines of the focused
    entry (live selection while navigating, captured reply target while composing — survives the
    compose dim veil). Owns `row_for_click`, `header_text`, `confirm_prompt`, and the scrollbar.
  - `pane/compose.rs` (~150) — the inline-reply strip: `draw_compose` (the `Reply to <label>` rule +
    the `tui-textarea` field + right-aligned `enter send · esc cancel` hint) and `dim_area` (the veil).
  - **Testing foundation:** ratatui `TestBackend` snapshot tests in `pane/mod.rs` lock the rendered
    *content* (empty queue, grouped CHECKIN/DONE sections, the compose strip, the `> ` cursor) in CI
    with no herdr. They trim horizontal styling (centering, band, dim/bold) on purpose — that stays
    under live tuning; the maintainer confirms the pixel look at the terminal.
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
- **Status-pane keys:** `j`/`k`/arrows or **left-click** move/select (headers/spacers/detail lines are
  non-selectable to `j`/`k`, but a click on either row of an entry selects it), `Enter` jump+evict-on-
  success (**and closes the popup**), **`space` reply inline**, `d` drop, `c` clear-all (`y`/`n`
  confirm), `q`/`Esc` close.
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

### BIG FEATURE (priority) — the Agents view. See [docs/agents-view-design.md](docs/agents-view-design.md).
The next evolution: add a live **Agents view** roster beside the durable queue in the popup, mirroring
Claude Code's agent view but powered by herdr primitives. **Fully aligned with the maintainer**
(research → interview → Fable advisory, 2026-07-22) and specced end-to-end in
`docs/agents-view-design.md` — read it first. In brief:
- Two views in one popup, `Tab`/`Ctrl+S` toggle: **Queue** (durable, unchanged) + **Agents** (live).
- Agents lists **every** detected agent pane (all states), grouped by workspace; row status = the last
  terminal line via `agent read`; time = **time-in-state stamped by the `status-changed` event binary**
  (the pane isn't running to observe transitions — poll-loop tracking would fabricate zeros).
- Actions: `Enter` jump, `space` reply. **Peek deferred; reorder is pin-to-top only.**
- **Surface: the popup modal is KEPT** (re-ruled under the live-roster premise, not the old
  triage-queue one — Fable, design doc §9). Not a dedicated pane/tab: the Agents view is a
  *switchboard* (cross-workspace consolidation on demand, rows are jump targets), which is a summon
  job; herdr toasts + the workspace itself already own ambient awareness. Two refinements fold in:
  (a) **ratatui `TestBackend` snapshot tests** (Slice 0/2) to retire the eyeball-only QA pain;
  (b) treat popup **width/height as a tunable** — bump 50%×50% toward ~85–90% at Slice 2 if the roster
  reads cramped. Surface is a `--placement` flag, so the choice stays cheaply reversible.
- Live data via a **worker thread** (never CLI on the render tick); `roster.json` is a **separate,
  prunable** store from `state.json` (new **invariant #7**).
- **Build order (tracer-bullet, each green + eyeballed):** ~~Slice 0 split `pane.rs`~~ **DONE** →
  ~~1 data seam + `roster.rs` + hidden `roster` debug cmd~~ **DONE** → 2 tab toggle + read-only live
  roster — the tracer bullet (**START HERE, issue #3**) → 3 jump/reply parity → 4 last-line column →
  5 `roster.json` + time-in-state → 6 pin-to-top. Full slice table in the design doc §8.
- **Per-slice tracker: GitHub issues [#1–#7](https://github.com/Akram012388/herdr-checkin/issues)**
  (Slice 0→#1 … Slice 6→#7), dependency-ordered, labeled `ready-for-agent` (AFK: #1/#2/#6) or
  `ready-for-human` (HITL live-eyeball: #3/#4/#5/#7). This doc + the design doc are the durable
  in-repo tracker; the issues are the work queue. **Start with #1** (Slice 0, AFK, no blockers).

### DONE this session (folded into untagged 0.4.0)
- **`ctrl+u` in the reply bar** now clears to line-start. Root cause: tui-textarea 0.7 binds `ctrl+u`
  to `undo` (delete-to-head is on `ctrl+j`), so the old binding did nothing expected. Fix: intercept
  `Char('u')+CONTROL` in the reply branch → `delete_line_by_head` (`pane.rs`), unit-tested. **Watch out:**
  the live pane runs `target/release/herdr-checkin` — a `cargo test` (debug) does NOT update it; rebuild
  `--release` before eyeballing.
- **CI clippy break fixed** — `collapsible_match` in the `Event::Paste` arm (`pane.rs`) had shipped on
  `6f6f9fc`; `main` is green again.

### Continue polishing (maintainer's standing intent)
Small, tasteful, terminal-first QoL — the same bar as this session. Candidates, low-priority, pick by
feel and eyeball live: fine-tune the `SELECTION_BG` grey shade if it reads too dark/light; the reply
placeholder shade; the two-line indent. **Refresh `docs/pane-demo.gif`** — it predates all of this
session's changes (old single-line reply footer, no two-line rows, no grey band); regenerate with the
**`demo-gif`** skill (VHS; `scripts/pane-demo.tape` + `scripts/pane-demo-setup.sh`, no real agents)
once the polish settles. **Deferred by the maintainer until all polish is done.** Two findings from
a first pass, apply when regenerating:
  1. **Seed needs the four identity fields.** `pane-demo-setup.sh`'s `state.json` predates the
     redesign — add `tab_id`/`workspace_label`/`tab_label` (leave `pane_label:null`) to each entry
     so the two-line destination renders human names (`api-server · claude · pane 1`) instead of
     sparse id fallbacks.
  2. **The tape's mid-sequence `Enter` now EXITS the pane.** A successful jump returns `Ok(())` and
     closes the pane regardless of popup mode (`pane.rs` ~L160), and the demo's fake `agent focus`
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
  delegate mechanical work; this session's tui-textarea swap was a Sonnet implementation from a recorded
  spec). A **Fable-5 advisor** for genuine load-bearing calls, used sparingly (this session: the input-
  scope call).
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
