//! The Queue view — the durable FIFO attention inbox rendered as grouped status sections
//! (`CHECKIN` then `DONE`), with Herdr-themed selection, two-line rows, and the overflow
//! scrollbar. Split out of the pane shell (Slice 0) so the coming Agents view is a sibling render
//! module, not more weight in the loop. Pure view: it reads the [`PaneModel`] the shell owns and
//! never mutates state.
//!
//! [`PaneModel`]: super::PaneModel

use super::theme::PaneTheme;
use super::PaneModel;
use crate::{entry_destination, entry_detail, QueueEntry, WaitStatus};
use ratatui::layout::Rect;
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState};
use ratatui::Frame;

/// The section header text for each `WaitStatus`, in the order sections are shown: agents that need
/// input first (on-brand `CHECKIN`), then finished ones (`DONE`).
const CHECKIN_HEADER: &str = "CHECKIN";
const DONE_HEADER: &str = "DONE";

/// One rendered line of the grouped agents-view: a blank spacer, a non-selectable section header,
/// an entry's **primary** line (the go-to destination, carrying its index into `entries` — the
/// selection source of truth), or that entry's secondary **detail** line beneath it.
/// Built per-frame by [`layout_rows`]. Keeping one `Row` per painted line preserves the invariant
/// the click hit-testing and scrollbar math rely on. `Entry` is the selectable/clickable line;
/// `Detail` clicks map back to the same entry, but the cursor and highlight anchor on `Entry`.
pub(super) enum Row {
    Spacer,
    Header(&'static str),
    Entry(usize),
    Detail(usize),
}

/// Group the queue into status sections for display — `CHECKIN` (`blocked`) then `DONE` (`done`),
/// FIFO within each — as a pure view transform. Each non-empty section is preceded by a blank
/// spacer row so the groups read as visually distinct blocks (and the first spacer separates them
/// from the count line above). It never reorders `entries`: each `Row::Entry` keeps the entry's
/// original index, and a section (spacer + header) is emitted only when that section has at least
/// one entry (so an all-`done` queue shows no `CHECKIN` heading). This is the only place the
/// on-screen row order diverges from `entries`; `draw` and `row_for_click` both go through it, so
/// the paint and the click hit-testing always agree.
pub(super) fn layout_rows(entries: &[QueueEntry]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (header, status) in [
        (CHECKIN_HEADER, WaitStatus::Blocked),
        (DONE_HEADER, WaitStatus::Done),
    ] {
        let mut section: Vec<Row> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.status == status)
            // Each entry paints as two lines: its destination (Entry) then its detail (Detail).
            .flat_map(|(index, _)| [Row::Entry(index), Row::Detail(index)])
            .collect();
        if !section.is_empty() {
            rows.push(Row::Spacer);
            rows.push(Row::Header(header));
            rows.append(&mut section);
        }
    }
    rows
}

/// Render the grouped queue into `area`, recording the painted rect into `list_area` for click
/// hit-testing and drawing a scrollbar when the rows overflow. The focused entry gets Herdr's
/// accent selection treatment in both modes: the live selection while navigating, the captured
/// reply target while composing (so the answered agent stays obvious under the caller's dim veil).
pub(super) fn draw_list(
    frame: &mut Frame,
    theme: &PaneTheme,
    model: &PaneModel,
    now_ms: u64,
    list_state: &mut ListState,
    list_area: &mut Option<Rect>,
    area: Rect,
) {
    let compose = model.reply.as_ref();
    // The CC-agents-view look: entries grouped into status sections with non-selectable headers.
    // `layout_rows` is a pure view over the FIFO queue — it never reorders `entries`, so `selected`
    // stays an index into `entries` and we translate it to its on-screen row here.
    let rows = layout_rows(&model.entries);

    // A restrained grey band marks the focused entry in BOTH modes: while navigating it's the selection;
    // while composing it's the reply target (so it's obvious which agent you're answering, on top of
    // the `> ` marker). In compose mode the whole list is then veiled dim by the caller, but the
    // band's background survives the DIM (which only mutes the foreground), so the target still reads.
    let highlight_index = match compose {
        Some(draft) => model.entries.iter().position(|e| e.pane_id == draft.target),
        None => Some(model.selected),
    };
    let highlight_style = theme.row_selection();

    // Each entry is two lines on one content edge: List's permanent two-column marker gutter aligns
    // both the destination and detail. The focused detail takes the same explicit-contrast band —
    // not dim — so its two lines read as one highlighted block.
    let items: Vec<ListItem> =
        rows.iter()
            .map(|row| match row {
                Row::Spacer => ListItem::new("").style(theme.base()),
                Row::Header(title) => ListItem::new(*title).style(theme.heading()),
                Row::Entry(index) => match entry_destination(&model.entries[*index]) {
                    Some(destination) => ListItem::new(destination).style(theme.base()),
                    // No destination resolved yet (un-enriched/legacy row): fall back to the detail so
                    // the row is never blank.
                    None => ListItem::new(entry_detail(&model.entries[*index], now_ms))
                        .style(theme.base()),
                },
                Row::Detail(index) => {
                    let entry = &model.entries[*index];
                    // The primary line already carries the detail when no destination resolved, so this
                    // line stays blank rather than repeat it.
                    if entry_destination(entry).is_none() {
                        ListItem::new("")
                    } else {
                        let detail = entry_detail(entry, now_ms);
                        if highlight_index == Some(*index) {
                            ListItem::new(detail).style(theme.row_selection())
                        } else {
                            ListItem::new(detail).style(theme.secondary())
                        }
                    }
                }
            })
            .collect();
    // Highlight the display row that carries the highlighted entry (headers are never selected).
    let selected_row = highlight_index.and_then(|target| {
        rows.iter()
            .position(|row| matches!(row, Row::Entry(index) if *index == target))
    });
    list_state.select(selected_row);

    let list = List::new(items)
        .highlight_style(highlight_style)
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);

    // Reserve the right-most column for a scrollbar when the grouped rows overflow the viewport, so
    // the list text never collides with the thumb. `List`+`ListState` already scrolls to keep the
    // selection in view; the scrollbar only makes that off-screen content discoverable. Each
    // `ListItem` is one row, so display-row count == item count == the units `offset()` counts in.
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
    // Record the list's rect for this frame so mouse clicks can be hit-tested against exactly what
    // was painted; `render_stateful_widget` updates `list_state`'s scroll offset in place.
    *list_area = Some(list_rect);
    frame.render_stateful_widget(list, list_rect, list_state);
    if overflow {
        let track = Rect {
            x: area.x + area.width - 1,
            width: 1,
            ..area
        };
        render_list_scrollbar(
            frame,
            theme,
            track,
            rows.len(),
            viewport,
            list_state.offset(),
        );
    }
}

/// Map a click at terminal cell `(col, row)` to the queue index it lands on, or `None` if the
/// click is outside the list `area`, on a section header, or on a blank row below the last row.
/// `offset` is the index of the first visible row (the list's scroll position), so the clicked
/// display row is `offset + (row - area.y)`; that row is translated back to an entry index through
/// the grouped `rows` layout (headers map to `None`). Pure and unit-tested.
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
        // A click on either the destination line or its dim detail selects that entry.
        Some(Row::Entry(index) | Row::Detail(index)) => Some(*index),
        _ => None,
    }
}

/// The footer prompt shown while a clear-all confirm is pending. Only armed on a non-empty queue,
/// so `count >= 1`; pluralized so the singular case doesn't read "1 entries".
pub(super) fn confirm_prompt(count: usize) -> String {
    match count {
        1 => "clear all 1 entry? y/n".to_string(),
        n => format!("clear all {n} entries? y/n"),
    }
}

/// The one-line count shown at the top of the pane. herdr draws the pane name ("Check-in") on the
/// popup's border title, so this line carries only the live count — no redundant "Check-in —" prefix.
pub(super) fn header_text(count: usize) -> String {
    match count {
        0 => "queue empty".to_string(),
        1 => "1 agent waiting".to_string(),
        n => format!("{n} agents waiting"),
    }
}

/// Geometry of a vertical scrollbar thumb: its top offset within the track and its length, both in
/// cells. Produced by [`scrollbar_thumb`], consumed by [`render_list_scrollbar`].
pub(super) struct Thumb {
    top: u16,
    len: u16,
}

/// Proportional geometry for a vertical scrollbar thumb — the same shape herdr draws for its own
/// popups (thumb length scaled to the visible fraction, position scaled to the scroll offset),
/// reduced to integer math. Returns `None` when everything fits (no scrollbar).
/// `total` display rows, `viewport` visible rows, `offset` index of the first visible row.
pub(super) fn scrollbar_thumb(
    total: usize,
    viewport: usize,
    offset: usize,
    track_height: u16,
) -> Option<Thumb> {
    if viewport == 0 || track_height == 0 || total <= viewport {
        return None;
    }
    let track = track_height as usize;
    // total > viewport here, so max_offset >= 1 and the divisions below never hit zero.
    let len = (viewport * track / total).clamp(1, track);
    let max_top = track - len;
    let max_offset = total - viewport;
    let top = if max_top == 0 {
        0
    } else {
        offset.min(max_offset) * max_top / max_offset
    };
    Some(Thumb {
        top: top as u16,
        len: len as u16,
    })
}

/// Draw a 1-column vertical scrollbar in `track` when the grouped rows overflow the viewport, using
/// Herdr's surface and overlay roles for the track and thumb. A no-op when it all fits.
pub(super) fn render_list_scrollbar(
    frame: &mut Frame,
    theme: &PaneTheme,
    track: Rect,
    total: usize,
    viewport: usize,
    offset: usize,
) {
    let Some(thumb) = scrollbar_thumb(total, viewport, offset, track.height) else {
        return;
    };
    let buf = frame.buffer_mut();
    for y in track.y..track.y.saturating_add(track.height) {
        buf[(track.x, y)]
            .set_symbol("▕")
            .set_style(theme.base().fg(theme.surface_dim));
    }
    let thumb_top = track.y.saturating_add(thumb.top);
    for y in thumb_top..thumb_top.saturating_add(thumb.len) {
        buf[(track.x, y)]
            .set_symbol("▐")
            .set_style(theme.base().fg(theme.overlay0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pane_id: &str) -> QueueEntry {
        entry_with_status(pane_id, WaitStatus::Blocked)
    }

    fn entry_with_status(pane_id: &str, status: WaitStatus) -> QueueEntry {
        QueueEntry {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("").to_string(),
            tab_id: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            agent: Some("claude".to_string()),
            display_agent: Some("Claude".to_string()),
            title: Some("t".to_string()),
            status,
            enqueued_at_ms: 1_000,
            last_touched_ms: 1_000,
        }
    }

    // The list rect used across the click tests: starts at row 1 (row 0 is the top header line),
    // 10 rows tall, 40 wide.
    fn list_area() -> Rect {
        Rect {
            x: 0,
            y: 1,
            width: 40,
            height: 10,
        }
    }

    // All-blocked entries render as [Spacer, Header("CHECKIN"), Entry(0), Detail(0), Entry(1),
    // Detail(1), ...] — a spacer then the header occupy the first two display rows, then each entry
    // is two rows (its destination then its dim detail), so entry N's primary sits at display
    // index 2 + 2N and its detail at 3 + 2N.
    fn blocked_rows(count: usize) -> Vec<Row> {
        let entries: Vec<QueueEntry> = (0..count).map(|i| entry(&format!("w{i}:p1"))).collect();
        layout_rows(&entries)
    }

    #[test]
    fn row_for_click_maps_rows_to_entries_past_the_header() {
        // No scroll: row 1 spacer, row 2 CHECKIN header, so row 3 (index 2) is entry 0's primary,
        // row 4 (index 3) is entry 0's detail (same entry), row 5 (index 4) is entry 1's primary.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 3), Some(0)); // primary
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 4), Some(0)); // detail -> same entry
        assert_eq!(row_for_click(list_area(), 0, &rows, 0, 5), Some(1)); // next entry's primary
    }

    #[test]
    fn row_for_click_skips_the_spacer_and_section_header() {
        // The leading spacer (row 1) and the CHECKIN header (row 2) select nothing, like a blank row.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 1), None); // spacer
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 2), None); // header
    }

    #[test]
    fn row_for_click_accounts_for_scroll_offset() {
        // Scrolled down by 2: display row at area.y is display index 2 — Entry(0)'s primary here
        // ([Spacer, Header, Entry(0), Detail(0), Entry(1), ...]).
        let rows = blocked_rows(5);
        assert_eq!(row_for_click(list_area(), 2, &rows, 10, 1), Some(0));
        assert_eq!(row_for_click(list_area(), 2, &rows, 10, 3), Some(1));
    }

    #[test]
    fn row_for_click_rejects_blank_rows_below_the_last_entry() {
        // Spacer + header + 3 entries × 2 rows = 8 display rows (indices 0..7); a click on display
        // row 8 (row 9) is past the last detail line and selects nothing.
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 9), None);
    }

    #[test]
    fn row_for_click_rejects_clicks_outside_the_area() {
        let rows = blocked_rows(3);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 0), None); // above the list
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 11), None); // below the list
        assert_eq!(row_for_click(list_area(), 0, &rows, 40, 3), None); // one past the right edge (entry 0 primary)
    }

    #[test]
    fn row_for_click_is_safe_on_an_empty_queue() {
        let rows = blocked_rows(0);
        assert_eq!(row_for_click(list_area(), 0, &rows, 5, 1), None);
    }

    #[test]
    fn layout_rows_groups_by_status_fifo_without_reordering_entries() {
        // A queue interleaving blocked and done: the layout emits a spacer + CHECKIN (blocked, FIFO)
        // then a spacer + DONE (done, FIFO), each Entry keeping its original index into `entries`.
        let entries = vec![
            entry_with_status("w0:p1", WaitStatus::Blocked),
            entry_with_status("w1:p1", WaitStatus::Done),
            entry_with_status("w2:p1", WaitStatus::Blocked),
            entry_with_status("w3:p1", WaitStatus::Done),
        ];
        let rows = layout_rows(&entries);
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Spacer => "~".to_string(),
                Row::Header(title) => format!("#{title}"),
                Row::Entry(index) => format!("{index}"),
                Row::Detail(index) => format!("d{index}"),
            })
            .collect();
        assert_eq!(
            shape,
            vec!["~", "#CHECKIN", "0", "d0", "2", "d2", "~", "#DONE", "1", "d1", "3", "d3"],
            "each entry is its Entry line then its Detail line; sections FIFO, indices unchanged"
        );
    }

    #[test]
    fn layout_rows_omits_an_empty_section() {
        // All done: no CHECKIN spacer/heading — the DONE section (spacer + header) leads.
        let entries = vec![
            entry_with_status("w0:p1", WaitStatus::Done),
            entry_with_status("w1:p1", WaitStatus::Done),
        ];
        let rows = layout_rows(&entries);
        assert!(matches!(rows.first(), Some(Row::Spacer)));
        assert!(
            matches!(rows.get(1), Some(Row::Header(DONE_HEADER))),
            "an all-done queue leads with the DONE section, not CHECKIN"
        );
        assert_eq!(
            rows.len(),
            6,
            "spacer + header + two entries × (Entry + Detail)"
        );
    }

    #[test]
    fn header_text_pluralizes() {
        assert_eq!(header_text(0), "queue empty");
        assert_eq!(header_text(1), "1 agent waiting");
        assert_eq!(header_text(3), "3 agents waiting");
    }

    #[test]
    fn confirm_prompt_pluralizes() {
        assert_eq!(confirm_prompt(1), "clear all 1 entry? y/n");
        assert_eq!(confirm_prompt(3), "clear all 3 entries? y/n");
    }

    #[test]
    fn scrollbar_thumb_is_none_when_everything_fits() {
        // Content shorter than or equal to the viewport needs no scrollbar.
        assert!(scrollbar_thumb(5, 10, 0, 10).is_none());
        assert!(scrollbar_thumb(10, 10, 0, 10).is_none());
        // Degenerate tracks/viewports are also a no-op (never divide by zero).
        assert!(scrollbar_thumb(20, 0, 0, 10).is_none());
        assert!(scrollbar_thumb(20, 10, 0, 0).is_none());
    }

    #[test]
    fn scrollbar_thumb_scales_length_and_slides_with_the_offset() {
        // 20 rows through a 10-row viewport: the thumb spans half the track (10 * 10 / 20 = 5),
        // and its top slides from 0 (top) through the middle to max_top (bottom) as we scroll.
        let at = |offset| scrollbar_thumb(20, 10, offset, 10).expect("overflow => thumb");
        let top = at(0);
        assert_eq!((top.top, top.len), (0, 5), "scrolled to the top");
        let mid = at(5);
        assert_eq!((mid.top, mid.len), (2, 5), "5 * (10-5) / (20-10) = 2");
        let bottom = at(10);
        assert_eq!(
            (bottom.top, bottom.len),
            (5, 5),
            "max offset pins the thumb to the bottom"
        );
        // An offset past the max is clamped, never overruns the track.
        let over = at(99);
        assert_eq!(
            over.top + over.len,
            10,
            "thumb bottom never exceeds the track height"
        );
    }

    #[test]
    fn scrollbar_thumb_length_is_at_least_one_cell() {
        // A huge list through a tiny viewport still shows a visible (>= 1 cell) thumb.
        let thumb = scrollbar_thumb(1_000, 1, 0, 8).expect("overflow => thumb");
        assert!(thumb.len >= 1, "the thumb is never zero-height");
        assert!(thumb.top + thumb.len <= 8, "and stays within the track");
    }
}
