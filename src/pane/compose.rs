//! The inline-reply compose strip: the titled rule, the single-line input, and the hint row, plus
//! the dim veil that recedes the queue behind it. Split out of the pane shell (Slice 0) so a future
//! Agents view can share one compose surface. Pure rendering — the [`ReplyDraft`] it paints is owned
//! by the pane model in [`super`].
//!
//! [`ReplyDraft`]: super::ReplyDraft

use super::ReplyDraft;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// How many terminal rows the reply input occupies. The message is still one logical line (Enter
/// sends, nothing here inserts a newline), but the display soft-wraps across these rows so a longer
/// reply reads as wrapped text instead of scrolling off the right edge. Up/Down walk the wrapped
/// rows (see [`cursor_move_vertical`]).
pub(super) const INPUT_ROWS: u16 = 3;

/// The dim prompt shown when the input is empty. Rendered here (not by the `TextArea` widget) because
/// the input paints its own soft-wrapped rows.
const PLACEHOLDER: &str = "type your reply";

/// Draw the inline-reply compose strip: a titled rule, the soft-wrapped input, and a hint row. The
/// input holds a single logical line but paints it wrapped across [`INPUT_ROWS`] rows with a manual
/// block caret, so a long reply wraps at the popup edge instead of scrolling horizontally.
pub(super) fn draw_compose(
    frame: &mut Frame,
    draft: &ReplyDraft,
    rule_area: Rect,
    input_area: Rect,
    hint_area: Rect,
) {
    // The titled rule announces the mode switch and names the captured target (pinned at arm time,
    // so it stays correct even if the queue re-orders under the dimmed list).
    frame.render_widget(reply_rule(&draft.label, rule_area.width), rule_area);

    // 1-col left pad aligns the input under the rule's "Reply" label.
    let input_rect = Rect {
        x: input_area.x + 1,
        width: input_area.width.saturating_sub(1),
        ..input_area
    };
    draw_input(frame, draft, input_rect);

    // The affordances: dim (reference, not the work), right-aligned to keep the typing edge clean.
    frame.render_widget(
        Paragraph::new(reply_hint(hint_area.width))
            .dim()
            .alignment(Alignment::Right),
        hint_area,
    );
}

/// Paint the single logical reply line soft-wrapped across `area`, with a manual block caret. The
/// wrap width is recorded on the draft so the Up/Down handlers wrap the same way the render does.
fn draw_input(frame: &mut Frame, draft: &ReplyDraft, area: Rect) {
    // Record the render width so `cursor_move_vertical` (driven from the event loop) wraps
    // identically — otherwise Up/Down could land the caret on a row the render never drew.
    draft.wrap_width.set(area.width);

    let line = &draft.input.lines()[0]; // always exactly one line: nothing inserts a newline.
    if line.is_empty() {
        frame.render_widget(Paragraph::new(PLACEHOLDER).fg(Color::DarkGray), area);
        paint_caret(frame, area.x, area.y); // block caret over the placeholder's first cell
        return;
    }

    let width = area.width as usize;
    let height = area.height as usize;
    let rows = wrap_line(line, width);
    let (_, caret_col) = draft.input.cursor(); // one line, so row is always 0
    let (caret_row, caret_display_col) = caret_row_col(&rows, caret_col);

    // Anchor the caret's row to the bottom of the strip once the wrapped text overflows it, so the
    // line you're typing stays visible.
    let scroll = caret_row.saturating_sub(height.saturating_sub(1));
    let visible: Vec<Line> = rows
        .iter()
        .skip(scroll)
        .take(height)
        .map(|(_, text)| Line::from(text.clone()))
        .collect();
    frame.render_widget(Paragraph::new(visible), area);

    let cx = area.x + caret_display_col.min(width.saturating_sub(1)) as u16;
    let cy = area.y + (caret_row - scroll) as u16;
    paint_caret(frame, cx, cy);
}

/// Invert the cell at `(x, y)` to draw a block caret over whatever glyph (or blank) sits there —
/// the same reverse-video block the `TextArea` widget drew, but placed by our own wrap math. Guarded
/// against the buffer bounds so a caret at the very end of a full row can't index out of range.
fn paint_caret(frame: &mut Frame, x: u16, y: u16) {
    let bounds = frame.area();
    if x >= bounds.right() || y >= bounds.bottom() {
        return;
    }
    let cell = &mut frame.buffer_mut()[(x, y)];
    let style = cell.style().add_modifier(Modifier::REVERSED);
    cell.set_style(style);
}

/// Char-wrap `line` into display rows no wider than `width` columns, never splitting a character
/// (a lone glyph wider than `width` gets its own row). The rows partition `line` exactly — their
/// texts concatenate back to `line` — so a caret char-index maps unambiguously to a row and back.
/// Each row is `(start_char_index, text)`. A zero width yields one empty row.
fn wrap_line(line: &str, width: usize) -> Vec<(usize, String)> {
    if width == 0 {
        return vec![(0, String::new())];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_start = 0;
    let mut current_width = 0;
    for (index, ch) in line.chars().enumerate() {
        let w = char_width(ch);
        if current_width + w > width && !current.is_empty() {
            rows.push((current_start, std::mem::take(&mut current)));
            current_start = index;
            current_width = 0;
        }
        current.push(ch);
        current_width += w;
    }
    rows.push((current_start, current));
    rows
}

/// Locate the caret (a char index into the logical line) within `rows`: which display row it sits on
/// and its display column on that row. A caret at end-of-line lands at the end of the last row.
fn caret_row_col(rows: &[(usize, String)], col: usize) -> (usize, usize) {
    for (row, (start, text)) in rows.iter().enumerate() {
        let count = text.chars().count();
        if col >= *start && col < *start + count {
            let display_col = text.chars().take(col - start).map(char_width).sum();
            return (row, display_col);
        }
    }
    // col == total length: the caret trails the final character.
    let last = rows.len() - 1;
    let (start, text) = &rows[last];
    let display_col = text.chars().take(col - start).map(char_width).sum();
    (last, display_col)
}

/// Move a caret at char index `col` up or down one wrapped row within `line` (wrapped at `width`),
/// keeping its visual column, and return the new char index. A move off the top/bottom leaves `col`
/// unchanged. Wraps identically to [`draw_input`] so navigation matches what's on screen.
pub(super) fn cursor_move_vertical(line: &str, width: usize, col: usize, down: bool) -> usize {
    let rows = wrap_line(line, width);
    let (caret_row, goal_col) = caret_row_col(&rows, col);
    let target = if down {
        if caret_row + 1 >= rows.len() {
            return col;
        }
        caret_row + 1
    } else {
        if caret_row == 0 {
            return col;
        }
        caret_row - 1
    };
    let (start, text) = &rows[target];
    // Walk the target row to the char whose display column is nearest to (not past) the goal.
    let mut used = 0;
    let mut offset = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > goal_col {
            break;
        }
        used += w;
        offset += 1;
    }
    start + offset
}

/// Stamp every cell in `area` with the `DIM` modifier over whatever was already drawn there — the
/// same post-hoc veil herdr uses to recede content behind a modal. Dims the queue uniformly while
/// an inline reply is composed, including any scrollbar, so the compose strip reads as active.
pub(super) fn dim_area(frame: &mut Frame, area: Rect) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let cell = &mut buf[(x, y)];
            let dimmed = cell.style().add_modifier(Modifier::DIM);
            cell.set_style(dimmed);
        }
    }
}

/// The compose strip's titled rule: `─ Reply to <label> ───`, the "Reply to <label>" bold, the
/// leading and trailing dashes dim. The label (never the words "Reply to") is ellipsis-truncated on
/// a narrow popup so the rule keeps a few trailing dashes and always fits one line.
fn reply_rule(label: &str, width: u16) -> Paragraph<'static> {
    const PREFIX: &str = "Reply to ";
    const HEAD: usize = 2; // the leading "─ "
    const GAP: usize = 1; // the space before the trailing dashes
    const MIN_TAIL: usize = 3; // keep at least a few trailing dashes

    let width = width as usize;
    let label_budget = width
        .saturating_sub(HEAD + display_width(PREFIX) + GAP + MIN_TAIL)
        .max(1);
    let title = format!("{PREFIX}{}", truncate_display(label, label_budget));
    let tail = width.saturating_sub(HEAD + display_width(&title) + GAP);
    Paragraph::new(Line::from(vec![
        "─ ".dim(),
        title.bold(),
        Span::raw(" "),
        "─".repeat(tail).dim(),
    ]))
}

/// The full send/cancel hint, degrading to a terse form before it would clip on a narrow popup.
fn reply_hint(width: u16) -> &'static str {
    const FULL: &str = "enter send · esc cancel";
    const SHORT: &str = "enter · esc";
    if (width as usize) >= display_width(FULL) {
        FULL
    } else {
        SHORT
    }
}

/// Truncate `s` to at most `max` display columns, marking a cut with a trailing `…` (which itself
/// occupies the last column). Returns `s` unchanged when it already fits.
fn truncate_display(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // reserve one column for the ellipsis
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let ch_width = char_width(ch);
        if used + ch_width > budget {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

/// Display width of a string in terminal columns (wide/CJK characters count as 2).
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of a single character in terminal columns; control characters count as 0.
fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_display_keeps_short_labels_and_ellipsizes_long_ones() {
        assert_eq!(truncate_display("Claude", 10), "Claude");
        // Cut to 5 columns: 4 chars + the ellipsis cell.
        assert_eq!(truncate_display("claude-backend", 5), "clau…");
        assert_eq!(truncate_display("anything", 0), "");
    }

    #[test]
    fn reply_hint_degrades_on_a_narrow_popup() {
        assert_eq!(reply_hint(40), "enter send · esc cancel");
        assert_eq!(reply_hint(12), "enter · esc");
    }

    // The wrapped rows always concatenate back to the original line — the property the caret math
    // relies on to map a char index to a row and back.
    fn joined(rows: &[(usize, String)]) -> String {
        rows.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn wrap_line_breaks_at_the_width_and_partitions_the_line_exactly() {
        let rows = wrap_line("abcdefghij", 4);
        assert_eq!(
            rows,
            vec![
                (0, "abcd".to_string()),
                (4, "efgh".to_string()),
                (8, "ij".to_string()),
            ]
        );
        assert_eq!(joined(&rows), "abcdefghij");
    }

    #[test]
    fn wrap_line_handles_empty_and_zero_width() {
        assert_eq!(wrap_line("", 8), vec![(0, String::new())]);
        assert_eq!(wrap_line("hi", 0), vec![(0, String::new())]);
    }

    #[test]
    fn wrap_line_never_splits_a_wide_char() {
        // Two double-width glyphs at width 3: each takes 2 columns, so they can't share a 3-col row.
        let rows = wrap_line("我你", 3);
        assert_eq!(rows, vec![(0, "我".to_string()), (1, "你".to_string())]);
        assert_eq!(joined(&rows), "我你");
    }

    #[test]
    fn caret_row_col_locates_the_caret_including_the_end() {
        let rows = wrap_line("abcdefgh", 4); // ["abcd", "efgh"]
        assert_eq!(caret_row_col(&rows, 0), (0, 0)); // start
        assert_eq!(caret_row_col(&rows, 4), (1, 0)); // boundary -> start of the next row
        assert_eq!(caret_row_col(&rows, 6), (1, 2)); // mid second row
        assert_eq!(caret_row_col(&rows, 8), (1, 4)); // trailing end of the last row
    }

    #[test]
    fn cursor_move_vertical_walks_wrapped_rows_and_keeps_the_column() {
        // "abcdefghij" at width 4 -> ["abcd", "efgh", "ij"].
        assert_eq!(cursor_move_vertical("abcdefghij", 4, 1, true), 5); // col 1 down -> row 2 col 1
        assert_eq!(cursor_move_vertical("abcdefghij", 4, 5, false), 1); // and back up
                                                                        // Down onto a short final row clamps to its end.
        assert_eq!(cursor_move_vertical("abcdefghij", 4, 7, true), 10); // "gh" -> "ij" end
    }

    #[test]
    fn cursor_move_vertical_is_a_noop_off_the_top_and_bottom() {
        assert_eq!(cursor_move_vertical("abcdefgh", 4, 2, false), 2); // up from the first row
        assert_eq!(cursor_move_vertical("abcdefgh", 4, 6, true), 6); // down from the last row
    }
}
