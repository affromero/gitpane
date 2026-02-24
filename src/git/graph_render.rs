use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::git::graph::{GraphRow, LaneSegment, lane_color};

const PALETTE: [Color; 6] = [
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
];

pub(crate) fn render_graph_prefix(row: &GraphRow) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    for (col, segment) in row.lanes.iter().enumerate() {
        let color = PALETTE[lane_color(col)];
        let style = Style::default().fg(color);

        let ch = match segment {
            LaneSegment::Empty => " ",
            LaneSegment::Straight => "│",
            LaneSegment::Commit => "●",
            LaneSegment::MergeLeft => "╯",
            LaneSegment::MergeRight => "╰",
            LaneSegment::ForkLeft => "╮",
            LaneSegment::ForkRight => "╭",
        };

        spans.push(Span::styled(ch.to_string(), style));
        spans.push(Span::raw(" "));
    }

    spans
}
