//! Screen geometry for click targets, in terminal cells: the two types [`crate::app`] keeps its
//! hit boxes in. Same field names and `contains` rule as ratatui's, which the TUI converts from.

/// A cell on screen, counted from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Position {
    pub x: u16,
    pub y: u16,
}

impl Position {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

/// A rectangle of cells: `x`/`y` is its top-left cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The first column right of the rectangle.
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// The first row below the rectangle.
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// Whether the cell lies inside: the right and bottom edges are exclusive.
    pub const fn contains(self, at: Position) -> bool {
        at.x >= self.x && at.x < self.right() && at.y >= self.y && at.y < self.bottom()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_excludes_the_right_and_bottom_edges() {
        let r = Rect::new(2, 3, 4, 2);
        assert!(r.contains(Position::new(2, 3)));
        assert!(r.contains(Position::new(5, 4)));
        assert!(!r.contains(Position::new(6, 4)));
        assert!(!r.contains(Position::new(5, 5)));
        assert!(!r.contains(Position::new(1, 3)));
        assert!(!Rect::new(0, 0, 0, 0).contains(Position::new(0, 0)));
    }

    #[test]
    fn edges_saturate_at_the_end_of_the_range() {
        let r = Rect::new(u16::MAX - 1, 0, 10, 1);
        assert_eq!(r.right(), u16::MAX);
    }
}
