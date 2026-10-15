//! Bounded, renderer-neutral fixed-cell surfaces for interactive plugins.
use serde::{Deserialize, Serialize};
pub const MAX_COLS: u16 = 200;
pub const MAX_ROWS: u16 = 60;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
    pub cols: u16,
    pub rows: u16,
    pub background: [u8; 3],
    /// Sparse row-major cells. Continuations of wide graphemes are omitted.
    pub cells: Vec<Cell>,
    pub styles: Vec<Style>,
    pub cursor: Option<[u16; 2]>,
    pub status: String,
}
/// Row, column, grapheme text, occupied columns (1 or 2), style index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell(pub u16, pub u16, pub String, pub u8, pub u16);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    /// Bold, italic, underline, strike, faint, overline.
    pub flags: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Input {
    Resize {
        cols: u16,
        rows: u16,
    },
    Text {
        text: String,
        paste: bool,
    },
    Key {
        key: String,
        control: bool,
        alt: bool,
        shift: bool,
    },
    Scroll {
        lines: i32,
    },
}
impl Input {
    pub fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::Resize { cols, rows } => {
                *cols > 0 && *cols <= MAX_COLS && *rows > 0 && *rows <= MAX_ROWS
            }
            Self::Text { text, .. } => text.len() <= 64 * 1024,
            Self::Key { key, .. } => {
                !key.is_empty() && key.len() <= 64 && !key.chars().any(char::is_control)
            }
            Self::Scroll { lines } => lines.unsigned_abs() <= 10_000,
        };
        if valid {
            Ok(())
        } else {
            Err("invalid grid input".into())
        }
    }
}
impl Grid {
    /// Plain text for accessibility/export, preserving gaps in sparse rows.
    pub fn plain_text(&self) -> String {
        let mut rows = vec![String::new(); usize::from(self.rows.min(MAX_ROWS))];
        let mut columns = vec![0; rows.len()];
        for Cell(row, col, text, span, _) in &self.cells {
            let y = usize::from(*row);
            if y >= rows.len() || *col >= MAX_COLS {
                continue;
            }
            while columns[y] < *col {
                rows[y].push(' ');
                columns[y] += 1;
            }
            rows[y].push_str(if text.is_empty() { " " } else { text });
            columns[y] = col.saturating_add(u16::from(*span));
        }
        rows.join("\n").trim_end().to_string()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.cols == 0
            || self.cols > MAX_COLS
            || self.rows == 0
            || self.rows > MAX_ROWS
            || self.styles.len() > 4096
            || self.status.len() > 256
            || self.cells.len() > usize::from(self.cols) * usize::from(self.rows)
            || self
                .cursor
                .is_some_and(|[x, y]| x >= self.cols || y >= self.rows)
        {
            return Err("invalid grid dimensions or bounds".into());
        }
        let mut previous = None;
        for Cell(row, col, text, width, style) in &self.cells {
            if *row >= self.rows
                || *col >= self.cols
                || !matches!(*width, 1 | 2)
                || *col + u16::from(*width) > self.cols
                || usize::from(*style) >= self.styles.len()
                || text.len() > 128
                || text.chars().any(char::is_control)
                || previous.is_some_and(|(r, c)| *row < r || (*row == r && *col < c))
            {
                return Err("invalid or overlapping grid cell".into());
            }
            previous = Some((*row, *col + u16::from(*width)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_overlapping_and_out_of_bounds_cells_and_excessive_input() {
        let mut grid = Grid {
            cols: 80,
            rows: 24,
            background: [0; 3],
            styles: vec![Style {
                foreground: [255; 3],
                background: [0; 3],
                flags: 0,
            }],
            cells: vec![Cell(0, 0, "世".into(), 2, 0), Cell(0, 2, "!".into(), 1, 0)],
            cursor: Some([2, 0]),
            status: String::new(),
        };
        grid.validate().unwrap();
        let bytes = crate::encode(&grid).unwrap();
        assert_eq!(crate::decode::<Grid>(&bytes).unwrap(), grid);
        grid.cells[1].1 = 1;
        assert!(grid.validate().is_err());
        grid.cells[1].1 = 80;
        assert!(grid.validate().is_err());
        grid.cells[1].1 = 2;
        grid.cells[1].4 = 1;
        assert!(grid.validate().is_err());
        for input in [
            Input::Resize { cols: 0, rows: 24 },
            Input::Scroll { lines: i32::MIN },
            Input::Text {
                text: "x".repeat(65537),
                paste: true,
            },
            Input::Key {
                key: "\n".into(),
                control: false,
                alt: false,
                shift: false,
            },
        ] {
            assert!(input.validate().is_err());
        }
    }
}
