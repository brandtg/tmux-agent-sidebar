use vte::{Params, Parser, Perform};

#[derive(Debug, Clone, PartialEq)]
pub struct StyledCell {
    pub ch: char,
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    pub bold: bool,
    pub reversed: bool,
    pub underlined: bool,
    /// SGR 2 (faint). Reset by 22, which also clears `bold` — normal
    /// intensity is one code for both.
    pub faint: bool,
    /// SGR 3 (italic), reset by 23.
    pub italic: bool,
    /// SGR 9 (strike), reset by 29.
    pub strike: bool,
    /// SGR 53 (overline), reset by 55. The overview grid has no
    /// ratatui modifier for it, so only the HTML renderer shows it.
    pub overline: bool,
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
            faint: false,
            italic: false,
            strike: false,
            overline: false,
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

/// Nearest xterm-256 palette entry for an RGB triple (squared-distance
/// search over the palette decomposition). Truecolor agent themes
/// (opencode emits nothing else) quantize cleanly: #e0def4 lands on the
/// light grayscale ramp, #1f1d2e on a dark cube corner.
pub(crate) fn nearest_indexed(r: u16, g: u16, b: u16) -> u8 {
    let (r, g, b) = (r.min(255) as u32, g.min(255) as u32, b.min(255) as u32);
    let mut best_index = 0u8;
    let mut best_dist = u32::MAX;
    for index in 0..=255u8 {
        let (pr, pg, pb) = palette_rgb(index);
        let dr = pr.abs_diff(r);
        let dg = pg.abs_diff(g);
        let db = pb.abs_diff(b);
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best_index = index;
        }
    }
    best_index
}

/// RGB decomposition of an xterm-256 palette index: the 16 standard
/// colors, the 6x6x6 cube (16-231), and the grayscale ramp (232-255).
pub(crate) fn palette_rgb(index: u8) -> (u32, u32, u32) {
    match index {
        0..=15 => {
            let (r, g, b) = STANDARD_16[index as usize];
            (r as u32, g as u32, b as u32)
        }
        16..=231 => {
            let v = index as u32 - 16;
            let level = |c: u32| (c * 255) / 5;
            (level(v / 36), level((v / 6) % 6), level(v % 6))
        }
        _ => {
            let v = 8 + 10 * (index as u32 - 232);
            (v, v, v)
        }
    }
}

/// The canonical 16 ANSI colors (VGA values; terminals vary slightly, but
/// luminance verdicts and nearest-color searches are stable).
const STANDARD_16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

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
        use unicode_width::UnicodeWidthChar;
        if self.cursor_y >= self.rows {
            return;
        }
        // Terminal columns, not chars: CJK/emoji occupy two cells in the
        // source pane, so every following char must land two cells later
        // or the whole row renders progressively shifted left (audit
        // 2026-11-25 M-UI-2).
        let width = UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if width == 0 {
            // Zero-width (combining marks): the cluster belongs to the
            // previous cell, which stays untouched, and the cursor does
            // not move. [`StyledCell`] holds one char, so the mark
            // itself is dropped — alignment (every following char in
            // its true column) is what the grid needs.
            return;
        }
        if self.cursor_x + width <= self.cols {
            let mut cell = self.style.clone();
            cell.ch = ch;
            self.grid[self.cursor_y as usize][self.cursor_x as usize] = cell;
            if width == 2 {
                // Trailing-half placeholder: a real terminal paints the
                // wide glyph across both columns, so the placeholder
                // carries the same style (background included) and a
                // blank glyph.
                let mut cont = self.style.clone();
                cont.ch = ' ';
                self.grid[self.cursor_y as usize][self.cursor_x as usize + 1] = cont;
            }
        }
        // Park the cursor on the right edge when a wide char would
        // straddle it (the source terminal wraps instead of splitting).
        self.cursor_x = (self.cursor_x + width).min(self.cols);
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
                2 => self.style.faint = true,
                3 => self.style.italic = true,
                4 => self.style.underlined = true,
                7 => self.style.reversed = true,
                9 => self.style.strike = true,
                22 => {
                    // Normal intensity clears both bold (1) and faint (2).
                    self.style.bold = false;
                    self.style.faint = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underlined = false,
                27 => self.style.reversed = false,
                29 => self.style.strike = false,
                53 => self.style.overline = true,
                55 => self.style.overline = false,
                39 => self.style.fg = None,
                49 => self.style.bg = None,
                30..=37 => self.style.fg = Some((code - 30) as u8),
                40..=47 => self.style.bg = Some((code - 40) as u8),
                90..=97 => self.style.fg = Some((code - 90 + 8) as u8),
                100..=107 => self.style.bg = Some((code - 100 + 8) as u8),
                38 => {
                    if p.len() >= 3 && p[1] == 5 {
                        self.style.fg = Some(p[2] as u8);
                    } else if let (Some(mode), Some(r)) = (iter.next(), iter.next())
                        && !mode.is_empty()
                    {
                        match mode[0] {
                            // 38;5;N — indexed.
                            5 => self.style.fg = Some(r[0] as u8),
                            // 38;2;R;G;B — truecolor. Consume ALL remaining
                            // components: leaving G and B would let them be
                            // re-parsed as SGR codes (an RGB component of
                            // 106 turned into a solid background-14 bar).
                            // Quantize to the 256-color palette.
                            2 => {
                                if let (Some(g), Some(b)) = (iter.next(), iter.next())
                                    && !r.is_empty()
                                    && !g.is_empty()
                                    && !b.is_empty()
                                {
                                    self.style.fg = Some(nearest_indexed(r[0], g[0], b[0]));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                48 => {
                    if p.len() >= 3 && p[1] == 5 {
                        self.style.bg = Some(p[2] as u8);
                    } else if let (Some(mode), Some(r)) = (iter.next(), iter.next())
                        && !mode.is_empty()
                    {
                        match mode[0] {
                            5 => self.style.bg = Some(r[0] as u8),
                            2 => {
                                if let (Some(g), Some(b)) = (iter.next(), iter.next())
                                    && !r.is_empty()
                                    && !g.is_empty()
                                    && !b.is_empty()
                                {
                                    self.style.bg = Some(nearest_indexed(r[0], g[0], b[0]));
                                }
                            }
                            _ => {}
                        }
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

    #[test]
    fn truecolor_quantizes_to_the_256_palette() {
        let grid = parse_ansi(b"\x1b[38;2;224;222;244mX\x1b[48;2;31;29;46mY", 4, 1);
        // Lavender #e0def4 lands on the light grayscale ramp.
        assert_eq!(grid[0][0].fg, Some(254));
        // Dark navy #1f1d2e lands on the dark grayscale ramp.
        assert_eq!(grid[0][1].bg, Some(235));
    }

    #[test]
    fn truecolor_components_never_leak_as_sgr_codes() {
        // The regression that painted solid bars behind opencode's muted
        // text: 38;2;110;106;134 left its G component (106) in the param
        // stream, where it re-parsed as SGR 106 — background 14.
        let grid = parse_ansi(b"\x1b[38;2;110;106;134mX\x1b[48;2;38;35;58mY", 4, 1);
        assert_eq!(grid[0][0].bg, None);
        assert_eq!(grid[0][0].fg, Some(nearest_indexed(110, 106, 134)));
        // The background truecolor's R component (38) must not become
        // fg 5 (magenta) on the next cell; fg simply carries over.
        assert_eq!(grid[0][1].fg, Some(nearest_indexed(110, 106, 134)));
        assert_eq!(grid[0][1].bg, Some(nearest_indexed(38, 35, 58)));
    }

    #[test]
    fn truncated_truecolor_leaves_style_untouched() {
        let grid = parse_ansi(b"\x1b[38;2;99mX", 4, 1);
        assert_eq!(grid[0][0].fg, None);
        assert_eq!(grid[0][0].bg, None);
    }

    #[test]
    fn wide_chars_advance_two_cells_with_a_styled_placeholder() {
        // Audit 2026-11-25 M-UI-2: the source pane renders 日 over two
        // columns; the grid must place the next char two cells later, and
        // the trailing half must carry the same style (a background color
        // spans both halves on a real terminal).
        let grid = parse_ansi("\u{1b}[41m日a".as_bytes(), 4, 1);
        assert_eq!(grid[0][0].ch, '日');
        assert_eq!(grid[0][1].ch, ' ');
        assert_eq!(grid[0][1].bg, Some(1), "placeholder carries the style");
        assert_eq!(grid[0][2].ch, 'a');
    }

    #[test]
    fn zero_width_chars_do_not_consume_a_cell() {
        // A combining mark belongs to the previous cell: it neither
        // advances the cursor nor overwrites the next column.
        let grid = parse_ansi("e\u{301}f".as_bytes(), 4, 1);
        assert_eq!(grid[0][0].ch, 'e');
        assert_eq!(grid[0][1].ch, 'f');
    }

    #[test]
    fn a_wide_char_at_the_last_column_wraps_instead_of_splitting() {
        // 3-col grid: the wide char needs columns 2-3, only 3 exists —
        // the source terminal parks on the edge; nothing is written.
        let grid = parse_ansi("ab日".as_bytes(), 3, 1);
        assert_eq!(grid[0][2].ch, ' ', "no split glyph at the edge");
    }

    #[test]
    fn parses_faint_italic_strike_and_overline() {
        let grid = parse_ansi(b"\x1b[2;3;9;53mX\x1b[22;23;29;55mY", 4, 1);
        assert!(grid[0][0].faint);
        assert!(grid[0][0].italic);
        assert!(grid[0][0].strike);
        assert!(grid[0][0].overline);
        assert!(!grid[0][1].faint);
        assert!(!grid[0][1].italic);
        assert!(!grid[0][1].strike);
        assert!(!grid[0][1].overline);
    }

    #[test]
    fn normal_intensity_clears_bold_and_faint() {
        // SGR 22 is "normal intensity": one code resets both 1 (bold)
        // and 2 (faint).
        let grid = parse_ansi(b"\x1b[1;2mX\x1b[22mY", 4, 1);
        assert!(grid[0][0].bold);
        assert!(grid[0][0].faint);
        assert!(!grid[0][1].bold);
        assert!(!grid[0][1].faint);
    }
}
