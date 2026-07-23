//! The Agents view — a live roster of every agent pane herdr knows about, grouped by workspace,
//! with full interaction parity with the Queue: `j`/`k`/click selection, `space` inline reply, and
//! `Enter` jump. Sibling of [`queue_view`](super::queue_view): the shell owns the loop, the tick,
//! and the [`RosterSnapshot`] the sampler thread delivers; this module paints it and hit-tests
//! clicks. Pure view — it reads the [`PaneModel`](super::PaneModel) the shell owns and never mutates
//! state or touches herdr.
//!
//! The two-line row idiom, the soft grey selection band, the `> ` cursor, and the overflow
//! scrollbar all mirror [`queue_view`](super::queue_view) so the two views read as one surface; the
//! only differences are the grouping key (workspace, not status) and the row's text source
//! ([`agent_destination`]/[`agent_detail`] over a [`RosterAgent`]).

use super::compose::{dim_area, draw_compose};
use super::queue_view::{render_list_scrollbar, SELECTION_BG, SELECTION_FG};
use super::{draw_tab_bar, ActiveTab, PaneModel, ReplyDraft};
use crate::roster::{agent_destination, agent_detail, workspace_display_label, RosterAgent};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

const AGENTS_FOOTER_HINTS: &str = "j/k move  ·  enter jump  ·  space reply  ·  q quit";

/// One rendered line of the grouped roster: a blank spacer, a non-selectable workspace header, an
/// agent's **primary** line (agent identity + destination, carrying its index into the display-order
/// agent list — the selection source of truth), or that agent's dim **detail** line. Built per-frame by
/// [`layout_rows`], the roster analogue of [`queue_view::Row`](super::queue_view). `Detail` clicks
/// map back to the same agent; the cursor and highlight anchor on `Entry`.
pub(super) enum Row {
    Spacer,
    Header(String),
    Entry(usize),
    Detail(usize),
}

/// Lay the display-order agents out into rows: a blank spacer + a bold workspace header each time the
/// workspace changes, then two lines per agent (its identity + destination, then its dim detail).
/// `agents` must be in display order (grouped by workspace, from
/// `roster::agents_in_display_order`), so a workspace change delimits each section and every
/// `Entry`/`Detail` index is a position in that same slice — the paint and click hit-testing agree.
pub(super) fn layout_rows(agents: &[&RosterAgent]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut current: Option<&str> = None;
    for (index, agent) in agents.iter().enumerate() {
        // Group by the stable workspace id, but title each section with herdr's human name.
        if current != Some(agent.workspace_id.as_str()) {
            rows.push(Row::Spacer);
            rows.push(Row::Header(workspace_display_label(agent).to_string()));
            current = Some(agent.workspace_id.as_str());
        }
        rows.push(Row::Entry(index));
        rows.push(Row::Detail(index));
    }
    rows
}

/// Draw the Agents view into the whole frame: the shared tab bar, a count header, the grouped roster
/// (or a placeholder while the first sample is still in flight), and a footer — or, while composing a
/// reply, the shared compose strip docked below the roster (mirroring the Queue). Records the roster
/// rect into `list_area` for click hit-testing (`None` when there is nothing to click).
pub(super) fn draw_agents(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
) {
    let interior = frame.area();
    let compose = model.reply.as_ref();

    let areas = match compose {
        Some(_) => Layout::vertical([
            Constraint::Length(1),                          // tab bar
            Constraint::Length(1),                          // count header
            Constraint::Min(0),                             // the roster (dimmed while composing)
            Constraint::Length(1),                          // titled rule
            Constraint::Length(super::compose::INPUT_ROWS), // input
            Constraint::Length(1),                          // hint
        ])
        .split(interior),
        None => Layout::vertical([
            Constraint::Length(1), // tab bar
            Constraint::Length(1), // count header
            Constraint::Min(0),    // the roster
            Constraint::Length(1), // footer hint
        ])
        .split(interior),
    };

    draw_tab_bar(frame, areas[0], ActiveTab::Agents);

    let agents = model.roster_display_agents();
    frame.render_widget(
        Paragraph::new(roster_header_text(agents.len())).bold(),
        areas[1],
    );

    if agents.is_empty() {
        // `roster` is `None` until the first sample lands; a delivered-but-empty snapshot is a real
        // "herdr reports no agents" reading. Worded apart so a blank view never looks like a hang.
        *list_area = None;
        let message = match model.roster {
            None => "Sampling agents...",
            Some(_) => "No agents running.",
        };
        frame.render_widget(
            Paragraph::new(message).dim().alignment(Alignment::Center),
            areas[2],
        );
    } else {
        draw_roster(
            frame, model, now_ms, list_state, list_area, areas[2], compose,
        );
    }

    match compose {
        // Composing: darken the tab bar + header + roster as one veil so the strip is the only lit
        // surface, then draw it — the same focus-by-receding treatment as the Queue.
        Some(draft) => {
            let veil = Rect {
                height: areas[0].height + areas[1].height + areas[2].height,
                ..areas[0]
            };
            dim_area(frame, veil);
            draw_compose(frame, draft, areas[3], areas[4], areas[5]);
        }
        None => {
            frame.render_widget(
                Paragraph::new(AGENTS_FOOTER_HINTS)
                    .dim()
                    .alignment(Alignment::Center),
                areas[3],
            );
        }
    }
}

/// Paint the grouped roster into `area`, recording the painted rect into `list_area` and drawing a
/// scrollbar when the rows overflow — the roster twin of [`queue_view::draw_list`](super::queue_view).
/// The focused agent gets the soft grey band in both modes: the live selection while navigating, the
/// captured reply target while composing (so the answered agent stays obvious under the dim veil).
fn draw_roster(
    frame: &mut Frame,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
    area: Rect,
    compose: Option<&ReplyDraft>,
) {
    // Recomputed from the model (the same display order `draw_agents` already checked was non-empty)
    // rather than threaded in as a ninth argument.
    let agents = model.roster_display_agents();
    let agents = agents.as_slice();
    let rows = layout_rows(agents);

    // While navigating, highlight the selected agent; while composing, the reply target by pane id
    // (so it reads under the veil, whose DIM only mutes the foreground and leaves the band).
    let highlight_index = match compose {
        Some(draft) => agents.iter().position(|a| a.pane_id == draft.target),
        None => (!agents.is_empty()).then_some(model.roster_selected.min(agents.len() - 1)),
    };

    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            Row::Spacer => ListItem::new(""),
            Row::Header(workspace) => ListItem::new(workspace.clone()).bold(),
            Row::Entry(index) => ListItem::new(agent_destination(agents[*index])),
            Row::Detail(index) => {
                let detail = agent_detail(agents[*index], now_ms);
                if highlight_index == Some(*index) {
                    ListItem::new(detail).fg(SELECTION_FG).bg(SELECTION_BG)
                } else {
                    ListItem::new(detail).dim()
                }
            }
        })
        .collect();

    let selected_row = highlight_index.and_then(|target| {
        rows.iter()
            .position(|row| matches!(row, Row::Entry(index) if *index == target))
    });
    list_state.select(selected_row);

    let list = List::new(items)
        .highlight_style(Style::new().fg(SELECTION_FG).bg(SELECTION_BG))
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);

    let viewport = area.height as usize;
    let overflow = rows.len() > viewport;
    let list_rect = if overflow {
        Rect {
            width: area.width.saturating_sub(1),
            ..area
        }
    } else {
        area
    };
    *list_area = Some(list_rect);
    frame.render_stateful_widget(list, list_rect, list_state);
    if overflow {
        let track = Rect {
            x: area.x + area.width - 1,
            width: 1,
            ..area
        };
        render_list_scrollbar(frame, track, rows.len(), viewport, list_state.offset());
    }
}

/// Map a click at terminal cell `(col, row)` to the display-order agent index it lands on, or `None`
/// for a click outside the list, on a workspace header, or on a blank row. The roster twin of
/// [`queue_view::row_for_click`](super::queue_view); pure and unit-tested.
pub(super) fn row_for_click(
    area: Rect,
    offset: usize,
    rows: &[Row],
    col: u16,
    row: u16,
) -> Option<usize> {
    let inside_x = col >= area.x && col < area.x.saturating_add(area.width);
    let inside_y = row >= area.y && row < area.y.saturating_add(area.height);
    if !inside_x || !inside_y {
        return None;
    }
    let display_index = offset + (row - area.y) as usize;
    match rows.get(display_index) {
        Some(Row::Entry(index) | Row::Detail(index)) => Some(*index),
        _ => None,
    }
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
    use crate::roster::AgentStatus;

    fn agent(pane_id: &str, workspace_id: &str) -> RosterAgent {
        RosterAgent {
            pane_id: pane_id.to_string(),
            workspace_id: workspace_id.to_string(),
            tab_id: Some(format!("{workspace_id}:t1")),
            agent: Some("claude".to_string()),
            agent_status: AgentStatus::Idle,
            agent_session: None,
            cwd: None,
            focused: false,
            terminal_title: Some("title".to_string()),
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
        }
    }

    // The list rect used across the click tests: starts at row 2 (rows 0-1 are the tab bar + count
    // header), 10 rows tall, 40 wide.
    fn list_area() -> Rect {
        Rect {
            x: 0,
            y: 2,
            width: 40,
            height: 10,
        }
    }

    #[test]
    fn roster_header_text_pluralizes() {
        assert_eq!(roster_header_text(0), "no agents");
        assert_eq!(roster_header_text(1), "1 agent");
        assert_eq!(roster_header_text(4), "4 agents");
    }

    #[test]
    fn layout_rows_opens_a_section_per_workspace_and_pairs_each_agent() {
        // Two workspaces in display order: each opens with a spacer + header, then each agent is its
        // Entry line then its Detail line, indices being positions in the display-order slice.
        let a0 = agent("w4:p1", "w4");
        let a1 = agent("w4:p2", "w4");
        let a2 = agent("wN:p1", "wN");
        let order = vec![&a0, &a1, &a2];
        let rows = layout_rows(&order);
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Spacer => "~".to_string(),
                Row::Header(w) => format!("#{w}"),
                Row::Entry(i) => format!("{i}"),
                Row::Detail(i) => format!("d{i}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec!["~", "#w4", "0", "d0", "1", "d1", "~", "#wN", "2", "d2"]
        );
    }

    #[test]
    fn row_for_click_maps_rows_to_agents_past_the_header() {
        // Display rows: [Spacer, Header, Entry(0), Detail(0), Entry(1), Detail(1)]. With the list at
        // y=2 and no scroll, Entry(0) is terminal row 4, its detail row 5, Entry(1) row 6.
        let a0 = agent("w4:p1", "w4");
        let a1 = agent("w4:p2", "w4");
        let order = vec![&a0, &a1];
        let rows = layout_rows(&order);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 4), Some(0)); // primary
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 5), Some(0)); // detail -> same agent
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 6), Some(1)); // next agent
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 2), None); // spacer
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 3), None); // header
    }
}
