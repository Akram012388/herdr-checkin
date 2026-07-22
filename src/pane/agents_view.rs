//! The Agents view — a read-only live roster of every agent pane herdr knows about, grouped by
//! workspace. Sibling of [`queue_view`](super::queue_view): the shell owns the loop, the tick, and
//! the [`RosterSnapshot`] the sampler thread delivers; this module only paints it. Pure view — it
//! reads the snapshot the [`PaneModel`](super::PaneModel) holds and never mutates state or touches
//! herdr.
//!
//! Slice 2 of the Agents view (design doc §8): the tab toggle plus this read-only render, fed by the
//! background sampler. Row actions (`Enter` jump, `space` reply, selection) land in Slice 3, so this
//! view has no selection cursor and no click hit-testing yet — pressing `Tab`/`Ctrl+S` returns to
//! the Queue.

use super::PaneModel;
use crate::roster::{agent_destination, agent_detail, group_by_workspace, RosterAgent};
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

const AGENTS_FOOTER_HINTS: &str = "tab: queue  ·  q quit";

/// Draw the Agents view into the whole frame: a count header, the grouped roster (or a placeholder
/// while the first sample is still in flight), and a footer teaching the toggle back to the Queue.
/// Read-only in Slice 2 — no selection band, no `> ` cursor, no scrollbar (a tall roster clips;
/// scrolling arrives with selection in a later slice).
pub(super) fn draw_agents(frame: &mut Frame, model: &PaneModel) {
    let interior = frame.area();
    let areas = Layout::vertical([
        Constraint::Length(1), // count header
        Constraint::Min(0),    // the roster
        Constraint::Length(1), // footer hint
    ])
    .split(interior);

    // `roster` is `None` until the sampler delivers its first snapshot; an empty snapshot is a real
    // "herdr reports no agents" reading. The two are shown distinctly so a blank view never looks
    // like a hang.
    let snapshot = model.roster.as_ref();
    let agents: &[RosterAgent] = snapshot.map(|s| s.agents.as_slice()).unwrap_or(&[]);

    frame.render_widget(
        Paragraph::new(roster_header_text(agents.len())).bold(),
        areas[0],
    );

    if agents.is_empty() {
        let message = match snapshot {
            None => "Sampling agents...",
            Some(_) => "No agents running.",
        };
        frame.render_widget(
            Paragraph::new(message).dim().alignment(Alignment::Center),
            areas[1],
        );
    } else {
        draw_roster(frame, agents, areas[1]);
    }

    frame.render_widget(
        Paragraph::new(AGENTS_FOOTER_HINTS)
            .dim()
            .alignment(Alignment::Center),
        areas[2],
    );
}

/// Paint the grouped roster: for each workspace, a blank spacer, a bold workspace header, then each
/// agent as two lines — its destination (`{tab} · pane {n}`) and a dim detail (`{status} · {title}`).
/// The two-line row idiom mirrors [`queue_view`](super::queue_view) so the two views read as one
/// surface. Grouping/ordering come from the Herdr-free [`group_by_workspace`]; this only lays it out.
fn draw_roster(frame: &mut Frame, agents: &[RosterAgent], area: ratatui::layout::Rect) {
    let groups = group_by_workspace(agents);
    let mut items: Vec<ListItem> = Vec::new();
    for group in &groups {
        items.push(ListItem::new("")); // spacer between workspace blocks
        items.push(ListItem::new(group.workspace_id.clone()).bold());
        for agent in &group.agents {
            items.push(ListItem::new(agent_destination(agent)));
            items.push(ListItem::new(format!("  {}", agent_detail(agent))).dim());
        }
    }
    // No highlight and no `ListState`: the view is read-only in Slice 2, so nothing is selected and
    // the list renders from the top (a tall roster clips rather than scrolls, for now).
    frame.render_widget(List::new(items), area);
}

/// The one-line count at the top of the Agents view — the roster's analogue of the Queue's
/// `header_text`. Pluralized so the singular case doesn't read "1 agents".
fn roster_header_text(count: usize) -> String {
    match count {
        0 => "no agents".to_string(),
        1 => "1 agent".to_string(),
        n => format!("{n} agents"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_header_text_pluralizes() {
        assert_eq!(roster_header_text(0), "no agents");
        assert_eq!(roster_header_text(1), "1 agent");
        assert_eq!(roster_header_text(4), "4 agents");
    }
}
