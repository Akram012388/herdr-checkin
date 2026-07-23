# herdr-checkin

A durable attention queue for agent panes in [herdr](https://herdr.dev).

Herdr's native jump-to-notification only reaches the toast that is currently on screen.
Once a toast fades the ping is gone, and if two agents ping at once you can only reach the
last one. `herdr-checkin` remembers them: every agent that goes **blocked** (needs input) or
**done** (finished) is queued, and one keypress jumps you to the oldest waiter and pops it —
so no ping is lost and agents queue instead of racing. A keyboard-driven popup pairs that durable
Queue with a live Agents roster, letting you jump to—or reply straight into—any agent.

## How it works

The plugin subscribes to three herdr events and keeps a small FIFO queue on disk:

- `pane.agent_status_changed` — **enqueue** the pane when its agent goes `blocked` or `done`;
  **evict** it when the agent returns to `working`.
- `pane.focused` — evict the pane (you looked at it, so it no longer needs your attention).
- `pane.closed` — evict the pane (it is gone).

The queue is oldest-first and deduplicated per pane: if a pane pings twice (say `blocked`
then `done`) it keeps its original place in line rather than jumping to the back.

herdr runs the plugin binary once per event and once per action. State lives in `state.json`
under the plugin's state directory, guarded by a lockfile so concurrent events stay
consistent.

## Actions

| Action | What it does |
| --- | --- |
| `Akram012388.checkin.next` | Focus the oldest waiting agent and pop it from the queue. Cross-workspace jumps are allowed. Stale entries (pane gone, or resumed to `working`) are skipped. An empty queue is a clean no-op. |
| `Akram012388.checkin.peek` | Show the current queue as a herdr toast — how many agents are waiting, and for each: agent, status, title, workspace, and how long it has waited. |
| `Akram012388.checkin.clear` | Empty the queue. |
| `Akram012388.checkin.open-pane` | Open the **status pane** (see below) — a persistent, keyboard-driven triage console, rendered as a centered popup modal. |

`next` uses `herdr agent focus`, which brings the agent's workspace, tab, and pane all into
view, so a single press takes you straight to the waiting agent wherever it lives.

## The status pane

`open-pane` opens a persistent TUI as a centered, session-modal popup — like herdr's own `prefix+s`
settings — with two tabs:

- **Queue** is the durable attention inbox. It groups enqueued waiters into **CHECKIN** (`blocked`)
  and **DONE** (`done`), oldest-first within each section.
- **Agents** is a live roster grouped by workspace. Each row shows the agent identity, human
  destination, time in state, and last meaningful terminal line.

`Tab` or `Ctrl+S` switches views. The popup opens on Agents when the Queue is empty and on Queue when
someone is waiting; each view preserves its own selection.

![The two-tab Check-in popup: durable Queue, live Agents roster, and inline reply](docs/pane-demo.gif)

| Key | Action |
| --- | --- |
| `Tab` / `Ctrl+S` | Switch between Queue and Agents. |
| `j` / `k` (or down / up) | Move the selection in on-screen order. |
| Left click | Select the clicked agent or waiter; section headers are not selectable. |
| `Enter` | Jump to the selected agent (`herdr agent focus`, cross-workspace), drop it from the queue, and close the popup. If the jump fails, the entry stays, the popup stays open, and the error shows in the footer. |
| `space` | **Reply inline:** open a soft-wrapping compose strip, type an answer, and `Enter` routes it into the selected agent's session. `Esc` cancels; a failed send keeps any queued entry. |
| `d` | Queue only: drop the selected entry without acting on it. |
| `c` | Queue only: clear the whole queue after a `y` / `n` confirmation. |
| `q` / `Esc` | Close the pane, dismissing its popup. |

Reply is fire-and-forget: the answer is delivered into the agent's session and the entry leaves the
queue the instant the send is accepted — the pane never blocks waiting for the agent's next turn. If
that agent later finishes again it re-enqueues at the tail, as a fresh waiter.

The pane refreshes itself — herdr delivers events only to short-lived handlers, not to a
running pane, so it re-reads the shared queue on a 250ms tick. As agents queue and drain (or you
act elsewhere), the list and the waiting-times update on their own.

The popup is a session-level singleton, so there's no open/focus/close toggle to reason about:
`open-pane` opens it, and it dismisses itself on `q`/`Esc` (or on a successful `Enter` jump).

The popup consumes Herdr's resolved palette when `HERDR_PLUGIN_PANE_THEME_JSON` is available. The
current producing build is the `0.7.5-akram.1` downstream candidate. Stock Herdr 0.7.5 does not
provide that snapshot, but remains supported: Check-in opens with its established terminal-native
styling. A present but malformed or unsupported snapshot fails with an actionable error before raw
terminal mode begins.

## Install

Core functionality requires **Herdr >= 0.7.5** and a local Rust toolchain (the manifest builds from
source with Cargo). Full theme inheritance currently requires the `0.7.5-akram.1` downstream
candidate; the minimum remains 0.7.5 because the legacy styling fallback is intentionally supported.

```sh
herdr plugin install Akram012388/herdr-checkin
```

herdr clones the repo, runs the build step, and registers the actions. Manage it with:

```sh
herdr plugin list
herdr plugin action list --plugin Akram012388.checkin
herdr plugin uninstall Akram012388.checkin
```

## Binding a key

Keybinds live in your herdr config (`~/.config/herdr/config.toml`), not in the plugin. Bind
`next` to `prefix+alt+o` so it sits alongside the native notification jump (`prefix+o`):

```toml
[[keys.command]]
key = "prefix+alt+o"
type = "plugin_action"
command = "Akram012388.checkin.next"
description = "check-in: next waiter"
```

Reload the config after editing:

```sh
herdr config check && herdr server reload-config
```

Then press your herdr prefix (default `ctrl+b`) followed by `alt+o`. `peek`, `clear`, and
`open-pane` can be bound the same way — for example `prefix+alt+p` for `peek` and a key of your
choice for `open-pane`:

```toml
[[keys.command]]
key = "prefix+alt+p"
type = "plugin_action"
command = "Akram012388.checkin.peek"
description = "check-in: peek queue"
```

## Local development

```sh
cargo build --release
herdr plugin link /path/to/herdr-checkin
```

Invoke an action by hand and inspect the run:

```sh
herdr plugin action invoke next --plugin Akram012388.checkin
herdr plugin log list --plugin Akram012388.checkin
```

Run the checks used in CI:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
