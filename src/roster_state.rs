//! Persisted roster registry: `roster.json` under `HERDR_PLUGIN_STATE_DIR`, guarded by its own
//! lockfile — a **separate store from `state.json`** (design §5). It holds only the time-in-state
//! registry (and, from Slice 6, pins). All mutations go through [`RosterStore::update`] — a delta
//! under the lock, temp+rename, exactly like [`crate::state::StateStore`].
//!
//! **Invariant #7 — this file is a prunable observation cache.** Nothing correctness-critical may
//! live *only* here: deleting `roster.json` must merely degrade timers/pins, never lose a ping. The
//! queue's durability lives in `state.json`; this store is best-effort throughout (every writer
//! ignores its errors), so a corrupt/absent registry simply renders honest `~` timers.
//!
//! **Provenance (design §4 — the correctness fix).** The popup is a summon-and-glance modal, not
//! running most of the day, so its poll loop *cannot* be the observer of state transitions — it
//! would render a fabricated `0s` at open. Instead the **`status-changed` event binary** (which
//! fires on every transition, popup open or not, with a wall clock in hand) stamps
//! `status_since_ms` via [`stamp_status`]. The pane sampler only **reads** the timer and
//! **back-fills** the session uuid ([`reconcile_pane`]); it resets the timer only on a genuine
//! identity change (a reused pane slot), never on a mere status difference.

use crate::roster::RosterAgent;
use crate::state::{current_unix_ms, PluginError};
use crate::RuntimeEnv;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) const ROSTER_FILE_NAME: &str = "roster.json";
const LOCK_FILE_NAME: &str = "roster.lock";
/// Bumped to 2 in Slice 6 (the `pins` field). A stored v1 file loads unchanged (`pins` is
/// `#[serde(default)]`, so absent = empty) and is rewritten at version 2 on its next update — harmless.
const ROSTER_VERSION: u32 = 2;

/// A pinned agent (Slice 6 / issue #7). Keyed by **`agent_session` uuid**, never `pane_id` (pane ids
/// are positional and reused, so a new agent in a recycled pane slot must not inherit the pin). The
/// pins list order *is* the pin order (list index = the agent's [`RosterAgent::pin_rank`]).
/// `pinned_at_ms` is when it was pinned; `last_seen_ms` is the last time this uuid was live in a
/// sample — it stops advancing once the agent vanishes (the pin becomes a **tombstone**), and drives
/// the GC that keeps the list bounded ([`gc_pins`]). If the uuid reappears (a resumed session) the
/// pin re-applies silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pin {
    pub(crate) agent_session: String,
    pub(crate) pinned_at_ms: u64,
    pub(crate) last_seen_ms: u64,
}

/// A tombstone (vanished-agent) pin is GC'd once it has gone unseen this long (~7 days), so a pin
/// survives a normal close/reopen and even a machine reboot, but a long-dead session eventually lapses.
const PIN_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
/// A hard cap on the pins list: past this, the oldest-seen tombstones are dropped first (live pins,
/// whose `last_seen_ms` was just bumped, are the newest and always survive). Bounds `roster.json`.
const PIN_CAP: usize = 50;

/// The full in-memory `roster.json` payload the store's [`RosterStore::update`] hands to its change
/// closure: the time-in-state [`Registry`] (Slice 5) plus the [`Pin`] list (Slice 6), so one locked
/// read-modify-write covers both. The `version` is not exposed here — it is stamped on write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RosterState {
    pub(crate) registry: Registry,
    pub(crate) pins: Vec<Pin>,
}

/// One pane's observed state in the registry, keyed by `pane_id` in [`Registry`]. `agent_session`
/// (the stable session uuid) is `None` until the pane sampler back-fills it — the event payload that
/// stamps transitions carries no uuid (design §4). `status_since_ms` is the wall clock of the last
/// transition into `status`; `first_seen_ms`/`last_seen_ms` bracket the pane's lifetime for the
/// tombstone GC that arrives with pins (Slice 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegistryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_session: Option<String>,
    pub(crate) status: String,
    pub(crate) status_since_ms: u64,
    pub(crate) first_seen_ms: u64,
    pub(crate) last_seen_ms: u64,
}

/// The time-in-state registry: `pane_id -> RegistryEntry`. A `BTreeMap` so the on-disk JSON is
/// deterministic (sorted keys) — stable diffs, stable tests.
pub(crate) type Registry = BTreeMap<String, RegistryEntry>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedRoster {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    agents: Registry,
    #[serde(default)]
    pins: Vec<Pin>,
}

struct LoadedRoster {
    state: RosterState,
    needs_repair: bool,
}

pub(crate) struct RosterStore {
    state_dir: PathBuf,
}

impl RosterStore {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
        }
    }

    /// Load the registry under an exclusive lock, apply `change`, and persist the result only if it
    /// changed (or the on-disk form needed repair) — the same delta-under-lock discipline as
    /// [`crate::state::StateStore::update`], on `roster.json`'s own lock.
    pub(crate) fn update<T>(
        &self,
        change: impl FnOnce(RosterState) -> (RosterState, T),
    ) -> Result<T, PluginError> {
        fs::create_dir_all(&self.state_dir).map_err(|error| {
            PluginError::new(format!(
                "failed to create plugin state directory {}: {error}",
                self.state_dir.display()
            ))
        })?;

        let _lock = RosterLock::acquire(&self.state_dir.join(LOCK_FILE_NAME))?;
        let loaded = read_roster(&self.state_dir.join(ROSTER_FILE_NAME))?;
        let previous = loaded.state.clone();
        let (next, result) = change(loaded.state);

        if loaded.needs_repair || next != previous {
            write_roster(&self.state_dir.join(ROSTER_FILE_NAME), &next)?;
        }

        Ok(result)
    }
}

struct RosterLock {
    file: File,
}

impl RosterLock {
    fn acquire(path: &Path) -> Result<Self, PluginError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to open roster lock {}: {error}",
                    path.display()
                ))
            })?;

        file.lock_exclusive().map_err(|error| {
            PluginError::new(format!("failed to lock roster {}: {error}", path.display()))
        })?;

        Ok(Self { file })
    }
}

impl Drop for RosterLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn read_roster(path: &Path) -> Result<LoadedRoster, PluginError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedRoster {
                state: RosterState::default(),
                needs_repair: false,
            });
        }
        Err(error) => {
            return Err(PluginError::new(format!(
                "failed to read roster {}: {error}",
                path.display()
            )));
        }
    };

    match serde_json::from_str::<PersistedRoster>(&contents) {
        Ok(persisted) => Ok(LoadedRoster {
            needs_repair: persisted.version < ROSTER_VERSION,
            state: RosterState {
                registry: persisted.agents,
                pins: persisted.pins,
            },
        }),
        // A prunable cache: a corrupt file degrades to an empty state (honest `~` timers, no pins),
        // never an error that could stall the event/queue path that best-effort-calls us.
        Err(_) => Ok(LoadedRoster {
            state: RosterState::default(),
            needs_repair: true,
        }),
    }
}

fn write_roster(path: &Path, state: &RosterState) -> Result<(), PluginError> {
    let parent = path.parent().ok_or_else(|| {
        PluginError::new(format!(
            "roster path has no parent directory: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        PluginError::new(format!(
            "failed to create plugin state directory {}: {error}",
            parent.display()
        ))
    })?;

    let temp_path = parent.join(format!(
        ".{ROSTER_FILE_NAME}.tmp.{}.{}",
        std::process::id(),
        current_unix_ms()
    ));
    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|error| {
            PluginError::new(format!(
                "failed to create temporary roster {}: {error}",
                temp_path.display()
            ))
        })?;
    let persisted = PersistedRoster {
        version: ROSTER_VERSION,
        agents: state.registry.clone(),
        pins: state.pins.clone(),
    };
    // Any failure after the temp file exists must not leave it behind as litter in the state dir.
    serde_json::to_writer_pretty(&mut temp_file, &persisted).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to serialize roster {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.write_all(b"\n").map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to write roster {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.sync_all().map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to sync roster {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to replace roster {}: {error}",
            path.display()
        ))
    })
}

/// Read the registry without the lock — writes are atomic temp+rename, so a reader always sees a
/// complete file — degrading to an empty registry on any error (invariant #7). The analogue of
/// [`crate::state::load_entries`]. Currently only the tests read the registry out-of-band: production
/// reads happen inside [`reconcile_roster`]'s locked update (which both reads and back-fills in one
/// pass), so this is gated to the test build to keep the release binary warning-clean.
#[cfg(test)]
pub(crate) fn load_registry(state_dir: &Path) -> Registry {
    load_roster_state(state_dir).registry
}

/// Read the whole persisted state (registry + pins) without the lock, degrading to an empty state on
/// any error (invariant #7). Test-only for the same reason as [`load_registry`] — production reads
/// happen inside the locked [`reconcile_roster`] pass.
#[cfg(test)]
pub(crate) fn load_roster_state(state_dir: &Path) -> RosterState {
    read_roster(&state_dir.join(ROSTER_FILE_NAME))
        .map(|loaded| loaded.state)
        .unwrap_or_default()
}

// --- pure registry transitions ---------------------------------------------

/// Stamp a status transition for `pane_id` (the event binary's job). A *new* pane is inserted with
/// its timer starting now; an *existing* pane resets `status_since_ms` only when the status actually
/// changed (a re-emitted identical status just refreshes `last_seen_ms`, never restarting the
/// clock). The session uuid is left untouched — the event payload carries none, so the pane sampler
/// back-fills it ([`reconcile_pane`]).
pub(crate) fn stamp_status(registry: &mut Registry, pane_id: &str, status: &str, now_ms: u64) {
    match registry.get_mut(pane_id) {
        Some(entry) => {
            if entry.status != status {
                entry.status = status.to_string();
                entry.status_since_ms = now_ms;
            }
            entry.last_seen_ms = now_ms;
        }
        None => {
            registry.insert(
                pane_id.to_string(),
                RegistryEntry {
                    agent_session: None,
                    status: status.to_string(),
                    status_since_ms: now_ms,
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                },
            );
        }
    }
}

/// Seed a pane into the registry **additively** for `startup` (invariant #4): a missing pane gets a
/// fresh entry timed from now, but an existing entry is left entirely untouched — its
/// `status_since_ms` (which may predate a herdr restart, or have been event-stamped since) is never
/// reset. That makes `startup` idempotent: a second run finds every pane present and changes nothing.
pub(crate) fn seed_status(registry: &mut Registry, pane_id: &str, status: &str, now_ms: u64) {
    registry
        .entry(pane_id.to_string())
        .or_insert_with(|| RegistryEntry {
            agent_session: None,
            status: status.to_string(),
            status_since_ms: now_ms,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
        });
}

/// The pane sampler's per-pane reconcile: back-fill the live session uuid and detect a reused pane
/// slot, returning the `status_since_ms` the row should trust (or `None` — render `~` — when the
/// pane has no registry entry). This never fabricates a transition time from a status difference
/// (design §4); it resets the timer *only* when the recorded uuid disagrees with the live one, which
/// means a **different agent** now occupies this positional pane id and the old timer is not ours.
pub(crate) fn reconcile_pane(
    registry: &mut Registry,
    pane_id: &str,
    live_session: Option<&str>,
    live_status: &str,
    now_ms: u64,
) -> Option<u64> {
    let entry = registry.get_mut(pane_id)?;
    match (entry.agent_session.as_deref(), live_session) {
        // Reused slot: a different session now. Reset to an honest "seen since now" for the new
        // agent, adopting its live status so the registry stays coherent until its next event.
        (Some(stored), Some(live)) if stored != live => {
            *entry = RegistryEntry {
                agent_session: Some(live.to_string()),
                status: live_status.to_string(),
                status_since_ms: now_ms,
                first_seen_ms: now_ms,
                last_seen_ms: now_ms,
            };
        }
        // First time the pane learns its uuid (the event stamps carry none): back-fill only, keeping
        // the event-stamped timer intact.
        (None, Some(live)) => entry.agent_session = Some(live.to_string()),
        // Matching uuid, or a session-less agent we can't disambiguate: trust the recorded timer.
        _ => {}
    }
    Some(entry.status_since_ms)
}

// --- pure pin transitions (Slice 6) ----------------------------------------

/// Toggle the pin for `agent_session`: remove it if present (unpin), else append it (pin). Appending
/// keeps list order = pin order, so a freshly pinned agent lands at the bottom of the existing pins
/// (highest rank) and floats above every unpinned row. Returns the new pinned state (`true` = now
/// pinned). A session-less agent never reaches here (the caller guards on a real uuid).
pub(crate) fn toggle_pin(pins: &mut Vec<Pin>, agent_session: &str, now_ms: u64) -> bool {
    match pins.iter().position(|p| p.agent_session == agent_session) {
        Some(index) => {
            pins.remove(index);
            false
        }
        None => {
            pins.push(Pin {
                agent_session: agent_session.to_string(),
                pinned_at_ms: now_ms,
                last_seen_ms: now_ms,
            });
            true
        }
    }
}

/// This session's rank in the pins list (its index), or `None` when it is not pinned. The list order
/// is the pin order, so the index is exactly [`RosterAgent::pin_rank`].
pub(crate) fn pin_rank(pins: &[Pin], agent_session: &str) -> Option<usize> {
    pins.iter().position(|p| p.agent_session == agent_session)
}

/// Reconcile the pins list against a freshly sampled roster: refresh `last_seen_ms` for every pinned
/// session that is currently live (so a present pin is never mistaken for a tombstone), then
/// [`gc_pins`]. A pin whose session is absent keeps its stale `last_seen_ms` — it is a tombstone,
/// retained so the pin re-applies if the session resumes, until GC lapses it.
pub(crate) fn reconcile_pins(pins: &mut Vec<Pin>, live_sessions: &[Option<&str>], now_ms: u64) {
    for session in live_sessions.iter().flatten() {
        if let Some(pin) = pins.iter_mut().find(|p| p.agent_session == *session) {
            pin.last_seen_ms = now_ms;
        }
    }
    gc_pins(pins, now_ms);
}

/// Bound the pins list: drop tombstones unseen for longer than [`PIN_TTL_MS`], then, if still over
/// [`PIN_CAP`], drop the oldest-seen pins until it fits. A live pin's `last_seen_ms` is `now` (just
/// bumped by [`reconcile_pins`]), so it is always the newest and never GC'd — only stale tombstones
/// are shed. Preserves pin order among the survivors (a stable partition, not a re-sort).
fn gc_pins(pins: &mut Vec<Pin>, now_ms: u64) {
    pins.retain(|pin| now_ms.saturating_sub(pin.last_seen_ms) <= PIN_TTL_MS);
    if pins.len() <= PIN_CAP {
        return;
    }
    // Over cap: find the `last_seen_ms` cutoff of the CAP newest pins and keep only those at or above
    // it, without disturbing the list (pin) order of the survivors.
    let mut seens: Vec<u64> = pins.iter().map(|p| p.last_seen_ms).collect();
    seens.sort_unstable();
    let cutoff = seens[pins.len() - PIN_CAP];
    let mut kept = 0usize;
    pins.retain(|pin| {
        // Keep pins newer than the cutoff outright; for pins exactly at the cutoff (ties), keep only
        // as many as the cap allows so the total lands at PIN_CAP, dropping the later-in-list ties.
        let keep = pin.last_seen_ms > cutoff || (pin.last_seen_ms == cutoff && kept < PIN_CAP);
        if keep {
            kept += 1;
        }
        keep
    });
}

// --- runtime bridges (best-effort throughout — invariant #7) ---------------

/// Stamp the roster registry from a `status-changed` event, best-effort. Called from the dispatch
/// layer *after* the queue mutation, so `queue.rs` never learns the registry exists (mirroring how
/// `enrich_location` is injected). Unlike the queue path this fires for **every** `agent_status` —
/// `idle`/`working` included — because the Agents view times every agent, not just waiters. An
/// absent/unparseable event or a `roster.json` failure is swallowed: a timer is never worth failing
/// the event that keeps the durable queue correct.
pub(crate) fn stamp_status_changed(runtime: &RuntimeEnv) {
    let Some(event) = runtime
        .event_json
        .as_deref()
        .and_then(crate::herdr::parse_status_event)
    else {
        return;
    };
    let now_ms = runtime.now_ms;
    let _ = RosterStore::new(&runtime.state_dir).update(|mut state| {
        stamp_status(
            &mut state.registry,
            &event.pane_id,
            &event.agent_status,
            now_ms,
        );
        (state, ())
    });
}

/// Seed the registry from `startup`'s `pane list`, additively (invariant #4), best-effort. Runs
/// after the queue re-seed so a `roster.json` problem can never abort re-seeding the durable queue.
pub(crate) fn seed_registry<'a>(
    runtime: &RuntimeEnv,
    panes: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    let now_ms = runtime.now_ms;
    let _ = RosterStore::new(&runtime.state_dir).update(|mut state| {
        for (pane_id, status) in panes {
            seed_status(&mut state.registry, pane_id, status, now_ms);
        }
        (state, ())
    });
}

/// Reconcile a freshly sampled roster against the registry (on the sampler thread), filling each
/// agent's [`RosterAgent::status_since_ms`] for the row's time-in-state. Back-fills session uuids and
/// resets reused-slot timers as a delta — a no-op write in steady state (every uuid already recorded
/// and matching). Best-effort: a `roster.json` failure leaves every `status_since_ms` at `None`, so
/// the rows honestly show `~` rather than blanking (invariant #7).
pub(crate) fn reconcile_roster(state_dir: &Path, agents: &mut [RosterAgent], now_ms: u64) {
    let result = RosterStore::new(state_dir).update(|mut state| {
        let sinces: Vec<Option<u64>> = agents
            .iter()
            .map(|agent| {
                reconcile_pane(
                    &mut state.registry,
                    &agent.pane_id,
                    agent.agent_session.as_deref(),
                    agent.agent_status.as_str(),
                    now_ms,
                )
            })
            .collect();
        // Pins (Slice 6): bump last_seen for every live pinned session (so a present pin never
        // tombstones), GC lapsed/overflowing tombstones, then read each agent's rank from the list.
        let live_sessions: Vec<Option<&str>> =
            agents.iter().map(|a| a.agent_session.as_deref()).collect();
        reconcile_pins(&mut state.pins, &live_sessions, now_ms);
        let ranks: Vec<Option<usize>> = live_sessions
            .iter()
            .map(|session| session.and_then(|s| pin_rank(&state.pins, s)))
            .collect();
        (state, (sinces, ranks))
    });
    if let Ok((sinces, ranks)) = result {
        for ((agent, since), rank) in agents.iter_mut().zip(sinces).zip(ranks) {
            agent.status_since_ms = since;
            agent.pin_rank = rank;
        }
    }
}

/// Toggle a pin for `agent_session` and persist it, returning the resulting pins list (so the caller
/// can re-derive every visible agent's [`RosterAgent::pin_rank`] for instant feedback without waiting
/// for the next sample). Best-effort: `None` on any `roster.json` failure, and the caller then leaves
/// the on-screen roster untouched — the next reconcile restores from disk (invariant #7).
pub(crate) fn toggle_pin_persist(
    state_dir: &Path,
    agent_session: &str,
    now_ms: u64,
) -> Option<Vec<Pin>> {
    RosterStore::new(state_dir)
        .update(|mut state| {
            toggle_pin(&mut state.pins, agent_session, now_ms);
            let pins = state.pins.clone();
            (state, pins)
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::AgentStatus;
    use crate::test_support::temp_state_dir;
    use std::fs;

    fn entry(registry: &Registry, pane_id: &str) -> RegistryEntry {
        registry
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| panic!("expected a registry entry for {pane_id}"))
    }

    #[test]
    fn stamp_status_inserts_a_new_pane_timed_from_now() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(e.status, "blocked");
        assert_eq!(e.status_since_ms, 1_000);
        assert_eq!(e.first_seen_ms, 1_000);
        assert_eq!(e.agent_session, None, "the event carries no session uuid");
    }

    #[test]
    fn stamp_status_resets_the_clock_only_on_a_real_transition() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "working", 1_000);
        // Same status re-emitted: refresh last_seen but keep the clock.
        stamp_status(&mut registry, "w1:p1", "working", 2_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(
            e.status_since_ms, 1_000,
            "an identical status keeps the clock"
        );
        assert_eq!(e.last_seen_ms, 2_000, "but last_seen advances");
        // A genuine transition restarts the clock.
        stamp_status(&mut registry, "w1:p1", "blocked", 3_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(e.status, "blocked");
        assert_eq!(e.status_since_ms, 3_000);
        assert_eq!(e.first_seen_ms, 1_000, "first_seen never moves");
    }

    #[test]
    fn seed_status_is_additive_and_idempotent() {
        let mut registry = Registry::new();
        seed_status(&mut registry, "w1:p1", "blocked", 1_000);
        // A surviving entry is never reset by a later seed (invariant #4).
        seed_status(&mut registry, "w1:p1", "done", 9_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(
            e.status, "blocked",
            "seed never overwrites a surviving entry"
        );
        assert_eq!(e.status_since_ms, 1_000, "and never resets its clock");
    }

    #[test]
    fn reconcile_backfills_the_uuid_without_touching_the_timer() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        let since = reconcile_pane(&mut registry, "w1:p1", Some("uuid-A"), "blocked", 5_000);
        assert_eq!(
            since,
            Some(1_000),
            "the event-stamped timer is trusted, not reset"
        );
        assert_eq!(
            entry(&registry, "w1:p1").agent_session.as_deref(),
            Some("uuid-A")
        );
    }

    #[test]
    fn reconcile_resets_the_timer_on_a_reused_pane_slot() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        reconcile_pane(&mut registry, "w1:p1", Some("uuid-A"), "blocked", 2_000);
        // A different session now occupies the same positional pane id: the old timer is not ours.
        let since = reconcile_pane(&mut registry, "w1:p1", Some("uuid-B"), "working", 8_000);
        assert_eq!(since, Some(8_000), "a reused slot resets to seen-since-now");
        let e = entry(&registry, "w1:p1");
        assert_eq!(e.agent_session.as_deref(), Some("uuid-B"));
        assert_eq!(
            e.status, "working",
            "and adopts the new agent's live status"
        );
    }

    #[test]
    fn reconcile_is_none_for_a_pane_with_no_registry_entry() {
        let mut registry = Registry::new();
        // No entry, no honest reading -> None -> the row renders `~`.
        assert_eq!(
            reconcile_pane(&mut registry, "w9:p9", Some("uuid-Z"), "idle", 1_000),
            None
        );
        assert!(registry.is_empty(), "reconcile never creates entries");
    }

    #[test]
    fn reconcile_trusts_a_sessionless_agents_timer() {
        // Some agents list no session uuid (seen live for a non-Claude/Codex agent). We can't detect
        // a reuse, so we trust the recorded timer rather than guessing.
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        let since = reconcile_pane(&mut registry, "w1:p1", None, "blocked", 5_000);
        assert_eq!(since, Some(1_000));
    }

    #[test]
    fn store_round_trips_through_the_lock_and_repairs_junk() {
        let dir = temp_state_dir("roster-store");
        let store = RosterStore::new(&dir);
        store
            .update(|mut state| {
                stamp_status(&mut state.registry, "w1:p1", "blocked", 1_000);
                (state, ())
            })
            .expect("first write should succeed");
        let loaded = load_registry(&dir);
        assert_eq!(
            loaded.get("w1:p1").map(|e| e.status.as_str()),
            Some("blocked")
        );

        // A corrupt file degrades to empty and is repaired on the next write (invariant #7).
        fs::write(dir.join(ROSTER_FILE_NAME), "not json").expect("clobber the roster");
        assert!(
            load_registry(&dir).is_empty(),
            "junk reads as an empty registry"
        );
        store
            .update(|mut state| {
                stamp_status(&mut state.registry, "w2:p1", "done", 2_000);
                (state, ())
            })
            .expect("write over junk should succeed");
        let loaded = load_registry(&dir);
        assert_eq!(loaded.len(), 1, "the junk was repaired, not merged");
        assert!(loaded.contains_key("w2:p1"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconcile_roster_fills_status_since_and_survives_a_missing_file() {
        let dir = temp_state_dir("roster-reconcile");
        // Seed one pane via a transition stamp; leave a second pane unknown to the registry.
        RosterStore::new(&dir)
            .update(|mut state| {
                stamp_status(&mut state.registry, "w1:p1", "blocked", 1_000);
                (state, ())
            })
            .expect("seed write");

        let mut agents = vec![
            sample_agent("w1:p1", AgentStatus::Blocked, Some("uuid-A")),
            sample_agent("w2:p1", AgentStatus::Working, Some("uuid-B")),
        ];
        reconcile_roster(&dir, &mut agents, 5_000);
        assert_eq!(
            agents[0].status_since_ms,
            Some(1_000),
            "known pane trusts its timer"
        );
        assert_eq!(agents[1].status_since_ms, None, "unknown pane stays `~`");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stamp_status_changed_is_a_data_path_from_the_event_json() {
        // The dispatch bridge: a raw status-changed event JSON stamps the registry (acceptance #2 at
        // the lib level; tests/cli.rs proves the same through the built binary).
        let dir = temp_state_dir("roster-stamp-bridge");
        let mut rt = crate::test_support::runtime(dir.clone(), 7_000);
        rt.event_json = Some(crate::test_support::status_event_json(
            "w1:p1", "w1", "working", "building",
        ));
        stamp_status_changed(&rt);
        let entry = entry(&load_registry(&dir), "w1:p1");
        assert_eq!(entry.status, "working");
        assert_eq!(entry.status_since_ms, 7_000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn seed_registry_is_additive_and_idempotent_across_runs() {
        // startup idempotence on roster.json (acceptance #3): a second seed, even at a later clock,
        // leaves every surviving entry's timer untouched.
        let dir = temp_state_dir("roster-seed-idempotent");
        let panes = [("w1:p1", "blocked"), ("w2:p1", "idle")];
        let rt_a = crate::test_support::runtime(dir.clone(), 1_000);
        seed_registry(&rt_a, panes.iter().map(|(p, s)| (*p, *s)));
        let rt_b = crate::test_support::runtime(dir.clone(), 9_000);
        seed_registry(&rt_b, panes.iter().map(|(p, s)| (*p, *s)));

        let registry = load_registry(&dir);
        assert_eq!(registry.len(), 2);
        assert_eq!(entry(&registry, "w1:p1").status_since_ms, 1_000);
        assert_eq!(entry(&registry, "w2:p1").status_since_ms, 1_000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invariant_7_deleting_roster_json_only_degrades_timers() {
        // roster.json is a prunable observation cache: deleting it must lose no correctness — the
        // registry reads back empty, and reconcile then renders honest `~` (None) instead of a timer.
        let dir = temp_state_dir("roster-invariant-7");
        RosterStore::new(&dir)
            .update(|mut state| {
                stamp_status(&mut state.registry, "w1:p1", "blocked", 1_000);
                (state, ())
            })
            .expect("seed the registry");
        fs::remove_file(dir.join(ROSTER_FILE_NAME)).expect("delete roster.json");

        assert!(
            load_registry(&dir).is_empty(),
            "a missing file degrades to empty"
        );
        let mut agents = vec![sample_agent("w1:p1", AgentStatus::Blocked, Some("uuid-A"))];
        reconcile_roster(&dir, &mut agents, 5_000);
        assert_eq!(
            agents[0].status_since_ms, None,
            "with the file gone the row shows `~`, never a stale or fabricated timer"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconcile_roster_never_writes_state_json() {
        // The pane's roster path touches only roster.json (acceptance #1): reconciling must never
        // create or write state.json — the durable queue store is off-limits to the live view.
        let dir = temp_state_dir("roster-no-state-write");
        let mut agents = vec![sample_agent("w1:p1", AgentStatus::Working, Some("uuid-A"))];
        reconcile_roster(&dir, &mut agents, 1_000);
        assert!(
            !dir.join(crate::state::STATE_FILE_NAME).exists(),
            "the roster path must not write state.json"
        );
        let _ = fs::remove_dir_all(dir);
    }

    // --- pins (Slice 6 / issue #7) -----------------------------------------

    #[test]
    fn toggle_pin_adds_then_removes_keyed_by_session() {
        let mut pins = Vec::new();
        assert!(toggle_pin(&mut pins, "uuid-A", 1_000), "first toggle pins");
        assert_eq!(pin_rank(&pins, "uuid-A"), Some(0));
        assert_eq!(pins[0].pinned_at_ms, 1_000);
        // A second pin appends (list order = pin order), so the newer pin ranks below the first.
        assert!(toggle_pin(&mut pins, "uuid-B", 2_000));
        assert_eq!(pin_rank(&pins, "uuid-B"), Some(1));
        // Toggling an existing pin unpins it and the survivor's rank compacts back to the front.
        assert!(!toggle_pin(&mut pins, "uuid-A", 3_000), "re-toggle unpins");
        assert_eq!(pin_rank(&pins, "uuid-A"), None);
        assert_eq!(pin_rank(&pins, "uuid-B"), Some(0));
    }

    #[test]
    fn reconcile_pins_bumps_last_seen_for_live_pins_only() {
        // A pinned session that is live gets its last_seen refreshed (never tombstoning); a pinned
        // session absent from the sample keeps its stale last_seen (a tombstone, retained).
        let mut pins = vec![
            Pin {
                agent_session: "live".to_string(),
                pinned_at_ms: 1_000,
                last_seen_ms: 1_000,
            },
            Pin {
                agent_session: "gone".to_string(),
                pinned_at_ms: 1_000,
                last_seen_ms: 1_000,
            },
        ];
        reconcile_pins(&mut pins, &[Some("live"), None], 5_000);
        assert_eq!(pins[0].last_seen_ms, 5_000, "the live pin advances");
        assert_eq!(pins[1].last_seen_ms, 1_000, "the vanished pin tombstones");
    }

    #[test]
    fn gc_pins_drops_tombstones_past_the_ttl() {
        let mut pins = vec![
            Pin {
                agent_session: "fresh".to_string(),
                pinned_at_ms: 0,
                last_seen_ms: 0,
            },
            Pin {
                agent_session: "ancient".to_string(),
                pinned_at_ms: 0,
                last_seen_ms: 0,
            },
        ];
        // "fresh" was seen just now; "ancient" lapsed past the 7-day TTL and is GC'd.
        reconcile_pins(&mut pins, &[Some("fresh")], PIN_TTL_MS + 10);
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].agent_session, "fresh");
    }

    #[test]
    fn gc_pins_caps_the_list_dropping_the_oldest_seen() {
        // Over the cap, the oldest-seen tombstones are shed until PIN_CAP remain; a live pin (seen
        // now) is always among the newest and survives.
        let mut pins: Vec<Pin> = (0..PIN_CAP as u64 + 5)
            .map(|i| Pin {
                agent_session: format!("uuid-{i}"),
                pinned_at_ms: 0,
                last_seen_ms: i, // strictly increasing, so the lowest ids are the oldest-seen
            })
            .collect();
        gc_pins(&mut pins, 1_000_000);
        assert_eq!(pins.len(), PIN_CAP, "the list is bounded to the cap");
        // The five oldest-seen (uuid-0..uuid-4) were dropped; the rest survive in pin order.
        assert_eq!(pins[0].agent_session, "uuid-5");
        assert!(pins.iter().all(|p| p.agent_session != "uuid-0"));
    }

    #[test]
    fn reconcile_roster_fills_pin_rank_and_is_uuid_keyed_not_pane_keyed() {
        // Acceptance: pinning is keyed by session uuid, so a DIFFERENT agent respawned in the same
        // positional pane slot does NOT inherit the pin.
        let dir = temp_state_dir("roster-pin-uuid-keyed");
        // Pin uuid-A (the agent currently in w1:p1).
        toggle_pin_persist(&dir, "uuid-A", 1_000).expect("pin write");

        // First reconcile: uuid-A is live in w1:p1 -> it carries pin_rank 0.
        let mut agents = vec![sample_agent("w1:p1", AgentStatus::Blocked, Some("uuid-A"))];
        reconcile_roster(&dir, &mut agents, 2_000);
        assert_eq!(agents[0].pin_rank, Some(0), "the pinned agent floats");

        // Kill uuid-A, spawn uuid-B in the SAME pane slot w1:p1. The pin must not misapply.
        let mut reused = vec![sample_agent("w1:p1", AgentStatus::Working, Some("uuid-B"))];
        reconcile_roster(&dir, &mut reused, 3_000);
        assert_eq!(
            reused[0].pin_rank, None,
            "a new session in a reused pane slot never inherits the old pin (uuid-keyed)"
        );
        // The pin for uuid-A survives as a tombstone, ready to re-apply if the session resumes.
        assert_eq!(pin_rank(&load_roster_state(&dir).pins, "uuid-A"), Some(0));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pin_survives_a_store_reopen() {
        // Acceptance: pins persist across popup reopen (a fresh RosterStore reading roster.json).
        let dir = temp_state_dir("roster-pin-persist");
        let pins = toggle_pin_persist(&dir, "uuid-A", 1_000).expect("pin write");
        assert_eq!(pins.len(), 1);
        // A brand-new store instance (as a reopened popup would build) still sees the pin on disk.
        let reopened = load_roster_state(&dir);
        assert_eq!(pin_rank(&reopened.pins, "uuid-A"), Some(0));
        // And toggling again through a fresh store unpins it, persisted.
        toggle_pin_persist(&dir, "uuid-A", 2_000).expect("unpin write");
        assert!(load_roster_state(&dir).pins.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invariant_7_deleting_roster_json_only_degrades_pins() {
        // roster.json is a prunable observation cache: deleting it loses pins (a nicety) but never a
        // ping, and reconcile then simply reports every row unpinned.
        let dir = temp_state_dir("roster-pin-invariant-7");
        toggle_pin_persist(&dir, "uuid-A", 1_000).expect("pin write");
        fs::remove_file(dir.join(ROSTER_FILE_NAME)).expect("delete roster.json");
        assert!(load_roster_state(&dir).pins.is_empty(), "the pin is gone");
        let mut agents = vec![sample_agent("w1:p1", AgentStatus::Blocked, Some("uuid-A"))];
        reconcile_roster(&dir, &mut agents, 5_000);
        assert_eq!(
            agents[0].pin_rank, None,
            "with the file gone the row is simply unpinned, never a crash"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn version_bump_preserves_a_v1_registry_and_adds_empty_pins() {
        // A stored v1 roster.json (registry only, no pins field) loads unchanged and gains an empty
        // pins list — old files keep working across the schema bump.
        let dir = temp_state_dir("roster-v1-compat");
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(
            dir.join(ROSTER_FILE_NAME),
            r#"{"version":1,"agents":{"w1:p1":{"status":"blocked","status_since_ms":1000,"first_seen_ms":1000,"last_seen_ms":1000}}}"#,
        )
        .expect("write a v1 file");
        let state = load_roster_state(&dir);
        assert_eq!(
            state.registry.get("w1:p1").map(|e| e.status.as_str()),
            Some("blocked"),
            "the v1 registry survives"
        );
        assert!(state.pins.is_empty(), "pins default to empty for a v1 file");
        let _ = fs::remove_dir_all(dir);
    }

    fn sample_agent(pane_id: &str, status: AgentStatus, session: Option<&str>) -> RosterAgent {
        RosterAgent {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("w1").to_string(),
            tab_id: None,
            agent: Some("claude".to_string()),
            agent_status: status,
            agent_session: session.map(str::to_string),
            cwd: None,
            focused: false,
            terminal_title: None,
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
            pin_rank: None,
        }
    }
}
