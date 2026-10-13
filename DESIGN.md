# note — architecture & module contracts

Ultra-light Win32 text editor. Pure Win32 + GDI, no frameworks, no menu bar.
One window with tabs: optional tab strip (shown with two or more tabs by default), text area,
1-line status bar, (toggleable) 2-line shortcut hint bar.
Dark by default, configurable via `%APPDATA%\note\note.ini`. Windows Notepad keys by default, plus
a few line-editing extras (ctrl+k cut line, ctrl+d duplicate, alt+up/down move line).

Crate layout (library + thin binary so everything but `win/` is unit-testable on Linux):

```
src/main.rs      #![windows_subsystem = "windows"]; on windows calls note::win::run(); else prints a message
src/lib.rs       pub mod simd; pub mod buffer; pub mod syntax; pub mod config; pub mod editor; pub mod encoding;
                 pub mod regex; pub mod search; #[cfg(windows)] pub mod win;
src/simd.rs      SIMD byte kernels (SSE2 baseline, AVX2 runtime-dispatched)
src/encoding.rs  file bytes <-> internal UTF-8 (+ line ending detection)
src/buffer.rs    gap buffer + line index + undo/redo
src/syntax.rs    tiny incremental lexer-highlighter
src/config.rs    INI parsing, theme, keymap
src/regex.rs     small backtracking regex engine + replacement templates
src/search.rs    find/replace queries (plain, escapes, regex, number range, fuzzy) and scans
src/editor.rs    platform-independent editor state & commands
src/win/mod.rs   window, message loop, input -> editor::Command, dialogs, clipboard
src/win/find.rs  Find/Replace dialog (in-memory template), live preview, regex completions
src/win/render.rs GDI double-buffered painting
```

Rules for ALL modules:
- No external crates except `windows-sys` (only in `src/win/`). std only.
- No `unwrap()` on user/file data paths; never panic on bad input.
- Hot paths must avoid allocation per frame / per keystroke where reasonable.
- `cargo test` (Linux host) must pass; `cargo check --target x86_64-pc-windows-gnu` must pass.
- Every module has `#[cfg(test)] mod tests` with meaningful tests.

---------------------------------------------------------------------------
## simd.rs

```rust
/// Index of first occurrence of `needle` in `hay`.
pub fn find_byte(hay: &[u8], needle: u8) -> Option<usize>;
/// Index of last occurrence.
pub fn rfind_byte(hay: &[u8], needle: u8) -> Option<usize>;
/// Number of occurrences of `needle` (used for '\n' counting).
pub fn count_byte(hay: &[u8], needle: u8) -> usize;
/// Push `base + i` for every index i where hay[i] == b'\n' into `out`.
pub fn newline_positions(hay: &[u8], base: usize, out: &mut Vec<usize>);
/// Substring search. Empty needle -> Some(0).
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize>;
/// ASCII-case-insensitive substring search.
pub fn find_ascii_ci(hay: &[u8], needle: &[u8]) -> Option<usize>;
/// Remove every b'\r' that is immediately followed by b'\n' (CRLF -> LF), in place-ish. Returns new Vec.
pub fn crlf_to_lf(src: &[u8]) -> Vec<u8>;
/// LF -> CRLF.
pub fn lf_to_crlf(src: &[u8]) -> Vec<u8>;
/// true if all bytes < 0x80.
pub fn is_ascii(src: &[u8]) -> bool;
```
Implementation: x86_64 uses `core::arch::x86_64`. SSE2 is baseline on x86_64 (always available).
AVX2 path chosen at runtime via `is_x86_feature_detected!("avx2")`, cached in a `static AtomicU8`.
`#[target_feature(enable = "avx2")] unsafe fn ..._avx2`. Substring search: first+last byte
SIMD filter (the "generic SIMD" / memchr-style algorithm) then memcmp verify. Non-x86 targets: scalar fallback.
Tests must compare every function against a naive scalar implementation on many random-ish
inputs (use a simple xorshift PRNG, no rand crate) across lengths 0..300 and unaligned offsets.

---------------------------------------------------------------------------
## encoding.rs

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding { Utf8, Utf8Bom, Utf16Le, Utf16Be, Latin1 }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding { Lf, CrLf }

pub struct Decoded { pub text: Vec<u8> /* valid UTF-8, LF only */, pub encoding: Encoding, pub line_ending: LineEnding }

/// Detect BOM / validity. Invalid UTF-8 without BOM -> Latin1 (each byte -> U+00xx), lossless.
/// Line ending = CrLf if the first '\n' in the file is preceded by '\r', else Lf (default CrLf for empty file).
/// Output text has CRLF normalised to LF (use simd::crlf_to_lf). Use simd::is_ascii fast path.
pub fn decode(bytes: &[u8]) -> Decoded;
/// Inverse: apply line ending (simd::lf_to_crlf) then encode (write BOM where applicable).
/// Latin1: chars > U+00FF become b'?'.
pub fn encode(text: &[u8], encoding: Encoding, le: LineEnding) -> Vec<u8>;
impl Encoding { pub fn name(self) -> &'static str; } // "UTF-8", "UTF-8 BOM", "UTF-16 LE", "UTF-16 BE", "ANSI"
impl LineEnding { pub fn name(self) -> &'static str; } // "LF", "CRLF"
```

---------------------------------------------------------------------------
## buffer.rs — gap buffer of UTF-8 bytes

All positions are BYTE offsets into the logical text. Callers keep them on char boundaries.

```rust
pub struct Buffer { /* gap buffer: Vec<u8> with gap_start/gap_end; line_starts: Vec<usize>; undo stacks; dirty flag */ }

impl Buffer {
    pub fn new() -> Self;
    pub fn from_text(text: Vec<u8>) -> Self;          // builds line index with simd::newline_positions
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn byte_at(&self, pos: usize) -> u8;
    pub fn line_count(&self) -> usize;                // >= 1 always
    pub fn line_start(&self, line: usize) -> usize;
    pub fn line_end(&self, line: usize) -> usize;      // byte offset of the '\n' (or len) — exclusive of newline
    pub fn line_of(&self, pos: usize) -> usize;        // binary search in line_starts
    /// Copy bytes [start,end) into `out` (clears it first). Used by renderer for a single line.
    pub fn slice_into(&self, start: usize, end: usize, out: &mut Vec<u8>);
    pub fn slice(&self, start: usize, end: usize) -> Vec<u8>;
    pub fn text(&self) -> Vec<u8>;                     // whole text, contiguous
    /// Contiguous view: moves gap to the end then returns &[u8] of the whole text (for search/save).
    pub fn make_contiguous(&mut self) -> &[u8];

    pub fn insert(&mut self, pos: usize, s: &[u8]);    // records undo, updates line index incrementally
    pub fn delete(&mut self, start: usize, end: usize);// records undo, updates line index incrementally

    // Undo: each record = {pos, deleted bytes, inserted bytes, cursor_before, cursor_after}.
    // Consecutive single-char inserts/deletes at adjacent positions coalesce into one record
    // unless break_undo_group() was called in between or a newline / word boundary is crossed.
    pub fn set_cursor_hint(&mut self, cursor: usize); // cursor_before for the next recorded edit
    pub fn break_undo_group(&mut self);
    pub fn begin_compound(&mut self); pub fn end_compound(&mut self); // group several edits into one undo step
    /// Returns cursor position to restore, or None if nothing to undo.
    pub fn undo(&mut self) -> Option<usize>;
    pub fn redo(&mut self) -> Option<usize>;

    pub fn is_dirty(&self) -> bool;                    // true if differs from last mark_clean (track via undo-stack depth/version id)
    pub fn mark_clean(&mut self);
    /// Monotonic counter bumped on every mutation (renderer/highlighter cache key).
    pub fn version(&self) -> u64;
    /// First line whose content changed since the last call (then resets to usize::MAX). For syntax cache invalidation.
    pub fn take_first_dirty_line(&mut self) -> usize;
}
```
Line index update: on insert, count newlines in inserted text with simd and splice new entries into
`line_starts`, then shift subsequent entries by `s.len()` (a tight loop the compiler vectorises).
On delete, remove entries whose newline was inside the range and shift the rest.
Gap growth: double capacity, min 4 KiB gap.

---------------------------------------------------------------------------
## syntax.rs

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Tok { Normal=0, Keyword=1, Type=2, String=3, Number=4, Comment=5, Preproc=6, Function=7, Punct=8, Heading=9 }
pub const TOK_COUNT: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang { Plain, Rust, C, Cpp, CSharp, Python, JavaScript, TypeScript, Json, Toml, Ini, Shell, PowerShell, Batch, Markdown, Html, Css, Go, Lua, Sql, Zig }

impl Lang {
    pub fn from_path(path: &str) -> Lang;   // by extension, case-insensitive; also "Makefile", "Dockerfile" -> Shell
    pub fn name(self) -> &'static str;
}

/// Lexer state carried across lines (block comments, multi-line strings). 0 = default.
pub type LineState = u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span { pub start: u32, pub len: u32, pub tok: Tok } // byte offsets within the line

/// Tokenise ONE line (no trailing '\n'). Clears and fills `out` with spans covering only non-Normal
/// regions (gaps are Normal). Returns the state at end of line.
pub fn highlight_line(lang: Lang, line: &[u8], state_in: LineState, out: &mut Vec<Span>) -> LineState;

/// Cache of line-start states so rendering line N only needs to lex from the nearest valid entry.
pub struct Highlighter { /* lang, states: Vec<LineState> (state at START of each line), valid_upto: usize */ }
impl Highlighter {
    pub fn new(lang: Lang) -> Self;
    pub fn lang(&self) -> Lang;
    pub fn set_lang(&mut self, lang: Lang);           // invalidates all
    pub fn invalidate_from(&mut self, line: usize);   // called with Buffer::take_first_dirty_line()
    /// State at start of `line`; lexes forward from last valid cached line using `get_line(i, &mut scratch)`
    /// which fills a scratch Vec<u8> with line i's bytes.
    pub fn state_at(&mut self, line: usize, get_line: &mut dyn FnMut(usize, &mut Vec<u8>)) -> LineState;
}
```
Keyword tables: static sorted `&[&str]` per language, matched with binary search on the identifier
bytes. Identifiers followed by `(` -> Function. Uppercase-initial identifiers in Rust/C#/Go/TS -> Type
(plus primitive type keyword lists). Numbers: dec/hex/bin/float with suffixes. Strings with escapes.
Rust raw strings r#".."# best effort, lifetimes `'a` not treated as char literal. Python triple quotes span lines.
C-like `/* */` spans lines. `#` lines in C/C++ -> Preproc. Markdown: `#` headings -> Heading, `` ` `` code -> String,
fenced ``` blocks -> String across lines. INI: `[section]` Keyword, `key` Type, `;`/`#` comment. HTML: tags Keyword, attrs Type, strings, `<!-- -->` comments spanning lines.

---------------------------------------------------------------------------
## config.rs

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);
impl Rgb { pub fn colorref(self) -> u32 /* 0x00BBGGRR */; pub fn parse(s: &str) -> Option<Rgb> /* "#rrggbb" or "rrggbb" */; }

pub struct Theme {
    pub bg: Rgb, pub fg: Rgb, pub caret: Rgb, pub selection_bg: Rgb, pub current_line_bg: Rgb,
    pub gutter_bg: Rgb, pub gutter_fg: Rgb, pub status_bg: Rgb, pub status_fg: Rgb,
    pub hint_key_fg: Rgb, pub hint_fg: Rgb,
    pub scrollbar_thumb: Rgb, pub scrollbar_thumb_hover: Rgb, pub scrollbar_track: Rgb,
    pub match_bg: Rgb, pub preview_fg: Rgb,   // live preview: match background, line-after-replace text
    pub tabbar_bg: Rgb, pub tab_hover_bg: Rgb, pub tab_fg: Rgb, // tab strip colours
    pub tok: [Rgb; syntax::TOK_COUNT],   // indexed by Tok as usize; tok[0] == fg
}
impl Theme { pub fn dark() -> Theme; pub fn light() -> Theme; }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Action {
    Save, SaveAs, Open, New, Exit, CutLine, Paste /* clipboard, else the cut buffer */, Copy, Cut, Find, FindNext, FindPrev,
    Replace, GotoLine, ShowPosition, Help, Undo, Redo, SelectAll, LineHome, LineEnd, PageUp, PageDown,
    DocStart, DocEnd, WordLeft, WordRight, DeleteWordLeft, DeleteWordRight, ToggleLineNumbers, ToggleWrap,
    ToggleHints, ZoomIn, ZoomOut, ZoomReset, Indent, Unindent, DuplicateLine, MarkToggle /* mark mode: movement extends selection */,
    ToggleComment, MoveLineUp, MoveLineDown,
    CloseTab, ReopenTab, NextTab, PrevTab, Tab1, Tab2, Tab3, Tab4, Tab5, Tab6, Tab7, Tab8,
    LastTab, MoveTabLeft, MoveTabRight, NewWindow,
}
impl Action { pub fn from_name(s: &str) -> Option<Action> /* snake_case e.g. "cut_line" */; pub fn name(self) -> &'static str; }

/// Key chord: virtual-key code (Win32 VK_* numbering: 'A'..'Z' = 0x41.., VK_F1=0x70 etc.) + modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Chord { pub vk: u16, pub ctrl: bool, pub alt: bool, pub shift: bool }
impl Chord { pub fn parse(s: &str) -> Option<Chord> /* "ctrl+k", "alt+u", "ctrl+shift+z", "f3", "ctrl+6", "ctrl+/", "ctrl+\\", "ctrl+_" (=ctrl+shift+minus), "ctrl+plus", "ctrl+minus" */; pub fn display(&self) -> String /* "^K", "M-U", "F3", "^S-Z" */; }

pub struct Keymap { /* small Vec<(Chord, Action)>, linear scan is fine (<100 entries) */ }
impl Keymap {
    pub fn standard() -> Keymap;      // default — see table below
    pub fn lookup(&self, c: Chord) -> Option<Action>;
    pub fn bind(&mut self, c: Chord, a: Action); // replaces existing binding for chord
    pub fn unbind(&mut self, c: Chord);
    pub fn chord_for(&self, a: Action) -> Option<Chord>;  // first binding, for hint bar
}

pub struct Config {
    pub font_faces: Vec<String>,// preference list, ["Paper Mono"] (renderer then falls back to "Consolas")
    pub font_size: i32,         // points, 11
    pub font_weight: i32,       // 100..=900, 400
    pub bold_keywords: bool,    // keyword/type runs use a weight-700 face, false
    pub line_height: f32,       // pitch = round(tmHeight * line_height), 1.0..=2.5, 1.2
    pub font_smoothing: FontSmoothing, // ClearType | Grayscale | Off
    pub tab_width: usize,       // 4
    pub insert_spaces: bool,    // false
    pub line_numbers: bool,     // true
    pub word_wrap: bool,        // false
    pub show_hints: bool,       // false (shortcut bar, Alt+X)
    pub highlight_current_line: bool, // true
    pub syntax: bool,           // true
    pub auto_indent: bool,      // true
    pub caret_width: i32,       // 2
    pub theme: Theme,
    pub keymap: Keymap,
}
impl Default for Config { /* dark theme + standard keymap */ }
impl Config {
    /// Parse INI text on top of defaults. Unknown keys ignored. Never fails.
    pub fn parse(src: &str) -> Config;
    /// Default file contents written on first run (commented, documents every key & default binding).
    pub fn default_ini() -> String;
}
```
INI format:
```
[editor]
font = Cascadia Mono
font_size = 11
tab_width = 4
insert_spaces = false
line_numbers = true
word_wrap = false
show_hints = false
highlight_current_line = true
syntax = true
auto_indent = true
[theme]
base = dark            ; dark | light — then any color override:
bg = #1e1e1e
keyword = #569cd6      ; token colors: normal keyword type string number comment preproc function punct heading
[keys]
ctrl+k = cut_line      ; rebind
ctrl+q = none          ; unbind
```

Default keymap: Windows Notepad keys plus line-editing extras:
| chord | action | | chord | action |
|---|---|---|---|---|
| ctrl+s | save | | ctrl+shift+s | save_as |
| ctrl+o | open | | ctrl+t / ctrl+n | new tab |
| ctrl+q | exit (alt+f4 via DefWindowProc) | | F1 | help |
| ctrl+w / ctrl+f4 | close_tab | | ctrl+shift+t | reopen_tab |
| ctrl+tab / ctrl+pgdn | next_tab | | ctrl+shift+tab / ctrl+pgup | prev_tab |
| ctrl+1 .. ctrl+8 | tab_1 .. tab_8 | | ctrl+9 | last_tab |
| ctrl+shift+pgup / pgdn | move_tab_left / right | | ctrl+shift+n | new_window |
| ctrl+z | undo | | ctrl+y / ctrl+shift+z | redo |
| ctrl+x / shift+delete | cut | | ctrl+c / ctrl+insert | copy |
| ctrl+v / shift+insert | paste | | ctrl+a | select_all |
| ctrl+k | cut_line | | ctrl+d | duplicate_line |
| ctrl+/ | toggle_comment | | alt+up / alt+down | move_line_up/down |
| tab / shift+tab | indent / unindent | | alt+a | mark_toggle |
| ctrl+backspace / ctrl+delete | delete_word_left/right | | | |
| ctrl+f | find | | ctrl+h | replace |
| F3 | find_next | | shift+F3 | find_prev |
| ctrl+g | goto_line | | alt+c | show_position |
| home / end | line_home / line_end | | pgup / pgdn | page_up / page_down |
| ctrl+home / ctrl+end | doc_start / doc_end | | ctrl+left/right | word_left/right |
| ctrl+plus/minus/0 | zoom in/out/reset | | alt+z | toggle_wrap |
| alt+n | toggle_line_numbers | | alt+x | toggle_hints |

(Shifted navigation keys — arrows, Home/End, PgUp/PgDn and their Ctrl forms — are not in the
keymap; the window layer handles them directly and extends the selection.)

---------------------------------------------------------------------------
## regex.rs — backtracking regex engine

```rust
pub struct Options { pub ignore_case: bool, pub whole_word: bool, pub literal: bool }
pub struct Error { pub pos: usize /* char index in the pattern */, pub msg: String }
pub struct TooComplex;                       // the step budget ran out
impl Regex {
    pub fn new(pattern: &str, opts: Options) -> Result<Regex, Error>;
    pub fn find_at(&self, t: &[u8], from: usize, limit: usize, caps: &mut Captures, budget: &mut u64)
        -> Result<Option<(usize, usize)>, TooComplex>;   // first match starting in from..=limit
    pub fn rfind_before(&self, t: &[u8], lo: usize, before: usize, caps, budget) -> ...; // last start in lo..before
    pub fn match_at(&self, t: &[u8], s: usize, caps, budget) -> Result<Option<usize>, TooComplex>;
    pub fn group_count(&self) -> usize; pub fn group_names(&self) -> impl Iterator<Item = (&str, usize)>;
}
impl Template { pub fn literal(s) -> Template; pub fn escapes(s) -> Result<..>; pub fn regex(s, &Regex) -> Result<..>;
                pub fn expand(&self, t: &[u8], caps: &Captures, out: &mut Vec<u8>); }
```
Pattern -> tree (parser with char positions for errors) -> instruction list -> backtracking VM over
UTF-8 bytes with an explicit stack (Try / restore-capture / restore-mark frames), so deep patterns do
not recurse. Lookaround and atomic groups run a sub-program; captures a successful positive
lookahead sets are kept and restored on backtracking. Lookbehind tries starts from `pos - min` back to
`pos - max` (byte length bounds from the tree; unbounded is allowed). `x*` over a body that can match
empty gets Mark/Check instructions so an empty iteration ends the loop. Counted repeats are unrolled
(max 1000; program max 200k instructions). Start positions are skipped with a hint computed from the
tree: a literal prefix (SIMD `find` / `find_ascii_ci`), line starts, text start, or a first-byte set.
Every instruction costs one step of the caller's budget. `^`/`$` are line anchors unless `(?-m)`.
`\r\n` and `\R` compile to `\r?\n` because the buffer holds LF line breaks. Case folding is
Unicode simple folding (single-char lowercase). Whole word wraps the pattern in a guard that only
applies on a side where the match starts or ends with a word char.

---------------------------------------------------------------------------
## search.rs — queries and scans

```rust
pub enum Mode { Plain, Escapes, Regex, NumberRange }
pub struct Query { pub find, pub replace: String, pub mode: Mode, pub match_case, pub whole_word,
                   pub fuzzy, pub preserve_case: bool }
pub type Scope = Option<(usize, usize)>;   // "In the Selection Only"
impl Searcher {
    pub fn new(q: &Query) -> Result<Searcher, QueryError>;   // bad Replace text -> replace_error()
    pub fn find(&mut self, t, from, limit, budget) -> Result<Option<(usize, usize)>, TooComplex>;
    pub fn rfind(&mut self, t, lo, before, budget) -> ...;
    pub fn expand(&self, t, out);                           // replacement for the last match
}
pub fn find_in(s, t, scope, from, forward, wrap, budget) -> Result<Option<Found>, TooComplex>;
pub fn count(s, t, scope, budget); pub fn replace_all(s, t, scope, track: &mut [usize], budget);
pub fn hits(s, t, from, limit, max, with_rep, budget) -> Result<Vec<Hit>, TooComplex>;
pub fn completions(before: &str, field: Field, groups: usize, names: &[String]) -> Option<(usize, Vec<Completion>)>;
```
Plain and escape-sequence text compile to literal regexes. Number range scans number tokens
(`-?digits(.digits)?`, never starting inside another number) and compares as f64. Fuzzy is Sellers'
edit-distance column with a swap rule (optimal string alignment), k = len / 4, end extended while the
distance drops, start found by an anchored backward run. Replace all rebuilds only first-match..last-match
and maps tracked positions (cursor, anchor, scope end). Budgets: FIND_BUDGET for commands,
PREVIEW_BUDGET for the live preview and live count. `FIND_TOKENS` / `REPLACE_TOKENS` feed the
dialog's ">" menus; `completions` feeds its completion list.

---------------------------------------------------------------------------
## editor.rs — platform independent

```rust
pub struct Editor {
    pub buf: Buffer,
    pub cursor: usize,               // byte offset
    pub anchor: Option<usize>,       // selection anchor; selection = [min(anchor,cursor), max)
    pub mark_mode: bool,             // alt+a: movement extends selection without shift
    pub preferred_col: Option<usize>,// visual column memory for up/down
    pub scroll_line: usize,          // first visible line (no-wrap mode) / first visible visual row
    pub scroll_col: usize,           // horizontal scroll in columns (no-wrap mode)
    pub cut_buffer: Vec<u8>,         // nano cut buffer
    pub last_was_cut_line: bool,     // consecutive ^K append to cut_buffer (nano semantics)
    pub path: Option<String>,
    pub encoding: Encoding, pub line_ending: LineEnding,
    pub tab_width: usize, pub insert_spaces: bool, pub auto_indent: bool,
    pub highlighter: Highlighter,
    pub message: Option<String>,     // transient status-bar message
}

pub enum Move { Left, Right, Up, Down, LineHome /* smart home: first non-ws then col 0 */, LineEnd,
                WordLeft, WordRight, PageUp(usize /*rows*/), PageDown(usize), DocStart, DocEnd }

impl Editor {
    pub fn new(cfg: &Config) -> Self;
    pub fn load(&mut self, path: &str, bytes: &[u8]);     // decode, set lang from path, reset state
    pub fn save_bytes(&mut self) -> Vec<u8>;              // encode with current encoding/line ending
    pub fn title(&self) -> String;                        // "*name - note" / "Untitled - note"

    pub fn move_cursor(&mut self, m: Move, extend: bool); // extend = shift held || mark_mode
    pub fn set_cursor(&mut self, pos: usize, extend: bool);
    pub fn selection(&self) -> Option<(usize, usize)>;
    pub fn selected_text(&self) -> Option<Vec<u8>>;

    pub fn insert_char(&mut self, ch: char);              // replaces selection; '\n' applies auto-indent; '\t' uses insert_spaces
    pub fn insert_text(&mut self, s: &[u8]);              // paste: replaces selection; compound undo
    pub fn backspace(&mut self);  pub fn delete_forward(&mut self);
    pub fn delete_word_left(&mut self); pub fn delete_word_right(&mut self);
    /// nano ^K: cut current line (or selection). Consecutive calls append to cut_buffer. Returns text for clipboard.
    pub fn cut_line(&mut self) -> Vec<u8>;
    pub fn cut_selection(&mut self) -> Option<Vec<u8>>;
    pub fn copy_selection(&self) -> Option<Vec<u8>>;
    pub fn indent(&mut self); pub fn unindent(&mut self);
    pub fn duplicate_line(&mut self); pub fn move_line(&mut self, up: bool); pub fn toggle_comment(&mut self);
    pub fn select_all(&mut self);
    pub fn undo(&mut self); pub fn redo(&mut self);
    /// Next match after the selection (or previous before it) in scope, wrapping if asked. Selects it.
    pub fn find(&mut self, s: &mut Searcher, forward: bool, wrap: bool, scope: Scope, budget: &mut u64)
        -> Result<Option<Found>, TooComplex>;
    /// If the selection is exactly a match, replace it; then find the next. Returns the moved scope.
    pub fn replace_next(&mut self, s, wrap, scope, budget) -> Result<(bool, Option<Found>, Scope), TooComplex>;
    pub fn replace_all(&mut self, s, scope, budget) -> Result<(usize, Scope), TooComplex>; // one undo step
    pub fn count_matches(&mut self, s, scope, budget) -> Result<usize, TooComplex>;
    pub fn hits(&mut self, s, first_line, last_line, max, with_rep, budget) -> Result<Vec<Hit>, TooComplex>;
    pub fn preview_lines(&self, hits: &[Hit], max_chars: usize) -> Vec<(usize, String)>; // line after replace
    pub fn goto_line(&mut self, line_1based: usize, col_1based: usize);
    /// Called before any action that is not cut_line, to reset nano append semantics.
    pub fn reset_cut_chain(&mut self);

    // ---- view helpers (no-wrap mode) ----
    pub fn cursor_line_col(&self) -> (usize, usize);      // 0-based line, 0-based VISUAL column (tabs expanded)
    /// Visual column of byte `pos` within its line, tabs expanded, each char = 1 cell
    /// (East Asian wide chars may count 2 — optional).
    pub fn visual_col(&self, pos: usize) -> usize;
    /// Byte offset in `line` closest to visual column `col`.
    pub fn pos_from_visual(&self, line: usize, col: usize) -> usize;
    /// Adjust scroll_line/scroll_col so the cursor is visible with given viewport size (rows, cols).
    pub fn ensure_cursor_visible(&mut self, rows: usize, cols: usize);
    pub fn scroll_by(&mut self, lines: isize);
}
```
Char boundaries: Left/Right/backspace/delete step over whole UTF-8 scalar values
(continuation bytes are 0b10xxxxxx). Word = alphanumeric or '_' (ASCII) or any non-ASCII byte.

---------------------------------------------------------------------------
## win/ (Windows only)

- `run()`: SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2); read config from %APPDATA%\note\note.ini
  (create with Config::default_ini() if missing); file paths from the command line
  (CommandLineToArgvW), one tab each (none: the saved session when restore_tabs is on);
  RegisterClassExW with no background brush (we paint everything; WM_ERASEBKGND returns 1);
  CreateWindowExW; dark title bar via DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE=20) when theme bg is dark;
  `DragAcceptFiles` for drag & drop open; message loop with GetMessageW/TranslateMessage/DispatchMessageW.
- State held in a `Box<App>` pointer stored in GWLP_USERDATA.
- Input: WM_KEYDOWN/WM_SYSKEYDOWN -> build Chord (GetKeyState for ctrl/alt/shift) -> keymap lookup -> dispatch Action.
  Unmapped navigation keys handled directly (arrows/Home/End/PgUp/PgDn/Delete/Backspace/Enter/Tab, Shift=extend).
  WM_CHAR inserts printable chars (>= 0x20, excluding when Ctrl is down unless AltGr i.e. ctrl+alt);
  handle UTF-16 surrogate pairs. WM_SYSCHAR swallowed when we handled alt combo (avoid beep).
  Mouse: click positions caret, shift+click extends, drag selects (SetCapture), double-click selects word,
  wheel scrolls 3 lines (ctrl+wheel zooms). Scrollbars are custom-painted (no WS_VSCROLL): a vertical
  strip on the text's right edge and a horizontal one above the status row (only without wrap, when a
  visible line runs past the view). Hit-tested before the text in mouse messages; WM_SETCURSOR shows an arrow over them.
- Prompt bar: the goto-line prompt is typed into the status line area — a tiny inline
  single-line input owned by App (no EDIT control): Enter confirms, Esc cancels.
- Find/Replace (find.rs): a modeless dialog from an in-memory DLGTEMPLATEEX (CreateDialogIndirectParamW,
  Segoe UI 9, laid out in dialog units like EmEditor's Replace dialog; Find mode hides the Replace row
  and moves the rest up). The dialog procedure only forwards WM_COMMAND to the main window as WM_FIND,
  so App reads the controls and runs commands from the main window procedure. The message loop calls
  `find::pre_translate` first (F3 / Shift+F3 / Ctrl+F / Ctrl+H inside the dialog, then
  IsDialogMessageW). The combo boxes' edits are subclassed for regex completions: a WS_POPUP list box
  (WS_EX_NOACTIVATE, MA_NOACTIVATE) under the caret; WM_GETDLGCODE asks for all keys while it shows.
  Options, history (20 per box) and the compiled Searcher live in `FindUi` and outlive the dialog window.
  Live preview: before each paint, matches for the visible lines (and what each line becomes after
  Replace) are computed with PREVIEW_BUDGET and cached by (buffer version, query generation, first and
  last line, mode, scope); the live match count is cached by (version, generation, scope).
- assets/note.manifest (embedded as RT_MANIFEST 1 by build.rs) selects Common Controls 6, so the
  dialog, message boxes and buttons use the current Windows look.
  Tabs: `App` keeps the active document in `editor` and the others in `tabs`, where
  `tabs[active]` is an empty placeholder. Switching is two `mem::swap`s (`swap_to`), then
  `switched`: no prompt stays open, Find resets its per-document caches, and the window
  follows the new tab. The strip (`tab_strip`, `TabStrip`) is 32 px at 96 dpi, shown per
  `tab_bar`. While it is shown, `layout()` takes its height off the editor, `render::paint`
  draws the strip at y=0 and offsets everything else with SetViewportOrgEx, and mouse y values
  have the strip height subtracted before the text and scrollbar code sees them.
  Closing a dirty tab, or exiting, asks with a MessageBoxW (`confirm_close_active`): Yes saves
  (a failed or cancelled save counts as Cancel), No discards, Cancel stops. `request_exit`
  asks about each dirty tab in turn and then quits; closing the last tab quits too. With
  `restore_tabs`, `save_session` writes the open file paths and `active=<index>` to
  `%APPDATA%\note\session.txt` as the window is destroyed, and `restore_session` reads them
  at startup when no file was given (missing files are skipped).
  Open/Save As use GetOpenFileNameW/GetSaveFileNameW (comdlg32) because users expect them.
  Save As lists "Text Documents (*.txt)" first: with it selected, a name typed without an extension
  gets .txt (`editor::save_as_path`); "All Files" keeps the name as typed. No lpstrDefExt: comdlg32
  can append it even after a typed extension it does not recognise (notes.md -> notes.md.txt).
  The overwrite prompt is a MessageBoxW after the extension is applied, so it checks the final name.
- Clipboard: CF_UNICODETEXT, convert LF <-> CRLF.
- Rendering (render.rs): one memory DC + DIB section bitmap sized to client rect, recreated on WM_SIZE only.
  Fonts: CreateFontW (fixed pitch), regular plus optional bold face; char width from GetTextExtentPoint32W("M").
  `win/fonts.rs` registers the embedded Paper Mono faces with AddFontMemResourceEx at startup (feature `embed-font`).
  Paint only visible lines: for each line, slice_into scratch Vec<u8>, highlight via Highlighter::state_at +
  highlight_line, convert UTF-8 -> UTF-16 into reusable scratch Vec<u16> while expanding tabs, emit
  ExtTextOutW runs per span color (SetTextColor). Selection: fill rect behind selected columns. Caret: own
  painted bar (no system caret) — blink with SetTimer 530ms, reset on input. Gutter with line numbers.
  Tab strip (`draw_tab_bar`): rounded active tab filled with `bg` so it joins the text area,
  hovered tab in tab_hover_bg, 1px separators between idle tabs, titles in Segoe UI 9 (`ui_font`),
  a close box or dirty dot, and the "+" button.
  Live preview: match_bg behind every match (under the selection), and the line-after-replace text in
  preview_fg after the end of each line that has a match ("→ ..."); line breaks show as '↵'.
  Status bar: "path  |  Ln x, Col y  |  Lang  |  UTF-8  |  CRLF  |  message". Hint bar: two rows of
  "^K Cut Line" pairs like nano, built from keymap.chord_for().
  BitBlt memory DC to window DC in WM_PAINT. Invalidate on change only.
