# Triage-overlay design (v0.5.0 direction)

Design gate for evolving the status pane from a passive **list + jump** into an active **triage
console**: per row, reply inline (route an answer straight into the agent's session) instead of only
jumping. Optionally summoned as an **overlay**. This is the design; no code yet.

Owner: this is a design-gate artifact (see [CLAUDE.md](../CLAUDE.md) "design gate before code").
Companion facts live in [HANDOFF.md](../HANDOFF.md) SS3-4. **Status: draft, pre-build.**

---

## 1. What the live probes verified (the foundation)

Both option-(b) unknowns from HANDOFF SS6 were probed against real herdr 0.7.5 this session. Both
resolve in favor of the direction being buildable on primitives we already have.

**Probe 1 - overlay placement: YES.** `herdr plugin pane open --placement overlay` launched our
existing `pane` TUI as an overlay that:
- rendered the full persistent TUI (title, entries, footer);
- **routed keyboard input** (`j` moved the selection cursor + reverse-video highlight);
- **persisted on blur** - after focus moved away it kept existing and kept rendering, selection
  intact; it did not dismiss;
- closed cleanly via `plugin pane close`.

Notes that correct the HANDOFF's assumptions:
- The placement enum is **`overlay`** (also `tab`, `zoomed`) - **not `popup`**.
- `--placement` is a **CLI flag** on `plugin pane open`. Offering overlay is a one-line change in
  `scripts/open-pane.sh` (the launcher), **not** a manifest rewrite. The `[[panes]]` manifest
  `placement = "split"` is only the default; the launcher can override per-open.
- Caveat (not yet verified): the overlay opened **within the current tab** (a tab-scoped layer),
  not a global cross-workspace floating summon like herdr's `prefix+s`. The three behaviors we need
  (persistent + keyboard-interactive + survives-blur) all hold; only the "summon-from-anywhere"
  flavor is unconfirmed. Per HANDOFF, placement is negotiable - the interaction is the value.

**Probe 2 - `agent prompt` target + reply routing:**
- **TARGET is the `pane_id` we already store** (`w4:p1` form). Prompting by pane_id delivered and the
  agent acted. The `agent_session` uuid is **rejected** (`agent_not_found`). So inline reply passes
  the pane_id already on each `QueueEntry` - **no new identifier to store**.
- **Inline reply routes cleanly**: `herdr agent prompt <pane_id> "<text>"` sent an answer into a
  waiting agent's session and it acted on it. This is exactly the "space to reply" enabler.
- **`blocked` is narrower than "waiting for me".** herdr did **not** mark a Claude agent that asks a
  prose question and ends its turn as `blocked` - it showed `idle`/`done`. `blocked` appears
  reserved for modal/permission prompts. So "conversationally waiting" is observed mostly as
  **`done`**, not `blocked`. This shapes the attention model (SS3).
- **`--wait --until <state>` is flaky from a non-working start** - both probe waits returned
  `timeout` even though the underlying submit succeeded. The reply path should be **fire-and-poll**
  (or `--until working` to confirm the turn kicked off), never trust `--until idle/blocked` to gate
  eviction.

---

## 2. Strategic frame - keep the durable queue as the differentiator

> _Confirmed by a Fable advisor pass (SS7). The framing below is the advisor's, sharpened._

herdr already ships a native live agent-status roster (`agent list`: agents grouped by
idle/working/blocked/done) plus transient toasts. This plugin's **only** differentiator is the
**durable FIFO attention queue** - it remembers pings across toast-fade, restart, and simultaneous
blocks. The design rule that follows:

**(a) The triage console stays strictly queue-backed.** It shows only **enqueued waiters** - agents
that pinged (went `done`/`blocked`) and haven't been handled. It does **not** grow into a live
roster of all agents; that _is_ the native view, and cloning it makes us a worse copy.

The principle (advisor): herdr's native view answers _"where is everything now"_ - that is **state**,
cheap to recompute, and the host will always render it better. The queue answers _"what happened that
I haven't acknowledged"_ - that is **history**, exactly what herdr throws away when the toast fades.
An inbox is not a worse dashboard; it is a different data structure. The moment we show an agent that
never pinged, we are re-deriving live state and in feature-competition with the host - and we lose.

> **Litmus test for every future feature: does it operate on an enqueued entry? If no, reject it.**

Grouping enqueued entries by status within the console is an acceptable view nicety; the underlying
set is always the queue, never "all agents".

**(b) Attention model: acknowledgment semantics, not signal-sniffing.** The `blocked`=`idle`/`done`
finding proves herdr's taxonomy will not carry a "asked a question" vs "just finished" distinction.
**Do not** try to recover it by sniffing status or reply content - heuristics silently drop pings,
the exact failure class the clinical review already caught twice. Instead:
- The queue holds **unacknowledged turn-endings**. Every turn-end (`done`; `blocked` too, for modal
  agents) is a delivery. **Acknowledgment - jump, reply, or drop - is the only eviction.**
- Kill the noise at the _other_ end: **auto-acknowledge on organic focus.** We already **evict on
  `pane.focused`** - if the user was watching the agent finish, the debt is paid and the entry
  self-evicts. That preserves "durable until seen by a human" without a single heuristic about what
  the agent meant.

Net: inline-reply is **additive** to the durable queue (act on a waiter without leaving your seat),
not a pivot toward a live mirror. That is the line we hold.

---

## 3. UX design - the triage console

### Layout
Keep the current single FIFO list (oldest first) as the backbone. Each row already reads:
`N. <Agent> - <status> - <title> [<workspace>, <age>]`. Minimal change: the row is unchanged; we add
a **reply affordance** and a **reply input line**.

Optional (defer): visually group `blocked` above `done` within the list. Low value early; the FIFO
order is the product promise. Ship flat first.

### Key bindings (extends the current set)
| Key | Action | Status |
| --- | --- | --- |
| `j`/`k`, arrows, left-click | move / select | exists |
| `Enter` | jump to agent, evict on success | exists |
| **`space` (or `r`)** | **enter reply mode on the selected row** | **new** |
| `d` / `x` | drop (evict without acting) | exists |
| `c` | clear-all (with `y`/`n` confirm) | exists |
| `q` / `Esc` | close pane | exists |

In **reply mode** (a modal input, mirroring the existing clear-confirm guard):
| Key | Action |
| --- | --- |
| printable chars | append to the reply buffer |
| `Backspace` | delete last char |
| `Enter` | submit the reply to the selected agent |
| `Esc` | cancel reply mode, discard buffer |

### The inline-reply flow (act, then evict on success)
1. `space` on a selected entry -> enter reply mode; footer becomes an input line
   `reply to <Agent>: <buffer>_`.
2. Type the answer; `Enter` submits.
3. Submit calls `herdr agent prompt <pane_id> <buffer>` (fire-and-forget; **no `--wait`** - it is
   flaky, SS1).
4. **On submit success -> evict the entry** (it is handled), same discipline as `Enter`/jump
   (invariant #2: act first, evict only on success). Set a transient status
   `replied to <Agent>`.
5. **On submit failure -> keep the entry**, show `reply failed: <err>` (mirrors the focus-failure
   path). Losing an unanswered waiter is the exact failure the plugin exists to prevent.
6. Leave reply mode either way.

Design choice: **fire-and-forget, evict on submit-accept.** We do not block the pane waiting for the
agent to finish the turn (would freeze the 250ms poll loop and the `--wait` gate is unreliable).
"Submit accepted" is the success boundary for eviction. If the user wants to watch the result they
still have `Enter` (jump). This keeps the pane responsive and the eviction semantics honest.

### Placement
- Offer **overlay** by switching the launcher's open to `--placement overlay` (keep split as a
  fallback / config choice). The `pane-decision` toggle logic (open/focus/close) is unaffected -
  it keys off the pane `label`, which is placement-independent.
- Do **not** chase the global-summon flavor yet; the tab-scoped overlay already delivers the
  interaction. Revisit only if herdr exposes a cross-workspace popup that routes full keyboard focus.

---

## 4. Technical design - the seams (grounded in current code)

**`src/herdr.rs` - one new trait method.** Add to `trait Herdr`:
```
fn prompt_agent(&self, pane_id: &str, text: &str) -> Result<(), PluginError>;
```
`CliHerdr` implements it as `herdr agent prompt <pane_id> <text>` - structurally identical to the
existing `focus_agent` (`herdr agent focus <pane_id>`), reusing `command_failure`. The
`#[cfg(test)]` fake in `src/test_support.rs` records the call so pane tests can assert on it.

**`src/pane.rs` - a reply mode mirroring `confirm_clear`.** `PaneModel` already carries a modal flag
(`confirm_clear`) hoisted above both key and mouse branches in `event_loop`. Add a parallel mode:
- `PaneModel` gains `reply: Option<String>` (Some = in reply mode, holding the buffer). Keep it a
  distinct field so the two modals never overlap (a reply cannot be armed while a clear-confirm is).
- `event_loop`: hoist a `if let Some(buf) = &mut model.reply { ... }` branch above the normal
  bindings, exactly like the `confirm_clear` guard at `pane.rs:113`. It handles char/Backspace/
  Enter/Esc; a mouse click cancels reply mode (like the clear-confirm mouse cancel at `pane.rs:131`).
- `space` (or `r`) in the normal key branch arms reply mode on the selected entry (no-op on empty
  queue, like the other row actions).
- Submit handler `on_reply_submit` mirrors `on_enter` (`pane.rs:153`): resolve the selected pane_id,
  call `herdr.prompt_agent`, then `evict_pane` on success, set `model.status`, `model.sync`.

**`draw` (`pane.rs:425`)** renders the footer input line when `model.reply.is_some()` (as the
clear-confirm prompt is rendered today). The list rows are unchanged.

**Invariant mapping (no regressions):**
- #1 mutations-are-deltas: eviction after reply reuses `evict_pane` -> `StateStore::update`. Clean.
- #2 act-then-evict-on-success: reply evicts only on submit success (SS3 step 4-5). Clean.
- #3/#4 (liveness/startup): untouched - reply is a pane-side action, not an event/action binary.
- #5 (single focused pane / `PANE_LABEL`): untouched.

**Tests:** unit-test the reply-mode state machine on `PaneModel` (arm/append/backspace/cancel/submit)
and the submit -> evict-on-success / keep-on-failure branches against the fake `Herdr`, matching the
existing `pane.rs` test style. No new e2e needed for the model; a `tests/cli.rs` case can cover the
`prompt_agent` command shaping against the fake `herdr` binary.

---

## 5. Build sequencing (tracer-bullet slices)

1. **`Herdr::prompt_agent`** + fake + a `tests/cli.rs` command-shaping test. Pure seam, no UI. (green)
2. **Reply-mode state machine** on `PaneModel` (field + arm/append/backspace/cancel), unit-tested,
   no herdr call yet - footer renders the buffer. (green)
3. **Submit wiring**: `on_reply_submit` -> `prompt_agent` -> evict-on-success / keep-on-failure,
   unit-tested against the fake. (green)
4. **`space` binding + footer render** in the live loop (thin, like the existing key wiring).
5. **Launcher overlay**: switch `scripts/open-pane.sh` open to `--placement overlay`; manual E2E.
6. Update README (new keybind + overlay note), CHANGELOG, HANDOFF.

Each slice keeps `cargo fmt --check && cargo clippy -D warnings && cargo test` green.

---

## 6. Open questions / risks

**Load-bearing (advisor's sharpest catch): inline reply is a feedback loop.** Reply to an entry ->
the agent goes `working` -> finishes -> `done` fires -> it **re-enqueues**. Under `done`-keying,
every reply schedules a future ping. Triage a five-agent queue and you have booked five more entries
before you reach the end - a treadmill. **Resolution (decided now):**
- **Reply evicts immediately** (SS3 step 4). Reply _is_ acknowledgment of the current debt; the entry
  leaves the queue the instant the submit is accepted. This is the difference between an inbox and a
  treadmill.
- When the replied-to agent later finishes its new turn, `done` re-enqueues it **at the tail** - and
  that is _correct_, not a bug: it is a **new** debt (the agent now wants you again), and FIFO means
  oldest-debt-first. The existing `enqueue` upsert already appends new pane_ids at the tail with a
  fresh `enqueued_at_ms`, so this falls out for free. No special-casing.
- If the user does not want the follow-up ping, they were going to `jump` (watch it) or `drop`
  anyway - both evict. The loop only "runs" for agents the user actively keeps replying to, which is
  precisely the work they asked for.

Other:
- **Reply to a `done` (idle) agent vs a truly `blocked` one** - probe showed a reply routes into a
  waiting Claude agent cleanly. Verify once more that replying to a `done`-then-idle agent (not just
  the prose-question case) lands as a fresh turn and not a dropped keystroke. Low risk; covered by
  the manual E2E in slice 5.
- **Multi-line / long replies** - `agent prompt` takes one `<text>` arg; a single-line buffer is
  fine for triage answers ("yes", "use option B", "rebase onto main"). Defer multi-line.
- **Reply while the agent is `working`** - `agent prompt` from a working state queues into the
  active turn (per `--help`). Acceptable; the buffer is still delivered. Document, don't gate.
- **Global-summon overlay** - unverified (SS1 caveat). Not on the critical path.

---

## 7. Advisor pass (Fable)

A Fable advisor was consulted on the one load-bearing product-design call (queue-vs-native drift +
the `done`-noise question). Verbatim conclusions, now folded into SS2 and SS6:

1. **Stay strictly queue-backed; never grow a roster.** "herdr's native view answers where is
   everything now - that's state... your queue answers what happened that I haven't acknowledged -
   that's history, exactly what herdr throws away when the toast fades. An inbox is not a worse
   dashboard; it's a different data structure." Litmus test: does a feature operate on an enqueued
   entry? If no, reject. (-> SS2a)
2. **Fix `done`-noise with acknowledgment semantics, not signal filtering.** The queue holds
   unacknowledged turn-endings; jump/reply/drop are the only evictions; auto-acknowledge on organic
   focus (already implemented as evict-on-`pane.focused`). Do not sniff status/content - heuristics
   drop pings, the failure class the clinical review caught twice. (-> SS2b)
3. **Sharpest unnamed risk: the reply feedback loop / treadmill.** Reply must evict immediately
   (acknowledgment); the re-enqueued turn-end re-enters at the tail as new debt. (-> SS6)

The advice aligns with and sharpens the pre-draft design; no direction change resulted, one risk was
promoted to load-bearing. Used sparingly, per this repo's design-gate pattern (CLAUDE.md).
