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

use ratatui::buffer::Buffer;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget, Wrap},
};
use std::collections::HashMap;

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
    /// The whole-document layout the virtual pane must match row for row.
    /// Production resolves layouts from a memoized [`RowIndex`] instead (see
    /// [`pane_scroll_layout`], [`window_lines`], [`render_window_pane`]); this
    /// re-wrap-everything path survives as the differential test's reference
    /// and for tests that construct an expected layout by hand.
    #[cfg(test)]
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
/// (a bordered rect). Whole-document twin of the cached-index clamp the panes
/// run in production; kept for tests that assert clamp behavior by hand.
#[cfg(test)]
pub(crate) fn clamp_text_offset(text: &str, pane: Rect, offset: u16) -> u16 {
    ScrollLayout::for_text(text, bordered_inner(pane), offset)
        .gauge
        .offset()
}

/// Prefix sums of per-source-line wrapped row counts for one text at one
/// width — the index a windowed pane binary-searches to find which source
/// lines cover the viewport, plus the byte range of each source line so the
/// window is sliced out in O(1) instead of scanning from the top of the
/// document. Building it is O(document), so it is memoized by the caller
/// (keyed by the text and the width, the only inputs it has); the per-frame
/// work afterwards is O(viewport) no matter where the viewport sits.
///
/// Wrapping is measured per line with the same `Paragraph` the pane renders
/// with, so the totals agree with a whole-document count by construction
/// (spec: `per_line_counts_sum_to_the_whole_document_count`).
pub(crate) struct RowIndex {
    /// `cum[i]` = wrapped rows of the first `i` source lines; `cum[0] == 0` and
    /// the last entry is the document's total. Strictly increasing: every line
    /// wraps to at least one row.
    cum: Vec<u32>,
    /// `starts[i]` = byte offset of source line `i` in the indexed text, as
    /// yielded by `str::lines()`.
    starts: Vec<u32>,
    /// Pre-wrapped rows for the pathological lines a viewport can cover only
    /// partially — a minified JSON or JS blob wraps into thousands of rows,
    /// and handing the whole line to `Paragraph` per frame would re-wrap all
    /// of it on every interaction. Extracted once, here, with the sentinel
    /// renderer below; normal-sized lines are wrapped per frame instead,
    /// where the cost is bounded by the viewport.
    huge: HashMap<u32, Vec<String>>,
    /// The width every count, byte range and pre-wrapped row was measured at.
    width: u16,
}

/// Lines wrapping to at least this many rows are pre-wrapped at index build
/// time (see [`RowIndex::huge`]).
const HUGE_LINE_ROWS: usize = 256;

fn wrapped_rows_exact(line: &str, width: u16, cap_rows: usize) -> Vec<String> {
    const SENTINEL: Color = Color::Rgb(0x53, 0x1b, 0x6e);
    /// Rows extracted per scratch render, so the buffer stays
    /// `width x CHUNK_ROWS` cells no matter how many rows the line wraps to.
    const CHUNK_ROWS: usize = 1024;
    if line.is_empty() {
        // The composer yields one empty row for an empty line.
        return vec![String::new()];
    }
    let styled = Line::from(Span::styled(line, Style::default().fg(SENTINEL)));
    let mut rows = Vec::new();
    let mut rest = line;
    while rows.len() < cap_rows && !rest.is_empty() {
        let take = (cap_rows - rows.len()).min(CHUNK_ROWS) as u16;
        let area = Rect::new(0, 0, width, take);
        let mut buf = Buffer::empty(area);
        Paragraph::new(styled.clone())
            .wrap(Wrap { trim: false })
            .scroll((0, 0))
            .render(area, &mut buf);
        let mut consumed = 0usize;
        for y in 0..take {
            let mut row = String::new();
            for x in 0..width {
                let cell = &buf[(x, y)];
                if cell.fg == SENTINEL {
                    row.push_str(cell.symbol());
                }
            }
            consumed += row.len();
            if row.is_empty() {
                break; // the remaining text produced a blank row: nothing left
            }
            rows.push(row);
        }
        if consumed == 0 {
            break;
        }
        // A viewport narrower than a grapheme truncates that grapheme's
        // cells; advance to the next char boundary so the remaining text
        // stays a valid `str`. The skipped bytes are exactly the tail of the
        // truncated grapheme, which no viewport of this width can display.
        while !rest.is_char_boundary(consumed) {
            consumed += 1;
        }
        rest = &rest[consumed..];
    }
    rows
}

/// The [`RowIndex`] for `text` at `width`.
pub(crate) fn row_index(text: &str, width: u16) -> RowIndex {
    let mut cum = Vec::with_capacity(text.lines().count().saturating_add(2));
    let mut starts = Vec::with_capacity(cum.capacity());
    cum.push(0);
    if text.is_empty() {
        // `Paragraph` renders the empty text as one blank row while `lines()`
        // yields none — match it so totals agree everywhere.
        cum.push(1);
        return RowIndex {
            cum,
            starts,
            huge: HashMap::new(),
            width,
        };
    }
    let base = text.as_ptr() as usize;
    let mut total: u32 = 0;
    let mut huge: HashMap<u32, Vec<String>> = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        starts.push(u32::try_from(line.as_ptr() as usize - base).unwrap_or(u32::MAX));
        let count = wrapped_rows(line, width);
        if count >= HUGE_LINE_ROWS {
            // Pre-wrap every row the line can produce, so a viewport at the
            // maximum offset still has rows below it to display.
            huge.insert(
                u32::try_from(i).unwrap_or(u32::MAX),
                wrapped_rows_exact(line, width, count),
            );
        }
        total = total.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
        cum.push(total);
    }
    RowIndex {
        cum,
        starts,
        huge,
        width,
    }
}

impl RowIndex {
    /// The document's wrapped row count at this index's width.
    pub(crate) fn total(&self) -> usize {
        self.cum.last().map_or(0, |&c| c as usize)
    }

    /// The width every count and pre-wrapped row was measured at.
    pub(crate) fn index_width(&self) -> u16 {
        self.width
    }

    /// The wrapped rows of one source line at this index's width.
    pub(crate) fn line_row_count(&self, line: usize) -> usize {
        (self.cum[line + 1] - self.cum[line]) as usize
    }

    /// Pre-wrapped rows for a huge source line, if it was cached at build.
    pub(crate) fn huge_rows(&self, line: usize) -> Option<&[String]> {
        let i = u32::try_from(line).ok()?;
        self.huge.get(&i).map(Vec::as_slice)
    }

    /// Source line `line` as a `str` slice of `text`, byte-range equivalent to
    /// what `str::lines()` yields (terminator stripped, CRLF included).
    pub(crate) fn line_slice<'t>(&self, text: &'t str, line: usize) -> &'t str {
        let bytes = text.as_bytes();
        let s = self.starts[line] as usize;
        let mut e = if line + 1 < self.starts.len() {
            self.starts[line + 1] as usize
        } else {
            text.len()
        };
        // Strip the terminator `lines()` strips: a newline, plus a CR right
        // before it (covers both interior lines and the last line, whether or
        // not the text ends with a newline).
        if e > s && bytes[e - 1] == b'\n' {
            e -= 1;
            if e > s && bytes[e - 1] == b'\r' {
                e -= 1;
            }
        }
        &text[s..e]
    }

    /// The source-line window covering wrapped rows `first..first+rows`:
    /// `(first source line, source line count, rows to skip inside the window)`.
    /// `first_row` beyond the end parks on the last row.
    pub(crate) fn window_range(&self, first_row: usize, rows: usize) -> (usize, usize, usize) {
        let first_row = first_row.min(self.total().saturating_sub(1));
        let start = self
            .cum
            .partition_point(|&c| (c as usize) <= first_row)
            .saturating_sub(1);
        let target = first_row.saturating_add(rows);
        // `cum.len() - 1` is the number of source lines: a target past the
        // end (bottom-clamped viewports) selects every remaining line.
        let end = self
            .cum
            .partition_point(|&c| (c as usize) < target)
            .max(start + 1)
            .min(self.cum.len() - 1);
        let skip = first_row - self.cum[start] as usize;
        (start, end - start, skip)
    }
}

/// A pane's memoized layout facts: the row index at the content width, plus
/// the exact wrapped-row count at the pane's full inner width. The caller
/// caches it keyed by (text version, inner width). Keeping the full-width
/// count means the thumb decision (`count > visible`) is O(1) per frame and
/// re-evaluated against the current height — a huge single line is never
/// re-wrapped to re-decide it.
pub(crate) struct PaneRows {
    index: RowIndex,
    total_at_full_width: usize,
}

/// The [`PaneRows`] for `text` in a pane whose inner area is `inner_width`
/// cells wide and `visible` rows tall.
pub(crate) fn pane_rows(text: &str, inner_width: u16, visible: usize) -> PaneRows {
    // Exact count at the full inner width: the thumb decision. For content
    // that fits, the full width IS the content width and this index serves
    // both; the extra wrap only happens once per (content, width).
    let total_at_full_width = wrapped_rows(text, inner_width);
    let bar = total_at_full_width > visible && inner_width >= 2;
    let content_width = inner_width - u16::from(bar);
    PaneRows {
        index: row_index(text, content_width),
        total_at_full_width,
    }
}

impl PaneRows {
    pub(crate) fn index(&self) -> &RowIndex {
        &self.index
    }

    /// Exact wrapped-row count at the pane's full inner width.
    pub(crate) fn total_at_full_width(&self) -> usize {
        self.total_at_full_width
    }

    /// Re-count the index at `content_width` when the pane's height flipped
    /// the thumb decision since it was built: a height-only resize changes
    /// which width the viewport renders at without changing the cache key
    /// (text version, inner width). The full-width count stays valid.
    pub(crate) fn retarget(&mut self, text: &str, content_width: u16) {
        if self.index.index_width() != content_width {
            self.index = row_index(text, content_width);
        }
    }
}

/// The frame's scroll layout from `rows`: the thumb decision is the cached
/// full-width count against the pane's current height (O(1), so a huge single
/// line is never re-wrapped for it), and the index must have been counted at
/// the content width that decision implies.
pub(crate) fn pane_scroll_layout(inner: Rect, rows: &PaneRows, offset: u16) -> ScrollLayout {
    let visible = usize::from(inner.height);
    let bar = rows.total_at_full_width() > visible && inner.width >= 2;
    debug_assert_eq!(
        rows.index().index_width(),
        inner.width - u16::from(bar),
        "the index must be (re)built for the current pane size"
    );
    let gauge = ScrollGauge::new(rows.index().total(), visible, offset);
    if bar {
        let (content, bar_rect) = split_off_bar(inner);
        ScrollLayout {
            content,
            bar: Some(bar_rect),
            gauge,
        }
    } else {
        ScrollLayout {
            content: inner,
            bar: None,
            gauge,
        }
    }
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

/// The offset a pointer `row` cells down a `track`-row indicator column selects.
///
/// The top row is the start of the content and the last row its end, so a click
/// lands where it points and a drag scrubs the whole pane. A track with no room to
/// travel selects the top.
pub(crate) fn offset_for_track_row(gauge: ScrollGauge, track: u16, row: u16) -> u16 {
    let last = track.saturating_sub(1);
    if last == 0 {
        return 0;
    }
    let rows = usize::from(row.min(last));
    let max = usize::from(gauge.max_offset());
    // Rounded to nearest, so the middle of the track is the middle of the content.
    ((rows * max + usize::from(last) / 2) / usize::from(last)) as u16
}

/// The offset a click at `pos` selects on `layout`'s indicator column, or `None`
/// when `pos` misses it — the caller then treats the click as content. The whole
/// bar rect is tested, not just its column: panes stacked along one axis share
/// that column, so the row is what says which pane was grabbed.
/// `layout` must be the layout the pane was last drawn with.
pub(crate) fn scrub_offset(layout: ScrollLayout, pos: Position) -> Option<u16> {
    let bar = layout.bar?;
    if !bar.contains(pos) {
        return None;
    }
    scrub_row_offset(layout, pos.y)
}

/// The offset a row of `layout`'s indicator column selects, ignoring the column's
/// x: a drag that wanders sideways keeps scrubbing the pane it grabbed.
pub(crate) fn scrub_row_offset(layout: ScrollLayout, row: u16) -> Option<u16> {
    let bar = layout.bar?;
    Some(offset_for_track_row(
        layout.gauge,
        bar.height,
        row.saturating_sub(bar.y),
    ))
}

/// `text` split into styled lines exactly as `Paragraph::new(text)` splits it, so
/// the rows measured from `text` always match the rows painted from the lines.
#[cfg(test)]
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
/// The whole-document pane the virtual renderer must match row for row: block
/// with the counter in its title, every line wrapped by one `Paragraph`, and
/// the thumb. Production renders only the viewport's rows instead (see
/// [`window_lines`], [`render_window_pane`]); this re-wrap-everything path
/// survives as the differential test's reference.
#[cfg(test)]
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

/// The viewport's rows of `text`, as final styled lines: the window `index`
/// locates for `offset` over a `height`-row viewport, wrapped to the index's
/// width and styled per source line by `line_style`. Rows are sliced from the
/// text by byte range and huge lines come from the index's pre-wrapped cache,
/// so the cost is O(viewport) wherever the viewport sits. Pair with
/// [`render_window_pane`].
pub(crate) fn window_lines(
    text: &str,
    index: &RowIndex,
    offset: u16,
    height: usize,
    line_style: impl Fn(&str) -> Style,
) -> Vec<Line<'static>> {
    if text.is_empty() {
        // `Paragraph` would render the empty text as one blank row; match it.
        return vec![Line::from(Span::styled("", line_style("")))];
    }
    let (start, line_count, mut skip) = index.window_range(usize::from(offset), height);
    let mut budget = height;
    let mut rows: Vec<Line> = Vec::with_capacity(height + 1);
    for k in 0..line_count {
        if budget == 0 {
            break;
        }
        let line = index.line_slice(text, start + k);
        let style = line_style(line);
        if let Some(pre) = index.huge_rows(start + k) {
            // Pre-wrapped at index build: slice the viewport's rows directly.
            for row in pre.iter().skip(skip).take(budget) {
                rows.push(Line::from(Span::styled(row.clone(), style)));
            }
            budget -= (pre.len() - skip).min(budget);
        } else {
            // A normal line: the viewport covers at most its whole wrapped
            // output, so wrapping it here costs at most the viewport.
            let count = index.line_row_count(start + k);
            let take = count.saturating_sub(skip).min(budget);
            let extracted = wrapped_rows_exact(line, index.index_width(), skip + take);
            for row in extracted.into_iter().skip(skip) {
                rows.push(Line::from(Span::styled(row, style)));
            }
            budget -= take;
        }
        skip = 0;
    }
    rows
}

/// [`render_pane`] painting pre-built window rows ([`window_lines`]): the block
/// with the counter in its title, only the viewport's wrapped rows, and the
/// thumb. Row-exact twin of [`render_pane`] — their buffers must match for the
/// same inputs (the differential test holds them to it) — but its per-frame
/// cost is bounded by the viewport instead of the document, so a long diff
/// scrolls as fast at the bottom as at the top.
pub(crate) fn render_window_pane<'a>(
    frame: &mut Frame,
    pane: Rect,
    block: Block<'a>,
    lines: Vec<Line<'a>>,
    layout: ScrollLayout,
    colors: BarColors,
) -> ScrollGauge {
    let mut block = block;
    if let Some(title) = layout.title() {
        block = block.title_top(
            Line::from(title)
                .right_aligned()
                .style(Style::default().fg(colors.thumb)),
        );
    }
    frame.render_widget(block, pane);

    // The rows arrive already wrapped to the layout's width, so no `Wrap`:
    // wrapping again would re-flow pre-wrapped rows.
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, layout.content);

    if let Some(bar) = layout.bar {
        render_bar(frame, bar, layout.gauge, colors);
    }
    layout.gauge
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
mod tests;
