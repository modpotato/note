//! Gap buffer of UTF-8 bytes with an incrementally maintained line index and
//! grouped undo/redo. See DESIGN.md (buffer.rs).
//!
//! All positions are byte offsets into the logical text. Out-of-range positions
//! are clamped rather than panicking.

use crate::simd::{count_byte, newline_positions};

/// Minimum free gap kept after any growth.
const MIN_GAP: usize = 4096;

/// One primitive change: `deleted` bytes were removed at `pos`, then `inserted`
/// bytes were placed there. Pure inserts have empty `deleted`, pure deletes
/// have empty `inserted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub pos: usize,
    pub deleted: Vec<u8>,
    pub inserted: Vec<u8>,
}

impl Edit {
    fn new(pos: usize, deleted: &[u8], inserted: &[u8]) -> Self {
        Edit { pos, deleted: deleted.to_vec(), inserted: inserted.to_vec() }
    }
}

/// One undo step: a run of edits that undo/redo together.
#[derive(Debug)]
struct Group {
    /// Unique per mutation of this group (new group or any merge into it).
    /// Used for dirty tracking; ids never repeat so a state is identified by its stack.
    id: u64,
    edits: Vec<Edit>,
    cursor_before: usize,
    cursor_after: usize,
    /// Created inside begin_compound/end_compound; never coalesced into.
    compound: bool,
}

pub struct Buffer {
    /// Physical storage. `buf.len()` is the capacity; `[gap_start, gap_end)` is the gap.
    buf: Vec<u8>,
    gap_start: usize,
    gap_end: usize,
    /// Logical byte offset of each line start. Always non-empty, `line_starts[0] == 0`.
    line_starts: Vec<usize>,
    undo: Vec<Group>,
    redo: Vec<Group>,
    /// begin_compound nesting depth.
    depth: usize,
    /// Set by break_undo_group / compound boundaries: next plain edit starts a new group.
    break_group: bool,
    /// True while the group at the top of `undo` is the open compound group.
    compound_open: bool,
    cursor_hint: Option<usize>,
    next_id: u64,
    /// Id of the top group when mark_clean was last called (0 = empty stack).
    saved_id: u64,
    version_ctr: u64,
    /// Smallest line touched since the last take_first_dirty_line.
    dirty_line: usize,
    /// Reused scratch for newline positions during insert.
    nl_scratch: Vec<usize>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// True if `s` is exactly one Unicode scalar value.
fn is_single_char(s: &[u8]) -> bool {
    match std::str::from_utf8(s) {
        Ok(t) => {
            let mut it = t.chars();
            it.next().is_some() && it.next().is_none()
        }
        Err(_) => false,
    }
}

/// Try to fold a new edit into `last` (the most recent edit of an undo group).
/// Returns true and mutates `last` only on success.
fn coalesce(last: &mut Edit, pos: usize, deleted: &[u8], inserted: &[u8]) -> bool {
    if last.deleted.is_empty() && !last.inserted.is_empty() {
        // Typing run: the new edit must be a single char directly after the run.
        if !deleted.is_empty() || !is_single_char(inserted) || pos != last.pos + last.inserted.len() {
            return false;
        }
        let c = inserted[0];
        let prev = last.inserted[last.inserted.len() - 1];
        if c == b'\n' || (c == b' ' && prev != b' ') {
            return false;
        }
        last.inserted.extend_from_slice(inserted);
        true
    } else if last.inserted.is_empty() && !last.deleted.is_empty() {
        // Deletion run: backspace (new char sits left of the run) or forward delete
        // (new char sits at the same position as the run).
        if !inserted.is_empty() || !is_single_char(deleted) {
            return false;
        }
        let c = deleted[0];
        if c == b'\n' {
            return false;
        }
        if pos + deleted.len() == last.pos {
            if c == b' ' && last.deleted[0] != b' ' {
                return false;
            }
            last.deleted.splice(0..0, deleted.iter().copied());
            last.pos = pos;
            true
        } else if pos == last.pos {
            if c == b' ' && last.deleted[last.deleted.len() - 1] != b' ' {
                return false;
            }
            last.deleted.extend_from_slice(deleted);
            true
        } else {
            false
        }
    } else {
        false
    }
}

impl Buffer {
    pub fn new() -> Self {
        Self::from_text(Vec::new())
    }

    /// Takes ownership of `text`, reusing its allocation as the gap buffer's storage.
    pub fn from_text(mut text: Vec<u8>) -> Self {
        let len = text.len();
        let mut line_starts = Vec::with_capacity(count_byte(&text, b'\n') + 1);
        line_starts.push(0);
        newline_positions(&text, 1, &mut line_starts);
        text.resize(len + MIN_GAP, 0);
        Buffer {
            buf: text,
            gap_start: len,
            gap_end: len + MIN_GAP,
            line_starts,
            undo: Vec::new(),
            redo: Vec::new(),
            depth: 0,
            break_group: false,
            compound_open: false,
            cursor_hint: None,
            next_id: 0,
            saved_id: 0,
            version_ctr: 0,
            dirty_line: usize::MAX,
            nl_scratch: Vec::new(),
        }
    }

    // ----- gap management -------------------------------------------------

    /// Logical length.
    pub fn len(&self) -> usize {
        self.buf.len() - (self.gap_end - self.gap_start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Move the gap so that it starts at logical offset `pos` (must be <= len).
    fn move_gap(&mut self, pos: usize) {
        if pos < self.gap_start {
            let d = self.gap_start - pos;
            self.buf.copy_within(pos..self.gap_start, self.gap_end - d);
            self.gap_start = pos;
            self.gap_end -= d;
        } else if pos > self.gap_start {
            let d = pos - self.gap_start;
            self.buf.copy_within(self.gap_end..self.gap_end + d, self.gap_start);
            self.gap_start = pos;
            self.gap_end += d;
        }
    }

    /// Ensure the gap holds at least `n` bytes. Gap must already sit at the insertion point.
    fn ensure_gap(&mut self, n: usize) {
        if self.gap_end - self.gap_start >= n {
            return;
        }
        let old_cap = self.buf.len();
        let tail = old_cap - self.gap_end;
        let len = self.len();
        let new_cap = (old_cap * 2).max(len + n + MIN_GAP);
        self.buf.resize(new_cap, 0);
        self.buf.copy_within(self.gap_end..old_cap, new_cap - tail);
        self.gap_end = new_cap - tail;
    }

    // ----- reading --------------------------------------------------------

    /// Byte at logical offset `pos`, or 0 if out of range.
    pub fn byte_at(&self, pos: usize) -> u8 {
        if pos >= self.len() {
            return 0;
        }
        if pos < self.gap_start {
            self.buf[pos]
        } else {
            self.buf[pos + (self.gap_end - self.gap_start)]
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Start offset of `line` (clamped to the last line).
    pub fn line_start(&self, line: usize) -> usize {
        let last = self.line_starts.len() - 1;
        self.line_starts[line.min(last)]
    }

    /// Offset of the '\n' ending `line`, or len() for the last line.
    pub fn line_end(&self, line: usize) -> usize {
        let n = self.line_starts.len();
        let l = line.min(n - 1);
        if l + 1 < n {
            self.line_starts[l + 1] - 1
        } else {
            self.len()
        }
    }

    /// Line containing byte offset `pos`.
    pub fn line_of(&self, pos: usize) -> usize {
        let p = pos.min(self.len());
        self.line_starts.partition_point(|&s| s <= p).saturating_sub(1)
    }

    /// Copy bytes [start, end) into `out` (cleared first). Positions are clamped.
    pub fn slice_into(&self, start: usize, end: usize, out: &mut Vec<u8>) {
        out.clear();
        let end = end.min(self.len());
        let start = start.min(end);
        let gl = self.gap_end - self.gap_start;
        if end <= self.gap_start {
            out.extend_from_slice(&self.buf[start..end]);
        } else if start >= self.gap_start {
            out.extend_from_slice(&self.buf[start + gl..end + gl]);
        } else {
            out.extend_from_slice(&self.buf[start..self.gap_start]);
            out.extend_from_slice(&self.buf[self.gap_end..end + gl]);
        }
    }

    pub fn slice(&self, start: usize, end: usize) -> Vec<u8> {
        let mut v = Vec::new();
        self.slice_into(start, end, &mut v);
        v
    }

    pub fn text(&self) -> Vec<u8> {
        self.slice(0, self.len())
    }

    /// Moves the gap to the end and returns the whole text as one slice.
    pub fn make_contiguous(&mut self) -> &[u8] {
        let len = self.len();
        self.move_gap(len);
        &self.buf[..len]
    }

    // ----- raw mutation (no undo bookkeeping) -------------------------------

    fn note_dirty(&mut self, line: usize) {
        if line < self.dirty_line {
            self.dirty_line = line;
        }
    }

    fn raw_insert(&mut self, pos: usize, s: &[u8]) {
        if s.is_empty() {
            return;
        }
        let n = s.len();
        let line = self.line_of(pos);
        self.note_dirty(line);
        self.version_ctr += 1;

        self.move_gap(pos);
        self.ensure_gap(n);
        let gs = self.gap_start;
        self.buf[gs..gs + n].copy_from_slice(s);
        self.gap_start += n;

        // Line index: new starts are (pos + 1 + i) for each '\n' at s[i].
        self.nl_scratch.clear();
        newline_positions(s, pos + 1, &mut self.nl_scratch);
        let tail = line + 1;
        for x in &mut self.line_starts[tail..] {
            *x += n;
        }
        if !self.nl_scratch.is_empty() {
            self.line_starts.splice(tail..tail, self.nl_scratch.iter().copied());
        }
    }

    fn raw_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let d = end - start;
        let line = self.line_of(start);
        self.note_dirty(line);
        self.version_ctr += 1;

        // Line starts in (start, end] belonged to lines whose preceding newline was
        // deleted; drop them, then shift everything after by d.
        let lo = self.line_starts.partition_point(|&s| s <= start);
        let hi = self.line_starts.partition_point(|&s| s <= end);
        if hi > lo {
            self.line_starts.drain(lo..hi);
        }
        for x in &mut self.line_starts[lo..] {
            *x -= d;
        }

        self.move_gap(start);
        self.gap_end += d;
    }

    // ----- public editing ---------------------------------------------------

    /// Insert `s` at `pos` (clamped to len).
    pub fn insert(&mut self, pos: usize, s: &[u8]) {
        if s.is_empty() {
            return;
        }
        let pos = pos.min(self.len());
        self.record(pos, &[], s);
        self.raw_insert(pos, s);
    }

    /// Delete [start, end) (clamped).
    pub fn delete(&mut self, start: usize, end: usize) {
        let end = end.min(self.len());
        let start = start.min(end);
        if start == end {
            return;
        }
        let deleted = self.slice(start, end);
        self.record(start, &deleted, &[]);
        self.raw_delete(start, end);
    }

    // ----- undo / redo ------------------------------------------------------

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn top_id(&self) -> u64 {
        self.undo.last().map_or(0, |g| g.id)
    }

    /// Record an edit (called before it is applied, so positions are pre-edit state).
    fn record(&mut self, pos: usize, deleted: &[u8], inserted: &[u8]) {
        self.redo.clear();
        let hint = self.cursor_hint.take();
        let id = self.alloc_id();
        let after = pos + inserted.len();

        if self.depth > 0 {
            if self.compound_open {
                if let Some(g) = self.undo.last_mut() {
                    g.edits.push(Edit::new(pos, deleted, inserted));
                    g.id = id;
                    g.cursor_after = after;
                    return;
                }
            }
            let before = hint.unwrap_or(pos + deleted.len());
            self.undo.push(Group {
                id,
                edits: vec![Edit::new(pos, deleted, inserted)],
                cursor_before: before,
                cursor_after: after,
                compound: true,
            });
            self.compound_open = true;
            return;
        }

        let may_join = !std::mem::take(&mut self.break_group);
        if may_join {
            if let Some(g) = self.undo.last_mut() {
                if !g.compound {
                    if let Some(last) = g.edits.last_mut() {
                        if coalesce(last, pos, deleted, inserted) {
                            g.id = id;
                            g.cursor_after = after;
                            return;
                        }
                    }
                }
            }
        }

        let before = hint.unwrap_or(pos + deleted.len());
        self.undo.push(Group {
            id,
            edits: vec![Edit::new(pos, deleted, inserted)],
            cursor_before: before,
            cursor_after: after,
            compound: false,
        });
    }

    /// Cursor position to restore before the next recorded edit's group.
    pub fn set_cursor_hint(&mut self, cursor: usize) {
        self.cursor_hint = Some(cursor);
    }

    /// Prevent the next single-char edit from joining the previous undo group.
    pub fn break_undo_group(&mut self) {
        self.break_group = true;
    }

    /// Start grouping all edits into one undo step. Nests with a depth counter.
    pub fn begin_compound(&mut self) {
        if self.depth == 0 {
            self.break_group = true;
            self.compound_open = false;
        }
        self.depth += 1;
    }

    pub fn end_compound(&mut self) {
        if self.depth == 0 {
            return;
        }
        self.depth -= 1;
        if self.depth == 0 {
            self.compound_open = false;
            self.break_group = true;
        }
    }

    /// Undo the top group. Returns the cursor position to restore.
    pub fn undo(&mut self) -> Option<usize> {
        let g = self.undo.pop()?;
        for e in g.edits.iter().rev() {
            if !e.inserted.is_empty() {
                self.raw_delete(e.pos, e.pos + e.inserted.len());
            }
            if !e.deleted.is_empty() {
                self.raw_insert(e.pos, &e.deleted);
            }
        }
        self.break_group = true;
        self.compound_open = false;
        let before = g.cursor_before;
        self.redo.push(g);
        Some(before)
    }

    /// Redo the most recently undone group. Returns the cursor position to restore.
    pub fn redo(&mut self) -> Option<usize> {
        let g = self.redo.pop()?;
        for e in &g.edits {
            if !e.deleted.is_empty() {
                self.raw_delete(e.pos, e.pos + e.deleted.len());
            }
            if !e.inserted.is_empty() {
                self.raw_insert(e.pos, &e.inserted);
            }
        }
        self.break_group = true;
        self.compound_open = false;
        let after = g.cursor_after;
        self.undo.push(g);
        Some(after)
    }

    // ----- dirty tracking, versioning ---------------------------------------

    pub fn is_dirty(&self) -> bool {
        self.top_id() != self.saved_id
    }

    pub fn mark_clean(&mut self) {
        self.saved_id = self.top_id();
    }

    /// Monotonic counter bumped on every mutation (including undo/redo).
    pub fn version(&self) -> u64 {
        self.version_ctr
    }

    /// Smallest line touched by a mutation since the last call. Resets to usize::MAX.
    pub fn take_first_dirty_line(&mut self) -> usize {
        std::mem::replace(&mut self.dirty_line, usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simd::find_byte;

    /// xorshift64 PRNG (no rand crate).
    struct Rng(u64);
    impl Rng {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n.max(1) as u64) as usize
        }
    }

    const PIECES: [&str; 11] = ["a", "bc", " ", "\n", "\n\n", "é", "日本語", "🎉", "x\ny", "  z\n", "\t"];

    fn rand_text(rng: &mut Rng) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..1 + rng.below(3) {
            out.extend_from_slice(PIECES[rng.below(PIECES.len())].as_bytes());
        }
        if rng.below(40) == 0 {
            out = out.repeat(60);
        }
        out
    }

    /// Clamp `p` down to a UTF-8 char boundary of `model`.
    fn boundary(model: &[u8], p: usize) -> usize {
        let mut p = p.min(model.len());
        while p > 0 && p < model.len() && (model[p] & 0xC0) == 0x80 {
            p -= 1;
        }
        p
    }

    /// Apply one random non-empty edit to both buffer and model. Returns false if it was a no-op.
    fn random_edit(rng: &mut Rng, buf: &mut Buffer, model: &mut Vec<u8>) -> bool {
        if model.is_empty() || rng.below(10) < 6 {
            let pos = boundary(model, rng.below(model.len() + 1));
            let s = rand_text(rng);
            buf.insert(pos, &s);
            model.splice(pos..pos, s.iter().copied());
            true
        } else {
            let a = boundary(model, rng.below(model.len() + 1));
            let b = boundary(model, rng.below(model.len() + 1));
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            if a == b {
                return false;
            }
            buf.delete(a, b);
            model.drain(a..b);
            true
        }
    }

    /// Naive recomputation of everything the buffer reports.
    fn check_state(buf: &Buffer, model: &[u8]) {
        let mut starts = vec![0usize];
        starts.extend(model.iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i + 1));

        assert_eq!(buf.len(), model.len());
        assert_eq!(buf.is_empty(), model.is_empty());
        assert_eq!(buf.text(), model);
        assert_eq!(buf.line_count(), starts.len());
        for (l, &s) in starts.iter().enumerate() {
            assert_eq!(buf.line_start(l), s, "line_start({l})");
            let e = if l + 1 < starts.len() { starts[l + 1] - 1 } else { model.len() };
            assert_eq!(buf.line_end(l), e, "line_end({l})");
        }
        let mut line = 0;
        for pos in 0..=model.len() {
            if pos > 0 && model[pos - 1] == b'\n' {
                line += 1;
            }
            assert_eq!(buf.line_of(pos), line, "line_of({pos})");
        }
    }

    #[test]
    fn differential_random_ops() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut buf = Buffer::new();
        let mut model = Vec::new();
        check_state(&buf, &model);
        for _ in 0..2000 {
            random_edit(&mut rng, &mut buf, &mut model);
            check_state(&buf, &model);
        }
    }

    #[test]
    fn random_undo_redo_matches_snapshots() {
        let mut rng = Rng(0xDEAD_BEEF_1234_5678);
        let mut buf = Buffer::new();
        let mut model = Vec::new();
        let mut undo_snaps: Vec<Vec<u8>> = Vec::new();
        let mut redo_snaps: Vec<Vec<u8>> = Vec::new();
        for _ in 0..1500 {
            match rng.below(10) {
                0..=6 => {
                    buf.break_undo_group();
                    let before = model.clone();
                    if random_edit(&mut rng, &mut buf, &mut model) {
                        undo_snaps.push(before);
                        redo_snaps.clear();
                    }
                }
                7 | 8 => {
                    let can = !undo_snaps.is_empty();
                    assert_eq!(buf.undo().is_some(), can);
                    if can {
                        let prev = undo_snaps.pop().unwrap();
                        redo_snaps.push(std::mem::replace(&mut model, prev));
                    }
                }
                _ => {
                    let can = !redo_snaps.is_empty();
                    assert_eq!(buf.redo().is_some(), can);
                    if can {
                        let next = redo_snaps.pop().unwrap();
                        undo_snaps.push(std::mem::replace(&mut model, next));
                    }
                }
            }
            check_state(&buf, &model);
        }
    }

    #[test]
    fn undo_all_then_redo_all() {
        let original = b"line one\nline two\n".to_vec();
        let mut buf = Buffer::from_text(original.clone());
        let mut model = original.clone();
        let mut states = vec![original.clone()];
        let mut rng = Rng(42);
        for _ in 0..200 {
            buf.break_undo_group();
            if random_edit(&mut rng, &mut buf, &mut model) {
                states.push(model.clone());
            }
        }
        let last = states.len() - 1;
        assert!(last > 50);
        for k in (0..last).rev() {
            assert!(buf.undo().is_some());
            assert_eq!(buf.text(), states[k]);
        }
        assert_eq!(buf.undo(), None);
        assert_eq!(buf.text(), original);
        for k in 1..=last {
            assert!(buf.redo().is_some());
            assert_eq!(buf.text(), states[k]);
        }
        assert_eq!(buf.redo(), None);
        check_state(&buf, &model);
    }

    #[test]
    fn coalesce_typing_respects_word_and_newline_boundaries() {
        let mut b = Buffer::new();
        for c in "ab cd".bytes() {
            let p = b.len();
            b.insert(p, &[c]);
        }
        assert_eq!(b.text(), b"ab cd");
        assert_eq!(b.undo(), Some(2)); // " cd" is one group, cursor_before defaults to its start
        assert_eq!(b.text(), b"ab");
        assert!(b.undo().is_some()); // "ab"
        assert_eq!(b.text(), b"");
        assert_eq!(b.undo(), None);
    }

    #[test]
    fn coalesce_newline_always_breaks() {
        let mut b = Buffer::new();
        b.insert(0, b"a");
        b.insert(1, b"\n");
        b.insert(2, b"b");
        assert_eq!(b.undo(), Some(1)); // removes "\nb"
        assert_eq!(b.text(), b"a");
        b.undo();
        assert_eq!(b.text(), b"");
    }

    #[test]
    fn break_undo_group_splits_typing_run() {
        let mut b = Buffer::new();
        b.insert(0, b"a");
        b.break_undo_group();
        b.insert(1, b"b");
        b.undo();
        assert_eq!(b.text(), b"a");
        b.undo();
        assert_eq!(b.text(), b"");
    }

    #[test]
    fn coalesce_backspace_and_forward_delete() {
        let mut b = Buffer::from_text(b"abc".to_vec());
        b.delete(2, 3);
        b.delete(1, 2);
        b.delete(0, 1);
        assert_eq!(b.text(), b"");
        assert!(b.undo().is_some());
        assert_eq!(b.text(), b"abc");
        assert_eq!(b.undo(), None);

        let mut f = Buffer::from_text(b"abc".to_vec());
        f.delete(0, 1);
        f.delete(0, 1);
        f.delete(0, 1);
        assert!(f.undo().is_some());
        assert_eq!(f.text(), b"abc");
        assert_eq!(f.undo(), None);
    }

    #[test]
    fn delete_coalescing_breaks_on_newline_and_word_boundary() {
        // Backspacing "ab\ncd" from the end: "cd" is one group, a newline starts a new group.
        let mut b = Buffer::from_text(b"ab\ncd".to_vec());
        b.delete(4, 5);
        b.delete(3, 4);
        b.delete(2, 3); // '\n' breaks the run
        b.delete(1, 2); // 'b' joins the newline's group
        assert_eq!(b.text(), b"a");
        b.undo();
        assert_eq!(b.text(), b"ab\n");
        b.undo();
        assert_eq!(b.text(), b"ab\ncd");
        assert_eq!(b.undo(), None);
        // Backspacing "x y": deleting a space after a non-space starts a new group.
        let mut c = Buffer::from_text(b"x y".to_vec());
        c.delete(2, 3);
        c.delete(1, 2); // ' ' breaks the run
        c.delete(0, 1); // 'x' joins the space's group
        assert_eq!(c.text(), b"");
        c.undo();
        assert_eq!(c.text(), b"x ");
        c.undo();
        assert_eq!(c.text(), b"x y");
    }

    #[test]
    fn compound_groups_nest_and_merge_into_one_step() {
        let mut b = Buffer::from_text(b"abc".to_vec());
        b.begin_compound();
        b.insert(3, b"d");
        b.break_undo_group(); // ignored inside compound
        b.insert(4, b"e");
        b.begin_compound();
        b.delete(0, 1);
        b.end_compound();
        b.end_compound();
        assert_eq!(b.text(), b"bcde");
        assert!(b.undo().is_some());
        assert_eq!(b.text(), b"abc");
        assert_eq!(b.undo(), None);
    }

    #[test]
    fn edits_after_compound_do_not_join_it() {
        let mut b = Buffer::new();
        b.begin_compound();
        b.insert(0, b"x");
        b.end_compound();
        b.insert(1, b"y");
        assert_eq!(b.text(), b"xy");
        b.undo();
        assert_eq!(b.text(), b"x");
        b.undo();
        assert_eq!(b.text(), b"");
    }

    #[test]
    fn cursor_hints_and_redo_stack() {
        let mut b = Buffer::new();
        b.set_cursor_hint(7);
        b.insert(0, b"hello");
        b.break_undo_group();
        b.set_cursor_hint(3);
        b.insert(5, b"!");
        assert_eq!(b.undo(), Some(3));
        assert_eq!(b.undo(), Some(7));
        assert_eq!(b.text(), b"");
        assert_eq!(b.redo(), Some(5)); // cursor_after of group 1
        assert_eq!(b.redo(), Some(6)); // cursor_after of group 2
        assert_eq!(b.text(), b"hello!");
        b.undo();
        b.insert(0, b"z");
        assert_eq!(b.redo(), None); // new edit cleared redo
        assert_eq!(b.text(), b"zhello");
    }

    #[test]
    fn dirty_clean_across_undo_redo() {
        let mut b = Buffer::new();
        assert!(!b.is_dirty());
        b.insert(0, b"a");
        assert!(b.is_dirty());
        b.mark_clean();
        assert!(!b.is_dirty());

        b.break_undo_group();
        b.insert(1, b"b");
        assert!(b.is_dirty());
        b.undo();
        assert_eq!(b.text(), b"a");
        assert!(!b.is_dirty()); // back at save point
        b.redo();
        assert!(b.is_dirty());
        b.undo();
        b.undo();
        assert!(b.is_dirty()); // empty stack != saved group

        // Saved state followed by a coalescing edit must read as dirty.
        let mut c = Buffer::new();
        c.insert(0, b"a");
        c.mark_clean();
        c.insert(1, b"c");
        assert!(c.is_dirty());
        c.undo();
        assert_eq!(c.text(), b"");
        assert!(c.is_dirty());

        // Clean state of an empty document.
        let mut e = Buffer::new();
        e.mark_clean();
        e.insert(0, b"q");
        assert!(e.is_dirty());
        e.undo();
        assert!(!e.is_dirty());
    }

    #[test]
    fn edits_at_gap_boundaries() {
        let mut b = Buffer::from_text(b"hello world".to_vec());
        b.insert(5, b"_X");
        assert_eq!(b.text(), b"hello_X world");
        b.insert(7, b"!");
        assert_eq!(b.text(), b"hello_X! world");
        b.delete(6, 9);
        assert_eq!(b.text(), b"hello_world");
        b.insert(0, b"\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line_start(1), 1);
        b.delete(0, 1);
        assert_eq!(b.line_count(), 1);

        // Read across the gap and a slice straddling it.
        let mut g = Buffer::from_text(b"abcdef".to_vec());
        g.insert(3, b"XY"); // gap now sits at 5
        assert_eq!(g.text(), b"abcXYdef");
        let mut out = Vec::new();
        g.slice_into(2, 6, &mut out);
        assert_eq!(out, b"cXYd");
        g.slice_into(0, 2, &mut out);
        assert_eq!(out, b"ab");
        g.slice_into(6, 8, &mut out);
        assert_eq!(out, b"ef");
        assert_eq!(g.byte_at(0), b'a');
        assert_eq!(g.byte_at(4), b'Y');
        assert_eq!(g.byte_at(5), b'd');
        assert_eq!(g.byte_at(7), b'f');
        assert_eq!(g.byte_at(8), 0);
        assert_eq!(g.make_contiguous(), b"abcXYdef");
        assert_eq!(g.text(), b"abcXYdef");
    }

    #[test]
    fn gap_growth_preserves_text_and_index() {
        let mut b = Buffer::new();
        let big = vec![b'x'; 100_000];
        b.insert(0, &big);
        assert_eq!(b.len(), 100_000);
        b.insert(50_000, b"\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line_start(1), 50_001);
        assert_eq!(b.line_of(50_001), 1);
        b.insert(0, b"head\n");
        assert_eq!(b.line_start(1), 5);
        assert_eq!(b.line_end(0), 4);
        assert_eq!(b.line_of(100_000), 2);
        assert_eq!(b.text().len(), 100_006);
    }

    #[test]
    fn empty_buffer_behaviour() {
        let mut b = Buffer::new();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_end(0), 0);
        assert_eq!(b.line_of(0), 0);
        assert_eq!(b.byte_at(0), 0);
        assert!(b.text().is_empty());
        assert!(b.make_contiguous().is_empty());
        b.delete(0, 10);
        assert!(b.is_empty());
        assert_eq!(b.undo(), None);
        assert_eq!(b.redo(), None);
        assert!(!b.is_dirty());
    }

    #[test]
    fn delete_everything_then_rebuild() {
        let mut b = Buffer::from_text(b"a\nb\nc".to_vec());
        assert_eq!(b.line_count(), 3);
        b.delete(0, 5);
        assert!(b.is_empty());
        assert_eq!(b.line_count(), 1);
        assert_eq!(b.line_end(0), 0);
        b.insert(0, b"x\ny");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line_start(1), 2);
        assert!(b.undo().is_some());
        assert!(b.is_empty());
        assert!(b.undo().is_some());
        assert_eq!(b.text(), b"a\nb\nc");
        assert_eq!(b.line_count(), 3);
    }

    #[test]
    fn clamps_out_of_range_positions() {
        let mut b = Buffer::from_text(b"ab".to_vec());
        b.insert(999, b"x");
        assert_eq!(b.text(), b"abx");
        b.delete(1, 999);
        assert_eq!(b.text(), b"a");
        b.delete(5, 2); // start > end after clamping: no-op
        assert_eq!(b.text(), b"a");
        assert_eq!(b.line_start(50), 0);
        assert_eq!(b.line_end(50), 1);
        assert_eq!(b.line_of(100), 0);
        assert_eq!(b.byte_at(100), 0);
        assert_eq!(b.slice(0, 100), b"a");
    }

    #[test]
    fn line_index_from_text_with_trailing_newline() {
        let b = Buffer::from_text(b"one\ntwo\n".to_vec());
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line_start(0), 0);
        assert_eq!(b.line_end(0), 3);
        assert_eq!(b.line_start(1), 4);
        assert_eq!(b.line_end(1), 7);
        assert_eq!(b.line_start(2), 8);
        assert_eq!(b.line_end(2), 8);
        assert_eq!(b.line_of(8), 2);
        assert_eq!(b.line_of(3), 0); // the '\n' belongs to line 0
        assert_eq!(find_byte(&b.text(), b'\n'), Some(3));
    }

    #[test]
    fn take_first_dirty_line_tracks_minimum() {
        let mut b = Buffer::from_text(b"a\nb\nc\n".to_vec());
        assert_eq!(b.take_first_dirty_line(), usize::MAX);
        b.insert(4, b"x"); // line 2
        assert_eq!(b.take_first_dirty_line(), 2);
        assert_eq!(b.take_first_dirty_line(), usize::MAX);
        b.insert(4, b"y"); // line 2
        b.delete(0, 1); // line 0
        assert_eq!(b.take_first_dirty_line(), 0);
    }

    #[test]
    fn version_bumps_on_every_mutation() {
        let mut b = Buffer::new();
        let v0 = b.version();
        b.insert(0, b"abc");
        let v1 = b.version();
        assert!(v1 > v0);
        b.delete(0, 1);
        let v2 = b.version();
        assert!(v2 > v1);
        b.undo();
        let v3 = b.version();
        assert!(v3 > v2);
        b.redo();
        assert!(b.version() > v3);
    }

    #[test]
    fn compound_edits_undo_with_cursor_before() {
        let mut b = Buffer::from_text(b"xyz".to_vec());
        b.set_cursor_hint(3);
        b.begin_compound();
        b.delete(0, 1);
        b.insert(2, b"Q");
        b.end_compound();
        assert_eq!(b.text(), b"yzQ");
        assert_eq!(b.undo(), Some(3));
        assert_eq!(b.text(), b"xyz");
        assert_eq!(b.redo(), Some(3));
        assert_eq!(b.text(), b"yzQ");
    }
}
