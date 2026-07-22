# Agents view — design + build plan

The next evolution of herdr-checkin: a **live "Agents view" roster** added alongside the durable
queue, mirroring Claude Code's agent view but powered by herdr primitives. Aligned with the
maintainer (research → interview → Fable advisory) on 2026-07-22. **Build via tracer-bullet slices,
each keeping the CI gate green, each eyeballed live before the next.**

Companion reading: [HANDOFF.md](../HANDOFF.md) (architecture, invariants, herdr API facts). The
research this drew on: Claude Code's agent view — <https://code.claude.com/docs/en/agent-view.md>
and <https://claude.com/blog/agent-view-in-claude-code>.

## 1. What we're building

The popup gains a **second view**. `Tab` / `Ctrl+S` toggles between:
- **Queue** — today's durable FIFO attention inbox (reads `state.json`). Unchanged.
- **Agents** — a live roster of **every detected agent pane**, all states, grouped by workspace.

The Agents view is the Claude-Code agent-view experience, scoped to what herdr can back.

## 2. Locked decisions (do not relitigate)

| Decision | Choice |
| --- | --- |
| Surface | **Popup modal — KEPT** (not a dedicated pane/tab). Fable-ruled under the new premise; see §9. |
| Model | Two views in one popup, `Tab`/`Ctrl+S` toggle. Queue stays durable; Agents is live. |
| Roster contents | **Every** detected agent pane, all states (`idle`/`working`/`blocked`/`done`/`unknown`). |
| Row actions | Attach/jump (`Enter`→`focus`, closes popup), inline reply (`space`→`prompt`). |
| Grouping | By **workspace**, all workspaces. |
| Row status line | The **last non-empty terminal line** via `herdr agent read` (no Haiku summary exists). |
| Time column | **Time-in-state**, stamped by the **event binary** (see §4), rendered as `blocked 4m`. |
| Peek panel | **Deferred** — the last-line column + one-keystroke `Enter` jump already cover it (same reasoning that cut multiline reply). Revisit only after living with v1. |
| Reorder | **Pin-to-top only** (persisted by session uuid). No arbitrary manual reorder. |

## 3. herdr primitives (live-verified 2026-07-22, herdr 0.7.5)

- **`herdr agent list`** → the roster source. Per agent: `agent` (claude/codex), `agent_status` ∈
  `{idle, working, blocked, done, unknown}`, `cwd`, `focused` (bool), `pane_id`, `workspace_id`,
  `tab_id`, `agent_session.value` (uuid), `terminal_title`. **No timestamps** — hence §4.
- **`herdr agent read <pane_id> [--source visible|recent] [--lines N] [--format text|ansi]`** →
  terminal output, ~8ms. Backs the last-line status column.
- **`herdr agent focus <pane_id>`** / **`herdr agent prompt <pane_id> <text>`** — jump / reply
  (already used by the Queue tab).
- **`herdr workspace list`** / **`herdr tab list`** — id→label maps for names.
- State vocabulary confirmed via `herdr agent wait --until`: `idle, working, blocked, done, unknown`.
  herdr has **no** separate `failed`/`stopped` (folded), and **no** process-alive "shape" dimension
  (panes are always live) — so we drop Claude Code's shape icon entirely.

## 4. Time-in-state provenance (the correctness fix)

The popup is a summon-and-glance modal — **not running most of the day**. So the pane's poll loop
cannot be the observer of state transitions: at popup-open every agent would read "blocked 0s", a
fabricated number exactly when it's read. Instead:

- The **`status-changed` event binary** — which fires on every `agent_status` transition, popup open
  or not, with a wall clock in hand — stamps `status_since_ms` into `roster.json` (keyed by
  `pane_id`; the event payload has no session uuid).
- The pane's sampler **only reads** the registry and back-fills `agent_session` from `agent list`; a
  uuid mismatch on a reused pane slot resets the timer. An agent with no registry entry renders an
  honest `~` / `4m+`, never a fake zero.
- `startup` seeds the registry **additively** (never resets a surviving entry's `status_since_ms`).

## 5. Architecture

- **Live roster is in-memory only.** `RosterSnapshot { sampled_at_ms, agents: Vec<RosterAgent> }`
  in the pane model, replaced wholesale each sampler delivery. Never persisted — persisting live-poll
  output would make the pane a writer racing the event binaries.
- **`roster.json` is a SEPARATE store from `state.json`**, own lock, same temp+rename delta pattern
  (`RosterStore`, sibling of `StateStore`). It holds only the time-in-state registry + pins. It is a
  **prunable observation cache**: losing it degrades timers/pins only — never a ping. **This is the
  new 7th invariant** (see §7).
- **A worker thread does all CLI**, never the render tick. Sampler cadences: `agent list` ~1s
  (status/grouping/time); `agent read` ~2s on **visible rows only**, budgeted round-robin
  (~15/sweep), invalidate-immediately-on-status-change. Snapshots + last-line cache flow to the tick
  over an mpsc; the 250ms tick only drains the channel and renders cached data (a row never blanks).
- **Modules:** new `roster.rs` (pure: `RosterAgent`, grouping, sort-with-pins, time math, last-line
  extraction, registry reconciliation — **Herdr-free, same rule as `queue.rs`**). `herdr.rs` grows
  `agent_list()`/`agent_read()` + parsers. New `roster_state.rs` (`RosterStore`). `pane/` splits into
  `mod.rs` (shell: loop, tick, `ActiveTab`, sampler ownership, channel, one shared exit path),
  `queue_view.rs`, `agents_view.rs`, `compose.rs` (shared by both views via a small
  `pane_id + label` target interface — not a faked `QueueEntry`).

## 6. Pin persistence

Key pins by **`agent_session` uuid, never `pane_id`** (pane ids are positional and reused). Store in
`roster.json`: `pins: [{ agent_session, pinned_at_ms, last_seen_ms }]` (list order = pin order).
Vanished uuid → keep as tombstone; reappears → re-applies silently; GC tombstones past ~50 or ~7d
inside `RosterStore::update` deltas. Render: pinned agents float to the top **of their workspace
group** (no global Pinned section — that would fight the grouped-by-workspace decision).

## 7. Invariants — guards for this feature

The existing six (see HANDOFF §3) plus a new one:

7. **`roster.json` is a prunable observation cache** — nothing correctness-critical may live only in
   it; deleting it must merely degrade timers/pins. (Test: delete `roster.json`, everything still
   works.)

Regression risks to guard while building:
- **#1 (deltas via `StateStore::update`)** — the registry must NOT land in `state.json`; test that the
  pane's roster path performs zero `state.json` writes.
- **#2/#3 (act-first / never prune unseen)** — `agents_view` must never call `StateStore`; jumping
  fires `pane.focused` which evicts via the existing tested path (correct + free). Keep `StateStore`
  out of `agents_view` imports.
- **#4 (startup additive)** — registry seeding must be additive too; startup-twice idempotence test on
  `roster.json`.
- **#5 (popup self-dismiss)** — `Tab` toggle must not touch popup lifecycle; `Enter` from either view
  exits through one shared close function.
- **#6 (queue.rs Herdr-free)** — extend identically to `roster.rs`.

## 8. Build slices (tracer-bullet; each ends green + eyeballed)

Each slice is tracked as a GitHub issue (dependency-ordered). Repo docs + issues are complementary:
this doc is the durable design, the issues are the per-slice work tracker.

| Slice | Issue | Type |
| --- | --- | --- |
| 0 split `pane.rs` + snapshots **(DONE)** | [#1](https://github.com/Akram012388/herdr-checkin/issues/1) | AFK |
| 1 `agent_list` seam + `roster.rs` **(DONE)** | [#2](https://github.com/Akram012388/herdr-checkin/issues/2) | AFK |
| 2 tab toggle + live roster | [#3](https://github.com/Akram012388/herdr-checkin/issues/3) | HITL |
| 3 jump + reply parity | [#4](https://github.com/Akram012388/herdr-checkin/issues/4) | HITL |
| 4 last-line status column | [#5](https://github.com/Akram012388/herdr-checkin/issues/5) | HITL |
| 5 `roster.json` + time-in-state | [#6](https://github.com/Akram012388/herdr-checkin/issues/6) | AFK |
| 6 pin-to-top | [#7](https://github.com/Akram012388/herdr-checkin/issues/7) | HITL |


- **Slice 0 (DONE)** — Split `pane.rs` (1513 lines) into `pane/{mod,queue_view,compose}.rs`. Pure
  motion — the render code moved verbatim; the model (`PaneModel`/`ReplyDraft`) stays in `mod.rs` so
  the descendant render modules read its private fields without widening visibility. **Introduced
  ratatui `TestBackend` snapshot coverage** (empty queue, grouped CHECKIN/DONE, compose strip, the
  `> ` cursor) — content-level, trimming horizontal styling that stays under live tuning. Gate met:
  CI green (84 lib + 5 CLI tests). Popup pixel-identical still wants the maintainer's live eyeball.
- **Slice 1 (DONE)** — `Herdr::agent_list` + `parse_agent_list` (over a pristine live-captured JSON
  fixture) + pure `roster.rs` (`AgentStatus`, `RosterAgent`, `RosterSnapshot`, `WorkspaceGroup`,
  `group_by_workspace`, `render_roster_text` — Herdr-free) + a hidden `roster` debug subcommand. Gate
  met: fixture unit tests (incl. an `unknown` status and a live agent with no `agent_session`) + a
  live printout verified inside herdr (groups by workspace, marks focus, dashes missing fields). CI
  green (94 lib + 5 CLI).
- **Slices 2 + 3 (built together; CI-green, pending the maintainer's live eyeball)** — Tab/Ctrl+S
  toggle + a **fully interactive** Agents view fed by the sampler thread. Slice 3 was folded forward
  after the first live eyeball: a read-only roster felt dead and its indicators couldn't be truthful,
  so selection + reply + jump landed in the same pass. **This is the tracer bullet — it proves the
  popup can host a live view without janking the durable one.** Built:
  - **Sampler:** `RosterSampler` worker thread (samples once immediately, then every 1s over an mpsc;
    interruptible `recv_timeout` shutdown; joined on `Drop` so every exit path tears it down); the
    250ms tick drains non-blocking (`try_recv`, newest-wins) so a slow/failed sample never blanks a
    row. `RuntimeEnv` gained `herdr_bin_path` so the worker builds its own `CliHerdr` (the borrowed
    `&dyn Herdr` isn't `Send`).
  - **Interaction parity:** `j`/`k`/click selection over the display-order agent list (anchored by
    pane id across the 1s refresh so the cursor never jumps); `space` reply through the shared
    compose target (`arm_reply(pane_id, label)`, the design's "not a faked QueueEntry" seam); `Enter`
    jump via the one shared `on_enter` dispatch — focus then act-then-evict-on-success (idempotent, a
    no-op when the agent wasn't queued). Rows: destination (`{tab} · pane {n}`, workspace is the group
    header) + `{status} · {title}`, grouped by workspace, with the same selection band + `> ` cursor +
    overflow scrollbar as the Queue (scrollbar helpers reused from `queue_view`).
  - **Consistent affordances (reversing the earlier call):** a **persistent tab bar** now tops both
    views (active tab carries the selection band), so `Tab` is discoverable and identical on each;
    both footers show `space reply`. This deliberately drops the "Queue byte-identical" stance —
    superseded by the maintainer's request for a clear, consistent indicator.
  - `pane/agents_view.rs` render sibling; `TestBackend` snapshots for the grouped roster (with cursor),
    the placeholder, the empty-roster wording, the toggle, plus model tests for agents selection/clamp,
    snapshot re-anchoring, agents-view reply target, and the agents-view jump. CI green (113 lib + 5 CLI).
  - *Gate (HITL, outstanding):* open the popup, `Tab` to Agents, drive `j`/`k`/`space`/`Enter`, and
    confirm a live status flip shows within ~1s with no jank; Queue tab still fully works. **Popup
    geometry:** the maintainer confirmed 50%×50% reads fine — left as-is.
- **Slice 4** — Last-line status column: 2s visible-rows `agent read` sweep, budgeted round-robin,
  invalidate-on-status-change, never-blank cache. *Gate:* smooth with 5+ agents; lines track output.
- **Slice 5** — `roster.json` + `RosterStore`; `status-changed` stamps `status_since_ms`; startup
  seeds additively; rows show `blocked 4m` / honest `~`. *Gate:* data-path test (drive the binary,
  inspect the file) + startup-idempotence test.
- **Slice 6** — Pin-to-top persisted by `agent_session` with tombstone GC. *Gate:* survives popup
  reopen and pane-slot reuse.
- **Slice 7 (optional, only if re-requested after lived experience)** — peek panel and/or arbitrary
  reorder.

## 9. Surface: popup, not a dedicated pane (ruling)

The plugin was built as a popup modal, deliberately over a dedicated pane, back when it was *only* a
summon-and-glance triage queue. Becoming a live agents view reopened that call — so it was re-ruled
under the new premise (Fable advisory, 2026-07-22). **Decision: keep the popup.** Do not relitigate
without new lived evidence (see the accepted counter below).

- **The "it's a dashboard now" intuition is wrong.** The ambient channel is already occupied twice:
  herdr's native toasts push the pings, and the workspace itself (your agent panes, on screen in
  their tabs) *is* the monitor. The Agents view adds neither — it adds **cross-workspace consolidation
  on demand**: "show me everyone, everywhere, right now, so I can pick where to go." That is a
  **switchboard, not a monitor**, and a switchboard is a summon job — its rows are jump targets and a
  jump closes the surface. The roster is *more* summon-shaped than the queue, not less.
- **The architecture already voted:** time-in-state is event-stamped (§4) precisely because the popup
  is not running most of the day. The right durability posture was designed for an intermittent surface.
- **Not a persistent split pane:** a roster of your neighbors, docked inside the workspace of the
  neighbors it lists, is redundancy dressed as awareness — and it is invisible exactly when you are
  working elsewhere, which is when you reach for the global keybind anyway.
- **Not a takeover tab:** Claude Code goes full-screen only because it has no windowing system; herdr
  has a better summon primitive (session-singleton float + global keybind, reachable from any
  workspace). "Full attention when summoned" is a **geometry parameter** — open the popup larger — not
  a surface change (folded into Slice 2).
- **Testability is solved one layer down, not by the surface:** ratatui `TestBackend` snapshot tests
  (Slice 0/2) cover rendering in CI with no herdr; the data path is already scriptable. The surface is
  a launch flag (`--placement`), so this whole decision stays **cheaply reversible**.
- **Accepted counter-argument:** a popup closed ~99% of the time gives zero signal for an agent
  silently wedged at `working 45m` that never pings. That is a **detection gap, not a surface gap** — a
  future "stalled" heuristic could toast it. If lived experience proves the roster wants ambient
  presence, the placement-flag escape hatch makes a later pane/hybrid a small change. **Earn it with
  evidence; never default to it.**
