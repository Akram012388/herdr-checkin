# HANDOFF

Orientation for the next session (human or agent). **Read this first, then start on §6.**
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md). The feature's original (overlay-era) design that
later pivoted to a popup: [docs/triage-overlay-design.md](docs/triage-overlay-design.md).

**Version:** **0.4.0 — the triage-popup release — is cut** (version in `Cargo.toml` +
`herdr-plugin.toml` + `Cargo.lock`, CHANGELOG dated 2026-07-22). **NOT tagged** — the maintainer tags
on request. **All the Agents-view work below is post-0.4.0 internal feature work — NOT in the
CHANGELOG.** · **License:** MIT · **Repo:** https://github.com/Akram012388/herdr-checkin · **State:**
`main` is green (`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` =
**154 lib + 6 CLI tests**), pushed, tip **`7c21b1b`**. Working tree clean.

**START HERE (§6): the popup is TWO tabs — the durable Queue + a live Agents roster, `Tab`/`Ctrl+S` to
toggle. Slices 0-5 are ALL DONE (#2/#3/#4/#5/#6 CLOSED) — that is the whole planned Agents-view build.
There is NO queued build; the remaining work is small polish (see §6).**

**What shipped THIS session (Slice 4 / issue #5 — last-line status column; DONE + HITL-eyeballed):**
- **`roster::last_terminal_line`** (pure, Herdr-free) — an agent's last output line from a `herdr
  agent read --source recent --format text` snapshot, read **bottom-up, skipping the rendered UI
  chrome block**: box borders (incl. embedded-title rules like `── slice-4 ──`), padded box sides
  (`│  …  │`), the `❯` prompt, the Claude Code status bar / footer, the running token counter, and
  **both** spinner frames — `✽ Finagling… (… tokens)` and the token-less `✳ Sautéed for 4m 28s` (every
  frame opens with a sparkle glyph U+2722..U+2749, **disjoint from** the content markers `✓`/`⏺` a real
  line carries). Returns `None` when only chrome is visible. Fixture tests over a **real amp capture**
  + crafted Claude tails (blocked question; spinner-skip). **Best-effort, expected to iterate** on the
  chrome vocabulary as new agents/frames appear (`src/fixtures/agent_read_*.txt`).
- **`herdr::TailCache`** — a second cache on the sampler thread beside `LabelCache`: a budgeted
  (`TAIL_READ_BUDGET=10`) `agent read` sweep, **status-changed panes first then round-robin** (a large
  fleet can't stall the worker), **never-blank** (a read miss / chrome-only screen keeps the last known
  line; a vanished pane is pruned) — a prunable observation cache, invariant #7. Reads run **only on
  the worker**, never the render tick, and are **skipped on the first sample** so the roster still
  paints instantly (preserves last session's instant-load win).
- **`RosterAgent.last_line`** carries it; **`agent_detail`** prefers it over the terminal title
  (`blocked 4m · Good to proceed?`), falling back to the title until the first read lands. **154 lib +
  6 CLI tests** (12 new). Also **hardened the fake-herdr test** against a Linux `ETXTBSY` flake (a
  `__warmup__` exec-readiness probe — test-only, no production change). Maintainer eyeballed live
  ("good job"); **#5 closed**. Two live findings folded into the code: amp reads its last message
  cleanly over its empty input box; a working Claude row shows its last settled line, not the spinner.

**What shipped THIS session (2026-07-23; Slice 5 / issue #6 — time-in-state):**
- **New `roster_state.rs` = `RosterStore`** — a **separate store from `state.json`** (`roster.json` +
  `roster.lock`, delta-under-lock temp+rename, the `StateStore` twin). Registry keyed by `pane_id`:
  `{agent_session, status, status_since_ms, first_seen_ms, last_seen_ms}` (`BTreeMap`, deterministic).
- **Provenance is event-stamped (design §4).** The `status-changed` **event binary** stamps
  `status_since_ms` on every transition (best-effort, after the queue mutation, wired in `lib.rs`'s
  dispatch so `queue.rs` never learns roster exists — fires for **every** status, not just waiters).
  `startup` seeds **additively** (invariant #4 — `or_insert`, idempotent). The **pane sampler only
  reads + back-fills the session uuid** and resets the timer **only on a reused pane slot** (uuid
  mismatch), never fabricating a transition time from a status difference (the poll-loop "0s" trap).
- **Rows now show `blocked 4m`** — `roster.rs` gained `RosterAgent::status_since_ms` (filled by the
  sampler's `reconcile_roster`), pure `format_age`/`time_in_state` (unknown → honest **`~`**), and
  `agent_detail` folds the age in. The sampler thread does all `roster.json` IO off the render tick
  (`state_dir` threaded into `RosterSampler`).
- **New invariant #7** (see §3) + its delete-`roster.json`-and-everything-works test; a real-binary
  data-path CLI test (`status-changed` stamps `roster.json`); startup-idempotence + zero-`state.json`-
  writes tests. **Not yet HITL-eyeballed** — the age numbers want a look with live agents, but the
  data path is fully tested. `cargo build --release` done (the live pane runs the release binary).

**What shipped THIS session (2026-07-23, on `main`, tip `fd92653`; reply-input + load-perf polish):**
- **Closed #3 + #4** — the Agents-view jump+reply E2E was HITL-confirmed live at the terminal (Tab to
  Agents, `Enter` jumps to a real agent + closes the popup, `space` reply routes into its session).
- **Reply input soft-wraps (`compose::draw_input`)** — the compose input is now **3 rows**
  (`compose::INPUT_ROWS`) and **soft-wraps the single logical line** at the popup edge instead of
  scrolling off to the right; **Up/Down walk the wrapped rows** keeping the visual column
  (`reply_cursor_vertical` → `compose::cursor_move_vertical`). **Single-line SEND semantics preserved**
  (`Enter` still sends, no newline is stored or sent) — a display improvement inside the settled
  "single-line ceiling" decision, NOT a multi-line composer. The `TextArea` stays the untouched text
  model (all paste-flatten/`ctrl+u`/send tests unchanged); `compose` paints the wrapped rows +
  placeholder + block caret itself. New pure helpers `wrap_line`/`caret_row_col`/`cursor_move_vertical`
  (unit-tested). Footer `Enter jump` → lowercase `enter jump`.
- **Agents view loads INSTANTLY (was ~1s)** — the roster was `None` at first draw, so rows lagged
  behind a sampler round-trip plus a 250ms tick. **Fable-5-advised**, two fixes:
  1. **Bounded first paint** — the pane draws the shell immediately, then does ONE bounded 200ms
     `recv_timeout` (`RosterSampler::recv_latest_within`, `FIRST_SAMPLE_WAIT`) for the sampler's
     immediate first sample **before** the loop, so rows appear WITH the popup. The CLI still never
     runs on the render thread; the wait is **skipped on the Queue tab** (roster off-screen).
  2. **`LabelCache` on the sampler thread** cuts steady state from **4 → 1 subprocess spawns/sec** —
     the workspace/tab/pane name maps are refetched only when a **new agent pane appears** (it must get
     its human name in the sample that first shows it) or on a **~15s periodic refresh** (renames). Adds
     the un-enriched **`agent_roster()`** trait seam, the pure **`apply_labels`**, and **`sample_roster`**
     + `LabelCache::needs_refresh` (both unit-tested). Fable REJECTED adaptive backoff (stale roster
     exactly when you Tab to it) and snapshot-diffing (negligible memory, adds bug surface).
- Maintainer confirmed both live ("works perfect now").

**What shipped the PRIOR session (Slices 0-3, on `main`; context):**
- **Slice 2 (`9bb25ce`)** — `Tab`/`Ctrl+S` toggle + a live Agents roster fed by a **`RosterSampler`
  worker thread** over an **mpsc** the 250ms render tick drains non-blocking (CLI never on the tick);
  interruptible `recv_timeout` shutdown, **joined on `Drop`**. `RuntimeEnv` gained **`herdr_bin_path`**
  so the worker builds its own `CliHerdr` (`&dyn Herdr` is neither `Send` nor `'static`).
- **Slice 3 (`f59ecc4`)** — full interaction parity: `j`/`k`/click selection (**anchored by `pane_id`**
  across the 1s refresh), `space` reply via shared **`arm_reply(pane_id,label)`**, `Enter` jump via
  shared `on_enter` (focus then evict-on-success, idempotent). A **persistent tab bar** tops BOTH views
  + a dim **`tab · switch`** tooltip.
- **Names (`91a68f8`)** — the roster reads `home / ~ · pane 1` (human names), enriched from
  `workspace/tab/pane list`; `RosterAgent` carries `workspace_label`/`tab_label`/`pane_label`.
- **Empty-queue default (`1e8de9e`)** — the popup opens on **Agents when the queue is empty**, else
  Queue (decided once in `PaneModel::new`; never re-evaluates).
- **Architectural fork considered + parked** — "do we even need the Queue now?" Fable-5 consulted;
  decision = **keep both tabs** + the empty-queue default. See §6.

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
- `src/roster_state.rs` (~600 incl. tests) — the **Slice 5 registry store**: `RosterStore` (the
  `StateStore` twin on **`roster.json` + `roster.lock`**, delta-under-lock temp+rename), `RegistryEntry`,
  `Registry` (`BTreeMap<pane_id, _>`). Pure mutators: **`stamp_status`** (event-binary transition
  stamp — resets `status_since_ms` only on a real status change), **`seed_status`** (startup, additive
  `or_insert`), **`reconcile_pane`** (sampler: back-fill uuid, reset only on a reused-slot uuid
  mismatch, return the trusted since). Runtime bridges (all **best-effort**, invariant #7):
  **`stamp_status_changed`** (parses the event, called from `lib.rs` after the queue mutation),
  **`seed_registry`** (from `startup`'s `pane list`), **`reconcile_roster`** (on the sampler thread —
  fills each `RosterAgent::status_since_ms`, the only production `roster.json` writer). Owns invariant
  #7. `load_registry` is `#[cfg(test)]` (prod reads happen inside `reconcile_roster`'s locked update).
- `src/herdr.rs` (~640) — the herdr seam. `Herdr` trait / `CliHerdr` / `PaneInfo` (+ `tab_id`,
  `label`); parsers `parse_pane_infos`, `parse_workspace_labels`/`parse_tab_labels` (shared
  `parse_id_label_map`), `parse_status_event`. Trait methods: `pane_status_map`, `pane_infos`,
  **`workspace_labels`**, **`tab_labels`**, **`agent_roster`** (parse-only, un-enriched — one spawn),
  **`agent_list`** (= `agent_roster` + **`enrich_roster_labels`**, the four-spawn one-shot used by the
  hidden `roster` debug cmd), `focus_agent`, `prompt_agent`, `show_notification`, `popup_close`.
  **Label-cache path (this session):** **`sample_roster(&dyn Herdr, &mut LabelCache)`** is what the
  Agents-view sampler calls — un-enriched `agent_roster` (one spawn) enriched from a cached
  `LabelCache`; the three label-map spawns fire only on a **membership change** (`LabelCache::needs_refresh`
  — a new agent pane appeared) or the **`LABEL_REFRESH_EVERY` (15-sample ≈15s) periodic refresh**, so
  steady state is **1 spawn/sec, not 4**. **`apply_labels(workspaces, tabs, pane_labels, roster)`** is
  the pure enrich shared by `enrich_roster_labels` and `sample_roster`. **`enrich_location(&dyn Herdr,
  &mut StatusEvent)`** is the Queue's analogue for the enqueue path, best-effort, never fails the
  enqueue.
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
  - `pane/mod.rs` (~1800, mostly tests) — the **shell**: `run`/`event_loop`/tick, the pure `PaneModel`
    (holds `tab`, `roster: Option<RosterSnapshot>`, `roster_selected`) + `ReplyDraft` (now carries
    `wrap_width: Cell<u16>`, set by `draw_input` so Up/Down wrap exactly as the render did), the
    `on_enter`/`on_drop`/`on_reply_submit`/`on_confirm_clear`/`on_mouse` handlers (all **tab-aware**),
    `reply_cursor_vertical` (Up/Down over the wrapped reply line), `draw` + `draw_queue` +
    `draw_tab_bar`, and the **`RosterSampler`** (worker thread + mpsc + `Drop`-join; `drain_latest`
    non-blocking each tick + **`recv_latest_within`** for the one bounded first-paint wait). The event
    loop does a **bounded first paint** (draw shell, then `recv_latest_within(FIRST_SAMPLE_WAIT)` when
    opening on Agents) before the loop, and the sampler thread owns a **`LabelCache`** feeding
    `sample_roster`. `PaneModel::new` picks the opening tab (Agents if the queue is empty). `begin_reply`
    funnels both views through `arm_reply(pane_id, label)`. The event loop handles `Tab`/`Ctrl+S`
    (toggle), `j`/`k`/`space`/`Enter` (both tabs), reply-mode `Up`/`Down`/`Enter`/`Esc`/`ctrl+u`, and
    `d`/`c` (Queue only).
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
  - `pane/compose.rs` (~300) — the inline-reply strip, shared by both views: `draw_compose` (`Reply to
    <label>` rule + the soft-wrapping input + `enter send · esc cancel` hint) and `dim_area` (the veil).
    **`draw_input`** paints the single logical reply line **soft-wrapped** across `INPUT_ROWS` (=3) rows
    with a manual reverse-video block caret and a dim placeholder — the `TextArea` is only the text
    model, not the renderer. Pure, unit-tested helpers: **`wrap_line`** (char-wrap by display width,
    never splitting a glyph; rows partition the line exactly so a caret char-index maps to a row and
    back), **`caret_row_col`**, and **`cursor_move_vertical`** (Up/Down one wrapped row, keeping the
    visual column). Send semantics are unchanged: nothing here inserts a newline, `Enter` still sends.
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
  **`tui-textarea`-backed field** with full cursor editing; the text is **one logical line that
  soft-wraps** across the 3-row input (`Up`/`Down` walk the wrapped rows) — `Enter` still **sends** (no
  newline is ever stored or sent), routing the text into that agent's session via `herdr agent prompt
  <pane_id> <text>`, then evicting the entry **only on submit success**. Empty/whitespace `Enter` sends
  nothing and stays in reply mode. `Esc`/click cancels. A **paste** is inserted as one edit with
  newlines/tabs flattened to spaces. The strip dims the header + queue as one veil and keeps the grey
  band on the target.
- **The Agents view loads instantly.** The pane draws the shell, then blocks up to `FIRST_SAMPLE_WAIT`
  (200ms) for the sampler's immediate first snapshot before the loop, so the roster paints WITH the
  popup rather than after a tick. The sampler then refreshes ~1s; its steady-state cost is one `agent
  list` spawn (label maps cached — see `sample_roster`). The CLI still never runs on the render thread.

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
7. **`roster.json` is a prunable observation cache** (Slice 5) — nothing correctness-critical may
   live *only* there; deleting it must merely degrade timers, never lose a ping. `RosterStore`
   (`roster_state.rs`) is a **separate store** from `state.json` with its own lock, and every writer
   is best-effort (the `status-changed` stamp, the `startup` seed, and the pane sampler's reconcile
   all swallow their own errors). Tests: delete `roster.json` → everything still works; the pane's
   roster path writes **zero** `state.json`.

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

### The Agents view — Slices 0-5 DONE (#2/#3/#4/#5/#6 CLOSED). This is the whole build. See [docs/agents-view-design.md](docs/agents-view-design.md).
The live **Agents view** roster sits beside the durable Queue in the popup (`Tab`/`Ctrl+S`), loads
instantly, has full jump/reply parity, shows time-in-state, and shows each agent's **last terminal
line**. See the header for the commit summary and `agents-view-design.md` §7 for the full slice table.
**There is no queued build** — the Agents view is grouped-by-workspace with no reorder. Remaining work
is small polish only. **Notes on the shipped slices:**

1. **Slice 4 / issue [#5](https://github.com/Akram012388/herdr-checkin/issues/5) — DONE (this
   session), #5 CLOSED.** The last-line status column: `roster::last_terminal_line` (pure chrome-
   stripping fn + fixtures) + `herdr::TailCache` (budgeted round-robin `agent read` sweep on the
   sampler thread, never-blank, first-sample-skipped). Row is `{status} {age} · {last terminal line}`,
   title fallback. See the header block for the full summary + the two live findings. **The premise
   shifted during the build:** `agent read` returns the *rendered* terminal whose tail is the agent's
   own UI chrome, so "last non-empty line" was replaced (with the maintainer) by "skip the chrome
   block, take the last content line" — see `roster::last_terminal_line`'s doc + the fixtures.
2. **Slice 5 / issue [#6](https://github.com/Akram012388/herdr-checkin/issues/6) — DONE + CLOSED.**
   `roster.json` + `RosterStore` (separate prunable store = **invariant #7**); `status-changed` event
   binary stamps `status_since_ms`; startup seeds additively; pane sampler reads + back-fills the uuid,
   resets on a reused slot; rows show `blocked 4m` / honest `~`.

All planned Agents-view slices are shipped. No further build is queued.

- **Tracker: GitHub issues [#1–#6](https://github.com/Akram012388/herdr-checkin/issues)** (Slice 0→#1
  … Slice 5→#6). **#1 (Slice 0)** stays open pending the maintainer's pixel-identical popup eyeball.
  **#2/#3/#4/#5/#6 done + closed.** This doc + the design doc are the durable in-repo tracker; the
  issues are the work queue.

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
- **The reply input ceiling is single-line SEND semantics** (`tui-textarea`). **No** `shift+enter`/
  multiline *messages*, **no** edit-in-`$EDITOR`, **no** reply history/draft-preservation. Rationale
  (Fable + maintainer): replies are short by design; anything substantial is composed by pressing
  **`Enter` to jump to the agent's real pane**, which is a full terminal with every editing binding for
  free. **NB (this session):** the input now **soft-wraps** the one logical line across 3 rows with
  Up/Down nav — that is a *display* improvement and does NOT relitigate this decision (the message is
  still one line, `Enter` still sends, no newline is stored or sent). Do not add multi-line *send*
  semantics; if richer editing is ever truly needed, enable more of tui-textarea in `compose`, do not
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
0.4.0 is **cut but not tagged**. The earlier UI polish (two-line rows, tui-textarea input, softer
highlights) is folded into the unreleased 0.4.0 CHANGELOG entry. **The entire Agents view (Slices 0-3),
the reply soft-wrap, and the load-perf work are post-0.4.0 internal feature work — NOT yet in the
CHANGELOG** (fold them in when the Agents-view feature settles / before tagging). Tagging `v0.4.0` is
the maintainer's call — **do NOT tag autonomously.**

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
