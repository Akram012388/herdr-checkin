# herdr-checkin

A durable attention queue for agent panes in [herdr](https://herdr.dev).

Herdr's native jump-to-notification only reaches the toast that is currently on screen.
Once a toast fades the ping is gone, and if two agents ping at once you can only reach the
last one. `herdr-checkin` remembers them: every agent that goes **blocked** (needs input) or
**done** (finished) is queued, and one keypress jumps you to the oldest waiter and pops it —
so no ping is lost and agents queue instead of racing. A keyboard-driven status pane (styled after
the Claude Code agents view) groups the waiters and lets you jump to — or reply straight into — the
one you pick.

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
settings — that herdr draws with a border and a "Check-in" title. It's a triage console styled after
the Claude Code agents view, and a richer alternative to the transient `peek` toast. Waiters are
grouped by status (**CHECKIN** for `blocked`, **DONE** for `done`, oldest-first within each), and you
act on them without leaving the keyboard — jump to one, or reply to it inline:

![The status pane: grouped waiters, reply inline, jump to an agent, drop entries, and clear the queue](docs/pane-demo.gif)

The same thing as text, if the GIF doesn't load:

```
4 agents waiting

  CHECKIN
> Claude — blocked — migrate auth to JWT [api, 8m]
  Codex — blocked — review terraform plan [infra, 3m]

  DONE
  Claude — done — fix flaky snapshot test [web, 5m]
  Claude — done — rewrite README intro [docs, 1m]
j/k move  ·  Enter jump  ·  space reply  ·  d drop  ·  c clear  ·  q quit
```

Only enqueued waiters ever appear — it is an inbox of what pinged you, not a live roster of every
agent (that is herdr's own view). Section headers are labels, not rows: they can't be selected.

| Key | Action |
| --- | --- |
| `j` / `k` (or down / up) | Move the selection, in on-screen order across the sections. |
| Left click | Select the clicked row (a click on a section header selects nothing). |
| `Enter` | Jump to the selected agent (`herdr agent focus`, cross-workspace), drop it from the queue, and close the popup. If the jump fails, the entry stays, the popup stays open, and the error shows in the footer. |
| `space` | **Reply inline:** open a compose strip for the selected agent (the queue dims behind it), type an answer, and `Enter` routes it into that agent's session (`herdr agent prompt`), then drops the entry. `Esc` cancels. A failed send keeps the entry. |
| `d` | Drop the selected entry without acting on it. |
| `c` | Clear the whole queue, after a `y` / `n` confirm in the footer. |
| `q` / `Esc` | Close the pane, dismissing its popup. |

Reply is fire-and-forget: the answer is delivered into the agent's session and the entry leaves the
queue the instant the send is accepted — the pane never blocks waiting for the agent's next turn. If
that agent later finishes again it re-enqueues at the tail, as a fresh waiter.

The pane refreshes itself — herdr delivers events only to short-lived handlers, not to a
running pane, so it re-reads the shared queue on a 250ms tick. As agents queue and drain (or you
act elsewhere), the list and the waiting-times update on their own.

The popup is a session-level singleton, so there's no open/focus/close toggle to reason about:
`open-pane` opens it, and it dismisses itself on `q`/`Esc` (or on a successful `Enter` jump).

## Install

Requires **herdr >= 0.7.5** and a local Rust toolchain (the manifest builds from source with
Cargo).

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
