//! The Agents-view roster: pure data + grouping over `herdr agent list` output. **Herdr-free by the
//! same rule as `queue.rs`** — the `Herdr` trait never reaches this module. The herdr seam
//! (`herdr.rs`) parses the CLI JSON into [`RosterAgent`]s; this module only groups, orders, and
//! renders them, so it stays trivially unit-testable with no herdr in the loop.
//!
//! The Agents view's pure core (design doc §5): the types, grouping-by-workspace, the display-order
//! flattening the live view's selection indexes into, and the per-row text formatters. The `Herdr`
//! seam samples `agent list` on a worker thread; this module only shapes what it delivers. Pins come
//! later (Slice 6). A plain-text `render_roster_text` also backs the hidden `roster` debug subcommand.

/// An agent pane's live status, from `herdr agent list`'s `agent_status`. The vocabulary is
/// closed (live-verified, herdr 0.7.5): `idle`/`working`/`blocked`/`done`, with **`Unknown` as the
/// catch-all** for an empty or unrecognized value — herdr has no separate `failed`/`stopped`, so an
/// unfamiliar string is a herdr we don't fully know, rendered honestly rather than dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    /// Map herdr's `agent_status` string to a variant; anything outside the known vocabulary (and
    /// the empty string) becomes [`AgentStatus::Unknown`].
    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }

    /// The lowercase word for this status (round-trips [`parse`](Self::parse) for known variants).
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }
}

/// One agent pane as surfaced by `herdr agent list`, reduced to the fields the Agents view needs.
/// Plain data the herdr seam parses and the view renders — never a place the `Herdr` trait reaches.
/// `agent_session` (the session uuid) is `None` for a pane herdr lists without one (seen live for a
/// non-Claude/Codex agent); it is the stable key pins will use later (design §6), so it is carried
/// even though Slice 1 only prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RosterAgent {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    pub(crate) tab_id: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) agent_status: AgentStatus,
    pub(crate) agent_session: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) focused: bool,
    pub(crate) terminal_title: Option<String>,
    /// The wall clock (ms) of this agent's last observed transition into its current status, read
    /// from the `roster.json` registry by the sampler ([`crate::roster_state::reconcile_roster`]) and
    /// used to render time-in-state (`blocked 4m`). `None` when there is no honest reading — the pane
    /// has no registry entry yet, or a reused-slot uuid mismatch invalidated the timer — in which
    /// case the row shows `~`. Not parsed from `agent list` (it carries no timestamps, design §4);
    /// filled after parse, mirroring the label fields below.
    pub(crate) status_since_ms: Option<u64>,
    /// Human names herdr shows for this pane's workspace/tab/pane (`w4` -> `home`, `w4:t1` -> `~`).
    /// `agent list` carries only positional ids, so the herdr seam enriches these from
    /// `workspace list`/`tab list`/`pane list` — `None` when a lookup missed, and the view then falls
    /// back to the id (mirroring the Queue's `workspace_label`/`tab_label`/`pane_label`).
    pub(crate) workspace_label: Option<String>,
    pub(crate) tab_label: Option<String>,
    pub(crate) pane_label: Option<String>,
    /// The agent's last line of visible terminal output (Slice 4 / issue #5) — the tail of its last
    /// message (a blocked agent's question, a done agent's closing line), extracted by
    /// [`last_terminal_line`] from a `herdr agent read` snapshot and filled by the sampler's tail
    /// sweep ([`crate::herdr::TailCache`]). `None` before this pane's first read lands, or when only
    /// UI chrome is on screen — the row then falls back to the terminal title. Not from `agent list`
    /// (it carries no terminal contents); filled after parse like the label and timer fields above.
    pub(crate) last_line: Option<String>,
}

/// A single sampler delivery: the whole roster at one instant. The view replaces this wholesale each
/// sample (design §5, never persisted); `sampled_at_ms` stamps when it was read so the view can age
/// it. Slice 1 constructs it only for the `roster` debug dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RosterSnapshot {
    pub(crate) sampled_at_ms: u64,
    pub(crate) agents: Vec<RosterAgent>,
}

/// Agents sharing one workspace, in display order. Produced by [`group_by_workspace`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceGroup {
    pub(crate) workspace_id: String,
    pub(crate) agents: Vec<RosterAgent>,
}

/// Group agents by `workspace_id`, preserving the order workspaces are first seen and the order of
/// agents within each. Pins float to the top of their workspace group in Slice 6; until then the
/// within-group order is plain encounter order, so this is the single place that ordering will hook
/// in (the view never re-sorts). Pure: clones its input into the groups.
pub(crate) fn group_by_workspace(agents: &[RosterAgent]) -> Vec<WorkspaceGroup> {
    let mut groups: Vec<WorkspaceGroup> = Vec::new();
    for agent in agents {
        match groups
            .iter_mut()
            .find(|group| group.workspace_id == agent.workspace_id)
        {
            Some(group) => group.agents.push(agent.clone()),
            None => groups.push(WorkspaceGroup {
                workspace_id: agent.workspace_id.clone(),
                agents: vec![agent.clone()],
            }),
        }
    }
    groups
}

/// The agents in on-screen order — grouped by workspace, first-seen workspace order, encounter order
/// within — the exact order [`group_by_workspace`] paints. Returns borrows (no clone) so the view's
/// selection cursor and its click hit-testing index into the same sequence the rows are laid out
/// from, and the two can never drift.
pub(crate) fn agents_in_display_order(agents: &[RosterAgent]) -> Vec<&RosterAgent> {
    let mut order: Vec<&RosterAgent> = Vec::with_capacity(agents.len());
    let mut seen: Vec<&str> = Vec::new();
    // Emit each workspace's agents contiguously the first time that workspace is encountered, so the
    // flat order matches the grouped render exactly (workspaces never interleave in the output).
    for agent in agents {
        if seen.contains(&agent.workspace_id.as_str()) {
            continue;
        }
        seen.push(agent.workspace_id.as_str());
        order.extend(
            agents
                .iter()
                .filter(|a| a.workspace_id == agent.workspace_id),
        );
    }
    order
}

/// The display name for the reply footer when answering a roster agent (`Reply to Claude`): the
/// agent name with its first letter capitalized (`claude` -> `Claude`, matching herdr's own display
/// agent), falling back to the raw name, then the pane id. The Queue's analogue is `actions::agent_label`.
pub(crate) fn roster_reply_label(agent: &RosterAgent) -> String {
    let name = agent
        .agent
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(&agent.pane_id);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => name.to_string(),
    }
}

/// The workspace label shown as an Agents-view group header — herdr's human name (`home`), falling
/// back to the raw `workspace_id` (`w4`) when the enrichment missed. The Queue's analogue is the
/// `workspace_label`-else-`workspace_id` branch of `actions::entry_destination`.
pub(crate) fn workspace_display_label(agent: &RosterAgent) -> &str {
    agent
        .workspace_label
        .as_deref()
        .filter(|label| !label.is_empty())
        .unwrap_or(&agent.workspace_id)
}

/// The primary (destination) line for an Agents-view row: `{tab} · {pane}` within the workspace
/// group. The workspace is the group header (agents are grouped by workspace), so it is not repeated
/// per row — this is the queue's `entry_destination` idiom minus the leading workspace. Each segment
/// prefers herdr's human name (`~`, a pane label) and falls back to the positional id (`t1`, `pane 1`),
/// so it reads the same as the Queue and herdr's own sidebar once the roster is enriched.
pub(crate) fn agent_destination(agent: &RosterAgent) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(2);

    // Tab: its label, else the `tN` segment of the tab id.
    if let Some(tab) = agent
        .tab_label
        .as_deref()
        .filter(|label| !label.is_empty())
        .or_else(|| agent.tab_id.as_deref().and_then(id_segment))
    {
        parts.push(tab.to_string());
    }

    // Pane: its label, else `pane N` from the `pN` segment, else the raw pane id — never blank.
    if let Some(pane) = agent
        .pane_label
        .as_deref()
        .filter(|label| !label.is_empty())
    {
        parts.push(pane.to_string());
    } else if let Some(number) = id_segment(&agent.pane_id).and_then(pane_number) {
        parts.push(format!("pane {number}"));
    } else {
        parts.push(agent.pane_id.clone());
    }

    parts.join(" · ")
}

/// The detail (second) line for an Agents-view row: the live status **with its time-in-state** then
/// the agent's **last terminal line** (`blocked 4m · Good to proceed?`), the tail of what it last did
/// or said (Slice 4). Before this pane's first `agent read` lands the last line is unknown, so it
/// falls back to the terminal title, then to nothing (`blocked 4m`). The age comes from the
/// `roster.json` registry via [`RosterAgent::status_since_ms`]; when unknown it renders as `~` (design
/// §4), so the status word is always present.
pub(crate) fn agent_detail(agent: &RosterAgent, now_ms: u64) -> String {
    let head = format!(
        "{} {}",
        agent.agent_status.as_str(),
        time_in_state(now_ms, agent.status_since_ms)
    );
    // Prefer the agent's live last terminal line — what it is doing, or last said (Slice 4). Before
    // this pane's first `agent read` lands it is `None`, so fall back to the terminal title; the row
    // is never bare.
    let tail = agent
        .last_line
        .as_deref()
        .filter(|line| !line.is_empty())
        .or_else(|| {
            agent
                .terminal_title
                .as_deref()
                .filter(|title| !title.is_empty())
        });
    match tail {
        Some(tail) => format!("{head} · {tail}"),
        None => head,
    }
}

/// The time-in-state label for a row: the compact age since the last transition (`4m`), or `~` when
/// there is no honest reading (`None` — no registry entry, or a reused-slot uuid mismatch). Pure and
/// Herdr-free; the registry lookup + reset logic lives in `roster_state.rs`, which hands the resolved
/// `since_ms` here.
pub(crate) fn time_in_state(now_ms: u64, since_ms: Option<u64>) -> String {
    match since_ms {
        Some(since) => format_age(now_ms, since),
        None => "~".to_string(),
    }
}

/// A wall-clock span rendered compactly, largest whole unit only: `45s`, `4m`, `2h`, `3d`. A `since`
/// at or after `now` (clock skew, a just-stamped transition) clamps to `0s` rather than underflowing.
pub(crate) fn format_age(now_ms: u64, since_ms: u64) -> String {
    let secs = now_ms.saturating_sub(since_ms) / 1_000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Extract an agent's last line of visible output from a `herdr agent read` terminal snapshot — the
/// tail of its last message (a blocked agent's question, a done agent's closing line) for the
/// Agents-view status column (Slice 4 / issue #5). `herdr agent read` returns the *rendered*
/// terminal, so its bottom rows are the agent's own UI chrome — the input box, the `❯` prompt, the
/// Claude Code status bar and footer, and, while generating, a spinner + token counter — never the
/// content. We read from the bottom, skip that chrome, and return the first real content line,
/// stripped of a trailing scrollbar column and surrounding whitespace. `None` when nothing but chrome
/// is visible (e.g. an agent whose output has scrolled off above the input box, or a blank pane), so
/// the caller keeps its cached line rather than ever showing a border or `-- INSERT --`.
///
/// **Best-effort and expected to iterate** (design §4): the chrome vocabulary is tuned to the agents
/// we see (Claude Code, amp); an unfamiliar TUI simply yields whatever its last non-chrome row is, or
/// `~` — never a crash, never a ping lost (this only feeds the live view, invariant #7).
pub(crate) fn last_terminal_line(snapshot: &str) -> Option<String> {
    snapshot
        .lines()
        .rev()
        .map(normalize_terminal_line)
        .find(|line| !is_terminal_chrome(line))
}

/// Trim a rendered terminal row down to its content: drop a trailing scrollbar column (block-element
/// glyphs like `█`/`▆` herdr's agents paint down the right edge) plus all surrounding whitespace.
fn normalize_terminal_line(line: &str) -> String {
    line.trim_end_matches(|c: char| c.is_whitespace() || is_block_glyph(c))
        .trim_start()
        .to_string()
}

/// True for a normalized row that is agent UI chrome rather than output — see [`last_terminal_line`].
/// Runs on the already-normalized line (leading/trailing whitespace and the scrollbar column gone).
fn is_terminal_chrome(line: &str) -> bool {
    line.is_empty()
        || is_box_rule(line)
        || is_box_side(line)
        || is_bare_prompt(line)
        || is_status_bar(line)
        || is_live_activity(line)
}

/// A horizontal box rule / border — the input-box top and bottom edges (`────`, `╭───`, `╰───`),
/// even with an embedded title (`── slice-4-and-beyond ──`). Detected by a run of ≥8 consecutive
/// box-drawing chars, which real content effectively never contains.
fn is_box_rule(line: &str) -> bool {
    let mut run = 0usize;
    for ch in line.chars() {
        if is_box_glyph(ch) {
            run += 1;
            if run >= 8 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// A vertical box side — an empty input box's left/right rails (`│              │`): a normalized line
/// that opens and closes with a vertical box glyph and holds only spaces between.
fn is_box_side(line: &str) -> bool {
    let mut chars = line.chars();
    match (chars.next(), line.chars().last()) {
        (Some(first), Some(last)) if is_vertical_glyph(first) && is_vertical_glyph(last) => {
            line.chars().all(|c| c == ' ' || is_vertical_glyph(c))
        }
        _ => false,
    }
}

/// A bare shell/agent prompt with no command typed after it.
fn is_bare_prompt(line: &str) -> bool {
    matches!(line, "❯" | "›" | ">" | "$" | "#")
}

/// A Claude Code status bar or footer row (the two lines herdr pins below the input box). Keyed on
/// stable substrings of that agent's chrome; unfamiliar agents fall through (best-effort).
fn is_status_bar(line: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "-- INSERT --",
        "-- NORMAL --",
        "auto mode on",
        "for agents",
        "| ctx:",
        "resets:",
    ];
    MARKERS.iter().any(|marker| line.contains(marker))
}

/// A live-activity row shown only while an agent is generating: the spinner line (`… ↓ 3.9k tokens)`)
/// or the right-aligned running token counter (`133659 tokens`). Skipped so the column shows the last
/// *settled* output line instead of a value that churns every refresh (the maintainer's call — the
/// `working 2m` time-in-state already signals "busy").
fn is_live_activity(line: &str) -> bool {
    line.ends_with("tokens)") || is_token_count(line)
}

/// The bare running-token counter `<digits> tokens` (commas allowed), nothing else on the line.
fn is_token_count(line: &str) -> bool {
    match line.strip_suffix(" tokens") {
        Some(count) => !count.is_empty() && count.chars().all(|c| c.is_ascii_digit() || c == ','),
        None => false,
    }
}

/// A box-drawing glyph (U+2500 block): the light/heavy lines and corners agents draw their input box
/// and rules with.
fn is_box_glyph(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{257F}')
}

/// A vertical box-drawing glyph specifically (`│`, `┃`, and their variants) — an input box's side.
fn is_vertical_glyph(ch: char) -> bool {
    matches!(ch, '│' | '┃' | '╎' | '╏' | '┆' | '┇' | '┊' | '┋')
}

/// A block-element glyph (U+2580 block: `█ ▆ ▌` …) — what agents paint a scrollbar column with down
/// the right edge; stripped from the end of a content line by [`normalize_terminal_line`].
fn is_block_glyph(ch: char) -> bool {
    matches!(ch, '\u{2580}'..='\u{259F}')
}

/// The last `:`-separated segment of an id (`wS:tN` -> `tN`), or `None` if empty. Mirrors the queue's
/// `actions::id_segment` — kept local so `roster.rs` stays self-contained and Herdr-free (design §5).
fn id_segment(id: &str) -> Option<&str> {
    id.rsplit_once(':')
        .map(|(_, segment)| segment)
        .filter(|segment| !segment.is_empty())
}

/// The numeric part of a `pN` pane segment (`p1` -> `1`), or `None` if it isn't `p`+digits. Mirrors
/// the queue's `actions::pane_number`.
fn pane_number(segment: &str) -> Option<&str> {
    let number = segment.strip_prefix('p')?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
}

/// Render a snapshot as a plain-text dump for the hidden `roster` debug subcommand: a header line
/// (sample time + counts) then, per workspace, one line per agent showing **every** parsed field
/// (pane/tab ids, agent, status, session uuid, cwd, focus, terminal title). Dev-only visibility into
/// the data path — not a UI. A missing optional renders as `-`, a focused pane is marked `*`.
pub(crate) fn render_roster_text(snapshot: &RosterSnapshot) -> String {
    let groups = group_by_workspace(&snapshot.agents);
    let mut out = String::new();
    out.push_str(&format!(
        "roster @ {}ms - {} agent(s), {} workspace(s)\n",
        snapshot.sampled_at_ms,
        snapshot.agents.len(),
        groups.len(),
    ));
    for group in &groups {
        out.push_str(&format!("{}\n", group.workspace_id));
        for agent in &group.agents {
            let focus = if agent.focused { "*" } else { " " };
            let pane_id = &agent.pane_id;
            let tab_id = agent.tab_id.as_deref().unwrap_or("-");
            let name = agent.agent.as_deref().unwrap_or("-");
            let status = agent.agent_status.as_str();
            let session = agent.agent_session.as_deref().unwrap_or("-");
            let cwd = agent.cwd.as_deref().unwrap_or("-");
            let title = agent.terminal_title.as_deref().unwrap_or("");
            out.push_str(&format!(
                "  {focus} {pane_id:<8} {tab_id:<8} {name:<8} {status:<8} {session}  {cwd}  :: {title}\n"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pane_id: &str, workspace_id: &str, status: AgentStatus) -> RosterAgent {
        RosterAgent {
            pane_id: pane_id.to_string(),
            workspace_id: workspace_id.to_string(),
            tab_id: Some(format!("{workspace_id}:t1")),
            agent: Some("claude".to_string()),
            agent_status: status,
            agent_session: Some("uuid-1".to_string()),
            cwd: Some("/tmp".to_string()),
            focused: false,
            terminal_title: Some("title".to_string()),
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
        }
    }

    #[test]
    fn agent_status_parses_the_known_vocabulary() {
        assert_eq!(AgentStatus::parse("idle"), AgentStatus::Idle);
        assert_eq!(AgentStatus::parse("working"), AgentStatus::Working);
        assert_eq!(AgentStatus::parse("blocked"), AgentStatus::Blocked);
        assert_eq!(AgentStatus::parse("done"), AgentStatus::Done);
    }

    #[test]
    fn agent_status_folds_unknown_and_empty_into_unknown() {
        // herdr has no failed/stopped; an unfamiliar or empty status is Unknown, never dropped.
        assert_eq!(AgentStatus::parse("unknown"), AgentStatus::Unknown);
        assert_eq!(
            AgentStatus::parse("some_future_state"),
            AgentStatus::Unknown
        );
        assert_eq!(AgentStatus::parse(""), AgentStatus::Unknown);
    }

    #[test]
    fn agent_status_as_str_round_trips_known_variants() {
        for status in [
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
            AgentStatus::Unknown,
        ] {
            assert_eq!(AgentStatus::parse(status.as_str()), status);
        }
    }

    #[test]
    fn group_by_workspace_preserves_workspace_and_within_group_order() {
        // Interleaved workspaces: the groups keep first-seen workspace order (w4, then wT), and each
        // group keeps its agents in encounter order — never reordered or sorted by id.
        let agents = vec![
            agent("w4:p1", "w4", AgentStatus::Idle),
            agent("wT:p1", "wT", AgentStatus::Working),
            agent("w4:p2", "w4", AgentStatus::Blocked),
        ];
        let groups = group_by_workspace(&agents);
        assert_eq!(groups.len(), 2, "one group per distinct workspace");
        assert_eq!(groups[0].workspace_id, "w4");
        assert_eq!(
            groups[0]
                .agents
                .iter()
                .map(|a| a.pane_id.as_str())
                .collect::<Vec<_>>(),
            vec!["w4:p1", "w4:p2"],
            "w4's two agents stay in encounter order"
        );
        assert_eq!(groups[1].workspace_id, "wT");
        assert_eq!(groups[1].agents.len(), 1);
    }

    #[test]
    fn group_by_workspace_is_empty_for_no_agents() {
        assert!(group_by_workspace(&[]).is_empty());
    }

    #[test]
    fn render_roster_text_headers_the_counts_and_lists_every_workspace() {
        let snapshot = RosterSnapshot {
            sampled_at_ms: 1_234,
            agents: vec![
                agent("w4:p1", "w4", AgentStatus::Idle),
                agent("wT:p1", "wT", AgentStatus::Working),
            ],
        };
        let text = render_roster_text(&snapshot);
        assert!(
            text.starts_with("roster @ 1234ms - 2 agent(s), 2 workspace(s)\n"),
            "header reads the sample time and counts; got:\n{text}"
        );
        assert!(text.contains("\nw4\n"), "the w4 group is listed");
        assert!(text.contains("\nwT\n"), "the wT group is listed");
        assert!(text.contains("w4:p1"), "the agent row shows its pane id");
    }

    #[test]
    fn agents_in_display_order_matches_the_grouped_render_order() {
        // Interleaved workspaces: the flat order groups w4's agents contiguously (first-seen), then
        // wT's — identical to what group_by_workspace lays out, so selection can't drift from the rows.
        let agents = vec![
            agent("w4:p1", "w4", AgentStatus::Idle),
            agent("wT:p1", "wT", AgentStatus::Working),
            agent("w4:p2", "w4", AgentStatus::Blocked),
        ];
        let order: Vec<&str> = agents_in_display_order(&agents)
            .iter()
            .map(|a| a.pane_id.as_str())
            .collect();
        assert_eq!(order, vec!["w4:p1", "w4:p2", "wT:p1"]);
        // And it flattens the same groups group_by_workspace produces.
        let groups = group_by_workspace(&agents);
        let grouped: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.agents.iter().map(|a| a.pane_id.as_str()))
            .collect();
        assert_eq!(order, grouped);
    }

    #[test]
    fn roster_reply_label_capitalizes_the_agent_name() {
        assert_eq!(
            roster_reply_label(&agent("w4:p1", "w4", AgentStatus::Idle)),
            "Claude"
        );
        let amp = RosterAgent {
            agent: Some("amp".to_string()),
            ..agent("w4:p1", "w4", AgentStatus::Idle)
        };
        assert_eq!(roster_reply_label(&amp), "Amp");
        let nameless = RosterAgent {
            agent: None,
            ..agent("w4:p9", "w4", AgentStatus::Idle)
        };
        assert_eq!(
            roster_reply_label(&nameless),
            "W4:p9",
            "no agent name falls back to the pane id (capitalized like any label)"
        );
    }

    #[test]
    fn agent_destination_shows_tab_and_pane_number_within_the_group() {
        // The workspace is the group header, so the row destination is `{tab} · pane {n}`.
        let a = agent("w4:p2", "w4", AgentStatus::Idle); // tab_id defaults to "w4:t1"
        assert_eq!(agent_destination(&a), "t1 · pane 2");
    }

    #[test]
    fn agent_destination_prefers_human_names_over_ids() {
        // Enriched with herdr's names: the row reads `{tab-label} · {pane-label}`, not `t1 · pane 2`.
        let a = RosterAgent {
            tab_label: Some("herdr-config".to_string()),
            pane_label: Some("editor".to_string()),
            ..agent("w4:p2", "w4", AgentStatus::Idle)
        };
        assert_eq!(agent_destination(&a), "herdr-config · editor");
        // A pane with only a tab name falls back to `pane N` for the pane segment.
        let tab_only = RosterAgent {
            tab_label: Some("~".to_string()),
            ..agent("w4:p3", "w4", AgentStatus::Idle)
        };
        assert_eq!(tab_only.pane_label, None);
        assert_eq!(agent_destination(&tab_only), "~ · pane 3");
    }

    #[test]
    fn workspace_display_label_prefers_the_name_then_the_id() {
        let named = RosterAgent {
            workspace_label: Some("home".to_string()),
            ..agent("w4:p1", "w4", AgentStatus::Idle)
        };
        assert_eq!(workspace_display_label(&named), "home");
        // No label (enrichment missed) falls back to the raw workspace id.
        let bare = agent("w4:p1", "w4", AgentStatus::Idle);
        assert_eq!(workspace_display_label(&bare), "w4");
    }

    #[test]
    fn agent_destination_falls_back_when_ids_do_not_parse() {
        // No tab id and a non-`pN` pane id: the raw pane id keeps the row identifiable, never blank.
        let a = RosterAgent {
            tab_id: None,
            ..agent("weird-pane", "wX", AgentStatus::Working)
        };
        assert_eq!(agent_destination(&a), "weird-pane");
    }

    #[test]
    fn agent_detail_joins_status_age_and_title_and_degrades_without_a_title() {
        // status_since 1_000, now 241_000 -> 240s -> "4m". The age sits between status and title.
        let with_title = RosterAgent {
            status_since_ms: Some(1_000),
            ..agent("w4:p1", "w4", AgentStatus::Blocked) // title defaults to "title"
        };
        assert_eq!(agent_detail(&with_title, 241_000), "blocked 4m · title");
        let no_title = RosterAgent {
            terminal_title: None,
            status_since_ms: Some(1_000),
            ..agent("w4:p1", "w4", AgentStatus::Working)
        };
        assert_eq!(agent_detail(&no_title, 241_000), "working 4m");
        let empty_title = RosterAgent {
            terminal_title: Some(String::new()),
            status_since_ms: Some(1_000),
            ..agent("w4:p1", "w4", AgentStatus::Done)
        };
        assert_eq!(
            agent_detail(&empty_title, 241_000),
            "done 4m",
            "an empty title is dropped, not shown as a trailing separator"
        );
    }

    #[test]
    fn agent_detail_prefers_the_last_terminal_line_over_the_title() {
        // Once a read lands, the row shows the agent's last line (what it is doing / last said), not
        // the static terminal title.
        let with_line = RosterAgent {
            status_since_ms: Some(1_000),
            last_line: Some("Good to proceed?".to_string()),
            ..agent("w4:p1", "w4", AgentStatus::Blocked) // title defaults to "title"
        };
        assert_eq!(
            agent_detail(&with_line, 241_000),
            "blocked 4m · Good to proceed?"
        );
        // An empty last line is ignored and the title still shows (never a trailing separator).
        let empty_line = RosterAgent {
            status_since_ms: Some(1_000),
            last_line: Some(String::new()),
            ..agent("w4:p1", "w4", AgentStatus::Working)
        };
        assert_eq!(agent_detail(&empty_line, 241_000), "working 4m · title");
    }

    #[test]
    fn agent_detail_shows_a_tilde_when_the_timer_is_unknown() {
        // No registry entry (status_since_ms None) renders an honest `~`, never a fake zero.
        let unknown = agent("w4:p1", "w4", AgentStatus::Blocked);
        assert_eq!(unknown.status_since_ms, None);
        assert_eq!(agent_detail(&unknown, 999_000), "blocked ~ · title");
    }

    #[test]
    fn format_age_renders_the_largest_whole_unit() {
        assert_eq!(format_age(0, 0), "0s");
        assert_eq!(format_age(45_000, 0), "45s");
        assert_eq!(format_age(240_000, 0), "4m");
        assert_eq!(format_age(7_200_000, 0), "2h");
        assert_eq!(format_age(3 * 86_400_000, 0), "3d");
        // A since after now (clock skew / a just-stamped transition) clamps to 0s, never underflows.
        assert_eq!(format_age(1_000, 5_000), "0s");
    }

    #[test]
    fn time_in_state_is_a_tilde_when_unknown() {
        assert_eq!(time_in_state(240_000, Some(0)), "4m");
        assert_eq!(time_in_state(240_000, None), "~");
    }

    #[test]
    fn last_terminal_line_reads_a_blocked_claudes_question_over_the_chrome() {
        // The maintainer's ground-truth case (a screenshot): a blocked Claude agent's last line is
        // its question to you, sitting above the input box / prompt / status bar / footer.
        let snapshot = include_str!("fixtures/agent_read_claude_blocked.txt");
        assert_eq!(
            last_terminal_line(snapshot).as_deref(),
            Some("Good to proceed on that, and which way on the spinner-vs-response choice?")
        );
    }

    #[test]
    fn last_terminal_line_skips_the_spinner_and_token_counter_while_working() {
        // While generating, Claude paints a spinner (`… ↓ 3.9k tokens)`) and a running token counter
        // just above the box. Both are skipped so the column shows the last *settled* output line.
        let snapshot = include_str!("fixtures/agent_read_claude_working.txt");
        assert_eq!(
            last_terminal_line(snapshot).as_deref(),
            Some("test result: ok. 42 passed")
        );
    }

    #[test]
    fn last_terminal_line_reads_amp_over_its_empty_input_box() {
        // A real amp capture: its last message sits above an empty input box whose sides are `│ … │`
        // and whose edges carry a price/label. The content lines also trail a `█` scrollbar column,
        // which must be stripped, not treated as content or a border.
        let snapshot = include_str!("fixtures/agent_read_amp_done.txt");
        assert_eq!(
            last_terminal_line(snapshot).as_deref(),
            Some("Then reply “installed and token saved.”")
        );
    }

    #[test]
    fn last_terminal_line_is_none_when_only_chrome_is_visible() {
        // An agent whose output has scrolled off leaves nothing but its input box on screen: no
        // honest content line, so the column keeps its cached value rather than showing a border.
        let only_chrome = "\
────────────────────────────────────── my-session ──
❯
──────────────────────────────────────────────────────
  herdr-checkin [main] | ctx: 13% | resets: 1d-10h
  -- INSERT -- ⏵⏵ auto mode on (shift+tab to cycle)
";
        assert_eq!(last_terminal_line(only_chrome), None);
        assert_eq!(last_terminal_line(""), None);
        assert_eq!(last_terminal_line("   \n  \n"), None);
    }

    #[test]
    fn box_rules_sides_and_prompts_are_chrome_but_content_is_not() {
        assert!(is_terminal_chrome("────────────── title ──")); // a rule with an embedded title
        assert!(is_terminal_chrome("╭──────────────────────╮"));
        assert!(is_terminal_chrome("│                      │")); // an empty box side
        assert!(is_terminal_chrome("❯"));
        assert!(is_terminal_chrome("133659 tokens")); // the running token counter
        assert!(is_terminal_chrome("· Churning… (1m 7s · ↓ 3.9k tokens)")); // the spinner
                                                                            // Real content with a stray box char or the word "tokens" mid-line is NOT chrome.
        assert!(!is_terminal_chrome(
            "Then reply “installed and token saved.”"
        ));
        assert!(!is_terminal_chrome("the pipe | splits two clauses here"));
        assert!(!is_terminal_chrome(
            "we spent 40000 tokens on that run, roughly"
        ));
    }

    #[test]
    fn normalize_strips_a_trailing_scrollbar_column_and_surrounding_space() {
        assert_eq!(
            normalize_terminal_line("  Then reply now.                    █"),
            "Then reply now."
        );
        assert_eq!(normalize_terminal_line("   plain line   "), "plain line");
    }

    #[test]
    fn render_roster_text_marks_focus_and_dashes_missing_optionals() {
        let focused_no_session = RosterAgent {
            focused: true,
            agent_session: None,
            terminal_title: None,
            cwd: None,
            tab_id: None,
            ..agent("wN:p2", "wN", AgentStatus::Idle)
        };
        let snapshot = RosterSnapshot {
            sampled_at_ms: 0,
            agents: vec![focused_no_session],
        };
        let text = render_roster_text(&snapshot);
        let row = text
            .lines()
            .find(|line| line.contains("wN:p2"))
            .expect("the agent row is present");
        assert!(
            row.trim_start().starts_with('*'),
            "a focused pane is marked"
        );
        // Missing session/tab/cwd all render as `-`, never a blank that hides the field.
        assert!(row.contains(" - "), "a missing optional shows a dash");
    }
}
