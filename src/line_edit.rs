//! A small cursor-positioned text buffer: the editing vocabulary the TODO
//! panel's line editor established (readline-style word moves and kills,
//! whitespace-split word boundaries), reimplemented as a standalone leaf
//! for contexts outside the sidebar's `AppState` — today the overview's
//! inline prompt bar (src/cli/overview.rs), which types straight into a
//! live pane and needs real editing more than one-line Google-Tasks
//! semantics. Also owns the one job a bar on a fixed-width row needs: a
//! visible window that keeps the cursor on screen while the text grows
//! past the row.

/// Editable single-line buffer with a character-offset cursor.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineEdit {
    chars: Vec<char>,
    cursor: usize,
}

impl LineEdit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let cursor = chars.len();
        Self { chars, cursor }
    }

    /// Replace the whole buffer, parking the cursor at the end (send
    /// failure restore: the draft reappears ready to edit).
    pub fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// True when the buffer is empty or all-whitespace — a prompt of
    /// nothing says nothing.
    pub fn is_blank(&self) -> bool {
        self.chars.iter().all(|c| c.is_whitespace())
    }

    pub fn content(&self) -> String {
        self.chars.iter().collect()
    }

    /// Empty the buffer and hand back what was in it (Enter-to-send).
    pub fn take(&mut self) -> String {
        let text = self.content();
        self.chars.clear();
        self.cursor = 0;
        text
    }

    pub fn push(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Backspace: delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.chars.remove(self.cursor - 1);
            self.cursor -= 1;
        }
    }

    /// Delete: remove the character under the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Ctrl-Left: back to the start of the previous word, skipping the
    /// whitespace and then the word's chars before it. The same
    /// whitespace/word split pop_word and word_right use.
    pub fn cursor_word_left(&mut self) {
        let mut at = self.cursor.min(self.chars.len());
        while at > 0 && self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        self.cursor = at;
    }

    /// Ctrl-Right: forward past the end of the next word, skipping the
    /// whitespace and then the word's chars after the cursor.
    pub fn cursor_word_right(&mut self) {
        let len = self.chars.len();
        let mut at = self.cursor.min(len);
        while at < len && self.chars[at].is_whitespace() {
            at += 1;
        }
        while at < len && !self.chars[at].is_whitespace() {
            at += 1;
        }
        self.cursor = at;
    }

    /// Ctrl-w / Alt-Backspace: delete the word before the cursor —
    /// trailing whitespace first, then the word's chars.
    pub fn pop_word(&mut self) {
        let mut at = self.cursor.min(self.chars.len());
        while at > 0 && self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        self.chars.drain(at..self.cursor);
        self.cursor = at;
    }

    /// Ctrl-u: discard the whole line.
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// The character range to render so the cursor column is on screen,
    /// given `width` available columns. The cursor occupies a column of
    /// its own (the character under it, or a block past the last
    /// character), so a window always leaves room for it. Returns
    /// `(start, end)` indices into chars(); empty when nothing fits.
    pub fn visible_window(&self, width: usize) -> (usize, usize) {
        let len = self.chars.len();
        let cursor = self.cursor.min(len);
        if width == 0 {
            return (cursor, cursor);
        }
        // The block past the last character adds one column on top of
        // whatever text is shown.
        let extra = if cursor == len { 1 } else { 0 };
        let text_cols = width - extra;
        let start = cursor.saturating_sub(text_cols.saturating_sub(1));
        let end = (start + text_cols).min(len);
        // Near the end of the text, slide the window left so a short
        // tail still fills the row.
        if end == len {
            let start = len.saturating_sub(text_cols);
            return (start, len);
        }
        (start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(text: &str, cursor: usize) -> LineEdit {
        let mut e = LineEdit::from_text(text);
        e.cursor = cursor;
        e
    }

    #[test]
    fn insert_and_backspace_are_cursor_relative() {
        let mut e = edit("hllo", 1);
        e.push('e');
        assert_eq!(e.content(), "hello");
        assert_eq!(e.cursor(), 2);
        e.backspace();
        assert_eq!(e.content(), "hllo");
        assert_eq!(e.cursor(), 1);
        // Backspace at home is inert, not a negative index.
        e.home();
        e.backspace();
        assert_eq!(e.content(), "hllo");
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn delete_removes_under_the_cursor() {
        let mut e = edit("helloo", 5);
        e.delete();
        assert_eq!(e.content(), "hello");
        // Delete at end is inert.
        e.delete();
        assert_eq!(e.content(), "hello");
    }

    #[test]
    fn single_step_moves_clamp() {
        let mut e = edit("ab", 0);
        e.left();
        assert_eq!(e.cursor(), 0);
        e.right();
        e.right();
        e.right();
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn word_moves_use_the_todos_whitespace_split() {
        // "  fix  the bug  " with the cursor at the end.
        let mut e = edit("  fix  the bug  ", 16);
        e.cursor_word_left();
        assert_eq!(e.cursor(), 11, "skips trailing space, lands at 'bug' start");
        e.cursor_word_left();
        assert_eq!(e.cursor(), 7, "'the' start");
        e.cursor_word_right();
        assert_eq!(e.cursor(), 10, "past 'the', before its space");
        e.cursor_word_right();
        assert_eq!(e.cursor(), 14, "past the space and 'bug'");
        // Moves clamp at both ends.
        e.cursor_word_right();
        assert_eq!(e.cursor(), 16);
        e.home();
        e.cursor_word_left();
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn pop_word_deletes_whitespace_then_the_word() {
        let mut e = edit("run the tests", 13);
        e.pop_word();
        assert_eq!(e.content(), "run the ");
        e.pop_word();
        assert_eq!(e.content(), "run ");
        e.pop_word();
        assert_eq!(e.content(), "");
        assert_eq!(e.cursor(), 0);
        // Popping mid-line takes the word before the cursor and keeps
        // everything after it.
        let mut e = edit("abc def ghi", 8);
        e.pop_word();
        assert_eq!(e.content(), "abc ghi");
        assert_eq!(e.cursor(), 4);
    }

    #[test]
    fn take_empties_and_clear_resets() {
        let mut e = LineEdit::from_text("draft");
        assert_eq!(e.take(), "draft");
        assert!(e.is_empty());
        assert!(e.is_blank());
        e.push(' ');
        assert!(e.is_blank(), "whitespace-only is blank: nothing to send");
        e.clear();
        assert!(e.is_empty());
    }

    #[test]
    fn short_text_is_shown_whole() {
        let e = edit("hello", 5);
        assert_eq!(e.visible_window(10), (0, 5));
        let e = edit("", 0);
        assert_eq!(e.visible_window(10), (0, 0));
    }

    #[test]
    fn window_follows_the_cursor_at_the_right_edge() {
        let long = "x".repeat(100);
        let e = edit(&long, 100);
        // Cursor past the last char: 9 chars + block column = 10.
        assert_eq!(e.visible_window(10), (91, 100));
        // Cursor mid-text: the char under the cursor counts as its column.
        let e = edit(&long, 50);
        let (start, end) = e.visible_window(10);
        assert!(start <= 50 && 50 < end, "cursor inside the window");
        assert_eq!(end - start, 10);
        // Home keeps the head of the line.
        let e = edit(&long, 0);
        assert_eq!(e.visible_window(10), (0, 10));
    }

    #[test]
    fn window_degrades_at_width_zero() {
        let e = edit("hello", 3);
        let (start, end) = e.visible_window(0);
        assert_eq!(start, 3);
        assert_eq!(end, 3);
    }
}
