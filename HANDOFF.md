# HANDOFF

Start here for the current state of Check-in and its Herdr pane-theme dependency.

## Current state

### Check-in

- Repository: `Akram012388/herdr-checkin`
- Branch: `main`; the current tip includes the interim demo checkpoint and the latest popup
  content-padding polish, and is synced with `origin/main`
- Version: `0.4.0`, not tagged
- Release wrap-up and final popup polish: committed and pushed; tagging remains explicitly deferred
- Validation on the current tree:
  - `cargo fmt --check` — pass
  - `cargo clippy --all-targets -- -D warnings` — pass
  - `cargo test` — pass: 179 library tests + 6 CLI tests
  - `cargo build --release` — pass

Latest Check-in changes:

- the `roster.json` registry now has a removal path: `startup`'s seed sweeps entries for departed
  panes (not in the live `pane list`, last observed before the startup snapshot) inside its existing
  locked update, so the registry stays bounded instead of accumulating closed panes forever. Live
  entries are untouched, too-new-to-judge entries are kept (the `next`/`peek` guard discipline), and
  the steady-state event/sampler paths pay nothing new
- compatibility copy now distinguishes stock Herdr 0.7.5 from the theme-producing
  `0.7.5-akram.1` downstream candidate
- README now documents both Queue and Agents tabs
- the Agents popup is centered against and dims the full Herdr frame
- Queue and Agents count labels plus their selection chevrons share a one-cell left inset, keeping
  interactive content off the popup border consistently
- each agent's status and time-in-state now share the identity row, leaving the second row for a
  brighter, longer terminal-context line
- Codex terminal-tail parsing now returns the final meaningful response above the input composer
  rather than the composer/footer itself
- blocked, done, and idle agents receive a two-phase initial tail sample, so settled context normally
  appears in the first populated paint; a bounded baseline still protects popup opening from slow
  terminal reads
- the checked-in `docs/pane-demo.gif` predates this final polish; a new 5.52-second, 1200x700
  candidate is rendered at `demo/herdr-checkin.gif` from the skill-compliant
  `demo/herdr-checkin.tape`. It records a real isolated Herdr session—not a standalone plugin
  pane—so the centered modal, full-frame background dimming, bright popup border, and inherited
  theme are represented faithfully. Akram accepted it only as an interim progress checkpoint, not
  as the final GIF. High-quality screenshots and a from-scratch GIF are deferred until the relevant
  upstream work is approved, and the interim candidate must not replace the README embed
- this handoff replaces the stale pre-theme work queue

### Herdr

Checkout: `../../herdr`

- `feature/plugin-pane-theme` at `768ba00`
  - clean, pushed, and synced with `origin/feature/plugin-pane-theme`
  - one upstream-ready commit over `upstream/master`
  - upstream [discussion #1796](https://github.com/ogulcancelik/herdr/discussions/1796) is posted
    and awaiting maintainer direction; no upstream issue or PR exists
- `akram` at `23147f9`, rebased onto the latest `upstream/master` (`c0fb777`), fully validated by
  the wrapper on 2026-07-24 (complete serialized suite green), pushed and synced with
  `origin/akram`; the built candidate `0.7.5-akram.20260724T1000` is installed and the live
  server was handed off to it (protocol 18, all panes preserved)
  - the wrapper now supports release cadence: `--release` rebases onto the newest upstream
    release tag (refusing when the tag is already contained, which would rewind the base), and
    every mode prints a release report comparing the newest tag against the stack and installed
    binary — the only new-release signal, since the downstream build rejects the official update
    checker. Intended cadence: `--check` for awareness, `--release` when it announces a release,
    master-tip mode for opportunistic pulls or validating an in-flight stack
  - carries a downstream test patch: upstream `e608a75` fakes an agent with `exec /bin/sleep`,
    but current macOS strips the environment block from `sysctl(KERN_PROCARGS2)` for Apple
    platform binaries, so the `HERDR_AGENT` hint is invisible and
    `live_handoff_keeps_unmanaged_agent_name_bound_to_saved_session` fails deterministically on
    pure `upstream/master` too (verified in an isolated worktree; non-platform binaries keep env
    readable, 5172 vs 168 procargs bytes). The patch execs Homebrew python3 instead; all 20
    live-handoff tests pass. Production detection is unaffected (real agents are non-platform
    binaries). Reporting this upstream is Akram's call and subject to the usual gates
  - contains the theme work, full-frame popup presentation, downstream update/install/backup/rollback
    management, and two separately reviewable stream/API test fixes
  - now also carries the smart update wrapper `scripts/akram-update.sh` (aliased as
    `herdr-akram-update` in `~/.zshrc`): refuses to run inside a Herdr pane, fast-paths when
    `akram` already contains `upstream/master`, stamps date-based `HERDR_BUILD_ID` values per
    sync, pushes the rebased stack with `--force-with-lease` on success, and reports
    `feature/plugin-pane-theme` drift via a `git merge-tree` test without rebasing it
  - `akram-manage-install.sh` gained `prune [keep]` and auto-prunes after each successful
    install: newest `HERDR_AKRAM_KEEP_BACKUPS` (default 5) downstream backups and
    `refs/akram-backups/` refs are kept; official baselines and the active rollback target are
    never pruned
  - validation runs as a stock build: the first terminal-driven update run failed 24 tests
    because the wrapper's compile-time `HERDR_BUILD_CHANNEL=akram` leaked into `cargo test`
    (turning the test binary into a downstream build whose own update rejection broke upstream
    update tests, poisoning `test_config_env_lock` for 20 more) and two update tests assert a
    noninteractive stdin. The sync script now scrubs the identity from clippy/test and feeds the
    suite `/dev/null` stdin; the wrapper records the exact validated commit
    (`last-validated-commit` in the state dir) and never fast-paths or pushes an unvalidated head
  - upstream branch `akbash/1752-forward-host-palette` investigated (2026-07-24): it is the
    maintainer's own automation (`akbash-bot`) fixing issue #1752 via merged PR #1759 — live OSC
    4/10/11 host-palette forwarding into every pane's Ghostty core. Orthogonal to the #1796
    plugin-pane theme contract (raw 256-slot resolved RGB via terminal queries vs semantic
    launch-time env snapshot); no overlap to reconcile, keep #1796 as posted and cite #1759 as
    adjacent prior art if the maintainer engages. #1796 and #1733 still have zero replies
  - local release candidate reports `herdr 0.7.5-akram.20260724T1000` (date-based build IDs are
    now stamped per sync)
  - `0.7.5-akram.20260724T1000` is installed at `~/.local/bin/herdr`; client and live-handoff
    server both run protocol 18
  - the prior `0.7.5-akram.1` builds and official `0.7.5` are backed up under
    `~/.local/state/herdr-akram/backups/`; the validation marker matches the pushed head

The `akram` branch changes:

- `scripts/akram-manage-install.sh`
- `scripts/akram-sync-and-build.sh`
- `scripts/akram-update.sh`
- `scripts/akram-downstream.md`
- `src/build_info.rs`
- `src/cli.rs`
- `src/update.rs`
- `src/api/server/pane_graphics_stream.rs`
- `tests/api_ping.rs`

Validation on the current Herdr tree:

- formatting and clippy with warnings denied — pass
- serialized Cargo suite — pass: 2,783 unit tests plus all integration suites
- maintenance Python suite — pass: 86 tests
- integration-asset Bun suites — pass: 17 tests
- plugin-marketplace Bun suite — pass: 12 tests
- downstream release build — pass; SHA-256
  `d3ad16eece49f8f9aa743128ca2d36a9d3ebb05da3872e8f4b625a41796ba5fc`
- real CLI probes — `herdr update` and `herdr channel set` both reject before changing config
- managed install/rollback lifecycle — pass in an isolated install root
- overridden or disposable install paths are proven unable to handoff the unrelated live server
- an already-current install repairs a stale running server, while a matching live server remains a
  no-op
- live switch from official `0.7.5`/protocol 17 — pass; all 30 panes transferred
- live switch to the modal build — pass; all 30 panes transferred and a rollback snapshot was
  created at `herdr-20260723T160536Z-0.7.5-akram.1`
- popup render coverage — pass; geometry uses the full frame, the background is dimmed, and the
  popup border remains undimmed
- fresh Check-in popup — opened successfully under the modal build; the configured theme is
  `one-dark`; Akram confirmed the final row layout and first-paint behavior live
- unavailable locally: `cargo-nextest` and `rustup`, so the literal `just check` wrapper and Windows
  cross-lint could not run

## Active dependency chain

1. [#8](https://github.com/Akram012388/herdr-checkin/issues/8) — **closed**. The theme snapshot was
   validated with native Herdr builds and dark, light, and terminal-default themes.
2. [#9](https://github.com/Akram012388/herdr-checkin/issues/9) — code and downstream candidate are
   ready locally. Upstream
   [discussion #1796](https://github.com/ogulcancelik/herdr/discussions/1796) proposes the pane-only
   theme contract and is awaiting maintainer direction. **Do not open an upstream issue or PR unless
   the maintainer accepts the direction, creates or converts an issue, and explicitly approves
   Akram's PR path (normally with `/approve @Akram012388`).**
3. [#10](https://github.com/Akram012388/herdr-checkin/issues/10) — consumer implementation and local
   validation are complete. Stock 0.7.5 keeps the legacy fallback; the first named producer is
   `0.7.5-akram.1`. Final official-version wording waits for #9 to land.
4. [#11](https://github.com/Akram012388/herdr-checkin/issues/11) — **closed as completed** after this
   handoff recorded the final popup polish, validation state, live confirmation, posted maintainer
   discussion, and remaining approval gates.
5. [#12](https://github.com/Akram012388/herdr-checkin/issues/12) — the existing README GIF predates
   the final row and first-paint polish. The new skill-generated candidate records the actual
   full-frame Herdr popup at `demo/herdr-checkin.gif` and is committed only as an interim progress
   checkpoint. It is explicitly not the final approved GIF. Akram will take high-quality screenshots
   and rebuild the GIF from scratch only after the relevant upstream work is approved; final visual
   approval remains required before the README embed changes or the issue closes.

Issues #1 through #8 and #11 are closed. Pin-to-top (#7) was implemented, reviewed, declined,
reverted, and scrubbed; it is not a pending feature.

## What the product does

Check-in is a Herdr plugin with one popup and two tabs:

- **Queue** — a durable FIFO attention ledger. `blocked`/`done` events enqueue; focus, close, return
  to working, successful jump, or successful reply evict.
- **Agents** — a live roster grouped by workspace with identity, human destination, time in state,
  and the last meaningful terminal line.

`Tab`/`Ctrl+S` toggles. Both tabs support selection, `Enter` jump, and `space` reply. Queue alone
supports `d` drop and `c` clear. The popup opens on Agents when Queue is empty and on Queue when a
waiter exists.

## Theme contract

Herdr resolves the palette once at pane launch and injects a protected
`HERDR_PLUGIN_PANE_THEME_JSON` snapshot:

- schema version 1
- effective theme name plus all 16 semantic palette fields
- Reset, ANSI, indexed, and RGB colors remain lossless
- plugins cannot spoof the protected value
- action, event, startup, and link-handler processes do not inherit it

Check-in parses the snapshot before raw terminal mode and maps it across Queue, Agents, tabs,
scrollbar, compose, selection, hints, and placeholders. Missing snapshot means the established
terminal-native fallback; malformed or unsupported snapshots fail early with an actionable error.

`min_herdr_version` remains `0.7.5` because the non-themed fallback is intentionally supported and
Herdr's manifest version field cannot express the downstream suffix. README/CHANGELOG name the
actual producing candidate explicitly.

## Load-bearing invariants

1. Queue mutations use `StateStore::update`; never overwrite a stale full snapshot.
2. Jump/reply act first and evict only after success.
3. `next`/`peek` retain entries newer than their liveness snapshot.
4. Startup re-seeding is additive and idempotent.
5. Tab switching never touches popup lifecycle.
6. `queue.rs` and `roster.rs` stay independent of the Herdr trait.
7. `roster.json` is a prunable observation cache; deleting it may lose timers, never a ping. Its
   only removal path is `startup`'s departed-pane sweep, which never touches live or
   too-new-to-judge entries.
8. CLI calls for the live roster run on the sampler thread, never the render tick.

## Next actions

1. Monitor upstream
   [discussion #1796](https://github.com/ogulcancelik/herdr/discussions/1796) for maintainer direction
   on the pane-theme contract tracked by #9.
2. Keep full-frame popup presentation separately reviewable through existing upstream
   [discussion #1733](https://github.com/ogulcancelik/herdr/discussions/1733); approval on one
   discussion does not imply approval on the other.
3. Only if the maintainer creates or converts an accepted issue and explicitly approves Akram's PR
   path, prepare the smallest upstream PR for that approved scope. Start with the already-isolated
   `feature/plugin-pane-theme` branch when #1796 is approved.
4. After an official Herdr version contains the theme contract, replace downstream-candidate
   wording, update Check-in's final version gate, rerun validation, and close #9/#10.
5. Once the relevant upstream work is approved and settled, take high-quality screenshots and
   rebuild the final demo GIF from scratch. After Akram's visual approval, update the README embed
   and close #12.
6. Tag Check-in `v0.4.0` or publish a fork release only with Akram's explicit approval.

Do not open an upstream issue or PR before Herdr's accepted-issue and contributor-approval gates are
satisfied. Until the upstream outcome is settled, do not replace the README GIF, remove the prior
artifact, or treat the interim candidate as final.

## Downstream modal behavior

- Session-modal terminal popups now center and size against the full Herdr frame.
- The interface behind them is dimmed to match native settings-modal presentation.
- Plugin actions and custom commands using `placement = "popup"` share the behavior.

This is implemented and installed on the `akram` fork. Any upstream proposal remains subject to
Herdr's contributor approval process.
