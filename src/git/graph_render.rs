use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::git::graph::{BranchLabel, GraphRow, LaneSegment, lane_color};
use crate::theme::GraphTheme;

fn lane_color_from(theme: &GraphTheme, idx: usize) -> Color {
    let palette = &theme.lane_palette;
    if palette.is_empty() {
        Color::Reset
    } else {
        palette[idx % palette.len()]
    }
}

pub(crate) fn render_graph_prefix(row: &GraphRow, theme: &GraphTheme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    for (col, segment) in row.lanes.iter().enumerate() {
        let color = match segment {
            LaneSegment::Horizontal
            | LaneSegment::CrossHorizontal
            | LaneSegment::RightTee
            | LaneSegment::LeftTee => row
                .horizontal_spans
                .iter()
                .find(|s| s.0 <= col && col <= s.1)
                .map(|s| lane_color_from(theme, s.2))
                .unwrap_or_else(|| lane_color_from(theme, lane_color(col))),
            _ => lane_color_from(theme, lane_color(col)),
        };
        let style = Style::default().fg(color);

        let ch = match segment {
            LaneSegment::Empty => " ",
            LaneSegment::Straight => "│",
            LaneSegment::Commit => "●",
            LaneSegment::MergeLeft => "╯",
            LaneSegment::MergeRight => "╰",
            LaneSegment::ForkLeft => "╮",
            LaneSegment::ForkRight => "╭",
            LaneSegment::Horizontal => "─",
            LaneSegment::CrossHorizontal => "┼",
            LaneSegment::RightTee => "├",
            LaneSegment::LeftTee => "┤",
        };

        spans.push(Span::styled(ch.to_string(), style));

        let h_span = row
            .horizontal_spans
            .iter()
            .find(|s| s.0 <= col && col < s.1);
        if let Some(s) = h_span {
            spans.push(Span::styled(
                "─".to_string(),
                Style::default().fg(lane_color_from(theme, s.2)),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
    }

    spans
}

pub(crate) fn render_branch_labels(
    labels: &[BranchLabel],
    max_len: usize,
    theme: &GraphTheme,
) -> Vec<Span<'static>> {
    if labels.is_empty() {
        return Vec::new();
    }

    let paren_style = Style::default().fg(theme.paren);
    let mut spans = vec![Span::styled("(".to_string(), paren_style)];

    for (i, label) in labels.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(", ".to_string(), paren_style));
        }

        let (prefix, color) = if label.is_head {
            ("* ", theme.head_marker)
        } else if label.is_worktree {
            ("\u{2302} ", theme.worktree_marker)
        } else if label.is_tag {
            ("", theme.tag_label)
        } else if label.is_remote {
            ("", theme.remote_label)
        } else {
            ("", theme.local_branch_label)
        };

        if !prefix.is_empty() {
            spans.push(Span::styled(prefix.to_string(), Style::default().fg(color)));
        }

        let name = if label.name.len() > max_len {
            let mut truncated = label.name[..max_len].to_string();
            truncated.push('\u{2026}'); // …
            truncated
        } else {
            label.name.clone()
        };

        spans.push(Span::styled(name, Style::default().fg(color)));
    }

    spans.push(Span::styled(") ".to_string(), paren_style));
    spans
}

/// Truncate a span list so its total display width fits within `max_width`.
/// Appends `..` at the cut point when truncation occurs.
pub(crate) fn truncate_line(spans: &mut Vec<Span<'static>>, max_width: usize) {
    if max_width == 0 {
        spans.clear();
        return;
    }

    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_width {
        return;
    }

    let mut used = 0;
    let mut cut_idx = spans.len();
    let mut remaining = 0;

    for (i, span) in spans.iter().enumerate() {
        let w = span.content.chars().count();
        if used + w > max_width {
            cut_idx = i;
            remaining = max_width - used;
            break;
        }
        used += w;
    }

    spans.truncate(cut_idx + 1);

    if let Some(last) = spans.last_mut() {
        if remaining > 2 {
            let content: String = last.content.chars().take(remaining - 2).collect();
            *last = Span::styled(format!("{}..", content), last.style);
        } else if remaining >= 1 {
            let dots: String = ".".repeat(remaining);
            *last = Span::styled(dots, last.style);
        } else {
            // No room in this span — back up one
            spans.pop();
            if let Some(prev) = spans.last_mut() {
                let content = prev.content.to_string();
                let n = content.chars().count();
                if n >= 2 {
                    let truncated: String = content.chars().take(n - 2).collect();
                    *prev = Span::styled(format!("{}..", truncated), prev.style);
                } else {
                    *prev = Span::styled(".".repeat(n), prev.style);
                }
            }
        }
    }
}

/// Apply horizontal scroll: skip `offset` characters from the left, then truncate to `max_width`.
pub(crate) fn h_scroll_line(spans: &mut Vec<Span<'static>>, offset: usize, max_width: usize) {
    if offset == 0 {
        truncate_line(spans, max_width);
        return;
    }

    // Phase 1: skip `offset` characters from the left
    let mut to_skip = offset;
    let mut first_kept = 0;

    for (i, span) in spans.iter().enumerate() {
        let w = span.content.chars().count();
        if to_skip >= w {
            to_skip -= w;
            first_kept = i + 1;
        } else {
            break;
        }
    }

    // Remove fully-skipped spans
    if first_kept > 0 {
        spans.drain(..first_kept);
    }

    // Partially skip the first remaining span
    if to_skip > 0
        && let Some(first) = spans.first_mut()
    {
        let remaining: String = first.content.chars().skip(to_skip).collect();
        *first = Span::styled(remaining, first.style);
    }

    // Phase 2: truncate to fit max_width
    truncate_line(spans, max_width);
}

pub(crate) fn format_relative_time(epoch_secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let delta = (now - epoch_secs).max(0) as u64;

    if delta < 60 {
        format!("{}s ago", delta)
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86400 {
        format!("{}h ago", delta / 3600)
    } else if delta < 604_800 {
        format!("{}d ago", delta / 86400)
    } else if delta < 2_592_000 {
        format!("{}w ago", delta / 604_800)
    } else if delta < 31_536_000 {
        format!("{}mo ago", delta / 2_592_000)
    } else {
        format!("{}y ago", delta / 31_536_000)
    }
}

pub(crate) fn author_color(name: &str, theme: &GraphTheme) -> Color {
    let palette = &theme.author_palette;
    if palette.is_empty() {
        return Color::Reset;
    }
    // FNV-1a hash
    let mut hash: u32 = 2_166_136_261;
    for byte in name.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    palette[(hash as usize) % palette.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(name: &str, is_head: bool, is_remote: bool, is_worktree: bool) -> BranchLabel {
        BranchLabel {
            name: name.to_string(),
            is_head,
            is_remote,
            is_worktree,
            is_tag: false,
        }
    }

    #[test]
    fn test_truncate_line_no_op_when_fits() {
        let mut spans = vec![Span::raw("abc"), Span::raw("def")];
        truncate_line(&mut spans, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcdef");
    }

    #[test]
    fn test_truncate_line_adds_ellipsis() {
        let mut spans = vec![Span::raw("hello "), Span::raw("world this is long")];
        truncate_line(&mut spans, 10);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "hello wo..");
    }

    #[test]
    fn test_truncate_line_cuts_at_span_boundary() {
        let mut spans = vec![Span::raw("12345"), Span::raw("67890")];
        truncate_line(&mut spans, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // First span fills exactly 5, second span starts overflow → back up into previous span
        assert_eq!(text, "123..");
    }

    #[test]
    fn test_truncate_line_zero_width() {
        let mut spans = vec![Span::raw("hello")];
        truncate_line(&mut spans, 0);
        assert!(spans.is_empty());
    }

    #[test]
    fn test_empty_labels_returns_empty() {
        let theme = GraphTheme::default();
        let spans = render_branch_labels(&[], 24, &theme);
        assert!(spans.is_empty());
    }

    #[test]
    fn test_head_label_has_star_prefix() {
        let theme = GraphTheme::default();
        let labels = vec![label("main", true, false, false)];
        let spans = render_branch_labels(&labels, 24, &theme);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("* main"), "got: {text}");
    }

    #[test]
    fn test_truncation_adds_ellipsis() {
        let theme = GraphTheme::default();
        let labels = vec![label("very-long-branch-name-here", false, false, false)];
        let spans = render_branch_labels(&labels, 10, &theme);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("very-long-\u{2026}"), "got: {text}");
        assert!(!text.contains("very-long-branch-name-here"));
    }

    #[test]
    fn test_worktree_label_has_house_prefix() {
        let theme = GraphTheme::default();
        let labels = vec![label("feature", false, false, true)];
        let spans = render_branch_labels(&labels, 24, &theme);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("\u{2302} feature"), "got: {text}");
    }

    #[test]
    fn test_multiple_labels_comma_separated() {
        let theme = GraphTheme::default();
        let labels = vec![
            label("main", true, false, false),
            label("origin/main", false, true, false),
        ];
        let spans = render_branch_labels(&labels, 24, &theme);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(", "), "got: {text}");
        assert!(text.starts_with('('));
        assert!(text.contains(')'));
    }

    #[test]
    fn test_relative_time_seconds() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_time(now - 30), "30s ago");
    }

    #[test]
    fn test_relative_time_hours() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_time(now - 7200), "2h ago");
    }

    #[test]
    fn test_relative_time_days() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative_time(now - 259200), "3d ago");
    }

    #[test]
    fn test_relative_time_weeks() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // 2 weeks = 14 days = 14*86400 = 1209600
        assert_eq!(format_relative_time(now - 1_209_600), "2w ago");
    }

    #[test]
    fn test_relative_time_months() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // ~5 months = 5 * 30 days = 12960000
        assert_eq!(format_relative_time(now - 12_960_000), "5mo ago");
    }

    #[test]
    fn test_relative_time_years() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // ~2 years = 2 * 365 days = 63072000
        assert_eq!(format_relative_time(now - 63_072_000), "2y ago");
    }

    #[test]
    fn test_relative_time_future_clamps_to_zero() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Future timestamp
        assert_eq!(format_relative_time(now + 1000), "0s ago");
    }

    #[test]
    fn test_truncate_line_unicode_chars() {
        // Box-drawing chars are each 1 display column
        let mut spans = vec![Span::raw("│ ● "), Span::raw("hello world")];
        truncate_line(&mut spans, 8);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // 4 chars from first span + 2 chars + ".." from second
        assert_eq!(text, "│ ● he..");
    }

    #[test]
    fn test_render_graph_prefix_horizontal_dash_between_spans() {
        use crate::git::graph::{GraphRow, LaneSegment, lane_color};
        use git2::Oid;

        let row = GraphRow {
            commit_col: 2,
            lanes: vec![
                LaneSegment::RightTee,
                LaneSegment::CrossHorizontal,
                LaneSegment::MergeLeft,
            ],
            horizontal_spans: vec![(0, 2, lane_color(2))],
            oid: Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            short_id: String::new(),
            message: String::new(),
            author: String::new(),
            time: 0,
            labels: Vec::new(),
            is_merge: false,
            parent_oids: Vec::new(),
            diff_stat: None,
            collapsed: None,
        };

        let theme = GraphTheme::default();
        let spans = render_graph_prefix(&row, &theme);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        // ├─┼─╯ (space after last glyph)
        assert_eq!(text, "├─┼─╯ ");
    }

    #[test]
    fn test_author_color_deterministic() {
        let theme = GraphTheme::default();
        let c1 = author_color("Alice", &theme);
        let c2 = author_color("Alice", &theme);
        assert_eq!(c1, c2);
        let c3 = author_color("Bob", &theme);
        assert_ne!(c1, c3);
    }

    #[test]
    fn test_tag_label_renders_yellow() {
        let theme = GraphTheme::default();
        let labels = vec![BranchLabel {
            name: "v1.0.0".to_string(),
            is_head: false,
            is_remote: false,
            is_worktree: false,
            is_tag: true,
        }];
        let spans = render_branch_labels(&labels, 24, &theme);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("v1.0.0"), "got: {text}");
        let tag_span = spans
            .iter()
            .find(|s| s.content.as_ref() == "v1.0.0")
            .unwrap();
        assert_eq!(tag_span.style.fg, Some(Color::LightYellow));
    }

    #[test]
    fn test_h_scroll_zero_offset_same_as_truncate() {
        let mut a = vec![Span::raw("hello "), Span::raw("world this is long")];
        let mut b = a.clone();
        h_scroll_line(&mut a, 0, 10);
        truncate_line(&mut b, 10);
        let text_a: String = a.iter().map(|s| s.content.as_ref()).collect();
        let text_b: String = b.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text_a, text_b);
    }

    #[test]
    fn test_h_scroll_skips_characters() {
        let mut spans = vec![Span::raw("abcdef"), Span::raw("ghij")];
        h_scroll_line(&mut spans, 3, 20);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "defghij");
    }

    #[test]
    fn test_h_scroll_skips_full_span() {
        let mut spans = vec![Span::raw("abc"), Span::raw("def"), Span::raw("ghi")];
        h_scroll_line(&mut spans, 4, 20);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "efghi");
    }

    #[test]
    fn test_h_scroll_then_truncate() {
        let mut spans = vec![Span::raw("abcdef"), Span::raw("ghijklmnop")];
        h_scroll_line(&mut spans, 3, 5);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "def..");
    }

    #[test]
    fn test_h_scroll_beyond_content_yields_empty() {
        let mut spans = vec![Span::raw("abc")];
        h_scroll_line(&mut spans, 10, 20);
        assert!(spans.is_empty());
    }
}
