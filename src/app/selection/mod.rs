use crossterm::event::MouseEvent;
use ratatui::{
    buffer::{Buffer, CellWidth},
    layout::{Position, Rect},
    style::{Modifier, Style},
};

mod routing;
#[cfg(test)]
mod tests;

/// A gesture snapshots only its text pane, once after the press. Frames compare
/// symbols against that snapshot so an update cannot silently change copied text.
#[derive(Default)]
pub(super) struct TextSelection {
    press: Option<MouseEvent>,
    area: Rect,
    anchor: Position,
    head: Position,
    dragged: bool,
    snapshot: Option<Buffer>,
    copy_on_draw: bool,
}

impl TextSelection {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn begin(&mut self, area: Rect, press: MouseEvent) {
        let anchor = Position::new(press.column, press.row);
        *self = Self {
            press: Some(press),
            area,
            anchor,
            head: anchor,
            ..Self::default()
        };
    }

    pub(super) fn origin(&self) -> Option<Position> {
        (!self.area.is_empty()).then_some(self.anchor)
    }

    pub(super) fn is_pressed(&self) -> bool {
        self.press.is_some()
    }

    pub(super) fn has_selection(&self) -> bool {
        self.dragged
    }

    pub(super) fn extend(&mut self, position: Position) {
        if self.press.is_none() {
            return;
        }
        self.head = Position::new(
            position.x.clamp(self.area.x, self.area.right() - 1),
            position.y.clamp(self.area.y, self.area.bottom() - 1),
        );
        self.dragged |= self.head != self.anchor;
    }

    /// Return the deferred normal click only when the pointer never selected text.
    pub(super) fn release(&mut self, position: Position) -> Option<MouseEvent> {
        self.extend(position);
        let press = self.press.take();
        if !self.dragged {
            self.clear();
            return press;
        }
        self.copy_on_draw = true;
        None
    }

    pub(super) fn take_copy(&mut self) -> Option<String> {
        if !self.copy_on_draw || self.snapshot.is_none() {
            return None;
        }
        self.copy_on_draw = false;
        self.copy_text()
    }

    fn bounds(&self) -> (Position, Position) {
        if (self.anchor.y, self.anchor.x) <= (self.head.y, self.head.x) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn columns(&self, row: u16) -> Option<(u16, u16)> {
        let (start, end) = self.bounds();
        (start.y..=end.y).contains(&row).then(|| {
            (
                if row == start.y { start.x } else { self.area.x },
                if row == end.y {
                    end.x
                } else {
                    self.area.right() - 1
                },
            )
        })
    }

    pub(super) fn copy_text(&self) -> Option<String> {
        if !self.dragged {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        let (start, end) = self.bounds();
        let mut text = String::new();
        for row in start.y..=end.y {
            let (left, right) = self.columns(row)?;
            for (column, symbol, width) in glyphs(snapshot, row) {
                if column <= right && column + width > left {
                    text.push_str(symbol);
                }
            }
            text.truncate(text.trim_end_matches(' ').len());
            if row != end.y {
                text.push('\n');
            }
        }
        Some(text)
    }

    pub(super) fn draw(&mut self, buffer: &mut Buffer, area: Option<Rect>) {
        if self.origin().is_none() {
            return;
        }
        if area != Some(self.area) || buffer.area.intersection(self.area) != self.area {
            self.clear();
            return;
        }
        if let Some(snapshot) = &self.snapshot {
            let changed = (self.area.y..self.area.bottom()).any(|row| {
                (self.area.x..self.area.right()).any(|column| {
                    buffer[(column, row)].symbol() != snapshot[(column, row)].symbol()
                })
            });
            if changed {
                self.clear();
                return;
            }
        } else {
            self.snapshot = Some(Buffer {
                area: self.area,
                content: (self.area.y..self.area.bottom())
                    .flat_map(|row| {
                        (self.area.x..self.area.right()).map(move |column| (column, row))
                    })
                    .map(|position| buffer[position].clone())
                    .collect(),
            });
        }
        if !self.dragged {
            return;
        }
        let style = Style::default().add_modifier(Modifier::REVERSED);
        let snapshot = self.snapshot.as_ref().unwrap();
        let (start, end) = self.bounds();
        for row in start.y..=end.y {
            let (left, right) = self.columns(row).unwrap();
            for (column, _, width) in glyphs(snapshot, row) {
                if column <= right && column + width > left {
                    for x in column..column + width {
                        buffer[(x, row)].set_style(style);
                    }
                }
            }
        }
    }
}

/// Ratatui stores a whole grapheme in its leading cell. Skip its continuation
/// cells rather than copying their padding or splitting a wide/combining glyph.
fn glyphs(buffer: &Buffer, row: u16) -> impl Iterator<Item = (u16, &str, u16)> {
    let mut column = buffer.area.x;
    std::iter::from_fn(move || {
        if column >= buffer.area.right() {
            return None;
        }
        let cell = &buffer[(column, row)];
        let symbol = cell.symbol();
        let width = cell.cell_width().max(1).min(buffer.area.right() - column);
        let start = column;
        column += width;
        Some((start, symbol, width))
    })
}
