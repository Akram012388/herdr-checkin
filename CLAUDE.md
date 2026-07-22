# CLAUDE.md

A Rust herdr plugin: a durable FIFO attention queue for agent panes. **Read [HANDOFF.md](HANDOFF.md)
first** — it has the architecture, herdr API facts, and the pending backlog.

## Checks (what CI runs)
`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`

## Dev loop
- `cargo build --release` then `herdr plugin link "$PWD"` to register the local build.
- `herdr plugin action invoke <next|peek|clear|open-pane> --plugin Akram012388.checkin`
- `herdr plugin log list --plugin Akram012388.checkin` — inspect event/action runs.
- Test the pane without real agents: seed `$HERDR_PLUGIN_STATE_DIR/state.json`, then
  `herdr pane read <id> --source visible` and `herdr pane send-keys <id> <key>`.

## Invariants (each has a regression test — don't regress)
- Mutations go through `StateStore::update` (delta under the lock), never a model write-back.
- `next` / pane `Enter`: focus first, evict only on success (a failed jump keeps the entry).
- `next`/`peek`: keep any entry with `max(enqueued_at_ms, last_touched_ms) >= snapshot` (don't
  prune what the pre-lock `pane list` snapshot couldn't see — including a persisted entry a
  concurrent event just refreshed; `last_touched_ms` is bumped by every `enqueue` upsert).
- `startup`: re-seed the queue additively via the `enqueue` upsert (never a wholesale rewrite); no
  eviction — leave stale-entry pruning to `next`/`peek`.
- Pure logic (queue transitions, `PaneModel`, `decide`) is unit-tested; keep the terminal loop thin.
  `tests/cli.rs` runs the built binary against a fake herdr on `HERDR_BIN_PATH`.

## Gotchas
- Always use the `HERDR_PLUGIN_STATE_DIR` env var — the plugin id is percent-encoded in the path,
  so never construct it.
- Manifest id `Akram012388.checkin` ≠ repo/dir name `herdr-checkin` — don't rename the dir.
- Keybinds live in the user's `~/.config/herdr/config.toml`, not the plugin manifest.
