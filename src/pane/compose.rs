//! The inline-reply compose strip: the titled rule, the single-line input, and the hint row, plus
//! the dim veil that recedes the queue behind it. Split out of the pane shell (Slice 0) so a future
//! Agents view can share one compose surface. Pure rendering — the [`ReplyDraft`] it paints is owned
//! by the pane model in [`super`].
//!
//! [`ReplyDraft`]: super::ReplyDraft

use super::ReplyDraft;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Draw the inline-reply compose strip: a titled rule, the input, and a hint row. The input is a
/// single fixed row rendered by the `TextArea` widget itself — it scrolls horizontally and draws
/// its own placeholder and block cursor, so there's no manual wrapping or caret math here.
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

    // 1-col left pad aligns the input under the rule's "Reply" label; the TextArea draws its own
    // placeholder + block cursor, so no manual caret (ratatui hides the hardware cursor when
    // `set_cursor_position` is not called).
    let input_rect = Rect {
        x: input_area.x + 1,
        width: input_area.width.saturating_sub(1),
        ..input_area
    };
    frame.render_widget(&draft.input, input_rect);

    // The affordances: dim (reference, not the work), right-aligned to keep the typing edge clean.
    frame.render_widget(
        Paragraph::new(reply_hint(hint_area.width))
            .dim()
            .alignment(Alignment::Right),
        hint_area,
    );
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
}
