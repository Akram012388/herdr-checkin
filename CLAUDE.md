# CLAUDE.md

A Rust herdr plugin: a durable FIFO attention queue for agent panes.
**Read [HANDOFF.md](HANDOFF.md) first** — architecture, herdr API facts, invariants, and what to
build next.

## Model strategy

Three tiers. Use the right one for the job.

- **Opus — default, the orchestrator.** You. Plan, decide, delegate, integrate, own correctness.
- **Sonnet — the workhorse.** Delegate to Sonnet subagents: research, code exploration, scoping,
  and mechanical implementation.
- **Fable — the advisor.** When genuinely in doubt on a load-bearing call (an invariant, a race, an
  API contract), ask a Fable subagent for a second opinion. Sparingly — only when it matters.

## Checks (what CI runs)

`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`

## Invariants (don't regress — each has a test)

- Mutations go through `StateStore::update` (a delta under the lock), never a full write-back.
- `next` / pane `Enter`: focus first, evict only on success (a failed jump keeps the entry).
- `next`/`peek`: keep any entry with `max(enqueued_at_ms, last_touched_ms) >= snapshot`.
- `startup`: re-seed additively via the `enqueue` upsert — never a wholesale rewrite, never evict.

## Gotchas

- Use the `HERDR_PLUGIN_STATE_DIR` env var — the plugin id is percent-encoded in the path; never
  build the path yourself.
- Manifest id `Akram012388.checkin` != repo/dir name `herdr-checkin` — don't rename the dir.
- Keybinds live in the user's `~/.config/herdr/config.toml`, not the plugin manifest.
