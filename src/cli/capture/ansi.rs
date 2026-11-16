use vte::{Params, Parser, Perform};

#[derive(Debug, Clone, PartialEq)]
pub struct StyledCell {
    pub ch: char,
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    pub bold: bool,
    pub reversed: bool,
    pub underlined: bool,
}

impl Default for StyledCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: None,
            bg: None,
            bold: false,
            reversed: false,
            underlined: false,
        }
    }
}

pub fn parse_ansi(bytes: &[u8], cols: u16, rows: u16) -> Vec<Vec<StyledCell>> {
    let mut grid: Vec<Vec<StyledCell>> = (0..rows)
        .map(|_| vec![StyledCell::default(); cols as usize])
        .collect();
    let mut state = State {
        grid: &mut grid,
        cols,
        rows,
        cursor_x: 0,
        cursor_y: 0,
        style: StyledCell::default(),
    };
    let mut parser = Parser::new();
    parser.advance(&mut state, bytes);
    grid
}

struct State<'a> {
    grid: &'a mut Vec<Vec<StyledCell>>,
    cols: u16,
    rows: u16,
    cursor_x: u16,
    cursor_y: u16,
    style: StyledCell,
}

impl Perform for State<'_> {
    fn print(&mut self, ch: char) {
        if self.cursor_y < self.rows && self.cursor_x < self.cols {
            let mut cell = self.style.clone();
            cell.ch = ch;
            self.grid[self.cursor_y as usize][self.cursor_x as usize] = cell;
            self.cursor_x += 1;
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => {
                if self.cursor_y + 1 < self.rows {
                    self.cursor_y += 1;
                }
                self.cursor_x = 0;
            }
            b'\r' => {
                self.cursor_x = 0;
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        if action != 'm' {
            return;
        }
        let mut iter = params.iter();
        while let Some(p) = iter.next() {
            // Bare `CSI m` (empty param) is equivalent to `CSI 0 m` per
            // ECMA-48 — treat a missing code as 0 (reset) instead of
            // skipping, otherwise the previous cell's fg/bg/bold/etc.
            // leaks into the next one.
            let code = p.first().copied().unwrap_or(0);
            match code {
                0 => {
                    self.style = StyledCell {
                        ch: self.style.ch,
                        ..Default::default()
                    };
                }
                1 => self.style.bold = true,
                4 => self.style.underlined = true,
                7 => self.style.reversed = true,
                22 => self.style.bold = false,
                24 => self.style.underlined = false,
                27 => self.style.reversed = false,
                39 => self.style.fg = None,
                49 => self.style.bg = None,
                30..=37 => self.style.fg = Some((code - 30) as u8),
                40..=47 => self.style.bg = Some((code - 40) as u8),
                90..=97 => self.style.fg = Some((code - 90 + 8) as u8),
                100..=107 => self.style.bg = Some((code - 100 + 8) as u8),
                38 => {
                    if p.len() >= 3 && p[1] == 5 {
                        self.style.fg = Some(p[2] as u8);
                    } else if let (Some(mode), Some(color)) = (iter.next(), iter.next())
                        && !mode.is_empty()
                        && mode[0] == 5
                        && !color.is_empty()
                    {
                        self.style.fg = Some(color[0] as u8);
                    }
                }
                48 => {
                    if p.len() >= 3 && p[1] == 5 {
                        self.style.bg = Some(p[2] as u8);
                    } else if let (Some(mode), Some(color)) = (iter.next(), iter.next())
                        && !mode.is_empty()
                        && mode[0] == 5
                        && !color.is_empty()
                    {
                        self.style.bg = Some(color[0] as u8);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_extended_fg_color() {
        let grid = parse_ansi(b"\x1b[38;5;114mgreen", 8, 1);
        assert_eq!(grid[0][0].fg, Some(114));
    }

    #[test]
    fn parses_basic_and_bright_sgr_colors() {
        let grid = parse_ansi(
            b"\x1b[31mR\x1b[0m\x1b[94mB\x1b[0m\x1b[43mY\x1b[0m\x1b[104mZ",
            8,
            1,
        );
        assert_eq!(grid[0][0].fg, Some(1));
        assert_eq!(grid[0][1].fg, Some(12));
        assert_eq!(grid[0][2].bg, Some(3));
        assert_eq!(grid[0][3].bg, Some(12));
    }

    #[test]
    fn parses_bg_and_attributes() {
        let grid = parse_ansi(b"\x1b[44;1;7mA", 4, 1);
        assert_eq!(grid[0][0].bg, Some(4));
        assert!(grid[0][0].bold);
        assert!(grid[0][0].reversed);
    }

    #[test]
    fn reset_clears_style_between_cells() {
        let grid = parse_ansi(b"\x1b[31mA\x1b[0mB", 4, 1);
        assert_eq!(grid[0][0].fg, Some(1));
        assert_eq!(grid[0][1].fg, None);
    }
}
