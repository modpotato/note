//! Platform-independent editor state and commands. See DESIGN.md (editor.rs).
//!
//! All positions are byte offsets into the buffer and are kept on UTF-8 char boundaries.
//! Every mutation goes through `splice`/`replace_compound`, which set the undo cursor hint
//! and feed the syntax highlighter's dirty line.

use std::borrow::Cow;

use crate::buffer::Buffer;
use crate::config::Config;
use crate::encoding::{self, Encoding, LineEnding};
use crate::simd;
use crate::syntax::{Highlighter, Lang};

/// Substring search function (case-sensitive or ASCII case-insensitive).
type Finder = fn(&[u8], &[u8]) -> Option<usize>;

pub struct Editor {
    pub buf: Buffer,
    pub cursor: usize,
    /// Selection anchor; selection = [min(anchor, cursor), max).
    pub anchor: Option<usize>,
    /// Mark mode (Alt+A): every movement extends the selection.
    pub mark_mode: bool,
    /// Visual column remembered across up/down moves.
    pub preferred_col: Option<usize>,
    pub scroll_line: usize,
    pub scroll_col: usize,
    /// Soft wrap (visual only; the buffer is unchanged). In wrap mode `scroll_line` and
    /// `scroll_subrow` address visual rows, and `scroll_col` stays 0.
    pub wrap: bool,
    /// Text columns available to the wrapper, set by the window. Wrapping uses at least 8.
    pub wrap_cols: usize,
    /// Index of the first visual row shown, inside the logical line `scroll_line`.
    pub scroll_subrow: usize,
    /// Set by End on a wrapped row. The cursor then belongs to the end of the row above,
    /// although its byte offset is also the start of the next row.
    end_affinity: bool,
    pub cut_buffer: Vec<u8>,
    /// Consecutive ^K calls append to `cut_buffer`.
    pub last_was_cut_line: bool,
    pub path: Option<String>,
    pub encoding: Encoding,
    pub line_ending: LineEnding,
    pub search: Vec<u8>,
    pub replace_with: Vec<u8>,
    /// Default false (case-insensitive, like nano).
    pub search_case_sensitive: bool,
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub auto_indent: bool,
    pub highlighter: Highlighter,
    /// Transient status-bar message.
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    /// Smart home: first non-whitespace, then column 0, toggling.
    LineHome,
    LineEnd,
    WordLeft,
    WordRight,
    PageUp(usize),
    PageDown(usize),
    DocStart,
    DocEnd,
}

impl Editor {
    pub fn new(cfg: &Config) -> Self {
        Editor {
            buf: Buffer::new(),
            cursor: 0,
            anchor: None,
            mark_mode: false,
            preferred_col: None,
            scroll_line: 0,
            scroll_col: 0,
            wrap: cfg.word_wrap,
            wrap_cols: 80,
            scroll_subrow: 0,
            end_affinity: false,
            cut_buffer: Vec::new(),
            last_was_cut_line: false,
            path: None,
            encoding: Encoding::Utf8,
            line_ending: LineEnding::CrLf,
            search: Vec::new(),
            replace_with: Vec::new(),
            search_case_sensitive: false,
            tab_width: cfg.tab_width,
            insert_spaces: cfg.insert_spaces,
            auto_indent: cfg.auto_indent,
            highlighter: Highlighter::new(Lang::Plain),
            message: None,
        }
    }

    /// Replaces the document with the decoded `bytes`, sets the language from `path`
    /// and resets cursor, selection and scroll. The cut buffer and search text are kept.
    pub fn load(&mut self, path: &str, bytes: &[u8]) {
        self.load_owned(path, bytes.to_vec());
    }

    /// Same as `load`, but takes the file bytes by value so a large file is not copied
    /// again while decoding.
    pub fn load_owned(&mut self, path: &str, bytes: Vec<u8>) {
        let d = encoding::decode_owned(bytes);
        self.buf = Buffer::from_text(d.text);
        self.buf.mark_clean();
        self.encoding = d.encoding;
        self.line_ending = d.line_ending;
        self.path = Some(path.to_string());
        self.highlighter.set_lang(Lang::from_path(path));
        self.cursor = 0;
        self.anchor = None;
        self.mark_mode = false;
        self.preferred_col = None;
        self.scroll_line = 0;
        self.scroll_col = 0;
        self.scroll_subrow = 0;
        self.end_affinity = false;
        self.last_was_cut_line = false;
        self.message = None;
    }

    /// Encodes the document with the current encoding and line ending. The document stays
    /// dirty until the caller has actually written the bytes and calls `mark_saved`.
    pub fn save_bytes(&mut self) -> Vec<u8> {
        encoding::encode(self.buf.make_contiguous(), self.encoding, self.line_ending)
    }

    /// Records that the current text has been written to disk.
    pub fn mark_saved(&mut self) {
        self.buf.mark_clean();
    }

    /// `*` prefix when dirty, then the file name (no directories), or "Untitled", then " - note".
    pub fn title(&self) -> String {
        let name = match &self.path {
            Some(p) => p.rsplit(['\\', '/']).next().unwrap_or(p.as_str()),
            None => "Untitled",
        };
        let star = if self.buf.is_dirty() { "*" } else { "" };
        format!("{star}{name} - note")
    }

    /// True if the document differs from the last save/load.
    pub fn is_dirty(&self) -> bool {
        self.buf.is_dirty()
    }

    // ----- selection and cursor -------------------------------------------

    pub fn selection(&self) -> Option<(usize, usize)> {
        match self.anchor {
            Some(a) if a != self.cursor => Some((a.min(self.cursor), a.max(self.cursor))),
            _ => None,
        }
    }

    pub fn selected_text(&self) -> Option<Vec<u8>> {
        let (s, e) = self.selection()?;
        Some(self.buf.slice(s, e))
    }

    /// Moves the cursor to `pos` (clamped, char-aligned). `extend` keeps or starts a selection.
    pub fn set_cursor(&mut self, pos: usize, extend: bool) {
        self.place(pos, extend);
        self.preferred_col = None;
    }

    /// Movement primitive: updates the cursor and anchor, and starts a new undo group.
    fn place(&mut self, pos: usize, extend: bool) {
        let pos = align_back(&self.buf, pos);
        self.end_affinity = false;
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.buf.break_undo_group();
    }

    pub fn select_all(&mut self) {
        let len = self.buf.len();
        self.buf.break_undo_group();
        self.anchor = Some(0);
        self.cursor = len;
        self.preferred_col = None;
        self.end_affinity = false;
    }

    pub fn move_cursor(&mut self, m: Move, extend: bool) {
        let extend = extend || self.mark_mode;
        let cur = self.cursor;
        let sel = self.selection();
        let keeps_col = matches!(
            m,
            Move::Up | Move::Down | Move::PageUp(_) | Move::PageDown(_)
        );
        let mut end_affinity = false;
        let target = match m {
            Move::Left => match sel {
                Some((s, _)) if !extend => s,
                _ => prev_char(&self.buf, cur),
            },
            Move::Right => match sel {
                Some((_, e)) if !extend => e,
                _ => next_char(&self.buf, cur),
            },
            Move::LineHome if self.wrap => self.wrap_home(cur),
            Move::LineEnd if self.wrap => {
                let (pos, at_row_end) = self.wrap_line_end(cur);
                end_affinity = at_row_end;
                pos
            }
            Move::LineHome => self.smart_home(self.buf.line_of(cur), cur),
            Move::LineEnd => self.buf.line_end(self.buf.line_of(cur)),
            Move::WordLeft => word_left(&self.buf, cur),
            Move::WordRight => word_right(&self.buf, cur),
            Move::DocStart => 0,
            Move::DocEnd => self.buf.len(),
            Move::Up | Move::Down | Move::PageUp(_) | Move::PageDown(_) if self.wrap => {
                let (down, n) = match m {
                    Move::Up => (false, 1),
                    Move::Down => (true, 1),
                    Move::PageUp(r) => (false, r.clamp(1, 1 << 30)),
                    Move::PageDown(r) => (true, r.clamp(1, 1 << 30)),
                    _ => unreachable!("guarded above"),
                };
                self.wrap_vertical(cur, down, n)
            }
            Move::Up | Move::Down | Move::PageUp(_) | Move::PageDown(_) => {
                let step: isize = match m {
                    Move::Up => -1,
                    Move::Down => 1,
                    Move::PageUp(r) => -(r.clamp(1, 1 << 30) as isize),
                    Move::PageDown(r) => r.clamp(1, 1 << 30) as isize,
                    _ => 0,
                };
                let line = self.buf.line_of(cur);
                let last = self.buf.line_count() - 1;
                let col = self.preferred_col.unwrap_or_else(|| self.visual_col(cur));
                self.preferred_col = Some(col);
                let want = (line as isize + step).clamp(0, last as isize) as usize;
                if want == line && step != 0 {
                    // Already at the top/bottom: go to the document edge in that direction.
                    if step < 0 {
                        0
                    } else {
                        self.buf.len()
                    }
                } else {
                    self.pos_from_visual(want, col)
                }
            }
        };
        self.place(target, extend);
        self.end_affinity = end_affinity;
        if !keeps_col {
            self.preferred_col = None;
        }
    }

    /// Smart home for logical line `line`: first non-blank, then column 0, toggling.
    fn smart_home(&self, line: usize, cur: usize) -> usize {
        let ls = self.buf.line_start(line);
        let le = self.buf.line_end(line);
        let mut fnw = ls;
        while fnw < le && matches!(self.buf.byte_at(fnw), b' ' | b'\t') {
            fnw += 1;
        }
        if cur == fnw && fnw != ls {
            ls
        } else {
            fnw
        }
    }

    // ----- editing primitives ---------------------------------------------

    /// Replaces [start, end) with `text` as one undo step (or a plain record when only one
    /// side is non-empty). Clears the anchor and the preferred column; the caller sets the cursor.
    fn splice(&mut self, start: usize, end: usize, text: &[u8]) {
        if start >= end && text.is_empty() {
            return;
        }
        self.buf.set_cursor_hint(self.cursor);
        if start < end && !text.is_empty() {
            self.buf.begin_compound();
            self.buf.delete(start, end);
            self.buf.insert(start, text);
            self.buf.end_compound();
        } else if start < end {
            self.buf.delete(start, end);
        } else {
            self.buf.insert(start, text);
        }
        self.after_edit();
    }

    /// Like `splice`, but always one compound undo step (used for paste and replace).
    fn replace_compound(&mut self, start: usize, end: usize, text: &[u8]) {
        if start >= end && text.is_empty() {
            return;
        }
        self.buf.set_cursor_hint(self.cursor);
        self.buf.begin_compound();
        if start < end {
            self.buf.delete(start, end);
        }
        if !text.is_empty() {
            self.buf.insert(start, text);
        }
        self.buf.end_compound();
        self.after_edit();
    }

    fn delete_range(&mut self, start: usize, end: usize) {
        self.splice(start, end, &[]);
        self.cursor = start;
        self.anchor = None;
        self.preferred_col = None;
    }

    /// Common bookkeeping after any mutation: highlighter invalidation and clamping.
    fn after_edit(&mut self) {
        self.anchor = None;
        self.preferred_col = None;
        self.end_affinity = false;
        let line = self.buf.take_first_dirty_line();
        self.highlighter.invalidate_from(line);
        let len = self.buf.len();
        self.cursor = align_back(&self.buf, self.cursor.min(len));
    }

    /// Rewrites whole lines first..=last. `f` appends each line's new content (no newline)
    /// to `out`. The region is replaced in one undo step; the cursor and anchor are mapped.
    fn rewrite_lines(&mut self, first: usize, last: usize, mut f: impl FnMut(&[u8], &mut Vec<u8>)) {
        let rs = self.buf.line_start(first);
        let re = self.buf.line_end(last);
        let old = self.buf.slice(rs, re);
        let mut out = Vec::with_capacity(old.len() + 16);
        let mut starts = Vec::new();
        let mut deltas = Vec::new();
        let mut off = 0usize;
        for (k, line) in old.split(|&b| b == b'\n').enumerate() {
            if k > 0 {
                out.push(b'\n');
            }
            starts.push(rs + off);
            let before = out.len();
            f(line, &mut out);
            deltas.push(out.len() as isize - before as isize - line.len() as isize);
            off += line.len() + 1;
        }
        if out == old {
            return;
        }
        let total: isize = deltas.iter().sum();
        let cursor = map_pos(self.cursor, rs, re, &starts, &deltas, total);
        let anchor = self
            .anchor
            .map(|a| map_pos(a, rs, re, &starts, &deltas, total));
        self.splice(rs, re, &out);
        self.cursor = cursor;
        self.anchor = anchor;
    }

    /// First and last line touched by a line command: the selection's lines (a selection
    /// ending at column 0 does not include its last line), or the cursor line.
    fn line_range(&self) -> (usize, usize) {
        match self.selection() {
            Some((s, e)) => {
                let first = self.buf.line_of(s);
                let mut last = self.buf.line_of(e);
                if last > first && e == self.buf.line_start(last) {
                    last -= 1;
                }
                (first, last)
            }
            None => {
                let l = self.buf.line_of(self.cursor);
                (l, l)
            }
        }
    }

    fn tw(&self) -> usize {
        self.tab_width.max(1)
    }

    // ----- text input and deletion ----------------------------------------

    /// Typed character. Replaces the selection. '\n' copies the current line's indentation
    /// when auto_indent is on. '\t' inserts spaces up to the next tab stop when insert_spaces is on.
    pub fn insert_char(&mut self, ch: char) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let mut tmp = [0u8; 4];
        let text: Cow<[u8]> = match ch {
            '\n' => {
                let mut v = vec![b'\n'];
                if self.auto_indent {
                    let ls = self.buf.line_start(self.buf.line_of(start));
                    let mut i = ls;
                    while i < start && matches!(self.buf.byte_at(i), b' ' | b'\t') {
                        i += 1;
                    }
                    v.extend((ls..i).map(|k| self.buf.byte_at(k)));
                }
                Cow::Owned(v)
            }
            '\t' if self.insert_spaces => {
                let tw = self.tw();
                let n = tw - self.visual_col(start) % tw;
                Cow::Owned(vec![b' '; n])
            }
            _ => Cow::Borrowed(ch.encode_utf8(&mut tmp).as_bytes()),
        };
        self.splice(start, end, &text);
        self.cursor = start + text.len();
    }

    /// Paste. Replaces the selection as one undo step. CRLF is normalised to LF.
    pub fn insert_text(&mut self, s: &[u8]) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        let text: Cow<[u8]> = if simd::find_byte(s, b'\r').is_some() {
            Cow::Owned(simd::crlf_to_lf(s))
        } else {
            Cow::Borrowed(s)
        };
        self.replace_compound(start, end, &text);
        self.cursor = start + text.len();
    }

    pub fn backspace(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.delete_range(s, e);
            return;
        }
        let cur = self.cursor;
        if cur == 0 {
            return;
        }
        let mut start = prev_char(&self.buf, cur);
        if self.insert_spaces {
            let ls = self.buf.line_start(self.buf.line_of(cur));
            if cur > ls && (ls..cur).all(|p| self.buf.byte_at(p) == b' ') {
                // Pure leading spaces: back to the previous tab stop.
                let tw = self.tw();
                let col = cur - ls;
                start = ls + (col - 1) / tw * tw;
            }
        }
        self.delete_range(start, cur);
    }

    pub fn delete_forward(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.delete_range(s, e);
            return;
        }
        let cur = self.cursor;
        let end = next_char(&self.buf, cur);
        if end > cur {
            self.delete_range(cur, end);
        }
    }

    pub fn delete_word_left(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.delete_range(s, e);
            return;
        }
        let cur = self.cursor;
        let start = word_left(&self.buf, cur);
        if start < cur {
            self.delete_range(start, cur);
        }
    }

    pub fn delete_word_right(&mut self) {
        if let Some((s, e)) = self.selection() {
            self.delete_range(s, e);
            return;
        }
        let cur = self.cursor;
        let end = word_right(&self.buf, cur);
        if end > cur {
            self.delete_range(cur, end);
        }
    }

    /// nano ^K. Without a selection cuts the whole current line including its '\n'
    /// (on the last line, which has no '\n', it cuts the contents and appends '\n' to the
    /// clipboard text so a paste restores the line). With a selection cuts the selection.
    /// Consecutive calls append to `cut_buffer`. Returns the cut buffer for the clipboard.
    pub fn cut_line(&mut self) -> Vec<u8> {
        self.buf.begin_compound();
        let chunk: Vec<u8> = if let Some((s, e)) = self.selection() {
            let t = self.buf.slice(s, e);
            self.delete_range(s, e);
            t
        } else {
            let line = self.buf.line_of(self.cursor);
            let ls = self.buf.line_start(line);
            let le = self.buf.line_end(line);
            if line + 1 < self.buf.line_count() {
                let t = self.buf.slice(ls, le + 1);
                self.delete_range(ls, le + 1);
                t
            } else {
                let mut t = self.buf.slice(ls, le);
                t.push(b'\n');
                self.delete_range(ls, le);
                t
            }
        };
        self.buf.end_compound();
        if self.last_was_cut_line {
            self.cut_buffer.extend_from_slice(&chunk);
        } else {
            self.cut_buffer = chunk;
        }
        self.last_was_cut_line = true;
        self.cut_buffer.clone()
    }

    /// Cuts the selection and returns its text. The nano cut buffer is not touched.
    pub fn cut_selection(&mut self) -> Option<Vec<u8>> {
        let (s, e) = self.selection()?;
        let t = self.buf.slice(s, e);
        self.delete_range(s, e);
        Some(t)
    }

    pub fn copy_selection(&self) -> Option<Vec<u8>> {
        self.selected_text()
    }

    /// Called for every action other than cut_line.
    pub fn reset_cut_chain(&mut self) {
        self.last_was_cut_line = false;
    }

    // ----- line commands ---------------------------------------------------

    /// Indents the selected lines (or the cursor line) by one unit: a tab, or tab_width spaces.
    pub fn indent(&mut self) {
        let (first, last) = self.line_range();
        let unit: Vec<u8> = if self.insert_spaces {
            vec![b' '; self.tw()]
        } else {
            vec![b'\t']
        };
        self.buf.begin_compound();
        self.rewrite_lines(first, last, |line, out| {
            out.extend_from_slice(&unit);
            out.extend_from_slice(line);
        });
        self.buf.end_compound();
    }

    /// Removes one indent unit (a leading tab, or up to tab_width leading spaces) per line.
    pub fn unindent(&mut self) {
        let (first, last) = self.line_range();
        let tw = self.tw();
        self.buf.begin_compound();
        self.rewrite_lines(first, last, |line, out| {
            let drop = if line.first() == Some(&b'\t') {
                1
            } else {
                line.iter().take_while(|&&b| b == b' ').count().min(tw)
            };
            out.extend_from_slice(&line[drop..]);
        });
        self.buf.end_compound();
    }

    /// Copies the selected lines (or the cursor line) below themselves. The cursor and
    /// anchor move into the copy.
    pub fn duplicate_line(&mut self) {
        let (first, last) = self.line_range();
        let s = self.buf.line_start(first);
        let e = self.buf.line_end(last);
        let mut ins = Vec::with_capacity(e - s + 1);
        ins.push(b'\n');
        ins.extend(self.buf.slice(s, e));
        let shift = ins.len();
        let map = |p: usize| if p < s { p } else { p + shift };
        let cursor = map(self.cursor);
        let anchor = self.anchor.map(&map);
        self.buf.begin_compound();
        self.splice(e, e, &ins);
        self.buf.end_compound();
        self.cursor = cursor;
        self.anchor = anchor;
    }

    /// Swaps the selected lines (or the cursor line) with the line above (`up`) or below.
    /// The moved lines stay selected. Does nothing at the document edge.
    pub fn move_line(&mut self, up: bool) {
        let (first, last) = self.line_range();
        if (up && first == 0) || (!up && last + 1 >= self.buf.line_count()) {
            return;
        }
        let block_s = self.buf.line_start(first);
        let block_e = self.buf.line_end(last);
        // X comes first in the old text, Y second; the new text is Y, '\n', X.
        let ((x_s, x_e), (y_s, y_e)) = if up {
            let above = first - 1;
            (
                (self.buf.line_start(above), self.buf.line_end(above)),
                (block_s, block_e),
            )
        } else {
            let below = last + 1;
            (
                (block_s, block_e),
                (self.buf.line_start(below), self.buf.line_end(below)),
            )
        };
        let rs = x_s;
        let re = y_e;
        let x = self.buf.slice(x_s, x_e);
        let y = self.buf.slice(y_s, y_e);
        let mut new = Vec::with_capacity(x.len() + y.len() + 1);
        new.extend_from_slice(&y);
        new.push(b'\n');
        new.extend_from_slice(&x);
        let ylen = y.len();
        let map = |p: usize| {
            if p >= x_s && p <= x_e {
                p - x_s + rs + ylen + 1
            } else if p >= y_s && p <= y_e {
                p - y_s + rs
            } else {
                p
            }
        };
        let cursor = map(self.cursor);
        let anchor = self.anchor.map(&map);
        self.buf.begin_compound();
        self.splice(rs, re, &new);
        self.buf.end_compound();
        self.cursor = cursor;
        self.anchor = anchor;
    }

    /// Line comment toggle using the language's prefix. Removes the prefix when every
    /// non-blank line in range already has it, otherwise adds it at the minimal common indent.
    /// Languages without a line comment (Html, Markdown, Json, Plain) do nothing.
    pub fn toggle_comment(&mut self) {
        let Some(tok) = comment_token(self.highlighter.lang()) else {
            return;
        };
        let tokb = tok.as_bytes();
        let tw = self.tw();
        let (first, last) = self.line_range();
        let rs = self.buf.line_start(first);
        let re = self.buf.line_end(last);
        let region = self.buf.slice(rs, re);

        let mut any = false;
        let mut all = true;
        let mut min_w = usize::MAX;
        for line in region.split(|&b| b == b'\n') {
            if is_blank(line) {
                continue;
            }
            any = true;
            let ind = indent_len(line);
            if !starts_with_token(&line[ind..], tokb) {
                all = false;
            }
            min_w = min_w.min(cell_offset(ind, tw, |i| line[i]));
        }
        if !any {
            return;
        }
        let remove = all;
        self.buf.begin_compound();
        self.rewrite_lines(first, last, |line, out| {
            if is_blank(line) {
                out.extend_from_slice(line);
                return;
            }
            let ind = indent_len(line);
            if remove {
                let mut skip = ind + tokb.len();
                if line.get(skip) == Some(&b' ') {
                    skip += 1;
                }
                let skip = skip.min(line.len());
                out.extend_from_slice(&line[..ind]);
                out.extend_from_slice(&line[skip..]);
            } else {
                let pos = offset_of_vcol(line.len(), min_w, tw, |i| line[i]);
                out.extend_from_slice(&line[..pos]);
                out.extend_from_slice(tokb);
                out.push(b' ');
                out.extend_from_slice(&line[pos..]);
            }
        });
        self.buf.end_compound();
    }

    // ----- undo / redo -----------------------------------------------------

    pub fn undo(&mut self) {
        match self.buf.undo() {
            Some(c) => self.restore_after_history(c),
            None => self.message = Some("Nothing to undo".to_string()),
        }
    }

    pub fn redo(&mut self) {
        match self.buf.redo() {
            Some(c) => self.restore_after_history(c),
            None => self.message = Some("Nothing to redo".to_string()),
        }
    }

    fn restore_after_history(&mut self, cursor: usize) {
        self.after_edit();
        self.cursor = align_back(&self.buf, cursor);
        self.anchor = None;
        self.preferred_col = None;
    }

    // ----- search ----------------------------------------------------------

    fn finder(&self) -> Finder {
        if self.search_case_sensitive {
            simd::find
        } else {
            simd::find_ascii_ci
        }
    }

    /// Finds the next (or previous) match of `search`, starting after the selection (or
    /// before it when searching backward), wrapping around the document. The match is selected
    /// (anchor at its start for forward search, at its end for backward search).
    pub fn find_next(&mut self, forward: bool) -> bool {
        if self.search.is_empty() {
            self.message = Some("No search text".to_string());
            return false;
        }
        let needle = self.search.clone();
        let finder = self.finder();
        let (sel_s, sel_e) = self.selection().unwrap_or((self.cursor, self.cursor));
        let from = if forward { sel_e } else { sel_s };
        let hay = self.buf.make_contiguous();
        let from = from.min(hay.len());
        let found = if forward {
            match finder(&hay[from..], &needle) {
                Some(i) => Some((from + i, false)),
                None => finder(hay, &needle).map(|i| (i, true)),
            }
        } else {
            match last_before(hay, &needle, from, finder) {
                Some(m) => Some((m, false)),
                None => last_before(hay, &needle, hay.len() + 1, finder).map(|m| (m, true)),
            }
        };
        match found {
            Some((m, wrapped)) => {
                let n = needle.len();
                self.buf.break_undo_group();
                if forward {
                    self.anchor = Some(m);
                    self.cursor = m + n;
                } else {
                    self.anchor = Some(m + n);
                    self.cursor = m;
                }
                self.preferred_col = None;
                self.message = if wrapped {
                    Some("Wrapped".to_string())
                } else {
                    None
                };
                true
            }
            None => {
                self.message = Some(format!("Not found: {}", String::from_utf8_lossy(&needle)));
                false
            }
        }
    }

    /// If the selection equals the search text, replaces it with `replace_with`, then finds
    /// the next match. Returns whether a match was found.
    pub fn replace_one(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            if self.selection_matches_search(s, e) {
                let rep = self.replace_with.clone();
                self.replace_compound(s, e, &rep);
                self.cursor = s + rep.len();
                self.anchor = None;
            }
        }
        self.find_next(true)
    }

    fn selection_matches_search(&self, s: usize, e: usize) -> bool {
        if self.search.is_empty() || e - s != self.search.len() {
            return false;
        }
        let sel = self.buf.slice(s, e);
        if self.search_case_sensitive {
            sel == self.search
        } else {
            sel.eq_ignore_ascii_case(&self.search)
        }
    }

    /// Replaces every match in one undo step. Returns the number of replacements.
    pub fn replace_all(&mut self) -> usize {
        let n = self.search.len();
        if n == 0 {
            return 0;
        }
        let needle = self.search.clone();
        let rep = self.replace_with.clone();
        let finder = self.finder();
        let cursor = self.cursor;
        let (out, count, cursor_new) = {
            let hay = self.buf.make_contiguous();
            let mut out = Vec::with_capacity(hay.len());
            let mut count = 0usize;
            let mut last = 0usize;
            let mut shift: isize = 0;
            let mut inside: Option<usize> = None;
            while let Some(i) = finder(&hay[last..], &needle) {
                let m = last + i;
                out.extend_from_slice(&hay[last..m]);
                if m < cursor && cursor < m + n {
                    inside = Some(out.len());
                } else if m + n <= cursor {
                    shift += rep.len() as isize - n as isize;
                }
                out.extend_from_slice(&rep);
                last = m + n;
                count += 1;
            }
            if count == 0 {
                return 0;
            }
            out.extend_from_slice(&hay[last..]);
            let cn = inside.unwrap_or_else(|| (cursor as isize + shift).max(0) as usize);
            (out, count, cn)
        };
        self.buf.set_cursor_hint(cursor);
        self.buf.begin_compound();
        let len = self.buf.len();
        self.buf.delete(0, len);
        self.buf.insert(0, &out);
        self.buf.end_compound();
        self.after_edit();
        self.cursor = align_back(&self.buf, cursor_new);
        self.anchor = None;
        self.message = Some(format!("Replaced {count} occurrence(s)"));
        count
    }

    /// 1-based line and column (columns are visual). Clamped to the document.
    pub fn goto_line(&mut self, line_1based: usize, col_1based: usize) {
        let last = self.buf.line_count() - 1;
        let line = line_1based.saturating_sub(1).min(last);
        let col = col_1based.saturating_sub(1);
        let pos = self.pos_from_visual(line, col);
        self.set_cursor(pos, false);
    }

    // ----- view helpers ----------------------------------------------------

    /// 0-based line and 0-based visual column of the cursor.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        (self.buf.line_of(self.cursor), self.visual_col(self.cursor))
    }

    /// Visual column of byte `pos` within its line (tabs expanded to tab_width).
    pub fn visual_col(&self, pos: usize) -> usize {
        let p = pos.min(self.buf.len());
        let ls = self.buf.line_start(self.buf.line_of(p));
        cell_offset(p - ls, self.tw(), |i| self.buf.byte_at(ls + i))
    }

    /// Byte offset in `line` closest to visual column `col` (clamped to the line end).
    pub fn pos_from_visual(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.buf.line_count() - 1);
        let ls = self.buf.line_start(line);
        let le = self.buf.line_end(line);
        ls + offset_of_vcol(le - ls, col, self.tw(), |i| self.buf.byte_at(ls + i))
    }

    /// Adjusts the scroll so the cursor is visible in a `rows` x `cols` viewport. Vertically
    /// keeps a 1-line margin when the viewport has more than 2 rows. Horizontal scrolls
    /// in jumps of cols/4.
    pub fn ensure_cursor_visible(&mut self, rows: usize, cols: usize) {
        if self.wrap {
            self.ensure_visible_wrapped(rows);
            return;
        }
        let line = self.buf.line_of(self.cursor);
        if rows > 0 {
            let margin = if rows > 2 { 1 } else { 0 };
            if line < self.scroll_line + margin {
                self.scroll_line = line.saturating_sub(margin);
            }
            if line + margin >= self.scroll_line + rows {
                self.scroll_line = line + margin + 1 - rows;
            }
        }
        if cols > 0 {
            let vc = self.visual_col(self.cursor);
            let jump = (cols / 4).max(1);
            if vc < self.scroll_col {
                self.scroll_col = vc / jump * jump;
            } else if vc >= self.scroll_col + cols {
                let need = vc + 1 - cols;
                self.scroll_col = need.div_ceil(jump) * jump;
            }
        }
    }

    /// Scrolls by `lines` logical lines, or by visual rows in wrap mode.
    pub fn scroll_by(&mut self, lines: isize) {
        if self.wrap {
            let w = self.wrap_width();
            let (line, sub) = self.top_row(w);
            let n = lines.unsigned_abs();
            let (nl, ns) = if lines < 0 {
                self.vrow_back(line, sub, n, w)
            } else {
                self.vrow_fwd(line, sub, n, w)
            };
            self.scroll_line = nl;
            self.scroll_subrow = ns;
            return;
        }
        let max = (self.buf.line_count() - 1) as isize;
        self.scroll_line = (self.scroll_line as isize)
            .saturating_add(lines)
            .clamp(0, max) as usize;
    }

    /// Widest of the `rows` logical lines from the view top, in cells. Only the lines on
    /// screen are measured, so the cost follows the view, not the file. Used without wrap.
    pub fn longest_visible(&self, rows: usize) -> usize {
        let count = self.buf.line_count();
        let first = self.scroll_line.min(count - 1);
        let end = first.saturating_add(rows).min(count);
        let tw = self.tw();
        (first..end)
            .map(|line| {
                let ls = self.buf.line_start(line);
                let le = self.buf.line_end(line);
                cell_offset(le - ls, tw, |i| self.buf.byte_at(ls + i))
            })
            .max()
            .unwrap_or(0)
    }

    /// True when the text does not fit in `rows` visible rows, or the view is scrolled
    /// away from the top, so a vertical scrollbar has something to show. In wrap mode the
    /// check walks only as far as the view, never the whole file.
    pub fn overflows_view(&self, rows: usize) -> bool {
        let rows = rows.max(1);
        if !self.wrap {
            return self.scroll_line > 0 || self.buf.line_count() > rows;
        }
        let w = self.wrap_width();
        if self.top_row(w) != (0, 0) {
            return true;
        }
        let last = self.buf.line_count() - 1;
        let mut starts = Vec::new();
        self.wrap_rows(last, w, &mut starts);
        self.vrow_fwd(0, 0, rows - 1, w) != (last, starts.len() - 1)
    }

    // ----- soft wrap -----------------------------------------------------------
    //
    // A visual row is a (logical line, sub) pair; `sub` indexes `wrap_rows(line)`. Every
    // helper here touches only the lines it needs, so the cost depends on the rows asked
    // for, never on the size of the file.

    /// Text columns used for wrapping: `wrap_cols`, at least 8.
    pub fn wrap_width(&self) -> usize {
        self.wrap_cols.max(8)
    }

    /// Fills `out` with the byte offset where each visual row of `line` starts. The first
    /// entry is always the line start. See `wrap_rows_buf` for the breaking rules.
    pub fn wrap_rows(&self, line: usize, width: usize, out: &mut Vec<usize>) {
        wrap_rows_buf(&self.buf, self.tw(), line, width, out);
    }

    /// The view top as a visual row (line, sub), clamped to the document.
    pub(crate) fn top_row(&self, width: usize) -> (usize, usize) {
        let line = self.scroll_line.min(self.buf.line_count() - 1);
        if !self.wrap {
            return (line, 0);
        }
        let mut rows = Vec::new();
        self.wrap_rows(line, width, &mut rows);
        (line, self.scroll_subrow.min(rows.len() - 1))
    }

    /// Index of the visual row that shows `pos`, where `rows` are the row starts of its line.
    fn sub_of(&self, pos: usize, rows: &[usize]) -> usize {
        let k = rows.partition_point(|&s| s <= pos).saturating_sub(1);
        if self.end_affinity && k > 0 && rows[k] == pos {
            k - 1
        } else {
            k
        }
    }

    /// Visual column of `pos` inside the row that starts at `rs`.
    fn vcol_in_row(&self, rs: usize, pos: usize) -> usize {
        cell_offset(pos.saturating_sub(rs), self.tw(), |i| self.buf.byte_at(rs + i))
    }

    /// Byte position in the visual row [rs, re) nearest to column `col`. A row that is
    /// followed by another stops before the break, so the position stays on this row.
    fn row_pos(&self, rs: usize, re: usize, last: bool, col: usize) -> usize {
        let n = re - rs;
        let off = offset_of_vcol(n, col, self.tw(), |i| self.buf.byte_at(rs + i));
        if off < n || last {
            rs + off
        } else {
            prev_char(&self.buf, re)
        }
    }

    /// The visual row `n` rows after (line, sub), clamped to the last row of the document.
    fn vrow_fwd(&self, line: usize, sub: usize, n: usize, width: usize) -> (usize, usize) {
        let last = self.buf.line_count() - 1;
        let (mut line, mut sub, mut n) = (line, sub, n);
        let mut rows = Vec::new();
        loop {
            self.wrap_rows(line, width, &mut rows);
            let sub_now = sub.min(rows.len() - 1);
            let remaining = rows.len() - 1 - sub_now;
            if n <= remaining {
                return (line, sub_now + n);
            }
            if line >= last {
                return (line, rows.len() - 1);
            }
            n -= remaining + 1;
            line += 1;
            sub = 0;
        }
    }

    /// The visual row `n` rows before (line, sub), clamped to the first row of the document.
    fn vrow_back(&self, line: usize, sub: usize, n: usize, width: usize) -> (usize, usize) {
        let (mut line, mut sub, mut n) = (line, sub, n);
        let mut rows = Vec::new();
        loop {
            if n <= sub {
                return (line, sub - n);
            }
            n -= sub + 1;
            if line == 0 {
                return (0, 0);
            }
            line -= 1;
            self.wrap_rows(line, width, &mut rows);
            sub = rows.len() - 1;
        }
    }

    /// Number of visual rows from `top` to visual row (line, sub), or None when that is
    /// more than `limit` rows or lies above `top`. Stops counting once `limit` is passed.
    fn view_distance(
        &self,
        top: (usize, usize),
        line: usize,
        sub: usize,
        width: usize,
        limit: usize,
    ) -> Option<usize> {
        let (tl, tk) = top;
        if line < tl || (line == tl && sub < tk) {
            return None;
        }
        let mut rows = Vec::new();
        let (mut l, mut from, mut d) = (tl, tk, 0usize);
        while l < line {
            self.wrap_rows(l, width, &mut rows);
            d += rows.len().saturating_sub(from);
            if d > limit {
                return None;
            }
            from = 0;
            l += 1;
        }
        let total = d + sub - from;
        (total <= limit).then_some(total)
    }

    /// Wrap-mode scroll fix-up: keeps the cursor row inside the view with a one-row margin
    /// when the view has more than two rows. Jumps straight to the target top instead of
    /// walking from the old one, so a cursor far away costs no more than the view height.
    fn ensure_visible_wrapped(&mut self, rows: usize) {
        self.scroll_col = 0;
        let w = self.wrap_width();
        let margin = if rows > 2 { 1 } else { 0 };
        let cur = self.cursor;
        let line = self.buf.line_of(cur);
        let mut row_starts = Vec::new();
        self.wrap_rows(line, w, &mut row_starts);
        let sub = self.sub_of(cur, &row_starts);
        let top = self.top_row(w);

        let above = self.vrow_back(line, sub, margin, w);
        if above < top {
            (self.scroll_line, self.scroll_subrow) = above;
            return;
        }
        let limit = rows.max(1) - 1 - margin;
        if self.view_distance(top, line, sub, w, limit).is_none() {
            (self.scroll_line, self.scroll_subrow) = self.vrow_back(line, sub, limit, w);
        }
    }

    /// Visual row and column of the cursor inside the view, both 0-based. The row counts
    /// from the view top and is None when the cursor is above the view, or more than a
    /// screen-sized distance below it. Column is relative to the text start (wrap mode:
    /// the column in its row; no wrap: after horizontal scrolling).
    pub fn cursor_view_pos(&self) -> Option<(usize, usize)> {
        if !self.wrap {
            let (line, col) = self.cursor_line_col();
            if line < self.scroll_line || col < self.scroll_col {
                return None;
            }
            return Some((line - self.scroll_line, col - self.scroll_col));
        }
        let w = self.wrap_width();
        let line = self.buf.line_of(self.cursor);
        let mut row_starts = Vec::new();
        self.wrap_rows(line, w, &mut row_starts);
        let sub = self.sub_of(self.cursor, &row_starts);
        let col = self.vcol_in_row(row_starts[sub], self.cursor);
        let row = self.view_distance(self.top_row(w), line, sub, w, VIEW_LIMIT_ROWS)?;
        Some((row, col))
    }

    /// Byte offset under a view cell: `row` visual rows below the view top (in wrap mode;
    /// a logical line without wrap) and column `col` from the text start. `rows_hint` is
    /// the number of visible rows, and `row` is clamped to it. Clamps to the document.
    pub fn pos_from_view(&self, row: usize, col: usize, rows_hint: usize) -> usize {
        if !self.wrap {
            let line = self.scroll_line.saturating_add(row);
            return self.pos_from_visual(line, col + self.scroll_col);
        }
        let row = if rows_hint > 0 {
            row.min(rows_hint - 1)
        } else {
            row
        };
        let w = self.wrap_width();
        let (top_line, top_sub) = self.top_row(w);
        let (line, sub) = self.vrow_fwd(top_line, top_sub, row, w);
        let mut row_starts = Vec::new();
        self.wrap_rows(line, w, &mut row_starts);
        let last = sub + 1 >= row_starts.len();
        let re = if last {
            self.buf.line_end(line)
        } else {
            row_starts[sub + 1]
        };
        self.row_pos(row_starts[sub], re, last, col)
    }

    /// Home in wrap mode. Moves to the start of the visual row. From the row start it goes
    /// to the logical line start, and on the first row it uses the smart-home rule.
    fn wrap_home(&self, cur: usize) -> usize {
        let line = self.buf.line_of(cur);
        let mut row_starts = Vec::new();
        self.wrap_rows(line, self.wrap_width(), &mut row_starts);
        let sub = self.sub_of(cur, &row_starts);
        let rs = row_starts[sub];
        if sub > 0 {
            if cur != rs {
                rs
            } else {
                self.buf.line_start(line)
            }
        } else {
            self.smart_home(line, cur)
        }
    }

    /// End in wrap mode. On a row that wraps, the end is the start of the next row, flagged
    /// so the cursor is drawn at the end of this row. Returns the flag too.
    fn wrap_line_end(&self, cur: usize) -> (usize, bool) {
        let line = self.buf.line_of(cur);
        let mut row_starts = Vec::new();
        self.wrap_rows(line, self.wrap_width(), &mut row_starts);
        let sub = self.sub_of(cur, &row_starts);
        if sub + 1 < row_starts.len() {
            (row_starts[sub + 1], true)
        } else {
            (self.buf.line_end(line), false)
        }
    }

    /// Up, Down, PageUp and PageDown in wrap mode: `n` visual rows down or up, keeping the
    /// preferred column. At the document edge the cursor goes to the start or the end.
    fn wrap_vertical(&mut self, cur: usize, down: bool, n: usize) -> usize {
        let w = self.wrap_width();
        let line = self.buf.line_of(cur);
        let mut row_starts = Vec::new();
        self.wrap_rows(line, w, &mut row_starts);
        let sub = self.sub_of(cur, &row_starts);
        let col = match self.preferred_col {
            Some(c) => c,
            None => self.vcol_in_row(row_starts[sub], cur),
        };
        self.preferred_col = Some(col);
        let (tl, tk) = if down {
            self.vrow_fwd(line, sub, n, w)
        } else {
            self.vrow_back(line, sub, n, w)
        };
        if (tl, tk) == (line, sub) {
            return if down { self.buf.len() } else { 0 };
        }
        if tl != line {
            self.wrap_rows(tl, w, &mut row_starts);
        }
        let k = tk.min(row_starts.len() - 1);
        let last = k + 1 >= row_starts.len();
        let re = if last {
            self.buf.line_end(tl)
        } else {
            row_starts[k + 1]
        };
        self.row_pos(row_starts[k], re, last, col)
    }
}

/// Rows that `cursor_view_pos` will count before giving up. Far beyond any screen.
const VIEW_LIMIT_ROWS: usize = 4096;

// ---------------------------------------------------------------------------
// Save As file names
// ---------------------------------------------------------------------------

/// Final path for a Save As. `default_ext` (without the dot) is added when the file name has
/// no extension of its own, so "notes" becomes "notes.txt" while "notes.md" and ".gitignore"
/// stay as typed. A name ending in '.' asks for no extension: Windows drops trailing dots
/// from file names, so they are removed here too and nothing is added.
pub fn save_as_path(chosen: &str, default_ext: Option<&str>) -> String {
    let name_start = chosen.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let name = &chosen[name_start..];
    if name.ends_with('.') {
        let stem = name.trim_end_matches('.');
        if stem.is_empty() {
            return chosen.to_string();
        }
        return chosen[..name_start + stem.len()].to_string();
    }
    match default_ext {
        Some(ext) if !has_extension(name) => format!("{chosen}.{ext}"),
        _ => chosen.to_string(),
    }
}

/// True when `name` ends in an extension: a '.' followed by at least one character and no
/// spaces, the way Windows (PathFindExtension) reads it.
fn has_extension(name: &str) -> bool {
    match name.rfind('.') {
        Some(i) => i + 1 < name.len() && !name[i + 1..].contains(' '),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

#[inline]
fn is_cont(b: u8) -> bool {
    b & 0xC0 == 0x80
}

fn prev_char(buf: &Buffer, pos: usize) -> usize {
    let mut p = pos.min(buf.len());
    if p == 0 {
        return 0;
    }
    p -= 1;
    while p > 0 && is_cont(buf.byte_at(p)) {
        p -= 1;
    }
    p
}

fn next_char(buf: &Buffer, pos: usize) -> usize {
    let len = buf.len();
    if pos >= len {
        return len;
    }
    let mut p = pos + 1;
    while p < len && is_cont(buf.byte_at(p)) {
        p += 1;
    }
    p
}

/// Moves `pos` back to the start of the char containing it.
fn align_back(buf: &Buffer, pos: usize) -> usize {
    let len = buf.len();
    let mut p = pos.min(len);
    while p > 0 && p < len && is_cont(buf.byte_at(p)) {
        p -= 1;
    }
    p
}

/// Word bytes: ASCII alphanumerics, '_', and any non-ASCII byte.
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn word_left(buf: &Buffer, pos: usize) -> usize {
    let mut p = pos.min(buf.len());
    while p > 0 {
        let q = prev_char(buf, p);
        if is_word(buf.byte_at(q)) {
            break;
        }
        p = q;
    }
    while p > 0 {
        let q = prev_char(buf, p);
        if !is_word(buf.byte_at(q)) {
            break;
        }
        p = q;
    }
    p
}

fn word_right(buf: &Buffer, pos: usize) -> usize {
    let len = buf.len();
    let mut p = pos.min(len);
    while p < len && !is_word(buf.byte_at(p)) {
        p = next_char(buf, p);
    }
    while p < len && is_word(buf.byte_at(p)) {
        p = next_char(buf, p);
    }
    p
}

/// Visual width of the first `n` bytes. Each char is one cell except tabs, which advance to
/// the next multiple of `tw`.
fn cell_offset(n: usize, tw: usize, at: impl Fn(usize) -> u8) -> usize {
    let tw = tw.max(1);
    let mut acc = 0usize;
    for i in 0..n {
        let b = at(i);
        if is_cont(b) {
            continue;
        }
        acc += if b == b'\t' { tw - acc % tw } else { 1 };
    }
    acc
}

/// Byte offset of the first char boundary whose visual column is >= `col`, or `n`.
fn offset_of_vcol(n: usize, col: usize, tw: usize, at: impl Fn(usize) -> u8) -> usize {
    let tw = tw.max(1);
    let mut acc = 0usize;
    let mut i = 0usize;
    while i < n {
        let b = at(i);
        if !is_cont(b) {
            if acc >= col {
                return i;
            }
            acc += if b == b'\t' { tw - acc % tw } else { 1 };
        }
        i += 1;
    }
    n
}

/// Last match of `needle` whose start is < `limit`. The scan is forward over the window
/// that can contain such matches, keeping the last hit.
fn last_before(hay: &[u8], needle: &[u8], limit: usize, finder: Finder) -> Option<usize> {
    let end = limit
        .saturating_add(needle.len())
        .saturating_sub(1)
        .min(hay.len());
    let window = &hay[..end];
    let mut pos = 0usize;
    let mut last = None;
    while pos < window.len() {
        let i = match finder(&window[pos..], needle) {
            Some(i) => i,
            None => break,
        };
        let abs = pos + i;
        if abs >= limit {
            break;
        }
        last = Some(abs);
        pos = abs + 1;
    }
    last
}

/// Maps an old position through a whole-line rewrite of the region [rs, re).
/// `starts` are the old line starts inside the region, `deltas` the per-line length changes.
fn map_pos(
    p: usize,
    rs: usize,
    re: usize,
    starts: &[usize],
    deltas: &[isize],
    total: isize,
) -> usize {
    let clamp = |v: isize| v.max(0) as usize;
    if p < rs {
        return p;
    }
    if p > re || starts.is_empty() {
        return clamp(p as isize + total);
    }
    let k = starts.partition_point(|&s| s <= p).saturating_sub(1);
    let before: isize = deltas[..k].iter().sum();
    let new_ls = starts[k] as isize + before;
    if p == starts[k] {
        return clamp(new_ls);
    }
    clamp((p as isize + before + deltas[k]).max(new_ls))
}

/// Byte offsets where each visual row of `line` starts (see `Editor::wrap_rows`). Cells are
/// chars, with tabs advancing to the next stop measured from the row start. A row breaks
/// before the first word whose end does not fit in `width`; a word that fits nowhere is cut
/// at `width`. Whitespace may run past the edge (it hangs), so a break is only ever taken
/// before a non-blank char. A break is always past the row start, so the scan terminates.
///
/// Each char is examined at most twice, so the cost is linear in the line's length.
pub(crate) fn wrap_rows_buf(
    buf: &Buffer,
    tab_width: usize,
    line: usize,
    width: usize,
    out: &mut Vec<usize>,
) {
    out.clear();
    let w = width.max(1);
    let tw = tab_width.max(1);
    let ls = buf.line_start(line);
    let le = buf.line_end(line);
    out.push(ls);
    let mut rs = ls;
    let mut col = 0usize;
    // Start of the last word in this row, the best break so far.
    let mut brk: Option<usize> = None;
    let mut prev_ws = false;
    let mut i = ls;
    while i < le {
        let b = buf.byte_at(i);
        let ws = b == b' ' || b == b'\t';
        if ws {
            col += if b == b'\t' { tw - col % tw } else { 1 };
            prev_ws = true;
            i += 1;
            continue;
        }
        if i > rs && prev_ws {
            brk = Some(i);
        }
        if i > rs && col + 1 > w {
            // The char does not fit: break at the last word start, or cut right here.
            let next = brk.unwrap_or(i);
            out.push(next);
            rs = next;
            col = 0;
            brk = None;
            prev_ws = false;
            i = next;
            continue;
        }
        col += 1;
        prev_ws = false;
        i += utf8_char_len(buf, i, le);
    }
}

/// Byte length of the char at `i`, not running past `end`.
fn utf8_char_len(buf: &Buffer, i: usize, end: usize) -> usize {
    let mut n = 1;
    while i + n < end && is_cont(buf.byte_at(i + n)) {
        n += 1;
    }
    n
}

fn indent_len(line: &[u8]) -> usize {
    line.iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count()
}

fn is_blank(line: &[u8]) -> bool {
    indent_len(line) == line.len()
}

/// True if `rest` starts with the comment token. Tokens ending in a letter (e.g. "REM")
/// must not be followed by another letter, digit or '_'.
fn starts_with_token(rest: &[u8], tok: &[u8]) -> bool {
    if !rest.starts_with(tok) {
        return false;
    }
    if !matches!(tok.last(), Some(b) if b.is_ascii_alphabetic()) {
        return true;
    }
    match rest.get(tok.len()) {
        None => true,
        Some(&b) => !(b.is_ascii_alphanumeric() || b == b'_'),
    }
}

/// Line comment prefix for a language, or None when it has no line comments.
fn comment_token(lang: Lang) -> Option<&'static str> {
    Some(match lang {
        Lang::Rust
        | Lang::C
        | Lang::Cpp
        | Lang::CSharp
        | Lang::JavaScript
        | Lang::TypeScript
        | Lang::Go
        | Lang::Zig => "//",
        Lang::Python | Lang::Shell | Lang::PowerShell | Lang::Toml => "#",
        Lang::Ini => ";",
        Lang::Lua | Lang::Sql => "--",
        Lang::Batch => "REM",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn ed(text: &str) -> Editor {
        let mut e = Editor::new(&Config::default());
        e.load("test.txt", text.as_bytes());
        e
    }

    fn with_spaces(text: &str) -> Editor {
        let cfg = Config {
            insert_spaces: true,
            ..Config::default()
        };
        let mut e = Editor::new(&cfg);
        e.load("test.txt", text.as_bytes());
        e
    }

    fn text(e: &Editor) -> String {
        String::from_utf8(e.buf.text()).expect("valid utf-8")
    }

    fn type_str(e: &mut Editor, s: &str) {
        for ch in s.chars() {
            e.insert_char(ch);
        }
    }

    fn b(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    // ----- movement ------------------------------------------------------

    #[test]
    fn left_right_step_over_multibyte_chars() {
        // a(1) é(2) 日(3) b(1)
        let mut e = ed("aé日b");
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 1);
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 3);
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 6);
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 7);
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 7, "right at end stays");
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 6);
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 3);
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 1);
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 0);
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 0, "left at start stays");
    }

    #[test]
    fn up_down_keep_preferred_column() {
        let mut e = ed("abcdef\nab\nabcdef");
        e.set_cursor(5, false); // line 0, col 5
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 9, "short line clamps to its end");
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 15, "preferred column restored on the long line");
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 9);
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 8);
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 1, "horizontal move reset the preferred column");
    }

    #[test]
    fn up_down_at_document_edges() {
        let mut e = ed("ab\ncd");
        e.set_cursor(1, false);
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 0, "up on the first line goes to the start");
        e.set_cursor(4, false);
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 5, "down on the last line goes to the end");
    }

    #[test]
    fn up_down_use_visual_columns_with_tabs() {
        // Line 0 is "\tab": visual cols 0, 4, 5, 6. Line 1 is "abcdef".
        let mut e = ed("\tab\nabcdef");
        e.set_cursor(10, false); // line 1, col 6
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 3, "col 6 on line 0 is its end");
        e.set_cursor(1, false); // after the tab, visual col 4
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 8, "visual col 4 is 'e' on line 1");
    }

    #[test]
    fn line_home_is_smart() {
        let mut e = ed("   foo");
        e.set_cursor(6, false);
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 3, "first non-whitespace");
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 0, "then column 0");
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 3, "toggles back");
    }

    #[test]
    fn line_home_without_indent_and_on_blank_line() {
        let mut e = ed("foo\n   \nbar");
        e.set_cursor(2, false);
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 0);
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 0, "no indent: stays at column 0");
        e.set_cursor(7, false); // blank line "   " spans 4..7
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 4, "whitespace-only line: from its end, column 0");
        e.move_cursor(Move::LineHome, false);
        assert_eq!(
            e.cursor, 7,
            "then back to the first non-ws, which is the end"
        );
    }

    #[test]
    fn line_end_and_doc_moves() {
        let mut e = ed("ab\ncdé\nx");
        e.set_cursor(0, false);
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 2);
        e.move_cursor(Move::Down, false);
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 7, "end of line with a multibyte char");
        e.move_cursor(Move::DocEnd, false);
        assert_eq!(e.cursor, 9);
        e.move_cursor(Move::DocStart, false);
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn word_moves() {
        let mut e = ed("foo bar_baz qux");
        e.move_cursor(Move::WordRight, false);
        assert_eq!(e.cursor, 3);
        e.move_cursor(Move::WordRight, false);
        assert_eq!(e.cursor, 11);
        e.move_cursor(Move::WordRight, false);
        assert_eq!(e.cursor, 15);
        e.move_cursor(Move::WordLeft, false);
        assert_eq!(e.cursor, 12);
        e.move_cursor(Move::WordLeft, false);
        assert_eq!(e.cursor, 4);
        e.move_cursor(Move::WordLeft, false);
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn word_moves_treat_non_ascii_as_word() {
        let mut e = ed("日本 go");
        e.move_cursor(Move::WordRight, false);
        assert_eq!(e.cursor, 6);
        e.move_cursor(Move::WordRight, false);
        assert_eq!(e.cursor, 9);
        e.move_cursor(Move::WordLeft, false);
        assert_eq!(e.cursor, 7);
    }

    #[test]
    fn page_moves() {
        let mut e = ed("0\n1\n2\n3\n4\n5\n6\n7");
        e.set_cursor(0, false);
        e.move_cursor(Move::PageDown(3), false);
        assert_eq!(e.cursor, 6, "line 3");
        e.move_cursor(Move::PageDown(10), false);
        assert_eq!(e.cursor, 14, "clamped to the last line");
        e.move_cursor(Move::PageDown(1), false);
        assert_eq!(e.cursor, 15, "at the bottom: to the document end");
        e.move_cursor(Move::PageUp(2), false);
        assert_eq!(e.cursor, 10, "line 5, preferred column 0");
        e.move_cursor(Move::PageUp(100), false);
        assert_eq!(e.cursor, 0);
        e.move_cursor(Move::PageUp(1), false);
        assert_eq!(e.cursor, 0, "at the top: to the document start");
    }

    #[test]
    fn move_with_shift_extends_and_plain_move_collapses() {
        let mut e = ed("abcdef");
        e.move_cursor(Move::Right, true);
        e.move_cursor(Move::Right, true);
        assert_eq!(e.selection(), Some((0, 2)));
        assert_eq!(e.selected_text(), Some(b("ab")));
        e.move_cursor(Move::Left, false);
        assert_eq!(e.cursor, 0, "left collapses to the selection start");
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn right_collapses_to_selection_end() {
        let mut e = ed("abcdef");
        e.set_cursor(1, false);
        e.set_cursor(4, true);
        assert_eq!(e.selection(), Some((1, 4)));
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 4);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn extend_then_extend_back_to_anchor_is_no_selection() {
        let mut e = ed("abc");
        e.move_cursor(Move::Right, true);
        e.move_cursor(Move::Left, true);
        assert_eq!(e.cursor, 0);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn mark_mode_makes_every_move_extend() {
        let mut e = ed("abcdef");
        e.mark_mode = true;
        e.move_cursor(Move::Right, false);
        assert_eq!(e.selection(), Some((0, 1)));
        e.mark_mode = false;
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 1, "collapses to the selection end first");
        assert_eq!(e.selection(), None);
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 2);
    }

    // ----- typing ----------------------------------------------------------

    #[test]
    fn newline_copies_indentation() {
        let mut e = ed("    foo");
        e.set_cursor(7, false);
        e.insert_char('\n');
        assert_eq!(text(&e), "    foo\n    ");
        assert_eq!(e.cursor, 12);
    }

    #[test]
    fn newline_indent_stops_at_cursor() {
        let mut e = ed("        x");
        e.set_cursor(2, false);
        e.insert_char('\n');
        assert_eq!(
            text(&e),
            "  \n        x",
            "rest of the line keeps its own indent"
        );
        assert_eq!(e.cursor, 5);
    }

    #[test]
    fn newline_without_auto_indent() {
        let cfg = Config {
            auto_indent: false,
            ..Config::default()
        };
        let mut e = Editor::new(&cfg);
        e.load("t.txt", b"  x");
        e.set_cursor(3, false);
        e.insert_char('\n');
        assert_eq!(text(&e), "  x\n");
    }

    #[test]
    fn tab_inserts_spaces_to_next_stop() {
        let mut e = with_spaces("");
        e.insert_char('\t');
        assert_eq!(text(&e), "    ");
        assert_eq!(e.cursor, 4);
        e.insert_char('x');
        e.insert_char('\t');
        assert_eq!(text(&e), "    x   ");
        assert_eq!(e.cursor, 8);
    }

    #[test]
    fn tab_inserts_tab_char_without_insert_spaces() {
        let mut e = ed("");
        e.insert_char('\t');
        assert_eq!(text(&e), "\t");
    }

    #[test]
    fn tab_counts_wide_chars_as_one_cell() {
        let mut e = with_spaces("é");
        e.set_cursor(2, false);
        e.insert_char('\t');
        assert_eq!(text(&e), "é   ", "é is one cell so 3 spaces reach column 4");
    }

    #[test]
    fn backspace_on_leading_spaces_goes_to_tab_stop() {
        let mut e = with_spaces("      x");
        e.set_cursor(6, false);
        e.backspace();
        assert_eq!(text(&e), "    x");
        assert_eq!(e.cursor, 4);
        e.backspace();
        assert_eq!(text(&e), "x");
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn backspace_mid_indent_goes_to_previous_stop() {
        let mut e = with_spaces("      ");
        e.set_cursor(5, false);
        e.backspace();
        assert_eq!(text(&e), "     ", "removes back to stop 4, keeps the rest");
    }

    #[test]
    fn backspace_after_text_removes_one_char() {
        let mut e = with_spaces("a    ");
        e.set_cursor(5, false);
        e.backspace();
        assert_eq!(text(&e), "a   ", "not pure leading whitespace: one char");
    }

    #[test]
    fn backspace_removes_multibyte_char_whole() {
        let mut e = ed("aé");
        e.set_cursor(3, false);
        e.backspace();
        assert_eq!(text(&e), "a");
        assert_eq!(e.cursor, 1);
    }

    #[test]
    fn backspace_joins_lines_at_line_start() {
        let mut e = ed("ab\ncd");
        e.set_cursor(3, false);
        e.backspace();
        assert_eq!(text(&e), "abcd");
        assert_eq!(e.cursor, 2);
    }

    #[test]
    fn delete_forward_and_word_deletes() {
        let mut e = ed("foo bar baz");
        e.delete_forward();
        assert_eq!(text(&e), "oo bar baz");
        e.delete_word_right();
        assert_eq!(text(&e), " bar baz");
        e.set_cursor(8, false);
        e.delete_word_left();
        assert_eq!(text(&e), " bar ");
        assert_eq!(e.cursor, 5);
    }

    #[test]
    fn typing_replaces_selection_in_one_undo_step() {
        let mut e = ed("hello world");
        e.set_cursor(0, false);
        e.set_cursor(5, true);
        e.insert_char('X');
        assert_eq!(text(&e), "X world");
        assert_eq!(e.cursor, 1);
        e.undo();
        assert_eq!(text(&e), "hello world");
        assert_eq!(e.cursor, 5, "undo restores the cursor from before the edit");
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn typing_after_movement_starts_new_undo_group() {
        let mut e = ed("");
        type_str(&mut e, "ab");
        e.move_cursor(Move::Left, false);
        type_str(&mut e, "c");
        assert_eq!(text(&e), "acb");
        e.undo();
        assert_eq!(text(&e), "ab");
        e.undo();
        assert_eq!(text(&e), "");
    }

    #[test]
    fn consecutive_typing_is_one_undo_group() {
        let mut e = ed("");
        type_str(&mut e, "abc");
        e.undo();
        assert_eq!(text(&e), "");
    }

    #[test]
    fn insert_text_replaces_selection_and_is_one_undo_step() {
        let mut e = ed("one two");
        e.set_cursor(4, false);
        e.set_cursor(7, true);
        e.insert_text(b"2\r\n3");
        assert_eq!(text(&e), "one 2\n3");
        assert_eq!(e.cursor, 7);
        e.undo();
        assert_eq!(text(&e), "one two");
        assert_eq!(e.cursor, 7);
    }

    #[test]
    fn insert_text_empty_without_selection_is_noop() {
        let mut e = ed("abc");
        e.set_cursor(1, false);
        e.insert_text(b"");
        assert_eq!(text(&e), "abc");
        assert_eq!(e.cursor, 1);
        e.undo();
        assert_eq!(text(&e), "abc");
    }

    // ----- cut, copy, paste -------------------------------------------------

    #[test]
    fn cut_line_append_chain_then_paste_restores_lines() {
        let mut e = ed("a\nb\nc");
        assert_eq!(e.cut_line(), b("a\n"));
        assert_eq!(text(&e), "b\nc");
        assert_eq!(e.cut_line(), b("a\nb\n"));
        assert_eq!(e.cut_line(), b("a\nb\nc\n"));
        assert_eq!(text(&e), "");
        let cb = e.cut_buffer.clone();
        e.insert_text(&cb);
        assert_eq!(text(&e), "a\nb\nc\n");
        assert_eq!(e.cursor, 6);
    }

    #[test]
    fn cut_line_chain_breaks_after_other_action() {
        let mut e = ed("a\nb\nc");
        e.cut_line();
        e.reset_cut_chain();
        let out = e.cut_line();
        assert_eq!(out, b("b\n"), "not appended after a reset");
        assert!(e.last_was_cut_line);
        e.reset_cut_chain();
        assert!(!e.last_was_cut_line);
    }

    #[test]
    fn cut_line_on_last_line_appends_newline_and_paste_restores() {
        let mut e = ed("a\nb");
        e.set_cursor(2, false);
        let out = e.cut_line();
        assert_eq!(out, b("b\n"));
        assert_eq!(text(&e), "a\n");
        assert_eq!(e.cursor, 2);
        e.reset_cut_chain();
        e.insert_text(&out);
        assert_eq!(text(&e), "a\nb\n");
    }

    #[test]
    fn cut_line_with_selection_cuts_selection() {
        let mut e = ed("hello world");
        e.set_cursor(0, false);
        e.set_cursor(5, true);
        let out = e.cut_line();
        assert_eq!(out, b("hello"));
        assert_eq!(text(&e), " world");
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn cut_line_on_middle_line_keeps_column_zero_cursor() {
        let mut e = ed("one\ntwo\nthree");
        e.set_cursor(5, false);
        let out = e.cut_line();
        assert_eq!(out, b("two\n"));
        assert_eq!(text(&e), "one\nthree");
        assert_eq!(e.cursor, 4);
    }

    #[test]
    fn cut_line_is_undoable_with_cursor() {
        let mut e = ed("a\nb\nc");
        e.set_cursor(2, false);
        e.cut_line();
        assert_eq!(text(&e), "a\nc");
        e.undo();
        assert_eq!(text(&e), "a\nb\nc");
        assert_eq!(e.cursor, 2);
    }

    #[test]
    fn cut_selection_and_copy_selection() {
        let mut e = ed("abcdef");
        e.set_cursor(1, false);
        e.set_cursor(4, true);
        assert_eq!(e.copy_selection(), Some(b("bcd")));
        assert_eq!(text(&e), "abcdef", "copy leaves the text");
        assert_eq!(e.cut_selection(), Some(b("bcd")));
        assert_eq!(text(&e), "aef");
        assert_eq!(e.cursor, 1);
        assert_eq!(e.cut_selection(), None);
    }

    #[test]
    fn cut_line_on_empty_document_gives_newline() {
        let mut e = ed("");
        assert_eq!(e.cut_line(), b("\n"));
        assert_eq!(text(&e), "");
    }

    // ----- indent and comments ----------------------------------------------

    #[test]
    fn indent_and_unindent_selected_lines() {
        let mut e = ed("a\nb\nc");
        e.set_cursor(0, false);
        e.set_cursor(3, true);
        e.indent();
        assert_eq!(text(&e), "\ta\n\tb\nc");
        assert_eq!(
            e.selection(),
            Some((0, 5)),
            "selection still covers both lines"
        );
        e.unindent();
        assert_eq!(text(&e), "a\nb\nc");
        assert_eq!(e.selection(), Some((0, 3)));
    }

    #[test]
    fn indent_with_spaces_and_partial_unindent() {
        let mut e = with_spaces("  x\n      y");
        e.set_cursor(0, false);
        e.set_cursor(12, true);
        e.unindent();
        assert_eq!(text(&e), "x\n  y", "removes up to tab_width spaces");
        e.indent();
        assert_eq!(text(&e), "    x\n      y");
    }

    #[test]
    fn selection_ending_at_column_zero_skips_last_line() {
        let mut e = ed("a\nb\nc");
        e.set_cursor(0, false);
        e.set_cursor(2, true); // ends at start of line 1
        e.indent();
        assert_eq!(text(&e), "\ta\nb\nc");
    }

    #[test]
    fn indent_without_selection_affects_cursor_line_and_cursor_follows() {
        let mut e = ed("ab\ncd");
        e.set_cursor(4, false);
        e.indent();
        assert_eq!(text(&e), "ab\n\tcd");
        assert_eq!(e.cursor, 5);
    }

    #[test]
    fn unindent_on_line_without_indent_is_noop() {
        let mut e = ed("x");
        e.unindent();
        assert_eq!(text(&e), "x");
        e.undo();
        assert_eq!(text(&e), "x");
    }

    #[test]
    fn compound_indent_is_one_undo_step() {
        let mut e = ed("a\nb\nc\nd");
        e.select_all();
        e.indent();
        assert_eq!(text(&e), "\ta\n\tb\n\tc\n\td");
        e.undo();
        assert_eq!(text(&e), "a\nb\nc\nd");
        e.redo();
        assert_eq!(text(&e), "\ta\n\tb\n\tc\n\td");
    }

    #[test]
    fn duplicate_line_and_selection() {
        let mut e = ed("a\nb");
        e.set_cursor(0, false);
        e.duplicate_line();
        assert_eq!(text(&e), "a\na\nb");
        assert_eq!(e.cursor, 2, "cursor moves into the copy");
        e.undo();
        assert_eq!(text(&e), "a\nb");
        e.set_cursor(2, false);
        e.duplicate_line();
        assert_eq!(text(&e), "a\nb\nb");
        assert_eq!(e.cursor, 4);
    }

    #[test]
    fn duplicate_selected_lines_keeps_selection_on_copy() {
        let mut e = ed("1\n2\n3");
        e.set_cursor(0, false);
        e.set_cursor(3, true);
        e.duplicate_line();
        assert_eq!(text(&e), "1\n2\n1\n2\n3");
        assert_eq!(e.selection(), Some((4, 7)));
        assert_eq!(e.selected_text(), Some(b("1\n2")));
    }

    #[test]
    fn move_line_down_and_up_with_selection() {
        let mut e = ed("1\n2\n3\n4");
        e.set_cursor(0, false);
        e.set_cursor(3, true); // lines "1" and "2"
        e.move_line(false);
        assert_eq!(text(&e), "3\n1\n2\n4");
        assert_eq!(
            e.selected_text(),
            Some(b("1\n2")),
            "selection follows the moved lines"
        );
        e.move_line(true);
        assert_eq!(text(&e), "1\n2\n3\n4");
        assert_eq!(e.selected_text(), Some(b("1\n2")));
    }

    #[test]
    fn move_line_single_line_and_cursor() {
        let mut e = ed("a\nbb\nc");
        e.set_cursor(3, false); // in "bb" at col 1
        e.move_line(true);
        assert_eq!(text(&e), "bb\na\nc");
        assert_eq!(e.cursor, 1, "cursor keeps its column");
        e.move_line(false);
        assert_eq!(text(&e), "a\nbb\nc");
        assert_eq!(e.cursor, 3);
    }

    #[test]
    fn move_line_at_edges_is_noop() {
        let mut e = ed("a\nb");
        e.set_cursor(0, false);
        e.move_line(true);
        assert_eq!(text(&e), "a\nb");
        e.set_cursor(2, false);
        e.move_line(false);
        assert_eq!(text(&e), "a\nb");
        assert!(!e.buf.is_dirty());
    }

    #[test]
    fn move_line_last_line_up_and_undo() {
        let mut e = ed("a\nb\nc");
        e.set_cursor(4, false);
        e.move_line(true);
        assert_eq!(text(&e), "a\nc\nb");
        e.undo();
        assert_eq!(text(&e), "a\nb\nc");
    }

    #[test]
    fn toggle_comment_adds_then_removes_rust() {
        let mut e = ed("fn a\n    b");
        e.highlighter.set_lang(Lang::Rust);
        e.select_all();
        e.toggle_comment();
        assert_eq!(text(&e), "// fn a\n//     b");
        e.toggle_comment();
        assert_eq!(text(&e), "fn a\n    b");
    }

    #[test]
    fn toggle_comment_adds_at_minimal_common_indent() {
        let mut e = ed("  a\n    b\n\n  c");
        e.highlighter.set_lang(Lang::Python);
        e.select_all();
        e.toggle_comment();
        assert_eq!(text(&e), "  # a\n  #   b\n\n  # c");
        e.toggle_comment();
        assert_eq!(text(&e), "  a\n    b\n\n  c");
    }

    #[test]
    fn toggle_comment_mixed_lines_adds_prefix() {
        let mut e = ed("// a\nb");
        e.highlighter.set_lang(Lang::Rust);
        e.select_all();
        e.toggle_comment();
        assert_eq!(text(&e), "// // a\n// b", "not all commented: add");
    }

    #[test]
    fn toggle_comment_token_per_language() {
        for (lang, tok) in [
            (Lang::Lua, "--"),
            (Lang::Sql, "--"),
            (Lang::Batch, "REM"),
            (Lang::Ini, ";"),
            (Lang::Shell, "#"),
        ] {
            let mut e = ed("x");
            e.highlighter.set_lang(lang);
            e.toggle_comment();
            assert_eq!(text(&e), format!("{tok} x"), "{lang:?}");
            e.toggle_comment();
            assert_eq!(text(&e), "x", "{lang:?} removes");
        }
    }

    #[test]
    fn toggle_comment_rem_needs_word_boundary() {
        let mut e = ed("REMARK x");
        e.highlighter.set_lang(Lang::Batch);
        e.toggle_comment();
        assert_eq!(text(&e), "REM REMARK x", "REMARK is not a REM comment");
    }

    #[test]
    fn toggle_comment_no_op_without_line_comments() {
        let mut e = ed("<p>x</p>");
        e.highlighter.set_lang(Lang::Html);
        e.toggle_comment();
        assert_eq!(text(&e), "<p>x</p>");
        assert!(!e.buf.is_dirty());
        e.highlighter.set_lang(Lang::Markdown);
        e.toggle_comment();
        assert_eq!(text(&e), "<p>x</p>");
    }

    #[test]
    fn toggle_comment_on_blank_only_range_is_noop() {
        let mut e = ed("   \n\t");
        e.highlighter.set_lang(Lang::Rust);
        e.select_all();
        e.toggle_comment();
        assert_eq!(text(&e), "   \n\t");
    }

    #[test]
    fn toggle_comment_single_line_removes_without_space() {
        let mut e = ed("//x");
        e.highlighter.set_lang(Lang::Rust);
        e.toggle_comment();
        assert_eq!(text(&e), "x");
    }

    // ----- undo, redo ------------------------------------------------------

    #[test]
    fn undo_redo_restore_cursor_and_clear_selection() {
        let mut e = ed("abc");
        e.set_cursor(3, false);
        type_str(&mut e, "d");
        e.set_cursor(0, true);
        e.undo();
        assert_eq!(text(&e), "abc");
        assert_eq!(e.cursor, 3);
        assert_eq!(e.selection(), None);
        e.redo();
        assert_eq!(text(&e), "abcd");
        assert_eq!(e.cursor, 4);
    }

    #[test]
    fn undo_with_nothing_sets_message() {
        let mut e = ed("abc");
        e.undo();
        assert_eq!(text(&e), "abc");
        assert!(e.message.is_some());
    }

    #[test]
    fn undo_after_compound_replace_all() {
        let mut e = ed("aXa aXa");
        e.search = b("a");
        e.replace_with = b("bb");
        assert_eq!(e.replace_all(), 4);
        assert_eq!(text(&e), "bbXbb bbXbb");
        e.undo();
        assert_eq!(text(&e), "aXa aXa");
    }

    // ----- search -----------------------------------------------------------

    #[test]
    fn find_next_wraps_and_is_case_insensitive() {
        let mut e = ed("foo Bar FOO baz");
        e.search = b("foo");
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((0, 3)));
        assert_eq!(e.message, None);
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((8, 11)), "matches FOO");
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((0, 3)));
        assert_eq!(e.message.as_deref(), Some("Wrapped"));
    }

    #[test]
    fn find_next_case_sensitive() {
        let mut e = ed("foo Foo");
        e.search = b("Foo");
        e.search_case_sensitive = true;
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((4, 7)));
        e.search_case_sensitive = false;
        e.set_cursor(0, false);
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((0, 3)), "insensitive matches foo first");
    }

    #[test]
    fn find_next_miss_sets_message_and_keeps_cursor() {
        let mut e = ed("abc");
        e.set_cursor(1, false);
        e.search = b("zed");
        assert!(!e.find_next(true));
        assert_eq!(e.cursor, 1);
        assert_eq!(e.message.as_deref(), Some("Not found: zed"));
    }

    #[test]
    fn find_next_with_empty_search_is_false() {
        let mut e = ed("abc");
        assert!(!e.find_next(true));
    }

    #[test]
    fn find_next_moves_past_current_selection() {
        let mut e = ed("aa aa");
        e.search = b("aa");
        e.search_case_sensitive = true;
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((0, 2)));
        assert!(e.find_next(true));
        assert_eq!(e.selection(), Some((3, 5)));
    }

    #[test]
    fn find_prev_scans_backward_and_wraps() {
        let mut e = ed("a x a x a");
        e.search = b("a");
        e.search_case_sensitive = true;
        e.set_cursor(8, false);
        assert!(e.find_next(false));
        assert_eq!(e.selection(), Some((4, 5)));
        assert_eq!(e.cursor, 4, "backward selection has cursor at start");
        assert!(e.find_next(false));
        assert_eq!(e.selection(), Some((0, 1)));
        assert!(e.find_next(false));
        assert_eq!(e.selection(), Some((8, 9)));
        assert_eq!(e.message.as_deref(), Some("Wrapped"));
    }

    #[test]
    fn replace_one_replaces_selected_match_then_finds_next() {
        let mut e = ed("x x x");
        e.search = b("x");
        e.replace_with = b("yy");
        e.search_case_sensitive = true;
        assert!(e.find_next(true));
        assert!(e.replace_one());
        assert_eq!(text(&e), "yy x x");
        assert_eq!(e.selection(), Some((3, 4)), "next match selected");
        e.undo();
        assert_eq!(text(&e), "x x x");
    }

    #[test]
    fn replace_one_without_matching_selection_only_finds() {
        let mut e = ed("abc x");
        e.search = b("x");
        e.replace_with = b("Q");
        e.search_case_sensitive = true;
        assert!(e.replace_one());
        assert_eq!(text(&e), "abc x", "nothing replaced yet");
        assert_eq!(e.selection(), Some((4, 5)));
        assert!(
            !e.replace_one(),
            "replaces the match, then no further match"
        );
        assert_eq!(text(&e), "abc Q");
        assert_eq!(e.selection(), None);
        assert!(!e.replace_one());
    }

    #[test]
    fn replace_all_counts_and_is_single_undo_step() {
        let mut e = ed("ab ab ab");
        e.search = b("ab");
        e.replace_with = b("\u{65e5}");
        e.search_case_sensitive = true;
        assert_eq!(e.replace_all(), 3);
        assert_eq!(text(&e), "\u{65e5} \u{65e5} \u{65e5}");
        e.undo();
        assert_eq!(text(&e), "ab ab ab");
        e.undo();
        assert_eq!(text(&e), "ab ab ab", "second undo has nothing to do");
    }

    #[test]
    fn replace_all_zero_matches_and_empty_search() {
        let mut e = ed("abc");
        e.search = b("zz");
        assert_eq!(e.replace_all(), 0);
        assert_eq!(text(&e), "abc");
        e.search.clear();
        assert_eq!(e.replace_all(), 0);
        assert!(!e.buf.is_dirty());
    }

    #[test]
    fn replace_all_maps_cursor_past_replacements() {
        let mut e = ed("a a end");
        e.search = b("a");
        e.replace_with = b("xxx");
        e.search_case_sensitive = true;
        e.set_cursor(6, false);
        e.replace_all();
        assert_eq!(text(&e), "xxx xxx end");
        assert_eq!(e.cursor, 10);
    }

    #[test]
    fn replace_all_can_delete_and_insert_newlines() {
        let mut e = ed("a,b,c");
        e.search = b(",");
        e.replace_with = b("\n");
        e.search_case_sensitive = true;
        assert_eq!(e.replace_all(), 2);
        assert_eq!(text(&e), "a\nb\nc");
        assert_eq!(e.buf.line_count(), 3);
    }

    // ----- goto, view, scrolling -------------------------------------------

    #[test]
    fn goto_line_clamps_line_and_column() {
        let mut e = ed("abc\ndef");
        e.goto_line(2, 3);
        assert_eq!(e.cursor, 6);
        e.goto_line(99, 1);
        assert_eq!(e.cursor, 4, "last line");
        e.goto_line(99, 99);
        assert_eq!(e.cursor, 7, "column clamped to line end");
        e.goto_line(0, 0);
        assert_eq!(e.cursor, 0);
        assert_eq!(e.selection(), None);
    }

    #[test]
    fn cursor_line_col_reports_visual_column() {
        let mut e = ed("\tx");
        e.set_cursor(1, false);
        assert_eq!(e.cursor_line_col(), (0, 4));
    }

    #[test]
    fn visual_col_and_pos_from_visual_round_trip_with_tabs() {
        let e = ed("a\tb\t\tc\né\tx\n\t\t");
        let line_text: [&str; 3] = ["a\tb\t\tc", "é\tx", "\t\t"];
        let mut line_start = 0;
        for (line, lt) in line_text.iter().enumerate() {
            assert_eq!(e.buf.line_start(line), line_start);
            for (off, _) in lt.char_indices().chain(std::iter::once((lt.len(), ' '))) {
                let pos = line_start + off;
                let vc = e.visual_col(pos);
                assert_eq!(e.pos_from_visual(line, vc), pos, "line {line} offset {off}");
            }
            line_start += lt.len() + 1;
        }
        assert_eq!(e.visual_col(1), 1);
        assert_eq!(e.visual_col(2), 4, "after a tab at col 1 is col 4");
        assert_eq!(e.visual_col(3), 5);
        assert_eq!(
            e.visual_col(5),
            12,
            "c is at visual column 12 after two tabs"
        );
    }

    #[test]
    fn pos_from_visual_clamps_to_line_end() {
        let e = ed("ab\ncd");
        assert_eq!(e.pos_from_visual(0, 50), 2);
        assert_eq!(e.pos_from_visual(99, 1), 4);
    }

    #[test]
    fn ensure_cursor_visible_vertical_margin() {
        let text: String = (0..50).map(|i| format!("l{i}\n")).collect();
        let mut e = ed(&text);
        e.goto_line(41, 1); // line 40
        e.ensure_cursor_visible(10, 80);
        assert_eq!(e.scroll_line, 32, "cursor at bottom row minus 1 margin");
        let line = e.buf.line_of(e.cursor);
        assert!(line > e.scroll_line && line + 1 < e.scroll_line + 10);
        e.goto_line(1, 1);
        e.ensure_cursor_visible(10, 80);
        assert_eq!(e.scroll_line, 0);
        e.scroll_line = 20;
        e.goto_line(21, 1); // line 20
        e.ensure_cursor_visible(10, 80);
        assert_eq!(e.scroll_line, 19, "top margin");
    }

    #[test]
    fn ensure_cursor_visible_small_viewport_has_no_margin() {
        let text: String = (0..10).map(|i| format!("l{i}\n")).collect();
        let mut e = ed(&text);
        e.goto_line(5, 1); // line 4
        e.ensure_cursor_visible(2, 80);
        assert_eq!(e.scroll_line, 3);
        e.ensure_cursor_visible(2, 80);
        assert_eq!(e.scroll_line, 3, "stable when already visible");
    }

    #[test]
    fn ensure_cursor_visible_horizontal_jumps() {
        let mut e = ed(&"x".repeat(100));
        e.set_cursor(100, false);
        e.ensure_cursor_visible(10, 40);
        assert_eq!(e.scroll_col, 70, "jumps in multiples of cols/4 = 10");
        let vc = e.visual_col(e.cursor);
        assert!(vc >= e.scroll_col && vc < e.scroll_col + 40);
        e.set_cursor(5, false);
        e.ensure_cursor_visible(10, 40);
        assert_eq!(e.scroll_col, 0);
    }

    #[test]
    fn scroll_by_clamps() {
        let mut e = ed("a\nb\nc");
        e.scroll_by(10);
        assert_eq!(e.scroll_line, 2);
        e.scroll_by(-10);
        assert_eq!(e.scroll_line, 0);
    }

    // ----- load, save, title, dirty ---------------------------------------

    #[test]
    fn title_and_dirty_tracking() {
        let mut e = Editor::new(&Config::default());
        assert_eq!(e.title(), "Untitled - note");
        assert!(!e.is_dirty());
        type_str(&mut e, "!");
        assert_eq!(e.title(), "*Untitled - note");
        assert!(e.is_dirty());
        e.load("C:\\docs\\notes.txt", b"hi");
        assert_eq!(
            e.title(),
            "notes.txt - note",
            "file name only, clean after load"
        );
        e.insert_char('x');
        assert_eq!(e.title(), "*notes.txt - note");
        e.save_bytes();
        e.mark_saved();
        assert_eq!(e.title(), "notes.txt - note");
        assert!(!e.is_dirty());
        e.undo();
        assert!(e.is_dirty(), "undo away from the saved state is dirty");
    }

    #[test]
    fn title_uses_forward_slashes_too() {
        let mut e = Editor::new(&Config::default());
        e.load("/home/u/readme.md", b"");
        assert_eq!(e.title(), "readme.md - note");
    }

    #[test]
    fn save_as_adds_the_default_extension_only_when_missing() {
        let txt = Some("txt");
        assert_eq!(save_as_path("C:\\d\\asdf", txt), "C:\\d\\asdf.txt");
        assert_eq!(save_as_path("C:\\d\\asdf.md", txt), "C:\\d\\asdf.md");
        assert_eq!(save_as_path("C:\\d\\asdf.txt", txt), "C:\\d\\asdf.txt");
        assert_eq!(save_as_path("C:\\d\\a.b.json", txt), "C:\\d\\a.b.json");
        assert_eq!(save_as_path("C:\\d\\.gitignore", txt), "C:\\d\\.gitignore");
        // A dot in a folder name is not the file's extension.
        assert_eq!(save_as_path("C:\\my.dir\\notes", txt), "C:\\my.dir\\notes.txt");
        assert_eq!(save_as_path("/home/u.v/notes", txt), "/home/u.v/notes.txt");
        // Text after the last dot that contains a space is not an extension.
        assert_eq!(save_as_path("C:\\d\\v1.2 notes", txt), "C:\\d\\v1.2 notes.txt");
    }

    #[test]
    fn save_as_trailing_dot_means_no_extension() {
        assert_eq!(save_as_path("C:\\d\\asdf.", Some("txt")), "C:\\d\\asdf");
        assert_eq!(save_as_path("C:\\d\\asdf...", Some("txt")), "C:\\d\\asdf");
        assert_eq!(save_as_path("C:\\d\\asdf.", None), "C:\\d\\asdf");
        assert_eq!(save_as_path("C:\\d\\..", Some("txt")), "C:\\d\\..");
    }

    #[test]
    fn save_as_without_default_extension_keeps_the_name() {
        assert_eq!(save_as_path("C:\\d\\LICENSE", None), "C:\\d\\LICENSE");
        assert_eq!(save_as_path("C:\\d\\asdf.md", None), "C:\\d\\asdf.md");
    }

    #[test]
    fn load_resets_state_and_sets_language() {
        let mut e = ed("hello");
        e.set_cursor(3, true);
        e.mark_mode = true;
        e.preferred_col = Some(2);
        e.load("code.rs", b"fn main() {}\n");
        assert_eq!(e.cursor, 0);
        assert_eq!(e.anchor, None);
        assert!(!e.mark_mode);
        assert_eq!(e.preferred_col, None);
        assert_eq!(e.highlighter.lang(), Lang::from_path("code.rs"));
        assert_eq!(e.path.as_deref(), Some("code.rs"));
        assert!(!e.buf.is_dirty());
    }

    #[test]
    fn crlf_file_round_trips() {
        let mut e = Editor::new(&Config::default());
        e.load("a.txt", b"one\r\ntwo");
        assert_eq!(text(&e), "one\ntwo");
        assert_eq!(e.line_ending, LineEnding::CrLf);
        assert_eq!(e.save_bytes(), b"one\r\ntwo".to_vec());
    }

    #[test]
    fn lf_file_round_trips_and_bom_is_kept() {
        let mut e = Editor::new(&Config::default());
        e.load("a.txt", b"\xEF\xBB\xBFx\ny");
        assert_eq!(e.encoding, Encoding::Utf8Bom);
        assert_eq!(text(&e), "x\ny");
        assert_eq!(e.line_ending, LineEnding::Lf, "first newline is bare LF");
        assert_eq!(e.save_bytes(), b"\xEF\xBB\xBFx\ny".to_vec());
    }

    #[test]
    fn save_bytes_does_not_mark_clean_until_mark_saved() {
        let mut e = ed("ab");
        e.set_cursor(2, false);
        type_str(&mut e, "c");
        let out = e.save_bytes();
        assert_eq!(out, b"abc".to_vec());
        assert!(e.buf.is_dirty(), "a failed write must leave the buffer dirty");
        assert_eq!(e.save_bytes(), b"abc".to_vec(), "encoding is repeatable");
        e.mark_saved();
        assert!(!e.buf.is_dirty());
    }

    #[test]
    fn load_owned_matches_load() {
        let file = b"\xEF\xBB\xBFx\r\ny\r\n".to_vec();
        let mut a = Editor::new(&Config::default());
        a.load("t.txt", &file);
        let mut b = Editor::new(&Config::default());
        b.load_owned("t.txt", file);
        assert_eq!(text(&a), text(&b));
        assert_eq!(a.encoding, b.encoding);
        assert_eq!(a.line_ending, b.line_ending);
        assert_eq!(a.save_bytes(), b.save_bytes());
        assert!(!b.buf.is_dirty());
    }

    // ----- misc invariants -------------------------------------------------

    #[test]
    fn select_all_and_selected_text() {
        let mut e = ed("héllo");
        e.select_all();
        assert_eq!(e.selection(), Some((0, 6)));
        assert_eq!(e.selected_text(), Some(b("héllo")));
    }

    #[test]
    fn cursor_stays_on_char_boundary_after_edits() {
        let mut e = ed("日本");
        // 日 occupies bytes 0..3, so byte 4 is inside 本 and must be aligned back to 3.
        e.set_cursor(4, false);
        assert_eq!(e.cursor, 3, "set_cursor aligns to a char start");
        e.backspace();
        assert_eq!(text(&e), "本");
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn insert_char_multibyte() {
        let mut e = ed("");
        type_str(&mut e, "é日");
        assert_eq!(text(&e), "é日");
        assert_eq!(e.cursor, 5);
        assert_eq!(e.visual_col(e.cursor), 2);
    }

    #[test]
    fn scroll_and_dirty_lines_do_not_panic_on_empty_doc() {
        let mut e = ed("");
        e.move_cursor(Move::Down, false);
        e.move_cursor(Move::Up, false);
        e.move_cursor(Move::PageDown(5), true);
        e.move_cursor(Move::LineHome, false);
        e.indent();
        e.unindent();
        e.duplicate_line();
        e.move_line(true);
        e.move_line(false);
        e.toggle_comment();
        assert!(!e.find_next(false));
        assert_eq!(e.replace_all(), 0);
        assert_eq!(e.cut_selection(), None);
        e.delete_forward();
        e.backspace();
        assert_eq!(text(&e), "");
    }

    // ----- scrollbar helpers ----------------------------------------------

    #[test]
    fn overflow_is_false_when_everything_fits_at_the_top() {
        let e = ed("a\nb\nc");
        assert!(!e.overflows_view(3));
        assert!(!e.overflows_view(10));
        assert!(e.overflows_view(2));
    }

    #[test]
    fn overflow_stays_true_once_scrolled_even_if_lines_fit() {
        let mut e = ed("a\nb\nc");
        e.scroll_line = 1;
        assert!(e.overflows_view(10));
    }

    #[test]
    fn wrapped_overflow_counts_visual_rows() {
        // "abcdefgh" at width 8 is one row; "abcdefghi" wraps to two.
        let one = wrapped("abcdefgh", 8);
        assert!(!one.overflows_view(1));
        let two = wrapped("abcdefghi\n", 8);
        assert!(!two.overflows_view(3));
        assert!(two.overflows_view(2));
        let many = wrapped(&"word ".repeat(200), 20);
        assert!(many.overflows_view(5));
        assert!(!many.overflows_view(500));
    }

    #[test]
    fn longest_visible_measures_only_the_view() {
        let mut e = ed("ab\nabcdef\nx\nlonger line here");
        assert_eq!(e.longest_visible(2), 6);
        assert_eq!(e.longest_visible(4), 16);
        e.scroll_line = 2;
        assert_eq!(e.longest_visible(1), 1);
        e.scroll_line = 3;
        assert_eq!(e.longest_visible(9), 16);
    }

    #[test]
    fn longest_visible_expands_tabs() {
        let e = ed("\tx");
        assert_eq!(e.longest_visible(1), 5);
    }

    // ----- soft wrap -----------------------------------------------------

    /// Editor on `text` with soft wrap on at `width` columns.
    fn wrapped(text: &str, width: usize) -> Editor {
        let mut e = ed(text);
        e.wrap = true;
        e.wrap_cols = width;
        e
    }

    /// Text of each visual row of `line` when wrapped at `width`.
    fn rows_at(e: &Editor, line: usize, width: usize) -> Vec<String> {
        let mut starts = Vec::new();
        e.wrap_rows(line, width, &mut starts);
        let le = e.buf.line_end(line);
        (0..starts.len())
            .map(|k| {
                let end = starts.get(k + 1).copied().unwrap_or(le);
                String::from_utf8(e.buf.slice(starts[k], end)).expect("utf-8")
            })
            .collect()
    }

    #[test]
    fn wrap_rows_breaks_after_the_last_word_that_fits() {
        let e = ed("aaa bbb ccc ddd eee");
        assert_eq!(rows_at(&e, 0, 8), ["aaa bbb ", "ccc ddd ", "eee"]);
        assert_eq!(rows_at(&e, 0, 7), ["aaa bbb ", "ccc ddd ", "eee"]);
        let mut starts = Vec::new();
        e.wrap_rows(0, 8, &mut starts);
        assert_eq!(starts, vec![0, 8, 16]);
    }

    #[test]
    fn wrap_rows_hard_breaks_long_unbreakable_word() {
        let e = ed("abcdefghijkl");
        assert_eq!(rows_at(&e, 0, 5), ["abcde", "fghij", "kl"]);
        // A long word after a short one: the short word stays, the long one is cut.
        let e = ed("ab abcdefghijk cd");
        let rows = rows_at(&e, 0, 6);
        assert_eq!(rows[0], "ab ", "break before the long word");
        assert_eq!(rows.concat(), "ab abcdefghijk cd", "rows cover the whole line");
        assert!(rows.iter().skip(1).all(|r| r.chars().count() <= 6));
    }

    #[test]
    fn wrap_rows_trailing_spaces_hang_past_the_edge() {
        let e = ed("aaa      bbb");
        assert_eq!(rows_at(&e, 0, 8), ["aaa      ", "bbb"]);
        let e = ed("          ");
        assert_eq!(rows_at(&e, 0, 8), ["          "], "a blank run never breaks");
    }

    #[test]
    fn wrap_rows_tabs_stop_relative_to_the_row_start() {
        // Tab stops are every 4 cells from the row start, not from the line start.
        let e = ed("ab\tcd ef");
        assert_eq!(rows_at(&e, 0, 8), ["ab\tcd ", "ef"]);
        let mut e = ed("aaaaaaaa b\tc");
        e.wrap = true;
        e.wrap_cols = 8;
        assert_eq!(rows_at(&e, 0, 8), ["aaaaaaaa ", "b\tc"]);
        // Cursor on 'c' in the second row: 'b' is 1 cell, the tab takes 3 more, so col 4.
        e.set_cursor(11, false);
        assert_eq!(e.cursor_view_pos(), Some((1, 4)));
    }

    #[test]
    fn wrap_rows_empty_lines_and_exact_width() {
        let e = ed("aaaa\n\nbb");
        assert_eq!(rows_at(&e, 1, 8), [""], "empty line is one row");
        let mut starts = Vec::new();
        e.wrap_rows(1, 8, &mut starts);
        assert_eq!(starts, vec![5]);

        let e = ed("abcdefgh");
        assert_eq!(rows_at(&e, 0, 8), ["abcdefgh"], "exactly W wide is one row");
        let e = ed("abcdefghi");
        assert_eq!(rows_at(&e, 0, 8), ["abcdefgh", "i"]);
    }

    #[test]
    fn wrap_rows_multibyte_chars_count_one_cell_and_never_split() {
        let e = ed("héllo wörld");
        assert_eq!(rows_at(&e, 0, 8), ["héllo ", "wörld"]);
        let e = ed("ééééééééé");
        assert_eq!(rows_at(&e, 0, 8), ["éééééééé", "é"]);
        // Row starts land on char boundaries.
        let mut starts = Vec::new();
        e.wrap_rows(0, 8, &mut starts);
        assert_eq!(starts, vec![0, 16]);
    }

    #[test]
    fn wrap_rows_reuses_out_without_growing_it() {
        let e = ed("aaa bbb ccc ddd eee fff ggg hhh iii");
        let mut out = Vec::with_capacity(64);
        e.wrap_rows(0, 8, &mut out);
        let cap = out.capacity();
        for _ in 0..5 {
            e.wrap_rows(0, 8, &mut out);
        }
        assert_eq!(out.capacity(), cap);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn wrap_off_is_the_default_and_config_turns_it_on() {
        let e = ed("text");
        assert!(!e.wrap);
        let cfg = Config {
            word_wrap: true,
            ..Config::default()
        };
        assert!(Editor::new(&cfg).wrap);
    }

    #[test]
    fn wrap_width_has_a_floor_of_eight() {
        let mut e = ed("x");
        e.wrap_cols = 3;
        assert_eq!(e.wrap_width(), 8);
        e.wrap_cols = 40;
        assert_eq!(e.wrap_width(), 40);
    }

    #[test]
    fn wrapped_up_and_down_cross_rows_and_lines_keeping_the_column() {
        // Rows of line 0 at width 8: "aaa bbb " (0), "ccc ddd " (8), "eee" (16). Line 1 starts at 20.
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        e.set_cursor(5, false); // column 5 of row 0
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 13, "column 5 of the second row");
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 19, "third row is 3 wide: clamped to its end");
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 22, "next line, row 0 is \"xy\": clamped to its end");
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 19, "back to the third row, same clamp");
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 13);
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 5, "the column survived every clamp");
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 0, "row 0 of the document: up goes to the start");
        e.move_cursor(Move::Up, false);
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn wrapped_down_steps_one_row_each_then_stops_at_the_end() {
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        let mut seen = vec![e.cursor];
        for _ in 0..3 {
            e.move_cursor(Move::Down, false);
            seen.push(e.cursor);
        }
        assert_eq!(seen, vec![0, 8, 16, 20]);
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 22);
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 22, "at the document end, down goes to the end");
    }

    #[test]
    fn wrapped_page_moves_by_visual_rows() {
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        e.move_cursor(Move::PageDown(2), false);
        assert_eq!(e.cursor, 16, "two rows down");
        e.move_cursor(Move::PageDown(10), false);
        assert_eq!(e.cursor, 20, "past the end clamps to the last row, column 0");
        e.move_cursor(Move::PageUp(2), false);
        assert_eq!(e.cursor, 8, "two rows up from line 1 row 0 is row 1 of line 0");
        e.move_cursor(Move::PageUp(2), false);
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn wrapped_home_goes_to_row_start_then_line_start() {
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        e.set_cursor(13, false); // row 1
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 8, "row start");
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 0, "pressed again at the row start: logical line start");
    }

    #[test]
    fn wrapped_home_on_first_row_is_smart() {
        let mut e = wrapped("    word wrapped here", 8);
        e.set_cursor(6, false);
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 4, "first non-blank");
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 0, "then column 0");
        e.move_cursor(Move::LineHome, false);
        assert_eq!(e.cursor, 4);
    }

    #[test]
    fn wrapped_end_goes_to_row_end_and_draws_there() {
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 8, "end of the first row, after its trailing space");
        assert_eq!(e.cursor_view_pos(), Some((0, 8)), "drawn at the end of row 0, not row 1");
        // From row 1, End goes to the end of row 1; pressed again it stays there.
        e.set_cursor(13, false);
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 16, "end of the second row");
        assert_eq!(e.cursor_view_pos(), Some((1, 8)));
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 16, "already at the row end");
        // The last row of the line ends at the newline.
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 19, "down keeps the column 8, clamped to the third row");
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 19);
        assert_eq!(e.cursor_view_pos(), Some((2, 3)));
    }

    #[test]
    fn wrapped_typing_after_end_appends_to_the_row() {
        let mut e = wrapped("aaa bbb ccc", 8);
        e.move_cursor(Move::LineEnd, false);
        assert_eq!(e.cursor, 8);
        e.insert_char('X');
        assert_eq!(text(&e), "aaa bbb Xccc");
        assert_eq!(e.cursor, 9);
    }

    #[test]
    fn wrapped_movement_clears_the_end_flag() {
        let mut e = wrapped("aaa bbb ccc ddd eee", 8);
        e.move_cursor(Move::LineEnd, false);
        e.move_cursor(Move::Left, false);
        e.move_cursor(Move::Right, false);
        assert_eq!(e.cursor, 8);
        assert_eq!(e.cursor_view_pos(), Some((1, 0)), "a plain move shows the start of row 1");
    }

    #[test]
    fn ensure_visible_wrapped_scrolls_to_a_cursor_below_the_view() {
        // Ten rows of "abcdefgh " (9 bytes each), starting at 0, 9, ..., 81.
        let text = "abcdefgh ".repeat(10);
        let mut e = wrapped(&text, 8);
        e.set_cursor(85, false);
        e.ensure_cursor_visible(3, 80);
        assert_eq!((e.scroll_line, e.scroll_subrow), (0, 8), "cursor row sits one row above the bottom");
        assert_eq!(e.cursor_view_pos(), Some((1, 4)));
        assert_eq!(e.scroll_col, 0);
        e.ensure_cursor_visible(3, 80);
        assert_eq!((e.scroll_line, e.scroll_subrow), (0, 8), "stable when visible");

        e.set_cursor(0, false);
        e.ensure_cursor_visible(3, 80);
        assert_eq!((e.scroll_line, e.scroll_subrow), (0, 0), "cursor above the view: top margin");
    }

    #[test]
    fn ensure_visible_wrapped_handles_a_cursor_far_below_quickly() {
        let text: String = (0..100_000).map(|i| format!("line {i} with words\n")).collect();
        let mut e = ed(&text);
        e.wrap = true;
        e.wrap_cols = 40;
        let last = e.buf.line_start(99_999);
        e.set_cursor(last, false);
        let t = std::time::Instant::now();
        e.ensure_cursor_visible(30, 80);
        let took = t.elapsed();
        assert!(took.as_millis() < 500, "took {took:?}");
        // One row per line here, so the top is 28 lines above the cursor (30 rows, bottom margin 1).
        assert_eq!((e.scroll_line, e.scroll_subrow), (99_971, 0));
        assert_eq!(e.cursor_view_pos(), Some((28, 0)));
    }

    #[test]
    fn cursor_view_pos_is_none_above_the_view_and_far_below() {
        let text: String = (0..20_000).map(|i| format!("{i}\n")).collect();
        let mut e = ed(&text);
        e.wrap = true;
        e.wrap_cols = 40;
        e.set_cursor(e.buf.line_start(19_000), false);
        assert_eq!(e.cursor_view_pos(), None, "more than a screen below the top");
        e.scroll_line = 18_990;
        assert_eq!(e.cursor_view_pos(), Some((10, 0)));
        e.scroll_line = 19_005;
        assert_eq!(e.cursor_view_pos(), None, "above the top");
    }

    #[test]
    fn pos_from_view_and_cursor_view_pos_round_trip() {
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        // Each char boundary, from the top of the view at each of the first three rows.
        let boundaries: Vec<usize> = (0..=22)
            .filter(|&p| p == 22 || e.buf.byte_at(p) & 0xC0 != 0x80)
            .collect();
        for top in 0..3 {
            e.scroll_line = 0;
            e.scroll_subrow = top;
            for &pos in &boundaries {
                e.set_cursor(pos, false);
                let Some((row, col)) = e.cursor_view_pos() else {
                    continue;
                };
                assert_eq!(e.pos_from_view(row, col, 100), pos, "top row {top}, pos {pos}");
            }
        }
        e.scroll_subrow = 0;
        e.set_cursor(16, false);
        assert_eq!(e.cursor_view_pos(), Some((2, 0)));
        assert_eq!(e.pos_from_view(2, 0, 100), 16);
        // Past the end of a row: the position is the last char of that row.
        assert_eq!(e.pos_from_view(0, 20, 100), 7);
        // Below the last row: the last row of the document, at the requested column.
        assert_eq!(e.pos_from_view(50, 0, 100), 20);
        assert_eq!(e.pos_from_view(50, 1, 100), 21);
    }

    #[test]
    fn pos_from_view_without_wrap_matches_visual_columns() {
        let mut e = ed("abc\tdef\nxy");
        for pos in [0usize, 1, 2, 4, 5, 7, 8] {
            e.set_cursor(pos, false);
            let (row, col) = e.cursor_view_pos().expect("visible");
            assert_eq!(e.pos_from_view(row, col, 100), pos);
        }
        e.set_cursor(7, false);
        assert_eq!(e.cursor_view_pos(), Some((0, 7)));
        assert_eq!(e.pos_from_view(1, 1, 100), 9);
        e.scroll_col = 4;
        assert_eq!(e.cursor_view_pos(), Some((0, 3)), "column relative to the scroll");
        e.set_cursor(0, false);
        assert_eq!(e.cursor_view_pos(), None, "left of the horizontal scroll");
    }

    #[test]
    fn scroll_by_steps_visual_rows_across_lines() {
        // Rows: line 0 has three (0, 8, 16), line 1 one (20), line 2 one (23).
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy\nzz", 8);
        e.scroll_by(2);
        assert_eq!((e.scroll_line, e.scroll_subrow), (0, 2));
        e.scroll_by(2);
        assert_eq!((e.scroll_line, e.scroll_subrow), (2, 0), "crosses line 1 and lands on line 2");
        e.scroll_by(-1);
        assert_eq!((e.scroll_line, e.scroll_subrow), (1, 0));
        e.scroll_by(-5);
        assert_eq!((e.scroll_line, e.scroll_subrow), (0, 0), "clamps at the top");
        e.scroll_by(100);
        assert_eq!((e.scroll_line, e.scroll_subrow), (2, 0), "clamps at the last row");
    }

    #[test]
    fn scroll_by_steps_logical_lines_without_wrap() {
        let mut e = ed("a\nb\nc\nd");
        e.scroll_by(2);
        assert_eq!(e.scroll_line, 2);
        assert_eq!(e.scroll_subrow, 0);
    }

    #[test]
    fn toggling_wrap_off_keeps_the_nowrap_behaviour() {
        let mut e = wrapped("aaa bbb ccc ddd eee\nxy", 8);
        e.wrap = false;
        e.set_cursor(5, false);
        e.move_cursor(Move::Down, false);
        assert_eq!(e.cursor, 22, "without wrap, Down goes to the next logical line, column 5 clamped");
        assert_eq!(e.cursor_view_pos(), Some((1, 2)));
    }
}
