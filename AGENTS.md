# herdr-checkin

Rust Herdr plugin with durable Queue and live Agents views. Read `HANDOFF.md` before changes; it is
the source of truth for current state, architecture, load-bearing invariants, and upstream gates.

## Rules

- Mutate queue state only through `StateStore::update`; act before eviction; keep startup additive
  and idempotent.
- Use `HERDR_PLUGIN_STATE_DIR`; never derive plugin state paths.
- Keep live-roster I/O off the render path.
- Do not publish, tag, or open upstream work without the approvals recorded in `HANDOFF.md`.

## Verify

`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`
