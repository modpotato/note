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
    ReleaseDC, SelectObject, SetBkColor, SetBkMode, SetTextColor, ANTIALIASED_QUALITY,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, ETO_CLIPPED, ETO_OPAQUE, FF_MODERN,
    FIXED_PITCH, FW_BOLD, HBITMAP, HDC, HFONT, HGDIOBJ, NONANTIALIASED_QUALITY,
    OUT_DEFAULT_PRECIS, SRCCOPY, SYSTEM_FONT, TEXTMETRICW, TRANSPARENT,
};

use super::{App, Axis, Bars, Layout, Prompt};
use crate::buffer::Buffer;
use crate::config::{Action, Config, FontSmoothing, Rgb};
use crate::editor::{wrap_rows_buf, Editor};
use crate::syntax::{highlight_line, Lang, Span, Tok};

/// Cells per hint column; 6 columns per row fill the two hint rows.
const HINT_CELLS: i32 = 18;
const HINT_PER_ROW: usize = 6;
/// Cells per help column, and where the action name starts inside a column. The key box
/// (" ^Backspace ", the widest default chord) is 12 cells, so the name starts after it.
const HELP_CELLS: i32 = 34;
const HELP_NAME_CELL: i32 = 14;
const HELP_HEADER: &str = "Keys: Esc closes, arrows / PgUp / PgDn scroll";
/// Hint bar entries, in display order.
const HINTS: [(Action, &str); 12] = [
    (Action::Help, "Help"),
    (Action::Save, "Save"),
    (Action::Open, "Open"),
    (Action::Find, "Find"),
    (Action::Replace, "Replace"),
    (Action::GotoLine, "Goto"),
    (Action::Undo, "Undo"),
    (Action::Redo, "Redo"),
    (Action::CutLine, "Cut Line"),
    (Action::DuplicateLine, "Duplicate"),
    (Action::ToggleComment, "Comment"),
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
    /// Regular text face.
    font: HFONT,
    /// Bold face for keywords and types, or null when `bold_keywords` is off.
    bold: HFONT,
    bold_keywords: bool,
    pub(super) char_w: i32,
    pub(super) line_h: i32,
    /// Offset of the glyphs from the top of a row, centring them in `line_h`.
    text_dy: i32,
    bytes: Vec<u8>,
    spans: Vec<Span>,
    /// Visual row starts of the line being painted (wrap mode).
    rows: Vec<usize>,
    text: Vec<u16>,
    dx: Vec<i32>,
    runs: Vec<Run>,
    tmp: String,
    tmp16: Vec<u16>,
    /// Pre-rendered hint bar cells: (" key ", label).
    hints: Vec<(Vec<u16>, Vec<u16>)>,
    /// Pre-rendered help overlay entries: (key box text, name). The key is empty when the
    /// action has no binding.
    help: Vec<(Vec<u16>, Vec<u16>)>,
}

/// The first chord bound to `action`, in nano notation, or None when it has no key.
fn key_text(cfg: &Config, action: Action) -> Option<String> {
    cfg.keymap.chord_for(action).map(|c| c.display())
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
                let key = key_text(cfg, action).unwrap_or_else(|| "-".to_string());
                (to_utf16(&format!(" {key} ")), to_utf16(label))
            })
            .collect();
        let help = Action::ALL
            .iter()
            .map(|&a| {
                let key = key_text(cfg, a).map_or_else(Vec::new, |k| to_utf16(&format!(" {k} ")));
                (key, to_utf16(&a.name().replace('_', " ")))
            })
            .collect();
        Renderer {
            mem,
            bmp: null_mut(),
            default_bmp: null_mut(),
            bmp_w: 0,
            bmp_h: 0,
            font: null_mut(),
            bold: null_mut(),
            bold_keywords: false,
            char_w: 8,
            line_h: 16,
            text_dy: 1,
            bytes: Vec::new(),
            spans: Vec::new(),
            rows: Vec::new(),
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

    /// Applies the font settings in `cfg` at `pt` points and `dpi`. Replaces the regular and
    /// bold faces, then re-measures the cell size and the line pitch.
    pub(super) fn set_font(&mut self, cfg: &Config, pt: i32, dpi: u32) {
        // Same rounding as MulDiv(pt, dpi, 72).
        let height = -((pt * dpi as i32 + 36) / 72);
        let quality = match cfg.font_smoothing {
            FontSmoothing::ClearType => CLEARTYPE_QUALITY as u32,
            FontSmoothing::Grayscale => ANTIALIASED_QUALITY as u32,
            FontSmoothing::Off => NONANTIALIASED_QUALITY as u32,
        };
        // SAFETY: every font is deselected before it is deleted; `mem` is ours.
        unsafe {
            let font = choose_face(self.mem, height, &cfg.font_faces, cfg.font_weight, quality);
            let bold = if cfg.bold_keywords {
                choose_face(self.mem, height, &cfg.font_faces, FW_BOLD as i32, quality)
            } else {
                null_mut()
            };

            SelectObject(self.mem, GetStockObject(SYSTEM_FONT));
            for old in [self.font, self.bold] {
                if !old.is_null() {
                    DeleteObject(old);
                }
            }
            self.font = font;
            self.bold = bold;
            self.bold_keywords = cfg.bold_keywords;
            SelectObject(self.mem, font);

            let mut tm: TEXTMETRICW = zeroed();
            GetTextMetricsW(self.mem, &mut tm);
            let mut sz: SIZE = zeroed();
            let m = [b'M' as u16];
            GetTextExtentPoint32W(self.mem, m.as_ptr(), 1, &mut sz);
            self.char_w = sz.cx.max(1);
            self.line_h = ((tm.tmHeight as f32 * cfg.line_height).round() as i32).max(1);
            self.text_dy = ((self.line_h - tm.tmHeight) / 2).max(0);
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

    /// Expands `self.bytes[a..b]`, one visual row of the loaded line, into glyph cells.
    /// Cell 0 is the row start, so tabs stop relative to it. Cells left of `scroll_col`
    /// are skipped and expansion stops at cell `right`.
    fn expand_row(&mut self, a: usize, b: usize, tw: usize, scroll_col: usize, right: usize, cw: i32) {
        let mut cell = 0usize;
        let mut last: Option<Tok> = None;
        let mut si = self
            .spans
            .partition_point(|sp| (sp.start + sp.len) as usize <= a);
        let mut i = a;
        while i < b && cell < right {
            while si < self.spans.len() && (self.spans[si].start + self.spans[si].len) as usize <= i {
                si += 1;
            }
            let tok = match self.spans.get(si) {
                Some(sp) if sp.start as usize <= i => sp.tok,
                _ => Tok::Normal,
            };
            let byte = self.bytes[i];
            if byte == b'\t' {
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
            let len = utf8_len(byte).min(b - i);
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
    }

    /// Visible text: gutter, current-line tint, selection, syntax-coloured glyphs. Walks
    /// visual rows: one per logical line, or one per wrapped row in wrap mode. Continuation
    /// rows show no line number but a "·" in the gutter.
    fn draw_text(&mut self, ed: &mut Editor, cfg: &Config, lay: Layout) {
        let mem = self.mem;
        let theme = &cfg.theme;
        let tw = ed.tab_width.max(1);
        let (cw, lh, gutter) = (lay.cw, lay.lh, lay.gutter);
        let wrap = ed.wrap;
        // Wrapped text never scrolls sideways.
        let scroll_col = if wrap { 0 } else { ed.scroll_col };
        let width = ed.wrap_width();
        let (mut line, mut sub) = if wrap {
            ed.top_row(width)
        } else {
            (ed.scroll_line, 0)
        };
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
            fill(mem, rect(0, 0, gutter, lay.view_h), theme.gutter_bg);
        }
        // Row starts of `rows_line`, and the line whose bytes and spans are loaded.
        let mut rows = std::mem::take(&mut self.rows);
        let mut rows_line = usize::MAX;
        let mut loaded = usize::MAX;
        let mut y = 0i32;
        while y < lay.view_h && line < line_count {
            if rows_line != line {
                if wrap {
                    wrap_rows_buf(buf, tw, line, width, &mut rows);
                } else {
                    rows.clear();
                    rows.push(buf.line_start(line));
                }
                rows_line = line;
            }
            if sub >= rows.len() {
                line += 1;
                sub = 0;
                continue;
            }
            let ls = buf.line_start(line);
            let le = buf.line_end(line);
            let rs = rows[sub];
            let last = sub + 1 >= rows.len();
            let re = if last { le } else { rows[sub + 1] };

            if loaded != line {
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
                loaded = line;
            }

            if cfg.highlight_current_line && line == cur_line {
                fill(mem, rect(gutter, y, lay.w, y + lh), theme.current_line_bg);
            }
            if let Some((s, e)) = sel {
                let hit = if last {
                    s <= le && e > rs
                } else {
                    s < re && e > rs
                };
                if hit {
                    // A selected newline shows as one extra cell at the end of the line.
                    let v0 = if s > rs { vcol(buf, rs, s, tw) } else { 0 };
                    let v1 = if last && e > le {
                        vcol(buf, rs, le, tw) + 1
                    } else {
                        vcol(buf, rs, e.min(re), tw)
                    };
                    let x0 = (gutter + (v0 as i32 - scroll_col as i32) * cw).max(gutter);
                    let x1 = (gutter + (v1 as i32 - scroll_col as i32) * cw).min(lay.w);
                    fill(mem, rect(x0, y, x1, y + lh), theme.selection_bg);
                }
            }

            self.text.clear();
            self.dx.clear();
            self.runs.clear();
            self.expand_row(rs - ls, re - ls, tw, scroll_col, scroll_col + lay.cols, cw);

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
                let bold_run =
                    self.bold_keywords && matches!(r.tok, Tok::Keyword | Tok::Type);
                // SAFETY: the slices are in bounds and of equal length. Each glyph still
                // advances by its lpDx cell, so the bold face keeps the cell grid.
                unsafe {
                    SelectObject(mem, if bold_run { self.bold } else { self.font });
                    ExtTextOutW(
                        mem,
                        x,
                        y + self.text_dy,
                        ETO_CLIPPED,
                        &clip,
                        self.text[r.start..end].as_ptr(),
                        (end - r.start) as u32,
                        self.dx[r.start..end].as_ptr(),
                    );
                }
            }
            // The bold run may have left its face selected; the gutter and the later bars
            // all use the regular face.
            // SAFETY: self.font is a live font and `mem` is our DC.
            unsafe {
                SelectObject(mem, self.font);
            }

            if gutter > 0 && sub > 0 && wrap {
                self.put(mem, gutter - 2 * cw, y + self.text_dy, theme.gutter_fg, "\u{00B7}");
            } else if gutter > 0 {
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
                        y + self.text_dy,
                        0,
                        null(),
                        self.tmp16.as_ptr(),
                        self.tmp16.len() as u32,
                        null(),
                    );
                }
            }

            y += lh;
            sub += 1;
        }
        self.rows = rows;
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
            self.put(mem, lay.cw, y + self.text_dy, theme.status_fg, &p.label);
            let x = lay.cw + p.label.chars().count() as i32 * lay.cw;
            self.put(mem, x, y + self.text_dy, theme.status_fg, &p.text);
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
        self.put(mem, lay.cw, y + self.text_dy, theme.status_fg, &left);

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
            y + self.text_dy,
            theme.status_fg,
            &left,
        );
        self.tmp = left;
    }

    /// Two rows of "KEY Label" pairs.
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
                    y + self.text_dy,
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
                    y + self.text_dy,
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
        self.put(mem, lay.cw, self.text_dy, theme.hint_fg, HELP_HEADER);

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
                if !key.is_empty() {
                    // Same look as the hint bar: key text on a hint_fg box.
                    let kw = key.len() as i32 * lay.cw;
                    fill(mem, rect(x, y, x + kw, y + lay.lh), theme.hint_fg);
                    set_text_color(mem, theme.hint_key_fg);
                    // SAFETY: key is owned by self for the duration of the call.
                    unsafe {
                        ExtTextOutW(
                            mem,
                            x,
                            y + self.text_dy,
                            0,
                            null(),
                            key.as_ptr(),
                            key.len() as u32,
                            null(),
                        );
                    }
                }
                set_text_color(mem, theme.fg);
                // SAFETY: name is owned by self for the duration of the call.
                unsafe {
                    ExtTextOutW(
                        mem,
                        x + HELP_NAME_CELL * lay.cw,
                        y + self.text_dy,
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
            for font in [self.font, self.bold] {
                if !font.is_null() {
                    DeleteObject(font);
                }
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

/// Paints the custom scrollbars over the finished text, so the thumb is never hidden by a
/// glyph. A bar the pointer is over, or that is being dragged, shows its track and takes the
/// hover thumb colour.
pub(super) fn paint_scrollbars(
    dc: HDC,
    cfg: &Config,
    lay: Layout,
    bars: Bars,
    hover: Option<Axis>,
    drag: Option<Axis>,
) {
    let theme = &cfg.theme;
    if lay.hbar {
        // The strip is opaque, so the bottom of a text row cut off by it does not show.
        fill(dc, rect(0, lay.view_h, lay.w, lay.text_h), theme.bg);
        fill(
            dc,
            rect(0, lay.view_h, lay.gutter, lay.text_h),
            theme.gutter_bg,
        );
    }
    for bar in [bars.v, bars.h].into_iter().flatten() {
        let active = hover == Some(bar.axis) || drag == Some(bar.axis);
        if active {
            fill(dc, bar.track, theme.scrollbar_track);
        }
        let thumb = if active {
            theme.scrollbar_thumb_hover
        } else {
            theme.scrollbar_thumb
        };
        fill(dc, bar.thumb, thumb);
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
    let (row, col) = ed.cursor_view_pos()?;
    if row >= lay.rows {
        return None;
    }
    // A wrapped row can end in hanging spaces past the edge; the caret stops at the edge.
    let col = if ed.wrap {
        col.min(lay.cols)
    } else if col < lay.cols {
        col
    } else {
        return None;
    };
    Some((lay.gutter + col as i32 * lay.cw, row as i32 * lay.lh))
}

/// Paints one frame into the back buffer and copies `clip` of it to `hdc`.
pub(super) fn paint(app: &mut App, hdc: HDC, clip: RECT) {
    let lay = app.layout();
    let spot = caret_spot(app, lay);
    let zoom = app.zoom_pct();
    let bars = app.scroll_bars(lay);
    let hover = app.sb_hover;
    let drag = app.sb_drag.map(|d| d.axis);
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
    paint_scrollbars(mem, config, lay, bars, hover, drag);
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

/// Creates a fixed-pitch font named `face` at `height` and `weight`, and reports whether
/// Windows used that face. The font is selected only while it is checked: on return the
/// stock font is selected again, so the caller can delete the result.
///
/// # Safety
/// `dc` must be a valid memory DC.
unsafe fn make_font(
    dc: HDC,
    height: i32,
    face: &str,
    weight: i32,
    quality: u32,
) -> (HFONT, bool) {
    let name: Vec<u16> = face.encode_utf16().chain(std::iter::once(0)).collect();
    let font = CreateFontW(
        height,
        0,
        0,
        0,
        weight,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        quality,
        (FIXED_PITCH | FF_MODERN) as u32,
        name.as_ptr(),
    );
    SelectObject(dc, font);
    let mut got = [0u16; 64];
    let n = GetTextFaceW(dc, got.len() as i32, got.as_mut_ptr());
    // GetTextFaceW counts the terminating NUL.
    let len = (n.max(1) - 1) as usize;
    let matched = String::from_utf16_lossy(&got[..len]).eq_ignore_ascii_case(face);
    SelectObject(dc, GetStockObject(SYSTEM_FONT));
    (font, matched)
}

/// Returns the first face in `faces` that Windows actually matches, then Consolas, at
/// `weight`. If Consolas does not match either, GDI's substitute is returned. The result is
/// not selected into `dc`.
///
/// # Safety
/// `dc` must be a valid memory DC.
unsafe fn choose_face(dc: HDC, height: i32, faces: &[String], weight: i32, quality: u32) -> HFONT {
    for face in faces.iter().map(String::as_str).chain(std::iter::once("Consolas")) {
        let (font, matched) = make_font(dc, height, face, weight, quality);
        if matched {
            return font;
        }
        DeleteObject(font);
    }
    make_font(dc, height, "Consolas", weight, quality).0
}

pub(super) fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
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
