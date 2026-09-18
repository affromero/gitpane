//! Shared scroll indicator for the panes that scroll vertically: a thumb in the
//! pane's right column plus a `seen/total` counter in the block title, so a long
//! diff never leaves you guessing whether you reached the end.
//!
//! The geometry and the row counting are pure so they can be unit-tested without
//! a terminal; [`render_pane`], [`render_bar`] and [`render_counter`] are the
//! thin callers that paint them.
//!
//! Panes are addressed by their *bordered* rect — the same rect their `Block` is
//! rendered into — because the thumb has to sit inside the border, and the
//! content has to wrap one column narrower than the pane while it does.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Paragraph, Wrap},
};

/// Thumb glyph; the track is a thin rail so the thumb reads as the moving part.
pub(crate) const THUMB: &str = "\u{2588}"; // █
pub(crate) const TRACK: &str = "\u{2502}"; // │

/// Thumb and track colors, so each pane keeps its own palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BarColors {
    pub thumb: Color,
    pub track: Color,
}

/// How much content a pane holds and where its viewport sits in it. `total` and
/// `visible` are wrapped rows for text and rows for a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollGauge {
    total: usize,
    visible: usize,
    offset: u16,
}

impl ScrollGauge {
    /// Gauge for `total` rows of content with `visible` of them on screen,
    /// scrolled to `offset`. The offset is clamped to the last full screenful,
    /// so callers can store it back verbatim.
    ///
    /// Content taller than `u16::MAX` rows is truncated to the reachable end:
    /// `Paragraph` scrolls in `u16` rows, so rows past that can never be shown,
    /// and counting them would leave the thumb at the bottom of its track while
    /// the counter still claimed rows were left.
    pub(crate) fn new(total: usize, visible: usize, offset: u16) -> Self {
        let total = total.min(usize::from(u16::MAX).saturating_add(visible));
        let max = total.saturating_sub(visible).min(usize::from(u16::MAX)) as u16;
        Self {
            total,
            visible,
            offset: offset.min(max),
        }
    }

    /// Rows the pane can show at once.
    pub(crate) fn visible(&self) -> usize {
        self.visible
    }

    /// Largest useful offset: `0` while the content fits (or the pane is empty).
    pub(crate) fn max_offset(&self) -> u16 {
        self.total
            .saturating_sub(self.visible)
            .min(usize::from(u16::MAX)) as u16
    }

    /// The offset actually on screen, after clamping.
    pub(crate) fn offset(&self) -> u16 {
        self.offset
    }

    /// Rows the viewport currently covers.
    fn seen(&self) -> usize {
        (usize::from(self.offset) + self.visible).min(self.total)
    }

    /// `seen/total` for the title, or `None` while everything fits — a pane that
    /// shows all of its content has nothing to indicate.
    pub(crate) fn label(&self) -> Option<String> {
        (self.max_offset() > 0).then(|| format!("{}/{}", self.seen(), self.total))
    }
}

/// A bordered pane's resolved scroll layout: where the content, the thumb and
/// the counter go, and how far the content extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScrollLayout {
    /// Where the paragraph (or list) is painted.
    pub(crate) content: Rect,
    /// The one-column thumb strip; `None` while the content fits.
    pub(crate) bar: Option<Rect>,
    pub(crate) gauge: ScrollGauge,
}

impl ScrollLayout {
    /// Layout for word-wrapped `text` inside a pane's `inner` area. The thumb
    /// column is only taken when the content overflows, and the rows are then
    /// re-counted at the narrower width that follows from taking it: a pane that
    /// barely fits must not lose a column to a thumb it does not need.
    pub(crate) fn for_text(text: &str, inner: Rect, offset: u16) -> Self {
        let gauge_at =
            |width| ScrollGauge::new(wrapped_rows(text, width), usize::from(inner.height), offset);
        let wide = gauge_at(inner.width);
        if inner.width < 2 || wide.max_offset() == 0 {
            return Self {
                content: inner,
                bar: None,
                gauge: wide,
            };
        }
        let narrow = gauge_at(inner.width - 1);
        if narrow.max_offset() == 0 {
            // Safety net: dropping a column cannot shorten a wrapped paragraph, so
            // a pane that did not fit above cannot fit here. Wide is the correct
            // gauge if it ever happens (a full-width render needs no clamp).
            return Self {
                content: inner,
                bar: None,
                gauge: wide,
            };
        }
        let (content, bar) = split_off_bar(inner);
        Self {
            content,
            bar: Some(bar),
            gauge: narrow,
        }
    }

    /// Layout for a list of `items` rows, whose own `offset` (the index of its
    /// first visible row) drives the thumb.
    pub(crate) fn for_list(items: usize, inner: Rect, offset: usize) -> Self {
        let gauge = ScrollGauge::new(items, usize::from(inner.height), offset_u16(offset));
        if inner.width < 2 || gauge.max_offset() == 0 {
            return Self {
                content: inner,
                bar: None,
                gauge,
            };
        }
        let (content, bar) = split_off_bar(inner);
        Self {
            content,
            bar: Some(bar),
            gauge,
        }
    }

    /// ` 12/340 ` for a block title, or `None` while everything fits.
    pub(crate) fn title(&self) -> Option<String> {
        self.gauge.label().map(|label| format!(" {label} "))
    }
}

/// Wrapped row count of `text` at `width`, using the same display-width word
/// wrapping `Paragraph` renders with — so wide (CJK) glyphs count as two cells
/// and the content end stays reachable.
pub(crate) fn wrapped_rows(text: &str, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width)
}

/// The area a bordered pane keeps for its content: `pane` minus one cell on
/// every side. Panes that paint a thumb must measure and render against this
/// rather than against the bordered rect, so `j`/wheel scrolling clamps to the
/// same last screenful the frame drew.
pub(crate) fn bordered_inner(pane: Rect) -> Rect {
    Rect {
        x: pane.x.saturating_add(1),
        y: pane.y.saturating_add(1),
        width: pane.width.saturating_sub(2),
        height: pane.height.saturating_sub(2),
    }
}

/// `offset` clamped to the last screenful the wrapped `text` allows in `pane`
/// (a bordered rect). What a key or wheel step should store back.
pub(crate) fn clamp_text_offset(text: &str, pane: Rect, offset: u16) -> u16 {
    ScrollLayout::for_text(text, bordered_inner(pane), offset)
        .gauge
        .offset()
}

/// Scroll offsets leave a list as `usize` and enter the gauge as `u16`; hold at
/// the maximum rather than wrapping around.
pub(crate) fn offset_u16(offset: usize) -> u16 {
    u16::try_from(offset).unwrap_or(u16::MAX)
}

/// Split a pane's inner area into the content column and a one-column thumb
/// strip along its right edge.
fn split_off_bar(inner: Rect) -> (Rect, Rect) {
    let bar = Rect {
        x: inner.x + inner.width - 1,
        width: 1,
        ..inner
    };
    let content = Rect {
        width: inner.width - 1,
        ..inner
    };
    (content, bar)
}

/// `text` split into styled lines exactly as `Paragraph::new(text)` splits it, so
/// the rows measured from `text` always match the rows painted from the lines.
pub(crate) fn styled_lines(text: &str, style: Style) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = if text.is_empty() {
        vec![Line::from("")]
    } else {
        text.lines()
            .map(|line| Line::from(line.to_owned()))
            .collect()
    };
    for line in &mut lines {
        line.style = style;
    }
    lines
}

/// Paint a scrolling text pane: the caller's `block` with the counter merged
/// into its top border, the wrapped `lines`, and the thumb.
///
/// `text` is the content the caller turned into `lines`; rows are measured from
/// it rather than from a per-frame clone of the styled lines. The block must use
/// `Borders::ALL`, so `block.inner(pane)` is the content area. Returns the gauge
/// that was rendered, so the caller can store the clamped offset back.
pub(crate) fn render_pane<'a>(
    frame: &mut Frame,
    pane: Rect,
    block: Block<'a>,
    text: &str,
    lines: Vec<Line<'a>>,
    offset: u16,
    colors: BarColors,
) -> ScrollGauge {
    let layout = ScrollLayout::for_text(text, block.inner(pane), offset);
    let mut block = block;
    if let Some(title) = layout.title() {
        block = block.title_top(
            Line::from(title)
                .right_aligned()
                .style(Style::default().fg(colors.thumb)),
        );
    }
    frame.render_widget(block, pane);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((layout.gauge.offset(), 0));
    frame.render_widget(paragraph, layout.content);

    if let Some(bar) = layout.bar {
        render_bar(frame, bar, layout.gauge, colors);
    }
    layout.gauge
}

/// The thumb as `(start, len)` rows inside a `track`-row column.
///
/// Integer math, and deliberately not ratatui's `Scrollbar`: that widget maps the
/// position over `content_length - 1 + viewport_length`, which leaves the thumb one
/// row short of the end when the content is scrolled all the way — exactly the
/// "am I at the bottom?" question this indicator exists to answer. Here the thumb
/// is flush with the first row at the top and the last row at the bottom.
pub(crate) fn thumb_range(gauge: ScrollGauge, track: u16) -> (u16, u16) {
    if track == 0 {
        return (0, 0);
    }
    let rows = usize::from(track);
    let total = gauge.total.max(1);
    // Share of the content on screen, at least one row so the thumb stays
    // visible — and never the whole track while there is anywhere to scroll, so
    // a pane overflowing by a single row still shows the thumb move instead of a
    // full-height bar that never budges.
    let cap = if gauge.max_offset() > 0 {
        rows.saturating_sub(1).max(1)
    } else {
        rows
    };
    let len = ((gauge.visible * rows).div_ceil(total)).clamp(1, cap);
    let max = usize::from(gauge.max_offset());
    let start = if max == 0 {
        0
    } else {
        // Spread the offset over the rows the thumb can travel.
        (usize::from(gauge.offset()) * (rows - len))
            .div_ceil(max)
            .min(rows - len)
    };
    (start as u16, len as u16)
}

/// Paint just the thumb strip, for panes whose content is a stateful widget the
/// caller renders itself (a list).
pub(crate) fn render_bar(frame: &mut Frame, area: Rect, gauge: ScrollGauge, colors: BarColors) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (start, len) = thumb_range(gauge, area.height);
    let thumb_style = Style::default().fg(colors.thumb);
    let track_style = Style::default().fg(colors.track);
    let lines: Vec<Line> = (0..area.height)
        .map(|row| {
            let on_thumb = row >= start && row < start + len;
            Line::styled(
                if on_thumb { THUMB } else { TRACK },
                if on_thumb { thumb_style } else { track_style },
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Paint the counter on the top border, right-aligned in the slot a right
/// aligned block title uses. For panes that render their own list widget, and so
/// can only add the counter once the list has settled its offset.
pub(crate) fn render_counter(frame: &mut Frame, pane: Rect, gauge: ScrollGauge, colors: BarColors) {
    let Some(label) = gauge.label() else {
        return;
    };
    // A collapsed pane has no border row of its own to paint on: the axis can
    // squeeze one between two neighbours, and painting there would write over
    // the pane that actually occupies the row.
    if pane.height == 0 {
        return;
    }
    let area = Rect {
        x: pane.x.saturating_add(1),
        y: pane.y,
        width: pane.width.saturating_sub(2),
        height: 1,
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    let counter = Line::from(format!(" {label} "))
        .right_aligned()
        .style(Style::default().fg(colors.thumb));
    frame.render_widget(Paragraph::new(counter), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, widgets::Borders};

    #[test]
    fn label_reports_seen_over_total_and_hides_when_content_fits() {
        // 340 rows, 10 on screen: the label counts what has been seen.
        assert_eq!(
            ScrollGauge::new(340, 10, 0).label().as_deref(),
            Some("10/340")
        );
        assert_eq!(
            ScrollGauge::new(340, 10, 100).label().as_deref(),
            Some("110/340")
        );
        // Scrolled past the end: clamped to the last screenful, so the label
        // ends at total/total — the "reached the bottom" signal.
        let gauge = ScrollGauge::new(340, 10, 9999);
        assert_eq!(gauge.offset(), 330);
        assert_eq!(gauge.label().as_deref(), Some("340/340"));
        assert_eq!(gauge.max_offset(), 330);
        // Nothing to indicate while the content fits.
        assert_eq!(ScrollGauge::new(5, 10, 0).max_offset(), 0);
        assert_eq!(ScrollGauge::new(5, 10, 3).offset(), 0);
        assert!(ScrollGauge::new(5, 10, 0).label().is_none());
        // Empty pane.
        assert_eq!(ScrollGauge::new(100, 0, 5).max_offset(), 100);
    }

    #[test]
    fn wrapped_rows_matches_paragraph_wrapping() {
        assert_eq!(wrapped_rows("short", 80), 1);
        let long = ["a".repeat(40), "b".repeat(40), "c".repeat(40)].join("\n");
        assert_eq!(wrapped_rows(&long, 10), 12); // 3 lines of 4 wrapped rows
        // CJK glyphs are two cells wide, so they wrap by display width.
        assert_eq!(wrapped_rows(&"中".repeat(5), 4), 3); // 10 cells / 4
        // Degenerate width.
        assert_eq!(wrapped_rows("anything", 0), 0);
    }

    #[test]
    fn text_layout_reserves_a_column_only_when_the_content_overflows() {
        let inner = Rect::new(1, 1, 20, 5);
        // Fits: full width, no thumb.
        let fits = ScrollLayout::for_text("one\ntwo", inner, 0);
        assert_eq!(fits.content, inner);
        assert!(fits.bar.is_none());
        assert!(fits.title().is_none());
        // Overflows: the content column gives up exactly one column, and the
        // rows are counted at that narrower width.
        let over = ScrollLayout::for_text(&"x\n".repeat(40), inner, 0);
        let bar = over.bar.expect("overflowing content gets a thumb");
        assert_eq!(over.content.width, inner.width - 1);
        assert_eq!(bar.width, 1);
        assert_eq!(bar.x + bar.width, inner.x + inner.width);
        assert_eq!(over.gauge.max_offset(), 40 - inner.height);
        assert_eq!(over.title().as_deref(), Some(" 5/40 "));
        // Too narrow to spare a column: no thumb, content keeps the width.
        let narrow = ScrollLayout::for_text(&"x\n".repeat(40), Rect::new(0, 0, 1, 5), 0);
        assert!(narrow.bar.is_none());
        assert_eq!(narrow.content.width, 1);
    }

    #[test]
    fn text_layout_only_takes_the_column_if_the_content_still_overflows() {
        // Exactly `inner.height` wrapped rows: with the thumb column taken the
        // content would need more rows, but widening is not needed either — the
        // first pass decides, so a fitting pane never loses a column.
        let inner = Rect::new(0, 0, 10, 3);
        let exact = ScrollLayout::for_text("a\nb\nc", inner, 0);
        assert!(exact.bar.is_none());
        assert_eq!(exact.content.width, 10);
    }

    #[test]
    fn list_layout_uses_the_list_offset() {
        let inner = Rect::new(2, 2, 30, 4);
        let visible_page = ScrollLayout::for_list(3, inner, 0);
        assert!(visible_page.bar.is_none());
        assert_eq!(visible_page.content, inner);

        let scrolling = ScrollLayout::for_list(40, inner, 5);
        assert_eq!(scrolling.gauge.offset(), 5);
        assert_eq!(scrolling.gauge.visible(), 4);
        assert_eq!(scrolling.gauge.max_offset(), 36);
        assert_eq!(scrolling.title().as_deref(), Some(" 9/40 "));
        assert_eq!(scrolling.content.width, inner.width - 1);
    }

    #[test]
    fn clamp_helpers_agree_with_the_pane_layout() {
        let pane = Rect::new(4, 4, 22, 7); // 20x5 inner
        let text = "x\n".repeat(50);
        assert_eq!(clamp_text_offset(&text, pane, u16::MAX), 50 - 5);
        assert_eq!(clamp_text_offset(&text, pane, 3), 3);
        assert_eq!(clamp_text_offset(&text, pane, 999), 45);
        // Content that fits never scrolls.
        assert_eq!(clamp_text_offset("one line", pane, u16::MAX), 0);
        assert_eq!(clamp_text_offset("one line", pane, 7), 0);
        // Degenerate pane (no detail drawn yet).
        assert_eq!(clamp_text_offset(&text, Rect::default(), u16::MAX), 0);
    }

    #[test]
    fn thumb_range_travels_the_whole_track_and_sizes_with_the_content() {
        // Half the content on screen: a thumb half the track, at the top.
        assert_eq!(thumb_range(ScrollGauge::new(100, 50, 0), 10), (0, 5));
        // Scrolled to the end: flush with the last row, so "thumb at the bottom"
        // and "the counter reads total/total" mean the same thing.
        assert_eq!(thumb_range(ScrollGauge::new(100, 50, 50), 10), (5, 5));
        // A sliver of the content: still one visible row for the thumb.
        assert_eq!(thumb_range(ScrollGauge::new(5000, 10, 0), 10), (0, 1));
        assert_eq!(thumb_range(ScrollGauge::new(5000, 10, 4990), 10), (9, 1));
        // Middle of a long document: the thumb sits in the middle.
        assert_eq!(thumb_range(ScrollGauge::new(1000, 100, 450), 10), (5, 1));
        // Overflowing by a single row must still move the thumb: a full-track bar
        // that never budges is worse than no indicator.
        let one_row = ScrollGauge::new(13, 12, 0);
        let (start, len) = thumb_range(one_row, 12);
        assert!(len < 12, "thumb must leave room to travel: {len}");
        assert_eq!((start, len), (0, 11));
        let scrolled = ScrollGauge::new(13, 12, one_row.max_offset());
        assert_eq!(thumb_range(scrolled, 12), (1, 11));
        assert_eq!(thumb_range(scrolled, 12).0 + 11, 12, "flush with the end");
        // A single-row track has nowhere to travel, but must not be empty.
        assert_eq!(thumb_range(ScrollGauge::new(9, 8, 1), 1), (0, 1));
        // Degenerate track.
        assert_eq!(thumb_range(ScrollGauge::new(100, 10, 0), 0), (0, 0));
    }

    #[test]
    fn gauge_reports_total_over_total_when_only_the_u16_reachable_end_is_left() {
        // `Paragraph` scrolls in u16 rows, so rows past 65 535 can never be shown.
        // Counting them would leave the thumb at the bottom of its track while the
        // counter still claimed rows were left to see.
        let gauge = ScrollGauge::new(70_000, 12, u16::MAX);
        let bottom = gauge.max_offset();
        assert_eq!(bottom, u16::MAX);
        assert_eq!(gauge.offset(), bottom);
        assert_eq!(gauge.label().as_deref(), Some("65547/65547"));
        // The thumb agrees: flush with the last row of the track.
        let (start, len) = thumb_range(gauge, 12);
        assert_eq!(start + len, 12);
    }

    #[test]
    fn counter_is_not_painted_for_a_collapsed_pane() {
        let colors = BarColors {
            thumb: Color::Cyan,
            track: Color::DarkGray,
        };
        let gauge = ScrollGauge::new(40, 12, 0);
        // The detail axis can squeeze a pane down to zero rows; its top border row
        // then belongs to the pane behind it, so the counter must stay out.
        let rows = |with_counter: bool| {
            let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
            terminal
                .draw(|frame| {
                    let block = Block::default().title(" Files ").borders(Borders::ALL);
                    frame.render_widget(block, Rect::new(0, 1, 20, 2));
                    if with_counter {
                        render_counter(frame, Rect::new(0, 1, 20, 0), gauge, colors);
                    }
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<String>>()
        };
        assert_eq!(
            rows(false),
            rows(true),
            "a zero-height pane must not paint a counter"
        );
    }

    #[test]
    fn render_pane_paints_counter_and_thumb_only_when_scrolling() {
        let colors = BarColors {
            thumb: Color::Cyan,
            track: Color::DarkGray,
        };
        let text = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let pane = Rect::new(0, 0, 24, 8); // 22x6 inside the borders

        // The pane for one offset, as rows of rendered text.
        let render = |offset: u16| {
            let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
            terminal
                .draw(|frame| {
                    let block = Block::default()
                        .title(" Diff ")
                        .borders(ratatui::widgets::Borders::ALL);
                    render_pane(
                        frame,
                        pane,
                        block,
                        &text,
                        styled_lines(&text, Style::default()),
                        offset,
                        colors,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect::<Vec<String>>()
        };

        // 30 rows of content, 6 visible: the counter counts what has been seen.
        let top = render(0);
        assert!(
            top[0].contains("6/30"),
            "counter in the title: {:?}",
            top[0]
        );
        // The thumb lives in the pane's last inner column (x = 22), starting at
        // the top of the track so a long diff reads as "you are at the top".
        let track = |rows: &[String]| {
            (1..7)
                .map(|y| rows[y].chars().nth(22).unwrap_or(' '))
                .collect::<String>()
        };
        assert!(
            track(&top).starts_with(THUMB),
            "thumb starts the track: {:?}",
            track(&top)
        );

        // Scrolled to the end: the counter reads total/total and the thumb has
        // moved to the bottom of its track.
        let bottom = render(24);
        assert!(bottom[0].contains("30/30"), "counter: {:?}", bottom[0]);
        assert!(
            track(&bottom).ends_with(THUMB),
            "thumb ends the track: {:?}",
            track(&bottom)
        );

        // Same pane with content that fits: no counter, no thumb, and the title
        // keeps its border row to itself.
        let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
        terminal
            .draw(|frame| {
                let block = Block::default()
                    .title(" Diff ")
                    .borders(ratatui::widgets::Borders::ALL);
                render_pane(
                    frame,
                    pane,
                    block,
                    "just one line",
                    styled_lines("just one line", Style::default()),
                    0,
                    colors,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text_rows: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        assert!(!text_rows.contains(THUMB), "no thumb: {text_rows:?}");
        assert!(
            text_rows.contains("\u{250c} Diff \u{2500}\u{2500}"),
            "title keeps the border: {text_rows:?}"
        );
    }
}
