# HANDOFF

Orientation for the next session (human or agent). **Read this first, then start on §6.**
User-facing docs: [README.md](README.md). Release log: [CHANGELOG.md](CHANGELOG.md). Working rules
and the model-tier strategy: [CLAUDE.md](CLAUDE.md). The feature's original (overlay-era) design that
later pivoted to a popup: [docs/triage-overlay-design.md](docs/triage-overlay-design.md).

**Version:** **0.4.0 — the triage-popup release — is cut** (version in `Cargo.toml` +
`herdr-plugin.toml` + `Cargo.lock`, CHANGELOG dated 2026-07-22). **NOT tagged** — the maintainer tags
on request. · **License:** MIT · **Repo:** https://github.com/Akram012388/herdr-checkin · **State:**
`main` is green (fmt + clippy + test, **77 lib + 5 CLI tests**) and pushed; HEAD `83875a8`. No open
branches, no worktrees, working tree clean.

**START HERE (§6): two more popup-polish tasks the maintainer wants next.**
- **A (priority) — fix the agent identifier metadata.** Each queue row should say *which
  space / tab / pane* the waiting agent belongs to, and today that is wrong/incomplete: a row shows
  only the raw `workspace_id` (e.g. `w1`) — no tab at all, and no pane. The groundwork (exactly which
  identity fields herdr exposes, and the load-bearing event-vs-`pane list` asymmetry) is already
  scouted below — start from §6 Task A.
- **B (quick) — lighten the reply placeholder.** The `type your reply` placeholder should be *lighter
  in color and not italic* (it is currently `dim().italic()`). One-spot change in `draw_compose`.

**What already shipped (this and prior sessions).** The status pane is a centered **popup modal**
(herdr `--placement popup`, like `prefix+s` settings): herdr draws the border + "Check-in" title; the
durable queue is grouped into **CHECKIN** (`blocked`) / **DONE** (`done`) sections; keyboard- and
mouse-navigable; `space` opens an inline **compose strip** to reply; `q`/`Esc` dismiss (the pane
fires `popup.close` on exit). A recent UI/UX/DX polish pass (folded into the untagged 0.4.0) added:
popup sized **50% x 50%** (`1b6018f`), an **overflow scrollbar** (`c7622bb`), and the **compose-strip
reply redesign** (`9ce89c0`). All verified live in herdr 0.7.5. One upstream follow-up (background
dim) is out for maintainer review as a herdr Discussion — see §6.

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
**Identity gap (Task A):** the only location fields we persist are `pane_id` (`w1:p3`) and
`workspace_id` (`w1`); there is **no `tab_id` and no pane `label`** yet — adding them is the heart of
Task A (§6).

**Files** (`lib.rs` was split into cohesive modules; each holds the `#[cfg(test)]` tests for its own
code, and `lib.rs` re-exports items as `pub(crate)` so `crate::X` paths still resolve):
- `src/lib.rs` (~160 lines) — the orientation page: argv dispatch (`run_from_env`/`run`), subcommand
  parsing, `RuntimeEnv`, and the `mod`/`pub(crate) use` wiring for everything below.
- `src/state.rs` (~306) — persisted state: `QueueEntry`, `WaitStatus`, `StateStore` (lock + atomic
  temp+rename write), `StateLock`, `read_state`/`write_state`/`load_entries`, `PluginError`. Owns
  the "all mutations via `StateStore::update`" invariant. **Task A adds fields here** (e.g. `tab_id`,
  `label`) — do it migration-safe (serde `#[serde(default)]`, like `last_touched_ms`).
- `src/herdr.rs` (~430) — the herdr seam (`Herdr` trait / `CliHerdr`, `PaneInfo`) plus JSON parsing
  for `pane list` responses (`parse_pane_infos`) and plugin event payloads (`parse_status_event` →
  `StatusEvent`). Trait methods: `pane_status_map`, `pane_infos`, `focus_agent`, `prompt_agent`,
  `show_notification`, **`popup_close`** (socket call). **Task A extends `PaneInfo` + `StatusEvent` +
  their parsers** to capture the new identity fields (see §4 for exactly what's available on each
  path).
- `src/queue.rs` (~215) — pure queue transitions (`enqueue`/`evict`/`is_live`) and the event handlers
  (`on_status_changed`/`on_focused`/`on_closed`). **Must never depend on the `Herdr` trait** (enforced
  by the module boundary). Load-bearing for Task A: the event path lives here and gets only the event
  payload — so a `pane list` lookup to fill a missing `tab_id` must happen in the dispatch/actions
  layer, not in queue.rs.
- `src/actions.rs` (~540) — the actions (`next`/`peek`/`clear`/`startup`), the toast copy they
  render, and **`describe_entry` + `agent_label`** (the row-rendering helpers, shared by the list
  rows, the `peek` toast, and the reply footer). **Task A's display change lands in `describe_entry`**
  (currently `{who} — {status} — {title} [{workspace_id}, {waited}]`).
- `src/test_support.rs` (~165) — `#[cfg(test)]`-only shared fake `Herdr` + state fixtures. The
  `FakeHerdr` records `focused`, `prompts` (`(pane_id, text)`), and `notifications`, with
  `with_failing_focus`/`with_failing_prompt` toggles; `popup_close` is a no-op. `feed_status(dir, ms,
  pane_id, workspace_id, status, title)` seeds an entry — extend its signature if Task A adds fields.
- `src/pane.rs` (~1100) — the ratatui TUI (`PaneModel`, event loop, grouped view, mouse hit-testing),
  the **inline reply compose strip** (`draw_compose` + the pure `wrap_display` / `truncate_display` /
  `reply_hint` / `input_width` helpers + the `dim_area` veil + real-cursor placement), the **overflow
  scrollbar** (`draw_list` reserves a column; pure `scrollbar_thumb` + thin `render_list_scrollbar`),
  the **grouped CHECKIN/DONE render** (`layout_rows` → `Row::Spacer | Header | Entry`), and the
  **popup lifecycle** (close-on-exit + close-on-jump). Pure model + view helpers are unit-tested; the
  terminal loop and buffer-drawing shells are thin. **Task B's placeholder tweak is in `draw_compose`.**
- `src/main.rs` — one-line entry into `lib::run_from_env`.
- `tests/cli.rs` — end-to-end tests that spawn the built binary against a fake `herdr` on
  `HERDR_BIN_PATH`.
- `herdr-plugin.toml` — manifest: `[[actions]]`, `[[events]]`, one `[[panes]]` (popup), `[[build]]`,
  `[[startup]]`.
- `scripts/open-pane.sh` — launcher for `open-pane`: opens the pane as a `popup` with
  `--width 50% --height 50%` and `--env HERDR_CHECKIN_POPUP=1`. No toggle logic (a popup is a
  singleton). Keep `--width`/`--height` in sync with the `[[panes]]` entry in the manifest.

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
- **Row copy (`describe_entry`, Task A target):** each row currently renders
  `{who} — {status} — {title} [{workspace_id}, {waited}]` (title omitted when empty; the bracket
  drops `workspace_id` when empty so it never shows `[, 3m]`). `{who}` is `agent_label` = display_agent
  → agent → pane_id fallback. **The `[{workspace_id}, …]` part is what's "not represented correctly"**
  — it shows only the raw workspace id, never the tab or pane.
- **Grouped render:** `layout_rows` groups the FIFO queue into **CHECKIN** (`blocked`) then **DONE**
  (`done`) sections, each preceded by a blank `Row::Spacer` for visual separation, FIFO within each.
  Pure view over the ordered `Vec` — it never reorders `entries`. `selected` stays an index into
  `entries`; only `draw`/`row_for_click` learn the spacer+header offsets. The top line is just the
  count (herdr draws "Check-in" on the popup border).
- **Inline reply:** `space` opens a **compose strip** for the selected waiter; you type an answer and
  `Enter` routes it into that agent's session via `herdr agent prompt <pane_id> <text>`, then evicts
  the entry **only on submit success** (a failed submit keeps it). `Esc`/click cancels; the reply's
  **target is captured when reply mode is armed**, so a concurrent queue refresh can't retarget it.
  Empty/whitespace `Enter` sends nothing and stays in reply mode. The strip (`draw_compose`) dims the
  header + queue as one `dim_area` veil (focus by receding everything else) and drops the reversed
  highlight to a plain `> ` marker on the captured target; it shows a titled `Reply to <label>` rule
  (label bold, dim dashes, ellipsis-truncated on a narrow popup), a **1-`MAX_INPUT_ROWS`(=3)** input
  that **character-wraps by display width** (`wrap_display`, unicode-width aware so wide chars don't
  drift the caret) showing the tail, and a right-aligned `enter send · esc cancel` hint that degrades
  to `enter · esc` under width pressure. It drives the **real terminal cursor**
  (`Frame::set_cursor_position`) to the end of the text — no fake `_` caret — and shows a
  `type your reply` placeholder on an empty buffer (**currently `dim().italic()`; Task B lightens it +
  drops italic**). Colorless throughout (bold/dim/italic/reversed only), matching herdr's restrained
  modal aesthetic.
- **Overflow scrollbar:** when the grouped rows exceed the visible height, `draw_list` reserves the
  right-most column and draws a 1-column scrollbar (dim track, brighter thumb) sized/positioned from
  the list's live `ListState` offset. `List`+`ListState` already scrolls to keep the selection in
  view; the bar only makes off-screen waiters discoverable. Thumb geometry (`scrollbar_thumb`) is
  pure and unit-tested.

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
- **PANE IDENTITY — the load-bearing facts for Task A** (read from herdr 0.7.5 source, repo
  `ogulcancelik/herdr` at `c234f22`):
  - **IDs are positional, not names.** `workspace_id` = `w1` (`Workspace.id`, a reserved number);
    `tab_id` = `w1:t2` (`{workspace_id}:t{N}`); `pane_id` = `w1:p3` (`{workspace_id}:p{N}`). See
    `public_workspace_id`/`public_tab_id_for_number`/`public_pane_id_for_number`. **Critically,
    `pane_id` does NOT encode the tab** — a pane's tab is only in the separate `tab_id` field.
  - **`herdr pane list` (`api::schema::PaneInfo`, built by `pane_info()` in `src/app/creation.rs`)
    returns per pane:** `pane_id`, `terminal_id`, `workspace_id`, **`tab_id`**, `focused`, `cwd`,
    `foreground_cwd`, **`label`** (= `terminal.manual_label`, a user-set pane label; may be null),
    `agent`, `title`, `terminal_title`, `terminal_title_stripped`, `display_agent`, `agent_status`,
    `state_labels`, `tokens`, `agent_session`, `scroll`, `revision`. **We currently parse only
    `pane_id`, `workspace_id`, `agent_status`, `agent`, `display_agent`, `title`** — `tab_id` and
    `label` are there for the taking.
  - **The event payload is LEANER than `pane list`.** `PaneAgentStatusChangedEvent`
    (`src/api/schema/events.rs`) has `pane_id`, `workspace_id`, `agent_status`, `agent`, `title`,
    `display_agent`, `state_labels` — and **NO `tab_id`, NO `label`.** So the **main enqueue path
    (an agent going blocked/done, handled by `status-changed` → `on_status_changed`) cannot see the
    tab at all.** Only `pane list` (used by `startup` re-seed and `next`/`peek`) carries `tab_id`.
    This asymmetry is the central design constraint for Task A (see §6 for the options).
  - **Human names vs ids:** `pane list` gives ids (`w1`, `w1:t2`, `w1:p3`) plus the per-pane manual
    `label`. Workspaces and tabs *can* be renamed in herdr, but those display names are **not** in
    the `PaneInfo` payload — if the row should show readable names rather than ids, the next session
    must confirm whether `herdr workspace list` / `herdr tab list` expose a name and join by id
    (probe live before assuming — see §6 Task A step 1).
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
- **Pane info:** `herdr pane list` → `result.panes[]` of `PaneInfo` (full field list above). (Note: a
  **popup pane is NOT in `pane list`** — see the plugin-pane bullet.)
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
  modals dim (see §6 background-dim note). (`--placement overlay`, the pre-popup choice, is tab-scoped
  and persists on blur but is not a global modal — superseded by `popup`.)
- **Env a pane/handler receives:** `HERDR_PLUGIN_STATE_DIR`, `HERDR_BIN_PATH`, `HERDR_SOCKET_PATH`,
  `HERDR_PLUGIN_CONTEXT_JSON`, `HERDR_PANE_ID`, `HERDR_PLUGIN_ROOT`, `HERDR_PLUGIN_ID`.
  **Gotcha:** the id is percent-encoded in the state-dir path (`%41kram012388.checkin`). Always use
  the `HERDR_PLUGIN_STATE_DIR` env var — never construct the path.
- **Toast:** `herdr notification show <title> [--body B] --sound none|request|done`.

## 5. Dev loop

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # what CI runs
cargo build --release
herdr plugin link "$PWD"                                   # register the local build (re-run after a manifest edit)
herdr plugin action invoke <next|peek|clear|open-pane> --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin        # inspect event/action/startup runs (see real event payloads here)
```

**Probe the real pane identity (do this FIRST for Task A).** From inside herdr, with a couple of
agents open across different workspaces/tabs:
```sh
herdr pane list            # inspect the JSON: real values of pane_id, workspace_id, tab_id, label, title
herdr workspace list       # does a workspace expose a display name distinct from its id?
herdr tab list             # does a tab expose a display name distinct from its id?
```
And read a real `pane.agent_status_changed` payload via `herdr plugin log list` to confirm it lacks
`tab_id` (as the source says). This tells you whether to show ids or names, and whether the event
path needs a `pane list` lookup to learn the tab.

**Manual E2E — seed the queue.** Seed `state.json` directly (find the path via the
`HERDR_PLUGIN_STATE_DIR` env — running any action once materializes it), then
`herdr plugin action invoke open-pane` to open the popup.

**Popup E2E is different from a normal pane (important gotcha).** A `popup` pane is a session-level
singleton — it is **not** in `pane list` and has **no addressable pane_id**, so you cannot
`herdr pane read`/`send-keys` it by id. To verify:
- **It opened:** the `open-pane` action's stdout is `{"type":"ok"}` (the popup path). `pgrep -f
  "herdr-checkin pane"` confirms the TUI process is alive (leave it undisturbed — running other herdr
  CLI commands from another pane can steal focus and close it).
- **Test/close the close path yourself over the socket** (exactly what the pane fires on exit):
  connect to `$HERDR_SOCKET_PATH` and write `{"id":"x","method":"popup.close","params":{}}\n`.
  `"result":{"type":"ok"}` = a popup was open and is now closed; `"error":{"code":"popup_not_open"}`
  = none was open. (A tiny Python `AF_UNIX` client does this.)
- **The visual (centered/bordered, colors, placeholder shade) and keyboard dismissal are user-only** —
  a popup can't be pane-read, so confirming the modal look + `q`/`Esc` behavior needs a human at the
  terminal. **This matters for Tasks A and B: the row layout and the placeholder shade have to be
  eyeballed live.**

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

### Task A (priority) — represent each agent's space / tab / pane correctly
**Problem.** A queue row shows only `[{workspace_id}, {waited}]` — e.g. `[w1, 3m]`. That names the
workspace but **not the tab and not the pane**, so you can't tell *where* a waiting agent actually is.
The maintainer wants the row to carry the space/tab/pane identity correctly.

**What's already scouted (see §4 "PANE IDENTITY"):**
- `pane list` gives `pane_id` (`w1:p3`), `workspace_id` (`w1`), **`tab_id` (`w1:t2`)**, and a per-pane
  manual **`label`** — but we only parse `pane_id`/`workspace_id`/`agent`/`display_agent`/`title`.
- `pane_id` does **not** encode the tab; the tab is only in `tab_id`.
- **The event payload has NO `tab_id`/`label`** — only `pane list` does. So the everyday enqueue path
  (agent goes blocked/done) can't learn the tab from the event alone.
- IDs are positional, not human names; workspaces/tabs may have separate display names not present in
  `PaneInfo` (confirm via `herdr workspace list` / `herdr tab list`).

**Do this in order (design-gate-before-code — this is a real design task, not a mechanical edit):**
1. **Verify foundations live** with the §5 "Probe the real pane identity" commands. Decide two
   things from real data: (a) show **ids** (`w1:t2:p3`-style) or **human names** (needs the
   workspace/tab-name join), and (b) how prominent the location is (inline suffix vs. a dim second
   line per row). Recommend a concrete row format and get maintainer alignment before coding.
2. **Resolve the event-vs-`pane list` asymmetry.** Options, pick with the maintainer:
   - **(a) Lookup on enqueue** — in the `status-changed` dispatch/actions layer (NOT `queue.rs`,
     which must stay Herdr-free), call `pane list` to fetch the pane's `tab_id`/`label` and pass them
     into the `enqueue` upsert. Cost: one extra CLI call per status event.
   - **(b) Refresh-only** — leave tab empty on the event path; fill it on `startup` re-seed and on
     the `next`/`peek` liveness pass (both already call `pane list`). Cheaper, but most rows lack a
     tab until a refresh.
   - **(c) Enrich in the pane** — the long-running pane already can't call herdr (it only polls
     state), so this would mean the pane shelling out to `pane list` on a tick; probably rejected
     (keeps the pane pure), but note it.
   Lean (a) for correctness unless the extra call is a concern.
3. **Capture the fields** — add `tab_id` (and `label`/name if chosen) to `PaneInfo` + `StatusEvent`
   in `src/herdr.rs` and their parsers; add matching fields to `QueueEntry` in `src/state.rs` with
   `#[serde(default)]` so old `state.json` still loads (mirror how `last_touched_ms` was added).
   Thread them through `enqueue` (queue.rs) and the seed path (actions.rs `startup`).
4. **Render it** in `describe_entry` (`src/actions.rs`) — this is the one place row copy is built, and
   it feeds the pane rows, the `peek` toast, and (via `agent_label`) the reply footer, so they stay
   consistent. Keep it colorless.
5. **Tests:** `describe_entry` already has unit tests (e.g. `describe_entry_omits_a_missing_workspace`)
   — extend them for the new fields incl. the missing-tab/missing-label cases; extend `feed_status`
   in `test_support.rs` if the signature grows; keep the `parse_pane_infos` / `parse_status_event`
   JSON tests in sync.
6. Keep each slice green (`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo
   test`); commit + push (pre-approved for this repo). **Eyeball the row live** — a popup can't be
   pane-read (§5).

### Task B (quick) — lighten the reply placeholder
In `src/pane.rs` `draw_compose`, the empty-buffer placeholder is currently rendered
`"type your reply".dim().italic()`. Make it **lighter in color and not italic**:
- Drop `.italic()`.
- "Lighter": the strip is at normal intensity and the placeholder is only `.dim()`. Our one faint
  modifier (DIM) is already on it, so going lighter likely means a **faint neutral color** for the
  placeholder specifically — e.g. `Style::new().fg(Color::DarkGray)` (import `ratatui::style::Color`).
  This is a small, deliberate exception to the otherwise-colorless design, scoped to placeholder text;
  confirm the exact shade by eye at the terminal (it can't be pane-read). Keep the leading pad space
  and the real cursor parked at column 0. No test needed (pure styling); `cargo fmt/clippy/test` still
  must stay green.

### Release status
0.4.0 is **cut but not tagged**. Tasks A/B land on top of it (untagged); fold any user-facing note
into the 0.4.0 CHANGELOG entry (it's unreleased). Tagging `v0.4.0` is the maintainer's call — **do NOT
tag autonomously.**

### Background-dim upstream contribution — proposed, awaiting maintainer
The popup does not dim the panes *behind* it the way `prefix+s` settings does, and this is **not
fixable from the plugin** — `render_popup_pane` (herdr `src/ui/panes.rs`, ~L401-429) never calls the
private `dim_background` (`src/ui.rs:552`), and no manifest/config/IPC knob exists; the dim is drawn
by herdr's compositor, which our TUI process can't reach. **Exact upstream fix:** insert
`super::dim_background(frame, area);` right after the three early-return guards in `render_popup_pane`,
mirroring `settings.rs:44` (one line; `panes.rs` is a sibling module under `ui`, no visibility change).
- **STATUS: proposed as GitHub Discussion #1733** (Ideas):
  https://github.com/ogulcancelik/herdr/discussions/1733 — awaiting a maintainer.
- **herdr gates first-time external contributors** (`CONTRIBUTING.md` + `AGENTS.md`): a UI/feel change
  starts as a Discussion; an agent must NOT open an issue/PR on the contributor's behalf or bypass the
  gate. Path: Discussion → maintainer converts to an issue and comments `/approve @Akram012388` →
  *then* the PR. **Do NOT open the PR (or an issue) until that approval lands.** Their commit style:
  lowercase conventional, **no emoji, no AI co-author line**; don't touch root README/CHANGELOG in the
  PR (their rule); `just ci` = `cargo fmt --check` + `cargo nextest run`.
- The ready 1-line change was committed on branch `popup-pane-dim-background` in a prior session's
  **ephemeral scratchpad** fork clone (`Akram012388/herdr`, not pushed) — re-create it from the
  one-liner above if it's gone. A local full `cargo check` of herdr is blocked in this environment by
  its vendored `libghostty-vt` zig build (unrelated to the change); rely on herdr CI for the compile.
- (The plugin *does* dim its **own** interior queue while composing a reply — §3 `dim_area` — but that
  is inside our pane and unrelated to dimming the background panes behind the popup.)

### Parked / optional
- **Refresh the demo gif (docs, plugin-side).** `docs/pane-demo.gif` predates the polish pass — it
  still shows the old single-line reply footer (fake `_` caret) and the 55%-tall popup, not the new
  compose strip / 50% size, and it will be further out of date after Task A changes the row copy. Do
  it **after** Tasks A/B settle. Regenerate with the **`demo-gif`** skill (VHS; `scripts/pane-demo.tape`
  + `scripts/pane-demo-setup.sh`, no real agents).
- **Global-summon popup** *(upstream-gated)* — the popup is summoned by a herdr keybind
  (`prefix+alt+q`) and is session-global. A dedicated `prefix+s`-style global-summon binding is not
  wired; low priority.
- **Docs note** — herdr 0.7.5 made plugin install/enabled state global-per-user; only relevant if a
  per-session-install section is ever added to the README.

### How we got here (overlay → popup, one paragraph)
The feature was designed and built as a 7-slice "triage overlay" (`--placement overlay`, full-tab;
cut as 0.4.0 at `1a618ae`). The maintainer pushed back that it should look like herdr's `prefix+s`
settings, and — since herdr is OSS — a source study (clone → Sonnet researcher → Fable advisor) proved
herdr has a **`popup`** placement giving the centered-modal look with **zero herdr changes** (the CLI
`--help` hides `popup` behind a stale value-parser). The pane switched to `popup` (`1d21213`), then a
UI/UX/DX polish pass sized it to 50%×50% (`1b6018f`), added the overflow scrollbar (`c7622bb`), and
redesigned the reply into the compose strip (`9ce89c0`). Full per-slice history is in the git log and
`CHANGELOG.md`.

### Suggested skills / helpers
- **`/herdr`** — control herdr from inside it (`HERDR_ENV=1`): split panes, spawn/read agents, run
  `herdr pane list`/`agent prompt`/`focus`. The tool for the Task A live probe and any E2E.
- **`demo-gif`** — regenerate the README demo gif (VHS; `scripts/pane-demo.tape`, no real agents).
- **Sonnet-5 subagents** for research/exploration and mechanical edits (maintainer preference: delegate
  mechanical work); a **Fable-5 advisor subagent** for a genuine load-bearing call (e.g. the Task A
  event-vs-`pane list` design), used sparingly.
- **`/handoff`** — to snapshot again at the end of the next session.

## 7. How we work here (see CLAUDE.md for the short version)

- **Model tiers:** Opus orchestrates (plan/decide/integrate/own correctness); Sonnet subagents do
  research, exploration, scoping, and mechanical implementation; a Fable subagent is the advisor for
  genuine doubt on load-bearing decisions — used sparingly.
- **Design gate before code, adversarial review after.** This keeps paying off: the `[[startup]]`
  race the Fable advisor caught → invariant #3; v0.2.0's clinical review found two ping-loss bugs;
  the overlay→popup pivot came from reading the herdr source rather than trusting the CLI `--help`;
  and the reply redesign came from a Fable design pass. Task A is squarely in this category — probe +
  design before coding.
- **Verify foundations first.** Confirm an API contract with a throwaway probe or a source read before
  building on it — e.g. `agent prompt`'s target, the `popup` placement, and (for Task A) the exact
  `pane list`/event identity fields were all confirmed against herdr 0.7.5 source.
- **Tracer-bullet slices, each green.** Small commits that each keep `fmt + clippy -D warnings + test`
  passing. When Rust's dead-code gate would force a lint suppression on a caller-less seam, prefer
  merging the seam with its first real caller over sprinkling `allow(dead_code)`.
- **Respect third-party contribution norms.** For upstream herdr work, its `CONTRIBUTING.md` +
  `AGENTS.md` govern (external-contributor gate; agents don't open issues/PRs on the human's behalf;
  no emoji, no AI co-author lines). See the background-dim note.
