//! Adaptive layout (ADR-0137): pure geometry that lays things out in the
//! room they get, in logical pixels, so apps and the desktop follow the
//! window's and the screen's size instead of fixed positions. Host-tested.
//!
//! - [`Bar`]: a toolbar's row: items taken from its left and its right
//!   end, and what is left between them for a field or a title.
//! - [`Flow`]: items placed left to right that wrap onto the next line.
//! - [`window_size`]: the size a window opens at on a screen.
//! - [`grid`]: how many columns and rows of tiles fit.
//! - [`pack`]: tiles of different sizes placed on a grid, first fit.

use crate::Rect;

/// A row of items: taken from either end, the rest between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    left: i32,
    right: i32,
    y: i32,
    h: i32,
    gap: i32,
}

impl Bar {
    /// Items of `r`'s height, `gap` apart, inside `r`.
    pub fn new(r: Rect, gap: i32) -> Self {
        Self {
            left: r.x,
            right: r.x + r.w,
            y: r.y,
            h: r.h,
            gap,
        }
    }

    /// The next item from the left, `width` wide.
    pub fn left(&mut self, width: i32) -> Rect {
        let r = Rect::new(self.left, self.y, width, self.h);
        self.left += width + self.gap;
        r
    }

    /// The next item from the right, `width` wide.
    pub fn right(&mut self, width: i32) -> Rect {
        self.right -= width;
        let r = Rect::new(self.right, self.y, width, self.h);
        self.right -= self.gap;
        r
    }

    /// Whether an item `width` wide still fits between the two ends.
    pub fn fits(&self, width: i32) -> bool {
        self.room() >= width
    }

    /// The room left between what was taken from either end.
    pub fn room(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    /// That room, as a rectangle (at most `max` wide, from the left).
    pub fn rest(&self, max: i32) -> Rect {
        Rect::new(self.left, self.y, self.room().min(max), self.h)
    }
}

/// Items placed left to right, onto the next line when one does not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flow {
    area: Rect,
    x: i32,
    y: i32,
    line: i32,
    gap: i32,
    line_gap: i32,
    started: bool,
}

impl Flow {
    /// From `area`'s top left, lines `line` high, items and lines `gap`
    /// apart.
    pub fn new(area: Rect, line: i32, gap: i32) -> Self {
        Self {
            area,
            x: area.x,
            y: area.y,
            line,
            gap,
            line_gap: gap,
            started: false,
        }
    }

    /// Lines `gap` apart (items stay as far apart as `new` said).
    pub fn line_gap(mut self, gap: i32) -> Self {
        self.line_gap = gap;
        self
    }

    /// The next item, `width` wide (at most the area's width).
    pub fn next(&mut self, width: i32) -> Rect {
        let width = width.min(self.area.w);
        if self.started && self.x + width > self.area.x + self.area.w {
            self.x = self.area.x;
            self.y += self.line + self.line_gap;
        }
        self.started = true;
        let r = Rect::new(self.x, self.y, width, self.line);
        self.x += width + self.gap;
        r
    }

    /// Where what follows the items goes: under their last line.
    pub fn bottom(&self) -> i32 {
        if self.started {
            self.y + self.line
        } else {
            self.y
        }
    }
}

/// The area windows have on the screen apps are drawn for: 1280 × 800
/// less the top bar and the dock (logical pixels).
pub const REFERENCE_AREA: (i32, i32) = (1280, 692);

/// The size a window opens at (ADR-0137): the size its app was drawn for
/// (`base`, on [`REFERENCE_AREA`]), as much larger as the area the windows
/// have (`area`) is, never smaller than `min` nor larger than the area.
/// On a screen of the reference size a window keeps the size asked for.
pub fn window_size(base: (i32, i32), min: (i32, i32), area: (i32, i32)) -> (i32, i32) {
    // What the frame around the content takes (a border, the title bar).
    let room = ((area.0 - 2).max(min.0), (area.1 - 29).max(min.1));
    let grow = |base: i32, area: i32, reference: i32| {
        (i64::from(base) * i64::from(area.max(reference)) / i64::from(reference)) as i32
    };
    (
        grow(base.0, area.0, REFERENCE_AREA.0)
            .min(room.0)
            .max(min.0),
        grow(base.1, area.1, REFERENCE_AREA.1)
            .min(room.1)
            .max(min.1),
    )
}

/// How many columns and rows of `cell`-sized tiles fit in `room`, at least
/// `min` and at most `max` columns (rows at least one).
pub fn grid(room: (i32, i32), cell: (i32, i32), min: i32, max: i32) -> (i32, i32) {
    let columns = (room.0 / cell.0.max(1)).clamp(min, max);
    let rows = (room.1 / cell.1.max(1)).max(1);
    (columns, rows)
}

/// Places tiles `spans` (columns × rows each) in order on a grid
/// `columns` wide, each in the first place it fits, row by row, as macOS
/// lays out Control Center (ADR-0151). Returns each tile's (column, row)
/// and the rows used. A tile wider than the grid takes all of it.
pub fn pack(columns: i32, spans: &[(i32, i32)]) -> (alloc::vec::Vec<(i32, i32)>, i32) {
    let columns = columns.max(1);
    let mut taken: alloc::vec::Vec<alloc::vec::Vec<bool>> = alloc::vec::Vec::new();
    let mut places = alloc::vec::Vec::with_capacity(spans.len());
    let mut rows = 0;
    for &(w, h) in spans {
        let (w, h) = (w.clamp(1, columns), h.max(1));
        let free = |taken: &[alloc::vec::Vec<bool>], col: i32, row: i32| {
            (row..row + h).all(|r| {
                (col..col + w).all(|c| taken.get(r as usize).is_none_or(|line| !line[c as usize]))
            })
        };
        let mut row = 0;
        let place = loop {
            if let Some(col) = (0..=columns - w).find(|&col| free(&taken, col, row)) {
                break (col, row);
            }
            row += 1;
        };
        while taken.len() < (place.1 + h) as usize {
            taken.push(alloc::vec![false; columns as usize]);
        }
        for r in place.1..place.1 + h {
            for c in place.0..place.0 + w {
                taken[r as usize][c as usize] = true;
            }
        }
        rows = rows.max(place.1 + h);
        places.push(place);
    }
    (places, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_take_from_both_ends() {
        let mut bar = Bar::new(Rect::new(12, 11, 736, 30), 8);
        assert_eq!(bar.left(64), Rect::new(12, 11, 64, 30));
        assert_eq!(bar.right(100), Rect::new(648, 11, 100, 30));
        assert_eq!(bar.right(72), Rect::new(568, 11, 72, 30));
        assert_eq!(bar.room(), 568 - 8 - (12 + 64 + 8));
        assert_eq!(bar.rest(320), Rect::new(84, 11, 320, 30));
        assert!(bar.fits(400) && !bar.fits(500));
        // Taken past each other: no room, never negative.
        bar.left(1000);
        assert_eq!(bar.room(), 0);
    }

    #[test]
    fn flows_wrap() {
        let mut flow = Flow::new(Rect::new(0, 100, 250, 0), 30, 8);
        assert_eq!(flow.bottom(), 100);
        assert_eq!(flow.next(120), Rect::new(0, 100, 120, 30));
        assert_eq!(flow.next(120), Rect::new(128, 100, 120, 30));
        // No room left on the line: the next one.
        assert_eq!(flow.next(120), Rect::new(0, 138, 120, 30));
        assert_eq!(flow.bottom(), 168);
        // Wider than the area: as wide as it.
        assert_eq!(flow.next(999).w, 250);
        // Lines further apart than items.
        let mut flow = Flow::new(Rect::new(0, 0, 100, 0), 30, 6).line_gap(16);
        flow.next(60);
        assert_eq!(flow.next(60), Rect::new(0, 46, 60, 30));
    }

    #[test]
    fn windows_grow_with_the_room() {
        let settings = |area| window_size((760, 520), (600, 420), area);
        // QEMU: the size asked for.
        assert_eq!(settings((1280, 692)), (760, 520));
        // A 1080p panel at 125 % (1536 × 864, the area 756 high).
        assert_eq!(settings((1536, 756)), (912, 568));
        // 1080p at 100 %.
        assert_eq!(settings((1920, 972)), (1140, 730));
        // A small screen: no larger than the area, no smaller than min.
        assert_eq!(settings((700, 500)), (698, 471));
        assert_eq!(settings((500, 300)), (600, 420));
    }

    #[test]
    fn grids_fit_the_room() {
        assert_eq!(grid((880, 336), (120, 112), 3, 10), (7, 3));
        assert_eq!(grid((5000, 50), (120, 112), 3, 10), (10, 1));
        assert_eq!(grid((100, 500), (120, 112), 3, 10), (3, 4));
    }

    #[test]
    fn packs_first_fit() {
        // Control Center's defaults: two wide, a large beside them, then
        // small ones in the gap, then full-width sliders.
        let spans = [(2, 1), (2, 2), (2, 1), (2, 1), (1, 1), (1, 1), (4, 1)];
        let (places, rows) = pack(4, &spans);
        assert_eq!(
            places,
            [(0, 0), (2, 0), (0, 1), (0, 2), (2, 2), (3, 2), (0, 3)]
        );
        assert_eq!(rows, 4);
        // A small one fills a hole left before a wide one.
        let (places, rows) = pack(4, &[(3, 1), (2, 1), (1, 1)]);
        assert_eq!(places, [(0, 0), (0, 1), (3, 0)]);
        assert_eq!(rows, 2);
        // Too wide: the whole row.
        assert_eq!(pack(4, &[(9, 1)]), (alloc::vec![(0, 0)], 1));
        assert_eq!(pack(4, &[]), (alloc::vec![], 0));
    }
}
