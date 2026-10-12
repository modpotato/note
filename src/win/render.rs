//! GDI painting. Each frame is drawn into a memory DC and copied to the window once per
//! WM_PAINT. Scratch buffers live in `Renderer`, so painting allocates nothing per frame
//! once they have grown to their working size.

use std::fmt::Write as _;
use std::mem::zeroed;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, DeleteDC, DeleteObject,
    ExtTextOutW, GetDC, GetStockObject, GetTextExtentPoint32W, GetTextFaceW, GetTextMetricsW,
    ReleaseDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, ETO_CLIPPED, ETO_OPAQUE, FF_MODERN, FIXED_PITCH,
    FW_NORMAL, HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, SRCCOPY, SYSTEM_FONT, TEXTMETRICW,
    TRANSPARENT,
};

use super::{App, Layout, Prompt};
use crate::buffer::Buffer;
use crate::config::{Action, Config, Rgb};
use crate::editor::Editor;
use crate::syntax::{highlight_line, Lang, Span, Tok};

/// Vertical offset of glyphs inside a row, in pixels.
const TEXT_DY: i32 = 1;
/// Cells per hint column; 6 columns per row fill the two hint rows.
const HINT_CELLS: i32 = 18;
const HINT_PER_ROW: usize = 6;
/// Cells per help column, and where the action name starts inside a column.
const HELP_CELLS: i32 = 34;
const HELP_NAME_CELL: i32 = 12;
const HELP_HEADER: &str = "Keys: Esc closes, arrows / PgUp / PgDn scroll";
/// Hint bar entries, in display order.
const HINTS: [(Action, &str); 12] = [
    (Action::Help, "Help"),
    (Action::Save, "Save"),
    (Action::Open, "Open"),
    (Action::Find, "Find"),
    (Action::Replace, "Replace"),
    (Action::CutLine, "Cut"),
    (Action::Paste, "Paste"),
    (Action::GotoLine, "Goto"),
    (Action::Undo, "Undo"),
    (Action::Redo, "Redo"),
    (Action::MarkToggle, "Mark"),
    (Action::Exit, "Exit"),
];
/// Source for ETO_OPAQUE fills, which need an empty string.
const EMPTY: [u16; 1] = [0];

/// A run of same-coloured glyphs: where it starts in `text` and on which cell.
#[derive(Clone, Copy)]
struct Run {
    start: usize,
    cell: usize,
    tok: Tok,
}

/// GDI resources and reusable scratch space for painting.
pub(super) struct Renderer {
    mem: HDC,
    bmp: HBITMAP,
    default_bmp: HGDIOBJ,
    bmp_w: i32,
    bmp_h: i32,
    font: HFONT,
    pub(super) char_w: i32,
    pub(super) line_h: i32,
    bytes: Vec<u8>,
    spans: Vec<Span>,
    text: Vec<u16>,
    dx: Vec<i32>,
    runs: Vec<Run>,
    tmp: String,
    tmp16: Vec<u16>,
    /// Pre-rendered hint bar cells: (" key ", label).
    hints: Vec<(Vec<u16>, Vec<u16>)>,
    /// Pre-rendered help overlay entries: (key, name).
    help: Vec<(Vec<u16>, Vec<u16>)>,
}

fn key_text(cfg: &Config, action: Action) -> String {
    cfg.keymap
        .chord_for(action)
        .map(|c| c.display())
        .unwrap_or_else(|| "-".to_string())
}

fn to_utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Renderer {
    pub(super) fn new(cfg: &Config) -> Renderer {
        // SAFETY: a memory DC compatible with the screen; released in Drop.
        let mem = unsafe { CreateCompatibleDC(null_mut()) };
        let hints = HINTS
            .iter()
            .map(|&(action, label)| {
                let key = key_text(cfg, action);
                (to_utf16(&format!(" {key} ")), to_utf16(label))
            })
            .collect();
        let help = Action::ALL
            .iter()
            .map(|&a| {
                (
                    to_utf16(&key_text(cfg, a)),
                    to_utf16(&a.name().replace('_', " ")),
                )
            })
            .collect();
        Renderer {
            mem,
            bmp: null_mut(),
            default_bmp: null_mut(),
            bmp_w: 0,
            bmp_h: 0,
            font: null_mut(),
            char_w: 8,
            line_h: 16,
            bytes: Vec::new(),
            spans: Vec::new(),
            text: Vec::new(),
            dx: Vec::new(),
            runs: Vec::new(),
            tmp: String::new(),
            tmp16: Vec::new(),
            hints,
            help,
        }
    }

    /// Makes the back buffer `w` x `h`. Recreated only when the size changes.
    fn ensure_bitmap(&mut self, w: i32, h: i32) {
        if !self.bmp.is_null() && w == self.bmp_w && h == self.bmp_h {
            return;
        }
        // SAFETY: the new bitmap is selected into our DC before the old one is deleted.
        unsafe {
            let screen = GetDC(null_mut());
            let bmp = CreateCompatibleBitmap(screen, w, h);
            ReleaseDC(null_mut(), screen);
            if bmp.is_null() {
                return;
            }
            let old = SelectObject(self.mem, bmp);
            if self.bmp.is_null() {
                self.default_bmp = old;
            } else {
                DeleteObject(old);
            }
            self.bmp = bmp;
        }
        self.bmp_w = w;
        self.bmp_h = h;
    }

    /// Sets the text font to `pt` points at `dpi` and re-measures the cell size.
    pub(super) fn set_font(&mut self, face: &str, pt: i32, dpi: u32) {
        // Same rounding as MulDiv(pt, dpi, 72).
        let height = -((pt * dpi as i32 + 36) / 72);
        // SAFETY: GDI objects are deselected before deletion; `mem` is ours.
        unsafe {
            let (mut font, matched) = select_face(self.mem, height, face);
            if !matched && !face.eq_ignore_ascii_case("Consolas") {
                let (fallback, _) = select_face(self.mem, height, "Consolas");
                DeleteObject(font);
                font = fallback;
            }
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            self.font = font;

            let mut tm: TEXTMETRICW = zeroed();
            GetTextMetricsW(self.mem, &mut tm);
            let mut sz: SIZE = zeroed();
            let m = [b'M' as u16];
            GetTextExtentPoint32W(self.mem, m.as_ptr(), 1, &mut sz);
            self.char_w = sz.cx.max(1);
            self.line_h = (tm.tmHeight + 2).max(1);
        }
    }

    /// Appends one glyph (and its advance) to the run buffers.
    fn push_char(&mut self, ch: char, tok: Tok, cell: usize, last: &mut Option<Tok>, cw: i32) {
        if *last != Some(tok) {
            self.runs.push(Run {
                start: self.text.len(),
                cell,
                tok,
            });
            *last = Some(tok);
        }
        let mut units = [0u16; 2];
        for (k, u) in ch.encode_utf16(&mut units).iter().enumerate() {
            self.text.push(*u);
            // Advance per code unit: the glyph takes one cell, a surrogate's tail takes none.
            self.dx.push(if k == 0 { cw } else { 0 });
        }
    }

    /// Draws `s` at (x, y) in `color` using the scratch UTF-16 buffer.
    fn put(&mut self, dc: HDC, x: i32, y: i32, color: Rgb, s: &str) {
        self.tmp16.clear();
        self.tmp16.extend(s.encode_utf16());
        set_text_color(dc, color);
        // SAFETY: tmp16 is valid for its length for the duration of the call.
        unsafe {
            ExtTextOutW(
                dc,
                x,
                y,
                0,
                null(),
                self.tmp16.as_ptr(),
                self.tmp16.len() as u32,
                null(),
            );
        }
    }

    /// Visible text: gutter, current-line tint, selection, syntax-coloured glyphs.
    fn draw_text(&mut self, ed: &mut Editor, cfg: &Config, lay: Layout) {
        let mem = self.mem;
        let theme = &cfg.theme;
        let tw = ed.tab_width.max(1);
        let (cw, lh, gutter) = (lay.cw, lay.lh, lay.gutter);
        let scroll_line = ed.scroll_line;
        let scroll_col = ed.scroll_col;
        let cur = ed.cursor;
        let sel = ed.selection();
        let lang = ed.highlighter.lang();
        let use_syntax = cfg.syntax && lang != Lang::Plain;
        let Editor {
            buf, highlighter, ..
        } = ed;

        let line_count = buf.line_count();
        let cur_line = buf.line_of(cur);

        if gutter > 0 {
            fill(mem, rect(0, 0, gutter, lay.text_h), theme.gutter_bg);
        }
        for row in 0..lay.rows {
            let y = row as i32 * lh;
            if y >= lay.text_h {
                break;
            }
            let line = scroll_line + row;
            if line >= line_count {
                break;
            }
            let ls = buf.line_start(line);
            let le = buf.line_end(line);

            if cfg.highlight_current_line && line == cur_line {
                fill(mem, rect(gutter, y, lay.w, y + lh), theme.current_line_bg);
            }
            if let Some((s, e)) = sel {
                if s <= le && e > ls {
                    // A selected newline shows as one extra cell at the end of the line.
                    let v0 = if s > ls { vcol(buf, ls, s, tw) } else { 0 };
                    let v1 = if e > le {
                        vcol(buf, ls, le, tw) + 1
                    } else {
                        vcol(buf, ls, e, tw)
                    };
                    let x0 = (gutter + (v0 as i32 - scroll_col as i32) * cw).max(gutter);
                    let x1 = (gutter + (v1 as i32 - scroll_col as i32) * cw).min(lay.w);
                    fill(mem, rect(x0, y, x1, y + lh), theme.selection_bg);
                }
            }

            buf.slice_into(ls, le, &mut self.bytes);
            let state = if use_syntax {
                highlighter.state_at(line, &mut |i: usize, out: &mut Vec<u8>| {
                    buf.slice_into(buf.line_start(i), buf.line_end(i), out)
                })
            } else {
                0
            };
            if use_syntax {
                highlight_line(lang, &self.bytes, state, &mut self.spans);
            } else {
                self.spans.clear();
            }

            // Expand the line into UTF-16 cells, skipping columns left of the view.
            self.text.clear();
            self.dx.clear();
            self.runs.clear();
            let right = scroll_col + lay.cols;
            let n = self.bytes.len();
            let mut cell = 0usize;
            let mut last: Option<Tok> = None;
            let mut i = 0usize;
            let mut si = 0usize;
            while i < n && cell < right {
                while si < self.spans.len()
                    && (self.spans[si].start + self.spans[si].len) as usize <= i
                {
                    si += 1;
                }
                let tok = match self.spans.get(si) {
                    Some(sp) if sp.start as usize <= i => sp.tok,
                    _ => Tok::Normal,
                };
                let b = self.bytes[i];
                if b == b'\t' {
                    for _ in 0..(tw - cell % tw) {
                        if cell >= right {
                            break;
                        }
                        if cell >= scroll_col {
                            self.push_char(' ', tok, cell, &mut last, cw);
                        }
                        cell += 1;
                    }
                    i += 1;
                    continue;
                }
                let len = utf8_len(b).min(n - i);
                let ch = std::str::from_utf8(&self.bytes[i..i + len])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .unwrap_or('\u{FFFD}');
                if cell >= scroll_col {
                    self.push_char(ch, tok, cell, &mut last, cw);
                }
                cell += 1;
                i += len;
            }

            let clip = rect(gutter, y, lay.w, y + lh);
            for (k, &r) in self.runs.iter().enumerate() {
                let end = self
                    .runs
                    .get(k + 1)
                    .map_or(self.text.len(), |next| next.start);
                if end <= r.start {
                    continue;
                }
                let x = gutter + (r.cell as i32 - scroll_col as i32) * cw;
                set_text_color(mem, theme.tok[r.tok as usize]);
                // SAFETY: the slices are in bounds and of equal length.
                unsafe {
                    ExtTextOutW(
                        mem,
                        x,
                        y + TEXT_DY,
                        ETO_CLIPPED,
                        &clip,
                        self.text[r.start..end].as_ptr(),
                        (end - r.start) as u32,
                        self.dx[r.start..end].as_ptr(),
                    );
                }
            }

            if gutter > 0 {
                self.tmp.clear();
                let _ = write!(self.tmp, "{}", line + 1);
                self.tmp16.clear();
                self.tmp16.extend(self.tmp.encode_utf16());
                let color = if line == cur_line {
                    theme.fg
                } else {
                    theme.gutter_fg
                };
                let x = gutter - (self.tmp16.len() as i32 + 1) * cw;
                set_text_color(mem, color);
                // SAFETY: tmp16 outlives the call.
                unsafe {
                    ExtTextOutW(
                        mem,
                        x,
                        y + TEXT_DY,
                        0,
                        null(),
                        self.tmp16.as_ptr(),
                        self.tmp16.len() as u32,
                        null(),
                    );
                }
            }
        }
    }

    /// Status row: the prompt while one is open, else message/file name and position info.
    fn draw_status(
        &mut self,
        ed: &Editor,
        cfg: &Config,
        lay: Layout,
        prompt: Option<&Prompt>,
        zoom: i32,
    ) {
        let mem = self.mem;
        let theme = &cfg.theme;
        let y = lay.text_h;
        fill(mem, rect(0, y, lay.w, y + lay.lh), theme.status_bg);

        if let Some(p) = prompt {
            self.put(mem, lay.cw, y + TEXT_DY, theme.status_fg, &p.label);
            let x = lay.cw + p.label.chars().count() as i32 * lay.cw;
            self.put(mem, x, y + TEXT_DY, theme.status_fg, &p.text);
            return;
        }

        let mut left = std::mem::take(&mut self.tmp);
        left.clear();
        match &ed.message {
            Some(m) => left.push_str(m),
            None => {
                let name = match ed.path.as_deref() {
                    Some(p) => p.rsplit(['\\', '/']).next().unwrap_or(p),
                    None => "Untitled",
                };
                left.push_str(name);
                if ed.is_dirty() {
                    left.push_str(" [modified]");
                }
            }
        }
        if ed.mark_mode {
            left.push_str("  [Mark Set]");
        }
        self.put(mem, lay.cw, y + TEXT_DY, theme.status_fg, &left);

        left.clear();
        let (line, col) = ed.cursor_line_col();
        let _ = write!(
            left,
            "Ln {}, Col {}  |  {}  |  {}  |  {}  |  {}%",
            line + 1,
            col + 1,
            ed.highlighter.lang().name(),
            ed.encoding.name(),
            ed.line_ending.name(),
            zoom
        );
        let width = left.chars().count() as i32;
        self.put(
            mem,
            (lay.w - (width + 1) * lay.cw).max(0),
            y + TEXT_DY,
            theme.status_fg,
            &left,
        );
        self.tmp = left;
    }

    /// Two rows of nano-style "KEY Label" pairs.
    fn draw_hints(&mut self, cfg: &Config, lay: Layout) {
        let mem = self.mem;
        let theme = &cfg.theme;
        let top = lay.text_h + lay.lh;
        fill(mem, rect(0, top, lay.w, top + 2 * lay.lh), theme.bg);
        for (k, (key, label)) in self.hints.iter().enumerate() {
            let x = (k % HINT_PER_ROW) as i32 * HINT_CELLS * lay.cw;
            let y = top + (k / HINT_PER_ROW) as i32 * lay.lh;
            if x >= lay.w {
                continue;
            }
            let kw = key.len() as i32 * lay.cw;
            fill(mem, rect(x, y, x + kw, y + lay.lh), theme.hint_fg);
            set_text_color(mem, theme.hint_key_fg);
            // SAFETY: key and label are owned by self for the duration of the call.
            unsafe {
                ExtTextOutW(
                    mem,
                    x,
                    y + TEXT_DY,
                    0,
                    null(),
                    key.as_ptr(),
                    key.len() as u32,
                    null(),
                );
            }
            set_text_color(mem, theme.hint_fg);
            unsafe {
                ExtTextOutW(
                    mem,
                    x + kw + lay.cw,
                    y + TEXT_DY,
                    0,
                    null(),
                    label.as_ptr(),
                    label.len() as u32,
                    null(),
                );
            }
        }
    }

    /// Help overlay over the text area: every action with its chord, in columns.
    fn draw_help(&mut self, cfg: &Config, lay: Layout, scroll: &mut usize) {
        let mem = self.mem;
        let theme = &cfg.theme;
        fill(mem, rect(0, 0, lay.w, lay.text_h), theme.bg);
        self.put(mem, lay.cw, TEXT_DY, theme.hint_fg, HELP_HEADER);

        let body_rows = lay.rows.saturating_sub(1);
        let cols = (lay.w / (HELP_CELLS * lay.cw)).max(1) as usize;
        let total_rows = self.help.len().div_ceil(cols);
        *scroll = (*scroll).min(total_rows.saturating_sub(body_rows));
        for r in 0..body_rows {
            let y = (r as i32 + 1) * lay.lh;
            if y >= lay.text_h {
                break;
            }
            for c in 0..cols {
                let Some((key, name)) = self.help.get((*scroll + r) * cols + c) else {
                    break;
                };
                let x = c as i32 * HELP_CELLS * lay.cw;
                set_text_color(mem, theme.hint_key_fg);
                // SAFETY: key and name are owned by self for the duration of the call.
                unsafe {
                    ExtTextOutW(
                        mem,
                        x + lay.cw,
                        y + TEXT_DY,
                        0,
                        null(),
                        key.as_ptr(),
                        key.len() as u32,
                        null(),
                    );
                }
                set_text_color(mem, theme.fg);
                unsafe {
                    ExtTextOutW(
                        mem,
                        x + HELP_NAME_CELL * lay.cw,
                        y + TEXT_DY,
                        0,
                        null(),
                        name.as_ptr(),
                        name.len() as u32,
                        null(),
                    );
                }
            }
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: every object is deselected from `mem` before it is deleted.
        unsafe {
            if self.mem.is_null() {
                return;
            }
            SelectObject(self.mem, GetStockObject(SYSTEM_FONT));
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
            if !self.default_bmp.is_null() {
                SelectObject(self.mem, self.default_bmp);
            }
            if !self.bmp.is_null() {
                DeleteObject(self.bmp);
            }
            DeleteDC(self.mem);
        }
    }
}

/// Caret top-left in client pixels, or None when no caret is shown (help overlay, a
/// confirmation prompt, or the cursor scrolled out of view). The prompt caret sits in the
/// status row.
pub(super) fn caret_spot(app: &App, lay: Layout) -> Option<(i32, i32)> {
    if app.show_help {
        return None;
    }
    if let Some(p) = &app.prompt {
        if !p.kind.takes_text() {
            return None;
        }
        let labels = p.label.chars().count() as i32;
        let before = p.text[..p.cursor].chars().count() as i32;
        return Some((lay.cw + (labels + before) * lay.cw, lay.text_h));
    }
    let ed = &app.editor;
    let (line, col) = ed.cursor_line_col();
    if line < ed.scroll_line || line >= ed.scroll_line + lay.rows {
        return None;
    }
    if col < ed.scroll_col || col >= ed.scroll_col + lay.cols {
        return None;
    }
    Some((
        lay.gutter + (col - ed.scroll_col) as i32 * lay.cw,
        (line - ed.scroll_line) as i32 * lay.lh,
    ))
}

/// Paints one frame into the back buffer and copies `clip` of it to `hdc`.
pub(super) fn paint(app: &mut App, hdc: HDC, clip: RECT) {
    let lay = app.layout();
    let spot = caret_spot(app, lay);
    let zoom = app.zoom_pct();
    let App {
        editor,
        config,
        renderer,
        prompt,
        show_help,
        help_scroll,
        caret_visible,
        ..
    } = app;

    renderer.ensure_bitmap(lay.w, lay.h);
    let mem = renderer.mem;
    if mem.is_null() || renderer.bmp.is_null() {
        return;
    }
    // Tell the highlighter which line changed since the last paint.
    let dirty = editor.buf.take_first_dirty_line();
    if dirty != usize::MAX {
        editor.highlighter.invalidate_from(dirty);
    }

    // SAFETY: mem is our memory DC with the bitmap and font selected.
    unsafe {
        SelectObject(mem, renderer.font);
        SetBkMode(mem, TRANSPARENT as i32);
    }
    fill(mem, rect(0, 0, lay.w, lay.h), config.theme.bg);
    renderer.draw_text(editor, config, lay);
    renderer.draw_status(editor, config, lay, prompt.as_ref(), zoom);
    if config.show_hints {
        renderer.draw_hints(config, lay);
    }
    if *show_help {
        renderer.draw_help(config, lay, help_scroll);
    }
    if *caret_visible {
        if let Some((x, y)) = spot {
            fill(
                mem,
                rect(x, y, x + config.caret_width.max(1), y + lay.lh),
                config.theme.caret,
            );
        }
    }

    // SAFETY: both DCs are valid; the copied rectangle lies inside the back buffer.
    unsafe {
        BitBlt(
            hdc,
            clip.left,
            clip.top,
            clip.right - clip.left,
            clip.bottom - clip.top,
            mem,
            clip.left,
            clip.top,
            SRCCOPY,
        );
    }
}

/// Creates the editor font named `face` at `height`, selects it into `dc`, and reports
/// whether Windows used that face.
///
/// # Safety
/// `dc` must be a valid memory DC.
unsafe fn select_face(dc: HDC, height: i32, face: &str) -> (HFONT, bool) {
    let name: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    let font = CreateFontW(
        height,
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (FIXED_PITCH | FF_MODERN) as u32,
        name.as_ptr(),
    );
    SelectObject(dc, font);
    let mut got = [0u16; 64];
    let n = GetTextFaceW(dc, got.len() as i32, got.as_mut_ptr());
    // GetTextFaceW counts the terminating NUL.
    let len = (n.max(1) - 1) as usize;
    let matched = String::from_utf16_lossy(&got[..len]).eq_ignore_ascii_case(face);
    (font, matched)
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

/// Fills `r` with `c`. ETO_OPAQUE with an empty string paints the rectangle.
fn fill(dc: HDC, r: RECT, c: Rgb) {
    if r.right <= r.left || r.bottom <= r.top {
        return;
    }
    // SAFETY: the rectangle is a local value and the empty string is never read.
    unsafe {
        SetBkColor(dc, c.colorref());
        ExtTextOutW(dc, 0, 0, ETO_OPAQUE, &r, EMPTY.as_ptr(), 0, null());
    }
}

fn set_text_color(dc: HDC, c: Rgb) {
    // SAFETY: plain state change on a valid DC.
    unsafe {
        SetTextColor(dc, c.colorref());
    }
}

/// Visual column of byte `pos` on the line starting at `ls`: tabs expand to `tw`, each
/// character is one cell.
fn vcol(buf: &Buffer, ls: usize, pos: usize, tw: usize) -> usize {
    let mut col = 0;
    let mut i = ls;
    while i < pos {
        let b = buf.byte_at(i);
        if b == b'\t' {
            col += tw - col % tw;
        } else if b & 0xC0 != 0x80 {
            col += 1;
        }
        i += 1;
    }
    col
}

/// Length of the UTF-8 sequence that starts with `b` (1 for invalid lead bytes).
fn utf8_len(b: u8) -> usize {
    match b {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
