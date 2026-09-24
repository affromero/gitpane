//! Tests for the scroll pane: the virtual renderer's differential gate
//! against the whole-document reference, the per-line wrapping contract the
//! row index builds on, and the geometry/counter unit tests.

use super::*;
use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::{Position, Rect},
    style::{Color, Style},
    widgets::Borders,
};

#[cfg(test)]
mod pane_tests {
    #[test]
    fn narrowing_keeps_wide_text_visible_when_it_would_remove_the_scrollbar() {
        let text = format!("a {}", "中".repeat(100));
        let inner = Rect::new(0, 0, 2, 6);
        let expected = ScrollLayout::for_text(&text, inner, 0);
        let rows = pane_rows(&text, inner.width, usize::from(inner.height));
        let actual = pane_scroll_layout(inner, &rows, 0);

        assert_eq!(actual.content, expected.content);
        assert_eq!(actual.bar, expected.bar);
        assert_eq!(actual.gauge.total, expected.gauge.total);

        let mut resized = pane_rows(&text, inner.width, 200);
        resized.retarget(&text, inner.width, usize::from(inner.height));
        let after_resize = pane_scroll_layout(inner, &resized, 0);
        assert_eq!(after_resize.content, expected.content);
        assert_eq!(after_resize.bar, expected.bar);
        assert_eq!(after_resize.gauge.total, expected.gauge.total);
    }

    #[test]
    fn virtual_renderer_keeps_a_wide_glyph_at_the_content_edge() {
        let text = format!("a {}", "中".repeat(100));
        let pane = Rect::new(0, 0, 80, 5);
        let inner = bordered_inner(pane);
        let rows = pane_rows(&text, inner.width, usize::from(inner.height));
        let layout = pane_scroll_layout(inner, &rows, 0);
        let style = Style::default().fg(Color::Cyan);
        let colors = BarColors {
            thumb: Color::Red,
            track: Color::DarkGray,
        };
        let paint = |old: bool| {
            let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
            terminal
                .draw(|frame| {
                    let block = Block::default().borders(Borders::ALL);
                    if old {
                        render_pane(
                            frame,
                            pane,
                            block,
                            &text,
                            styled_lines(&text, style),
                            0,
                            colors,
                        );
                    } else {
                        let lines = window_lines(
                            &text,
                            rows.index(),
                            layout.gauge.offset(),
                            usize::from(layout.content.height),
                            |_| style,
                        );
                        render_window_pane(frame, pane, block, lines, layout, colors);
                    }
                })
                .unwrap();
            terminal.backend().buffer()[(77, 2)].clone()
        };

        assert_eq!(paint(false), paint(true));
    }

    /// A genuinely large single line (minified JSON/JS shape) must not fall
    /// back to whole-line processing per interaction: the index pre-wraps it
    /// once (the `huge` cache) and every viewport afterwards slices rows from
    /// that cache, at any offset.
    #[test]
    fn huge_single_line_is_pre_wrapped_and_renders_from_the_cache() {
        // One line of ~35k chars: it wraps to ~450 rows at width 80, well
        // past the huge threshold, covering a viewport many times over.
        let text = format!("{}{}{}", "{\"a\":1,".repeat(4000), "z".repeat(400), "}");
        let index = row_index(&text, 80);
        let total = index.total();
        assert!(
            total > HUGE_LINE_ROWS,
            "the line must wrap past the huge threshold: {total}"
        );
        assert!(
            index.huge_rows(0).is_some(),
            "the single line is pre-wrapped into the index cache"
        );
        assert_eq!(index.huge_rows(0).expect("rows").len(), total);

        // Windows at the top, middle, and end agree with the whole-document
        // renderer, row for row.
        let style = Style::default().fg(Color::Cyan);
        for offset in [
            0u16,
            1,
            (total / 2).min(usize::from(u16::MAX)) as u16,
            u16::MAX,
        ] {
            let inner = Rect::new(0, 0, 82, 24);
            let rows = pane_rows(&text, inner.width, usize::from(inner.height));
            let layout = pane_scroll_layout(inner, &rows, offset);
            let virtual_rows = window_lines(
                &text,
                rows.index(),
                layout.gauge.offset(),
                usize::from(layout.content.height),
                |_| style,
            );
            let reference = {
                let width = inner.width - 1;
                let all = wrapped_rows(&text, width);
                let skip = usize::from(layout.gauge.offset()).min(all.saturating_sub(1));
                (0..usize::from(layout.content.height))
                    .map(|r| {
                        let i = skip + r;
                        // Re-derive the row content with the same renderer.
                        wrapped_rows_exact(&text, width, all)[i.min(all - 1)].clone()
                    })
                    .map(|row| Line::from(Span::styled(row, style)))
                    .collect::<Vec<_>>()
            };
            let got: Vec<&str> = virtual_rows
                .iter()
                .map(|l| l.spans[0].content.as_ref())
                .collect();
            let want: Vec<&str> = reference
                .iter()
                .map(|l| l.spans[0].content.as_ref())
                .collect();
            assert_eq!(got, want, "window drift at offset {offset}");
        }
    }

    /// A height-only resize flips the thumb decision without changing the
    /// cache key (text, inner width): the index must be re-counted at the
    /// width the new decision implies, or the layout lies about its extent.
    #[test]
    fn retarget_follows_a_height_only_resize() {
        // 360 cells of run-together text: at inner width 20 that wraps to 18
        // rows, at width 19 (beside a thumb) to 19 rows.
        let text = "x".repeat(360);
        // Taller than the content: it fits, so the index counts at width 20.
        let mut rows = pane_rows(&text, 20, 30);
        assert_eq!(rows.index().index_width(), 20);
        assert!(rows.total_at_full_width() <= 30);

        // Shorter than the content: it overflows, the thumb column appears,
        // and the index must move to width 19.
        rows.retarget(&text, 20, 14);
        assert_eq!(rows.index().index_width(), 19);
        assert!(rows.index().total() > rows.total_at_full_width());
        // And back: widening again restores the full-width index.
        rows.retarget(&text, 20, 30);
        assert_eq!(rows.index().index_width(), 20);
        assert_eq!(rows.index().total(), rows.total_at_full_width());
    }

    /// A line wrapping to 65,536+ rows must not overflow the u16 scratch
    /// height: extraction is capped at the reachable rows and chunked, and a
    /// viewport just past row 0 still renders correctly.
    /// The reviewer's reproduction: a line wrapping past the u16 offset cap,
    /// with the viewport parked at u16::MAX. The cache must retain enough
    /// rows for the whole final screenful (max_offset + visible), not just
    /// up to row u16::MAX.
    #[test]
    fn viewport_at_max_offset_renders_a_full_screenful_of_a_huge_line() {
        let text = "x".repeat(65_560); // wraps to 65,560 rows at content width 1
        let inner = Rect::new(0, 0, 2, 24); // content width 1 beside the thumb
        let rows = pane_rows(&text, inner.width, usize::from(inner.height));
        let layout = pane_scroll_layout(inner, &rows, u16::MAX);
        assert_eq!(layout.gauge.offset(), u16::MAX);
        let lines = window_lines(
            &text,
            rows.index(),
            layout.gauge.offset(),
            usize::from(layout.content.height),
            |_| Style::default(),
        );
        assert_eq!(
            lines.len(),
            usize::from(layout.content.height),
            "the final screenful must be fully drawn"
        );
        for line in &lines {
            assert_eq!(line.spans[0].content.as_ref(), "x");
        }
    }

    #[test]
    fn tall_viewport_at_max_offset_keeps_its_full_screenful_after_resize() {
        let text = format!("{}{}", "x".repeat(65_535), "y".repeat(2_048));
        let visible = 2_048;
        let check = |rows: &PaneRows| {
            let inner = Rect::new(0, 0, 2, visible as u16);
            let layout = pane_scroll_layout(inner, rows, u16::MAX);
            assert_eq!(layout.gauge.offset(), u16::MAX);
            let lines = window_lines(&text, rows.index(), layout.gauge.offset(), visible, |_| {
                Style::default()
            });
            assert_eq!(lines.len(), visible);
            assert!(lines.iter().all(|line| line.spans[0].content == "y"));
        };

        check(&pane_rows(&text, 2, visible));
        let mut resized = pane_rows(&text, 2, 24);
        resized.retarget(&text, 2, visible);
        check(&resized);
    }

    /// Offset 1 into the same huge line must not underflow the cached row
    /// slice (the original subtraction panic).
    #[test]
    fn viewport_at_offset_1_into_a_huge_line_does_not_underflow() {
        let text = "x".repeat(65_536); // exactly 65,536 rows at width 1
        let index = row_index(&text, 1);
        assert_eq!(index.total(), 65_536);
        let rows = index.huge_rows(0).expect("huge line is pre-wrapped");
        assert_eq!(rows.len(), 65_536);
        assert_eq!(&rows[65_535], "x");

        let inner = Rect::new(0, 0, 2, 24);
        let pane = pane_rows(&text, inner.width, usize::from(inner.height));
        let layout = pane_scroll_layout(inner, &pane, 1);
        let lines = window_lines(
            &text,
            pane.index(),
            layout.gauge.offset(),
            usize::from(layout.content.height),
            |_| Style::default(),
        );
        assert_eq!(lines.len(), usize::from(layout.content.height));
    }

    /// Rows `first_row .. first_row + rows` of `line` wrapped at `width`,
    /// from ONE render of the whole line at a vertical offset — the oracle
    /// the chunked cache extraction must reproduce row for row. The buffer
    /// carries the same spare column as the production scratch: a row
    /// ending in a wide grapheme on the last column writes one cell past
    /// the area, and the scan never reads it.
    fn single_render_rows(line: &str, width: u16, first_row: usize, rows: usize) -> Vec<String> {
        let area = Rect::new(0, 0, width, rows as u16);
        let mut buf = Buffer::empty(Rect::new(0, 0, width.saturating_add(1), rows as u16));
        Paragraph::new(Line::from(Span::styled(
            line,
            Style::default().fg(SENTINEL),
        )))
        .wrap(Wrap { trim: false })
        .scroll((first_row as u16, 0))
        .render(area, &mut buf);
        (0..rows as u16)
            .map(|y| {
                let mut row = String::new();
                for x in 0..width {
                    let cell = &buf[(x, y)];
                    if cell.fg == SENTINEL {
                        row.push_str(cell.symbol());
                    }
                }
                row
            })
            .collect()
    }

    /// Chunked extraction must be row-exact with a single render of the same
    /// line at the same vertical offset. The fourth review round found the
    /// chunk loop rendering the whole line from the top on every chunk (row
    /// 1,024 repeated row 0) and, worse, any byte-offset advance mangling
    /// word-wrapped rows past a boundary (the wrapper drops separator
    /// whitespace without painting it). One differential test with all the
    /// requested coverage: content that changes across the boundary, words
    /// separated by spaces, repeated whitespace, wide and combining Unicode,
    /// and the maximum `u16` offset with a multi-row viewport.
    #[test]
    fn chunked_extraction_matches_a_single_render_across_boundaries() {
        let cases: Vec<(&str, String, u16)> = vec![
            // Letters that change exactly at the boundary (the review's
            // `a`-then-`b` reproduction).
            (
                "letters change across the boundary",
                format!("{}{}", "a".repeat(1024), "b".repeat(2048)),
                1,
            ),
            // Words separated by spaces: separator whitespace is dropped at
            // break points, the trap for any byte-offset advance.
            ("words with spaces", "alpha beta gamma ".repeat(500), 5),
            // Repeated whitespace runs.
            ("repeated whitespace", "a      b      ".repeat(700), 7),
            // Wide glyphs, emoji, and combining marks in one line.
            (
                "wide and combining unicode",
                "\u{4e2d}\u{301}\u{6587} \u{1f600}\u{1f600} e\u{301}clair ".repeat(700),
                7,
            ),
        ];
        for (name, line, width) in cases {
            let count = wrapped_rows(&line, width);
            assert!(
                count > CHUNK_ROWS,
                "{name}: the case must wrap past a chunk boundary (rows: {count})"
            );
            // The whole extraction, chunk by chunk, against one tall render.
            let got = wrapped_rows_exact(&line, width, count);
            let want = single_render_rows(&line, width, 0, count);
            assert_eq!(got.len(), want.len(), "{name}: extracted row count");
            assert_eq!(got, want, "{name}: rows drift from the single render");

            // Viewports straddling the first boundary, through the production
            // path: index, huge-line cache, and the window slice.
            let inner = Rect::new(0, 0, width + 1, 24);
            let rows = pane_rows(&line, inner.width, usize::from(inner.height));
            for offset in [
                CHUNK_ROWS as u16 - 4,
                CHUNK_ROWS as u16,
                CHUNK_ROWS as u16 + 4,
            ] {
                let layout = pane_scroll_layout(inner, &rows, offset);
                let window = window_lines(
                    &line,
                    rows.index(),
                    layout.gauge.offset(),
                    usize::from(layout.content.height),
                    |_| Style::default(),
                );
                let got: Vec<&str> = window.iter().map(|l| l.spans[0].content.as_ref()).collect();
                let want = single_render_rows(
                    &line,
                    layout.content.width,
                    usize::from(layout.gauge.offset()),
                    usize::from(layout.content.height),
                );
                assert_eq!(
                    got,
                    want,
                    "{name}: window drift at offset {}",
                    layout.gauge.offset()
                );
            }
        }

        // The maximum u16 offset with a multi-row viewport: alternating
        // letters make the final screenful prove it holds the *right* rows,
        // not merely the right count.
        let line = "ab".repeat(32_780); // 65,560 rows at width 1
        let inner = Rect::new(0, 0, 2, 24);
        let rows = pane_rows(&line, inner.width, usize::from(inner.height));
        let layout = pane_scroll_layout(inner, &rows, u16::MAX);
        assert_eq!(layout.gauge.offset(), u16::MAX);
        let window = window_lines(
            &line,
            rows.index(),
            layout.gauge.offset(),
            usize::from(layout.content.height),
            |_| Style::default(),
        );
        let got: Vec<&str> = window.iter().map(|l| l.spans[0].content.as_ref()).collect();
        let want = single_render_rows(
            &line,
            layout.content.width,
            usize::from(u16::MAX),
            usize::from(layout.content.height),
        );
        assert_eq!(
            got.len(),
            usize::from(layout.content.height),
            "the final screenful must be fully drawn"
        );
        assert_eq!(got, want, "the final screenful at u16::MAX");

        // A line wrapping past every reachable row stops the cache at
        // u16::MAX + CHUNK_ROWS: deeper rows can never be read through a
        // u16 offset, and the count itself is not truncated.
        let deep = "x".repeat(66_600);
        let index = row_index(&deep, 1);
        assert_eq!(index.total(), 66_600);
        assert_eq!(
            index.huge_rows(0).expect("huge line is pre-wrapped").len(),
            usize::from(u16::MAX) + CHUNK_ROWS
        );
    }

    /// A wide grapheme landing on a row's last column paints its content one
    /// cell past the wrapping width (an unbreakable `CJK + ascii` word flushed
    /// past the limit), and that write panicked while building the huge-line
    /// cache when the scratch buffer ended at the render width. The buffer
    /// carries a spare column for it; the rows themselves are unchanged.
    #[test]
    fn wide_grapheme_continuation_does_not_panic_the_cache() {
        // The reviewer's reproduction: `CJK + a` repeated overflows a 10-row
        // pane at inner width 3, the thumb takes a column, and the line wraps
        // at width 2, where rows like `CJK a` overhang by exactly one cell.
        let text = "\u{4e2d}a".repeat(300);
        let rows = pane_rows(&text, 3, 10);
        assert_eq!(rows.index().index_width(), 2);
        let cached = rows.index().huge_rows(0).expect("the line is pre-wrapped");
        let want = single_render_rows(&text, 2, 0, rows.index().line_row_count(0));
        assert_eq!(cached, want, "the cache must match a single render");
    }

    /// Windows are sliced by the byte ranges the index stores, so the slice
    /// must match what `str::lines()` yields, including CRLF line endings and
    /// a trailing newline on the last line.
    #[test]
    fn line_slice_matches_lines_semantics() {
        let text = "alpha\r\nbeta\ngamma-with-\r-inside\nlast no newline";
        let index = row_index(text, 40);
        assert_eq!(index.line_slice(text, 0), "alpha");
        assert_eq!(index.line_slice(text, 1), "beta");
        assert_eq!(index.line_slice(text, 2), "gamma-with-\r-inside");
        assert_eq!(index.line_slice(text, 3), "last no newline");

        let crlf = "one\r\ntwo\r\n";
        let index = row_index(crlf, 40);
        assert_eq!(index.line_slice(crlf, 0), "one");
        assert_eq!(index.line_slice(crlf, 1), "two");
    }
    use super::*;

    /// The windowed pane must be row-exact with the whole-document pane: same
    /// inputs, identical buffers. This is the differential gate for the virtual
    /// renderer — the index only locates windows, and the window is wrapped by
    /// the same `Paragraph`, so any drift (CJK widths, unbreakable words, empty
    /// lines, counter or thumb arithmetic) shows up as a pixel here.
    #[test]
    fn virtual_pane_renders_row_exact_like_render_pane() {
        let style = Style::default().fg(Color::Cyan);
        let colors = BarColors {
            thumb: Color::Red,
            track: Color::DarkGray,
        };
        let corpus: Vec<String> = vec![
            String::new(),
            "one line".into(),
            "word ".repeat(40),
            "\u{4e2d}\u{6587} \u{6587}\u{5b57}".repeat(30),
            "\u{1f600}".repeat(60),
            [
                "a".repeat(100),
                String::new(),
                "\tindent".into(),
                "  trail  ".into(),
            ]
            .join("\n"),
            (0..300)
                .map(|i| match i % 4 {
                    0 => format!("diff --git a/f{i} b/f{i}"),
                    1 => format!("@@ -{i} +{i} @@ {}", "\u{4e2d}".repeat(i % 25)),
                    2 => format!("+{}", "x".repeat(i % 90)),
                    _ => i.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ];
        let sizes = [
            Rect::new(0, 0, 80, 24),
            Rect::new(2, 3, 40, 9),
            Rect::new(0, 0, 120, 40),
            Rect::new(5, 5, 3, 4),
        ];
        let pane_rows_of =
            |text: &str, inner: Rect| pane_rows(text, inner.width, usize::from(inner.height));
        let frame_rows = |paint: &dyn Fn(&mut Frame)| {
            let backend = TestBackend::new(120, 40);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(paint).unwrap();
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .flat_map(|y| {
                    (0..buffer.area.width).map(move |x| {
                        let cell = &buffer[(x, y)];
                        (cell.symbol().to_owned(), cell.fg, cell.bg, cell.modifier)
                    })
                })
                .collect::<Vec<_>>()
        };

        for text in &corpus {
            for size in sizes {
                let pane = size;
                let inner = bordered_inner(pane);
                let rows = pane_rows_of(text, inner);
                let layout = pane_scroll_layout(inner, &rows, 0);
                let max = layout.gauge.max_offset();
                for offset in [0u16, 1, max / 2, max.saturating_sub(1), max, u16::MAX] {
                    let block = || Block::default().title(" Diff ").borders(Borders::ALL);

                    let old = frame_rows(&|f| {
                        render_pane(
                            f,
                            pane,
                            block(),
                            text,
                            styled_lines(text, style),
                            offset,
                            colors,
                        );
                    });
                    let new = frame_rows(&|f| {
                        let layout = pane_scroll_layout(inner, &rows, offset);
                        let lines = window_lines(
                            text,
                            rows.index(),
                            layout.gauge.offset(),
                            usize::from(layout.content.height),
                            |_| style,
                        );
                        render_window_pane(f, pane, block(), lines, layout, colors);
                    });
                    assert_eq!(
                        old,
                        new,
                        "buffer drift: text len {} pane {size:?} offset {offset}",
                        text.len()
                    );
                }
            }
        }
    }

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
    fn exact_fit_keeps_full_width_when_a_scrollbar_would_cause_overflow() {
        // Exactly `inner.height` wrapped rows: with the thumb column taken the
        // content would need more rows, but widening is not needed either — the
        // first pass decides, so a fitting pane never loses a column.
        let inner = Rect::new(0, 0, 10, 3);
        let text = "x".repeat(30);
        assert_eq!(wrapped_rows(&text, 10), 3);
        assert_eq!(wrapped_rows(&text, 9), 4);
        let rows = pane_rows(&text, inner.width, usize::from(inner.height));
        for exact in [
            ScrollLayout::for_text(&text, inner, 0),
            pane_scroll_layout(inner, &rows, 0),
        ] {
            assert!(exact.bar.is_none());
            assert_eq!(exact.content, inner);
            assert_eq!(exact.gauge.max_offset(), 0);
        }
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
    fn track_row_selects_the_offset_it_points_at() {
        let gauge = ScrollGauge::new(340, 10, 0); // max offset 330
        let track = 12;
        assert_eq!(offset_for_track_row(gauge, track, 0), 0);
        assert_eq!(offset_for_track_row(gauge, track, 1), 30);
        assert_eq!(offset_for_track_row(gauge, track, 6), 180); // middle of the track
        assert_eq!(offset_for_track_row(gauge, track, 11), 330);
        // Rows beyond the track clamp to its ends.
        assert_eq!(offset_for_track_row(gauge, track, 99), 330);
        // A one-row track has nowhere to travel, and content that fits has
        // nothing to jump through.
        assert_eq!(offset_for_track_row(gauge, 1, 0), 0);
        assert_eq!(
            offset_for_track_row(ScrollGauge::new(5, 10, 0), track, 11),
            0
        );
    }

    #[test]
    fn scrub_hit_tests_the_whole_indicator_bar() {
        let pane = Rect::new(30, 1, 20, 12); // 18x10 inside the borders
        let text = "x\n".repeat(50);
        let layout = ScrollLayout::for_text(&text, bordered_inner(pane), 0);
        let bar = layout.bar.expect("overflowing content has an indicator");
        assert_eq!(bar.x, pane.x + pane.width - 2);
        assert_eq!(bar.height, 10);
        let max = layout.gauge.max_offset();

        // Clicking the column selects a position along the content.
        assert_eq!(scrub_offset(layout, Position::new(bar.x, bar.y)), Some(0));
        assert_eq!(
            scrub_offset(layout, Position::new(bar.x, bar.y + bar.height - 1)),
            Some(max)
        );
        // One column further left is content, not indicator.
        assert!(scrub_offset(layout, Position::new(bar.x - 1, bar.y)).is_none());
        // Rows above and below the bar belong to the neighbouring pane on the same
        // column — panes stacked along one axis share it.
        assert!(scrub_offset(layout, Position::new(bar.x, bar.y - 1)).is_none());
        assert!(scrub_offset(layout, Position::new(bar.x, bar.y + bar.height)).is_none());
        // A pane whose content fits has no column to grab.
        let fits = ScrollLayout::for_text("one line", bordered_inner(pane), 0);
        assert!(scrub_offset(fits, Position::new(bar.x, bar.y)).is_none());
        // A drag keeps scrubbing when it wanders off the column, clamped to the
        // track's ends.
        assert_eq!(scrub_row_offset(layout, 0), Some(0), "above the track");
        assert_eq!(scrub_row_offset(layout, bar.y + 100), Some(max));
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

#[cfg(test)]
mod per_line_wrapping_spec {
    use super::*;

    /// Wrapping is per source line: a line's wrapped row count never depends on
    /// its neighbours (`WordWrapper` composes line by line, `trim: false` never
    /// merges across the boundary). This is the contract any row-index cache
    /// builds on — per-line counts must sum to the whole-document count, or a
    /// windowed pane would disagree with a full `Paragraph` about its extent.
    /// Wide glyphs, emoji, unbreakable words and odd widths are where a
    /// hand-rolled counter would drift; they are the point of this test.
    #[test]
    fn per_line_counts_sum_to_the_whole_document_count() {
        let texts: Vec<String> = vec![
            // Real git diffs end with a newline: `str::lines()` drops the
            // trailing empty segment on both sides of the contract, and this
            // line pins that a future switch to `split('\n')` cannot drift.
            "a\nb\n".into(),
            "single".into(),
            "word ".repeat(50),
            "\u{4e2d}\u{6587}".repeat(80),
            "\u{1f600}".repeat(70),
            [
                "supercalifragilistic".repeat(8),
                String::new(),
                "  leading and trailing  ".into(),
                "tab\ttab".into(),
            ]
            .join("\n"),
            (0..500)
                .map(|i| format!("{i}: {}{}", "\u{4e2d}".repeat(i % 30), "x".repeat(i % 47)))
                .collect::<Vec<_>>()
                .join("\n"),
        ];
        for width in [1u16, 2, 7, 20, 80, 119, 120] {
            for text in &texts {
                let whole = wrapped_rows(text, width);
                let sum: usize = text.lines().map(|line| wrapped_rows(line, width)).sum();
                assert_eq!(whole, sum, "width {width}, text len {}", text.len());
            }
        }
        // The empty string degenerates: `Paragraph` renders it as one blank row
        // while `lines()` yields none, so windowed callers must special-case it.
        assert_eq!(wrapped_rows("", 10), 1);
    }
}
