# herdr-checkin

A durable attention queue for agent panes in [herdr](https://herdr.dev).

Herdr's native jump-to-notification only reaches the toast that is currently on screen.
Once a toast fades the ping is gone, and if two agents ping at once you can only reach the
last one. `herdr-checkin` remembers them: every agent that goes **blocked** (needs input) or
**done** (finished) is queued, and one keypress jumps you to the oldest waiter and pops it —
so no ping is lost and agents queue instead of racing.

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

`next` uses `herdr agent focus`, which brings the agent's workspace, tab, and pane all into
view, so a single press takes you straight to the waiting agent wherever it lives.

## Install

Requires **herdr >= 0.7.0** and a local Rust toolchain (the manifest builds from source with
Cargo).

```sh
herdr plugin install Akram012388/herdr-checkin
```

herdr clones the repo, runs the build step, and registers the three actions. Manage it with:

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

Then press your herdr prefix (default `ctrl+b`) followed by `alt+o`. `peek` and `clear` can be
bound the same way if you want them on keys.

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
