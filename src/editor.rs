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
    /// nano ^6: every movement extends the selection.
    pub mark_mode: bool,
    /// Visual column remembered across up/down moves.
    pub preferred_col: Option<usize>,
    pub scroll_line: usize,
    pub scroll_col: usize,
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
        let d = encoding::decode(bytes);
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
        self.last_was_cut_line = false;
        self.message = None;
    }

    /// Encodes the document with the current encoding and line ending, and marks it clean.
    pub fn save_bytes(&mut self) -> Vec<u8> {
        let out = encoding::encode(self.buf.make_contiguous(), self.encoding, self.line_ending);
        self.buf.mark_clean();
        out
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
    }

    pub fn move_cursor(&mut self, m: Move, extend: bool) {
        let extend = extend || self.mark_mode;
        let cur = self.cursor;
        let sel = self.selection();
        let keeps_col = matches!(
            m,
            Move::Up | Move::Down | Move::PageUp(_) | Move::PageDown(_)
        );
        let target = match m {
            Move::Left => match sel {
                Some((s, _)) if !extend => s,
                _ => prev_char(&self.buf, cur),
            },
            Move::Right => match sel {
                Some((_, e)) if !extend => e,
                _ => next_char(&self.buf, cur),
            },
            Move::LineHome => {
                let line = self.buf.line_of(cur);
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
            Move::LineEnd => self.buf.line_end(self.buf.line_of(cur)),
            Move::WordLeft => word_left(&self.buf, cur),
            Move::WordRight => word_right(&self.buf, cur),
            Move::DocStart => 0,
            Move::DocEnd => self.buf.len(),
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
        if !keeps_col {
            self.preferred_col = None;
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

    pub fn scroll_by(&mut self, lines: isize) {
        let max = (self.buf.line_count() - 1) as isize;
        self.scroll_line = (self.scroll_line as isize)
            .saturating_add(lines)
            .clamp(0, max) as usize;
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
        | Lang::Zig
        | Lang::Css => "//",
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
    fn save_marks_clean_and_writes_current_text() {
        let mut e = ed("ab");
        e.set_cursor(2, false);
        type_str(&mut e, "c");
        let out = e.save_bytes();
        assert_eq!(out, b"abc".to_vec());
        assert!(!e.buf.is_dirty());
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
}
