//! INI configuration, colour themes, key chords and the nano-style keymap.
//!
//! Pure std and platform independent. The `VK_*` codes are Win32 virtual-key
//! numbering, defined locally so this module builds and tests on any host.

/// Number of token colour slots. Must equal `syntax::TOK_COUNT`.
pub const TOK_COUNT: usize = 10;

// ---------------------------------------------------------------------------
// Win32 virtual-key codes (letters are 0x41..=0x5A, digits 0x30..=0x39).
// ---------------------------------------------------------------------------
pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_SPACE: u16 = 0x20;
pub const VK_PRIOR: u16 = 0x21;
pub const VK_NEXT: u16 = 0x22;
pub const VK_END: u16 = 0x23;
pub const VK_HOME: u16 = 0x24;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_INSERT: u16 = 0x2D;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_ADD: u16 = 0x6B;
pub const VK_SUBTRACT: u16 = 0x6D;
pub const VK_F1: u16 = 0x70;
pub const VK_OEM_PLUS: u16 = 0xBB;
pub const VK_OEM_COMMA: u16 = 0xBC;
pub const VK_OEM_MINUS: u16 = 0xBD;
pub const VK_OEM_PERIOD: u16 = 0xBE;
pub const VK_OEM_2: u16 = 0xBF;
pub const VK_OEM_5: u16 = 0xDC;

// ---------------------------------------------------------------------------
// Colours
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Win32 COLORREF, 0x00BBGGRR.
    pub fn colorref(self) -> u32 {
        u32::from_le_bytes([self.0, self.1, self.2, 0])
    }

    /// Accepts `#rrggbb`, `rrggbb`, `#rgb` and `rgb` (case-insensitive, surrounding whitespace ok).
    pub fn parse(s: &str) -> Option<Rgb> {
        let s = s.trim();
        let hex = s.strip_prefix('#').unwrap_or(s).as_bytes();
        if !hex.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let v = hex_val;
        match hex {
            [r, g, b] => Some(Rgb(v(*r) * 17, v(*g) * 17, v(*b) * 17)),
            [r1, r2, g1, g2, b1, b2] => Some(Rgb(
                (v(*r1) << 4) | v(*r2),
                (v(*g1) << 4) | v(*g2),
                (v(*b1) << 4) | v(*b2),
            )),
            _ => None,
        }
    }

    /// `#rrggbb`, lower case.
    fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// Value of an ASCII hex digit. Only called after the digit check, so the fallback is unreachable.
fn hex_val(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => 0,
    }
}

/// Token colour names, indexed like `Theme::tok` (`Tok as usize`). Index 0 is the same as `fg`.
const TOKEN_NAMES: [&str; TOK_COUNT] = [
    "normal", "keyword", "type", "string", "number", "comment", "preproc", "function", "punct",
    "heading",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    pub bg: Rgb,
    pub fg: Rgb,
    pub caret: Rgb,
    pub selection_bg: Rgb,
    pub current_line_bg: Rgb,
    pub gutter_bg: Rgb,
    pub gutter_fg: Rgb,
    pub status_bg: Rgb,
    pub status_fg: Rgb,
    pub hint_key_fg: Rgb,
    pub hint_fg: Rgb,
    /// Indexed by `Tok as usize`. `tok[0] == fg` always.
    pub tok: [Rgb; TOK_COUNT],
}

impl Theme {
    /// High-contrast dark theme in the spirit of VS Code Dark+.
    pub fn dark() -> Theme {
        let fg = Rgb(0xd4, 0xd4, 0xd4);
        Theme {
            bg: Rgb(0x1b, 0x1b, 0x1f),
            fg,
            caret: Rgb(0xae, 0xaf, 0xad),
            selection_bg: Rgb(0x26, 0x4f, 0x78),
            current_line_bg: Rgb(0x24, 0x24, 0x2b),
            gutter_bg: Rgb(0x17, 0x17, 0x1a),
            gutter_fg: Rgb(0x6e, 0x6e, 0x78),
            status_bg: Rgb(0x2c, 0x2c, 0x35),
            status_fg: Rgb(0xcc, 0xcc, 0xcc),
            hint_key_fg: Rgb(0x1b, 0x1b, 0x1f),
            hint_fg: Rgb(0x9d, 0x9d, 0xa6),
            tok: [
                fg,                    // normal
                Rgb(0x56, 0x9c, 0xd6), // keyword
                Rgb(0x4e, 0xc9, 0xb0), // type
                Rgb(0xce, 0x91, 0x78), // string
                Rgb(0xb5, 0xce, 0xa8), // number
                Rgb(0x6a, 0x99, 0x55), // comment
                Rgb(0xc5, 0x86, 0xc0), // preproc
                Rgb(0xdc, 0xdc, 0xaa), // function
                Rgb(0xb4, 0xb4, 0xb4), // punct
                Rgb(0x4f, 0xc1, 0xff), // heading
            ],
        }
    }

    /// Light theme matching `dark()` in structure.
    pub fn light() -> Theme {
        let fg = Rgb(0x1f, 0x1f, 0x1f);
        Theme {
            bg: Rgb(0xff, 0xff, 0xff),
            fg,
            caret: Rgb(0x00, 0x00, 0x00),
            selection_bg: Rgb(0xad, 0xd6, 0xff),
            current_line_bg: Rgb(0xf0, 0xf0, 0xf5),
            gutter_bg: Rgb(0xf3, 0xf3, 0xf6),
            gutter_fg: Rgb(0x8a, 0x8a, 0x94),
            status_bg: Rgb(0xe6, 0xe6, 0xec),
            status_fg: Rgb(0x1f, 0x1f, 0x1f),
            hint_key_fg: Rgb(0xff, 0xff, 0xff),
            hint_fg: Rgb(0x55, 0x55, 0x5e),
            tok: [
                fg,                    // normal
                Rgb(0x00, 0x00, 0xff), // keyword
                Rgb(0x26, 0x7f, 0x99), // type
                Rgb(0xa3, 0x15, 0x15), // string
                Rgb(0x09, 0x86, 0x58), // number
                Rgb(0x00, 0x80, 0x00), // comment
                Rgb(0xaf, 0x00, 0xdb), // preproc
                Rgb(0x79, 0x5e, 0x26), // function
                Rgb(0x3b, 0x3b, 0x3b), // punct
                Rgb(0x80, 0x00, 0x00), // heading
            ],
        }
    }

    /// Every colour as (INI key, value). Used by `default_ini` and tests.
    fn entries(&self) -> Vec<(&'static str, Rgb)> {
        let mut v = vec![
            ("bg", self.bg),
            ("fg", self.fg),
            ("caret", self.caret),
            ("selection_bg", self.selection_bg),
            ("current_line_bg", self.current_line_bg),
            ("gutter_bg", self.gutter_bg),
            ("gutter_fg", self.gutter_fg),
            ("status_bg", self.status_bg),
            ("status_fg", self.status_fg),
            ("hint_key_fg", self.hint_key_fg),
            ("hint_fg", self.hint_fg),
        ];
        v.extend(TOKEN_NAMES.iter().copied().zip(self.tok.iter().copied()));
        v
    }

    /// Sets a colour by INI key. Returns false for an unknown key.
    fn set_color(&mut self, key: &str, c: Rgb) -> bool {
        match key {
            "bg" => self.bg = c,
            "fg" | "normal" => {
                self.fg = c;
                self.tok[0] = c;
            }
            "caret" => self.caret = c,
            "selection_bg" => self.selection_bg = c,
            "current_line_bg" => self.current_line_bg = c,
            "gutter_bg" => self.gutter_bg = c,
            "gutter_fg" => self.gutter_fg = c,
            "status_bg" => self.status_bg = c,
            "status_fg" => self.status_fg = c,
            "hint_key_fg" => self.hint_key_fg = c,
            "hint_fg" => self.hint_fg = c,
            other => match TOKEN_NAMES.iter().position(|&n| n == other) {
                Some(i) => self.tok[i] = c,
                None => return false,
            },
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

macro_rules! actions {
    ($($variant:ident = $name:literal),* $(,)?) => {
        /// Editor commands that keys can be bound to.
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum Action {
            $($variant),*
        }

        impl Action {
            /// Every action, in declaration order.
            pub const ALL: &'static [Action] = &[$(Action::$variant),*];

            /// snake_case name used in the INI file.
            pub fn name(self) -> &'static str {
                match self {
                    $(Action::$variant => $name),*
                }
            }
        }
    };
}

actions! {
    Save = "save",
    SaveAs = "save_as",
    Open = "open",
    New = "new",
    Exit = "exit",
    CutLine = "cut_line",
    Paste = "paste",
    Copy = "copy",
    Cut = "cut",
    Find = "find",
    FindNext = "find_next",
    FindPrev = "find_prev",
    Replace = "replace",
    GotoLine = "goto_line",
    ShowPosition = "show_position",
    Help = "help",
    Undo = "undo",
    Redo = "redo",
    SelectAll = "select_all",
    LineHome = "line_home",
    LineEnd = "line_end",
    PageUp = "page_up",
    PageDown = "page_down",
    DocStart = "doc_start",
    DocEnd = "doc_end",
    WordLeft = "word_left",
    WordRight = "word_right",
    DeleteWordLeft = "delete_word_left",
    DeleteWordRight = "delete_word_right",
    ToggleLineNumbers = "toggle_line_numbers",
    ToggleWrap = "toggle_wrap",
    ToggleHints = "toggle_hints",
    ZoomIn = "zoom_in",
    ZoomOut = "zoom_out",
    ZoomReset = "zoom_reset",
    Indent = "indent",
    Unindent = "unindent",
    DuplicateLine = "duplicate_line",
    MarkToggle = "mark_toggle",
    ToggleComment = "toggle_comment",
    MoveLineUp = "move_line_up",
    MoveLineDown = "move_line_down",
}

impl Action {
    /// Case-insensitive lookup by INI name.
    pub fn from_name(s: &str) -> Option<Action> {
        Action::ALL
            .iter()
            .copied()
            .find(|a| a.name().eq_ignore_ascii_case(s.trim()))
    }
}

// ---------------------------------------------------------------------------
// Chords
// ---------------------------------------------------------------------------

/// A key chord: Win32 virtual-key code plus modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Chord {
    pub vk: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    /// Parses either name form (`ctrl+shift+z`, `alt+up`, `f3`, `ctrl+_`, `ctrl++`)
    /// or nano form (`^S-Z`, `M-U`, `^_`). Case-insensitive. The last part is the key.
    pub fn parse(s: &str) -> Option<Chord> {
        let lower = s.trim().to_ascii_lowercase();
        let mut rest = lower.as_str();
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        // Nano-style prefixes: ^ = Ctrl, M- = Alt, S- = Shift.
        loop {
            if let Some(r) = rest.strip_prefix('^') {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("m-") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("s-") {
                shift = true;
                rest = r;
            } else {
                break;
            }
        }
        // Split off the key. A trailing '+' means the key is plus ("ctrl++").
        let (mods, key) = if let Some(head) = rest.strip_suffix('+') {
            (head.trim_end_matches('+'), "+")
        } else if let Some(i) = rest.rfind('+') {
            (&rest[..i], &rest[i + 1..])
        } else {
            ("", rest)
        };
        if !mods.is_empty() {
            for m in mods.split('+') {
                match m.trim() {
                    "ctrl" | "control" | "c" => ctrl = true,
                    "alt" | "meta" | "m" => alt = true,
                    "shift" | "s" => shift = true,
                    _ => return None,
                }
            }
        }
        let (vk, implied_shift) = key_code(key.trim())?;
        Some(Chord {
            vk,
            ctrl,
            alt,
            shift: shift || implied_shift,
        })
    }

    /// Nano-style label: `^` Ctrl, `M-` Alt, `S-` Shift, then the key (`^S-Z`, `M-U`, `F3`).
    /// Shift+minus is shown as `_`. The output parses back with `Chord::parse`.
    pub fn display(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push('^');
        }
        if self.alt {
            out.push_str("M-");
        }
        // Shift+minus is the underscore key, so the shift prefix is implied and dropped.
        if self.vk == VK_OEM_MINUS && self.shift {
            out.push('_');
        } else {
            if self.shift {
                out.push_str("S-");
            }
            out.push_str(&key_label(self.vk));
        }
        out
    }
}

/// Key name to (vk, shift implied by the name).
fn key_code(k: &str) -> Option<(u16, bool)> {
    if let [c] = k.as_bytes() {
        let c = *c;
        return match c {
            b'a'..=b'z' | b'0'..=b'9' => Some((u16::from(c.to_ascii_uppercase()), false)),
            b'+' | b'=' => Some((VK_OEM_PLUS, false)),
            b'-' => Some((VK_OEM_MINUS, false)),
            b'_' => Some((VK_OEM_MINUS, true)),
            b'/' => Some((VK_OEM_2, false)),
            b'\\' => Some((VK_OEM_5, false)),
            b',' => Some((VK_OEM_COMMA, false)),
            b'.' => Some((VK_OEM_PERIOD, false)),
            _ => None,
        };
    }
    let named = match k {
        "plus" => (VK_OEM_PLUS, false),
        "minus" => (VK_OEM_MINUS, false),
        "underscore" => (VK_OEM_MINUS, true),
        "comma" => (VK_OEM_COMMA, false),
        "period" => (VK_OEM_PERIOD, false),
        "slash" => (VK_OEM_2, false),
        "backslash" => (VK_OEM_5, false),
        "up" => (VK_UP, false),
        "down" => (VK_DOWN, false),
        "left" => (VK_LEFT, false),
        "right" => (VK_RIGHT, false),
        "home" => (VK_HOME, false),
        "end" => (VK_END, false),
        "pgup" | "pageup" => (VK_PRIOR, false),
        "pgdn" | "pagedown" => (VK_NEXT, false),
        "insert" | "ins" => (VK_INSERT, false),
        "delete" | "del" => (VK_DELETE, false),
        "backspace" | "bksp" => (VK_BACK, false),
        "tab" => (VK_TAB, false),
        "enter" | "return" => (VK_RETURN, false),
        "esc" | "escape" => (VK_ESCAPE, false),
        "space" => (VK_SPACE, false),
        _ => return f_key(k).map(|vk| (vk, false)),
    };
    Some(named)
}

/// "f1" ..= "f12".
fn f_key(k: &str) -> Option<u16> {
    let digits = k.strip_prefix('f')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u16 = digits.parse().ok()?;
    (1..=12).contains(&n).then(|| VK_F1 + n - 1)
}

/// Display name of a virtual key.
fn key_label(vk: u16) -> String {
    let named = match vk {
        0x41..=0x5A | 0x30..=0x39 => return char::from(vk as u8).to_string(),
        0x70..=0x7B => return format!("F{}", vk - VK_F1 + 1),
        VK_UP => "Up",
        VK_DOWN => "Down",
        VK_LEFT => "Left",
        VK_RIGHT => "Right",
        VK_HOME => "Home",
        VK_END => "End",
        VK_PRIOR => "PgUp",
        VK_NEXT => "PgDn",
        VK_INSERT => "Ins",
        VK_DELETE => "Del",
        VK_BACK => "Backspace",
        VK_TAB => "Tab",
        VK_RETURN => "Enter",
        VK_ESCAPE => "Esc",
        VK_SPACE => "Space",
        VK_OEM_PLUS => "+",
        VK_OEM_MINUS => "-",
        VK_OEM_2 => "/",
        VK_OEM_5 => "\\",
        VK_OEM_COMMA => ",",
        VK_OEM_PERIOD => ".",
        _ => return format!("VK{vk:02X}"),
    };
    named.to_string()
}

// ---------------------------------------------------------------------------
// Keymap
// ---------------------------------------------------------------------------

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;

const fn ch(vk: u16, mods: u8) -> Chord {
    Chord {
        vk,
        ctrl: mods & CTRL != 0,
        alt: mods & ALT != 0,
        shift: mods & SHIFT != 0,
    }
}

/// Default nano keymap. The first binding for an action is the one shown in the hint bar.
const NANO: &[(Chord, Action)] = &[
    (ch(b'O' as u16, CTRL), Action::Save),
    (ch(b'S' as u16, CTRL), Action::Save),
    (ch(b'X' as u16, CTRL), Action::Exit),
    (ch(b'R' as u16, CTRL), Action::Open),
    (ch(b'K' as u16, CTRL), Action::CutLine),
    (ch(b'U' as u16, CTRL), Action::Paste),
    (ch(b'W' as u16, CTRL), Action::Find),
    (ch(b'F' as u16, CTRL), Action::Find),
    (ch(VK_OEM_5, CTRL), Action::Replace),
    (ch(b'H' as u16, CTRL), Action::Replace),
    (ch(VK_OEM_MINUS, CTRL | SHIFT), Action::GotoLine),
    (ch(b'L' as u16, CTRL), Action::GotoLine),
    (ch(b'G' as u16, CTRL), Action::Help),
    (ch(VK_F1, 0), Action::Help),
    (ch(b'C' as u16, CTRL), Action::ShowPosition),
    (ch(b'A' as u16, CTRL), Action::LineHome),
    (ch(b'E' as u16, CTRL), Action::LineEnd),
    (ch(b'Y' as u16, CTRL), Action::PageUp),
    (ch(b'V' as u16, CTRL), Action::PageDown),
    (ch(b'U' as u16, ALT), Action::Undo),
    (ch(b'Z' as u16, CTRL), Action::Undo),
    (ch(b'E' as u16, ALT), Action::Redo),
    (ch(b'Z' as u16, CTRL | SHIFT), Action::Redo),
    (ch(b'W' as u16, ALT), Action::FindNext),
    (ch(VK_F1 + 2, 0), Action::FindNext),
    (ch(b'Q' as u16, ALT), Action::FindPrev),
    (ch(VK_F1 + 2, SHIFT), Action::FindPrev),
    (ch(b'6' as u16, CTRL), Action::MarkToggle),
    (ch(b'A' as u16, ALT), Action::MarkToggle),
    (ch(b'6' as u16, ALT), Action::Copy),
    (ch(VK_INSERT, CTRL), Action::Copy),
    (ch(VK_DELETE, SHIFT), Action::Cut),
    (ch(VK_INSERT, SHIFT), Action::Paste),
    (ch(b'N' as u16, CTRL), Action::New),
    (ch(b'S' as u16, CTRL | SHIFT), Action::SaveAs),
    (ch(VK_OEM_5, ALT), Action::DocStart),
    (ch(VK_HOME, CTRL), Action::DocStart),
    (ch(VK_OEM_2, ALT), Action::DocEnd),
    (ch(VK_END, CTRL), Action::DocEnd),
    (ch(VK_LEFT, CTRL), Action::WordLeft),
    (ch(VK_RIGHT, CTRL), Action::WordRight),
    (ch(VK_BACK, CTRL), Action::DeleteWordLeft),
    (ch(VK_DELETE, CTRL), Action::DeleteWordRight),
    (ch(b'N' as u16, ALT), Action::ToggleLineNumbers),
    (ch(b'L' as u16, ALT), Action::ToggleWrap),
    (ch(b'X' as u16, ALT), Action::ToggleHints),
    (ch(VK_OEM_PLUS, CTRL), Action::ZoomIn),
    (ch(VK_OEM_MINUS, CTRL), Action::ZoomOut),
    (ch(b'0' as u16, CTRL), Action::ZoomReset),
    (ch(VK_TAB, 0), Action::Indent),
    (ch(VK_TAB, SHIFT), Action::Unindent),
    (ch(b'3' as u16, ALT), Action::ToggleComment),
    (ch(VK_UP, ALT), Action::MoveLineUp),
    (ch(VK_DOWN, ALT), Action::MoveLineDown),
    (ch(b'D' as u16, CTRL), Action::DuplicateLine),
    (ch(b'A' as u16, CTRL | SHIFT), Action::SelectAll),
];

/// Key bindings. Linear scan, which is fine for a few dozen entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    binds: Vec<(Chord, Action)>,
}

impl Keymap {
    /// The default nano-style keymap.
    pub fn nano() -> Keymap {
        Keymap {
            binds: NANO.to_vec(),
        }
    }

    pub fn lookup(&self, c: Chord) -> Option<Action> {
        self.binds.iter().find(|(k, _)| *k == c).map(|(_, a)| *a)
    }

    /// Binds `c` to `a`, replacing any existing binding for `c`.
    pub fn bind(&mut self, c: Chord, a: Action) {
        match self.binds.iter_mut().find(|(k, _)| *k == c) {
            Some(entry) => entry.1 = a,
            None => self.binds.push((c, a)),
        }
    }

    pub fn unbind(&mut self, c: Chord) {
        self.binds.retain(|(k, _)| *k != c);
    }

    /// First chord bound to `a`, for the hint bar.
    pub fn chord_for(&self, a: Action) -> Option<Chord> {
        self.binds.iter().find(|(_, x)| *x == a).map(|(c, _)| *c)
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Anti-aliasing mode for the text font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSmoothing {
    ClearType,
    Grayscale,
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Typefaces in order of preference, each at most 31 characters (LOGFONT limit). The
    /// renderer uses the first one Windows matches, then Consolas.
    pub font_faces: Vec<String>,
    /// Points, clamped to 6..=72.
    pub font_size: i32,
    /// Weight of the text font, 100..=900 (400 = regular).
    pub font_weight: i32,
    /// Draw keywords and types with a bold face (weight 700). Cell widths do not change.
    pub bold_keywords: bool,
    /// Line pitch as a multiple of the font height, clamped to 1.0..=2.5.
    pub line_height: f32,
    pub font_smoothing: FontSmoothing,
    /// Columns per tab stop, clamped to 1..=16.
    pub tab_width: usize,
    pub insert_spaces: bool,
    pub line_numbers: bool,
    pub word_wrap: bool,
    pub show_hints: bool,
    pub highlight_current_line: bool,
    pub syntax: bool,
    pub auto_indent: bool,
    /// Caret width in pixels, clamped to 1..=10.
    pub caret_width: i32,
    pub theme: Theme,
    pub keymap: Keymap,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_faces: vec!["Paper Mono".to_string()],
            font_size: 11,
            font_weight: 400,
            bold_keywords: false,
            line_height: 1.2,
            font_smoothing: FontSmoothing::ClearType,
            tab_width: 4,
            insert_spaces: false,
            line_numbers: true,
            word_wrap: false,
            show_hints: true,
            highlight_current_line: true,
            syntax: true,
            auto_indent: true,
            caret_width: 2,
            theme: Theme::dark(),
            keymap: Keymap::nano(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Before any header, or an unknown header: keys are ignored.
    None,
    Editor,
    Theme,
    Keys,
}

impl Config {
    /// Parses INI text on top of the defaults. Unknown keys are ignored, invalid values keep
    /// their default, and it never fails or panics. Lines are applied in file order.
    pub fn parse(src: &str) -> Config {
        let mut cfg = Config::default();
        let mut section = Section::None;
        for raw in src.lines() {
            let t = raw.trim();
            if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
                continue;
            }
            let line = strip_comment(t).trim();
            if let Some(name) = line.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                section = match name.trim().to_ascii_lowercase().as_str() {
                    "editor" => Section::Editor,
                    "theme" => Section::Theme,
                    "keys" => Section::Keys,
                    _ => Section::None,
                };
                continue;
            }
            if section == Section::Keys {
                if let Some((chord, action)) = parse_binding(line) {
                    match action {
                        Some(a) => cfg.keymap.bind(chord, a),
                        None => cfg.keymap.unbind(chord),
                    }
                }
                continue;
            }
            let Some(eq) = line.find('=') else {
                continue;
            };
            let key = line[..eq].trim().to_ascii_lowercase();
            let value = line[eq + 1..].trim();
            match section {
                Section::Editor => apply_editor(&mut cfg, &key, value),
                Section::Theme => apply_theme(&mut cfg, &key, value),
                Section::Keys | Section::None => {}
            }
        }
        cfg
    }

    /// Default INI file contents, written on first run. Parses back to `Config::default()`.
    pub fn default_ini() -> String {
        let mut s = String::from(INI_HEADER);
        s.push_str(INI_EDITOR);

        s.push_str("\n[theme]\n");
        s.push_str(
            "; base = dark | light. Put it first: it resets every colour to that palette.\n",
        );
        s.push_str("base = dark\n");
        s.push_str("; Colour overrides, applied after base. Uncomment a line to change it.\n");
        s.push_str("; Each shows the dark default, then the light default.\n");
        let dark = Theme::dark().entries();
        let light = Theme::light().entries();
        for ((name, d), (_, l)) in dark.into_iter().zip(light) {
            s.push_str(&format!("; {name} = {}   ; light: {}\n", d.hex(), l.hex()));
        }

        s.push_str("\n[keys]\n");
        s.push_str("; chord = action   binds a key.   chord = none   removes a default key.\n");
        s.push_str("; Default bindings (nano notation, see the header):\n");
        for (chord, action) in &Keymap::nano().binds {
            s.push_str(&format!("; {:<12} = {}\n", chord.display(), action.name()));
        }
        s.push_str("; Action names:\n");
        let mut line = String::from(";   ");
        for a in Action::ALL {
            if line.len() + a.name().len() > 78 {
                s.push_str(line.trim_end_matches(", "));
                s.push('\n');
                line = String::from(";   ");
            }
            line.push_str(a.name());
            line.push_str(", ");
        }
        s.push_str(line.trim_end_matches(", "));
        s.push('\n');
        s
    }
}

/// Removes an inline comment. A ';' or '#' counts only when preceded by whitespace. A '#'
/// that starts a hex colour (3 or 6 hex digits) is kept. The caller has already removed
/// whole-line comments.
fn strip_comment(line: &str) -> &str {
    let b = line.as_bytes();
    for i in 1..b.len() {
        if !b[i - 1].is_ascii_whitespace() {
            continue;
        }
        match b[i] {
            b';' => return &line[..i],
            b'#' if !starts_hex_color(&b[i..]) => return &line[..i],
            _ => {}
        }
    }
    line
}

/// `s` starts with '#' followed by exactly 3 or 6 hex digits.
fn starts_hex_color(s: &[u8]) -> bool {
    let n = s[1..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
    n == 3 || n == 6
}

/// `chord = action` or `chord = none`. Splits at the rightmost '=' that gives a valid chord,
/// so a chord such as `ctrl+=` still works.
fn parse_binding(line: &str) -> Option<(Chord, Option<Action>)> {
    for (i, _) in line.rmatch_indices('=') {
        let Some(chord) = Chord::parse(&line[..i]) else {
            continue;
        };
        let value = line[i + 1..].trim();
        if value.eq_ignore_ascii_case("none") {
            return Some((chord, None));
        }
        if let Some(a) = Action::from_name(value) {
            return Some((chord, Some(a)));
        }
    }
    None
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Some(true),
        "false" | "no" | "0" | "off" => Some(false),
        _ => None,
    }
}

/// Longest font preference list read from `font =`. Extra entries are ignored.
const MAX_FONT_FACES: usize = 8;

/// A numeric weight 100..=900, or one of the CSS-style names.
fn parse_weight(v: &str) -> Option<i32> {
    let w = match v.to_ascii_lowercase().as_str() {
        "thin" => 100,
        "extralight" => 200,
        "light" => 300,
        "regular" | "normal" => 400,
        "medium" => 500,
        "semibold" => 600,
        "bold" => 700,
        "extrabold" => 800,
        "black" => 900,
        n => {
            let n: i32 = n.parse().ok()?;
            if !(100..=900).contains(&n) {
                return None;
            }
            n
        }
    };
    Some(w)
}

fn set_bool(v: &str, slot: &mut bool) {
    if let Some(b) = parse_bool(v) {
        *slot = b;
    }
}

fn apply_editor(cfg: &mut Config, key: &str, value: &str) {
    let int = value.parse::<i64>().ok();
    match key {
        "font" => {
            // Comma-separated preference list, e.g. "Paper Mono, Cascadia Mono, Consolas".
            let faces: Vec<String> = value
                .split(',')
                .map(|f| f.trim().chars().take(31).collect::<String>())
                .filter(|f| !f.is_empty())
                .take(MAX_FONT_FACES)
                .collect();
            if !faces.is_empty() {
                cfg.font_faces = faces;
            }
        }
        "font_size" => {
            if let Some(n) = int {
                cfg.font_size = n.clamp(6, 72) as i32;
            }
        }
        "font_weight" => {
            if let Some(w) = parse_weight(value) {
                cfg.font_weight = w;
            }
        }
        "bold_keywords" => set_bool(value, &mut cfg.bold_keywords),
        "line_height" => {
            if let Ok(f) = value.parse::<f32>() {
                if f.is_finite() {
                    cfg.line_height = f.clamp(1.0, 2.5);
                }
            }
        }
        "font_smoothing" => {
            let mode = match value.to_ascii_lowercase().as_str() {
                "cleartype" => Some(FontSmoothing::ClearType),
                "grayscale" => Some(FontSmoothing::Grayscale),
                "none" => Some(FontSmoothing::Off),
                _ => None,
            };
            if let Some(m) = mode {
                cfg.font_smoothing = m;
            }
        }
        "tab_width" => {
            if let Some(n) = int {
                cfg.tab_width = n.clamp(1, 16) as usize;
            }
        }
        "caret_width" => {
            if let Some(n) = int {
                cfg.caret_width = n.clamp(1, 10) as i32;
            }
        }
        "insert_spaces" => set_bool(value, &mut cfg.insert_spaces),
        "line_numbers" => set_bool(value, &mut cfg.line_numbers),
        "word_wrap" => set_bool(value, &mut cfg.word_wrap),
        "show_hints" => set_bool(value, &mut cfg.show_hints),
        "highlight_current_line" => set_bool(value, &mut cfg.highlight_current_line),
        "syntax" => set_bool(value, &mut cfg.syntax),
        "auto_indent" => set_bool(value, &mut cfg.auto_indent),
        _ => {}
    }
}

fn apply_theme(cfg: &mut Config, key: &str, value: &str) {
    if key == "base" {
        match value.to_ascii_lowercase().as_str() {
            "dark" => cfg.theme = Theme::dark(),
            "light" => cfg.theme = Theme::light(),
            _ => {}
        }
    } else if let Some(c) = Rgb::parse(value) {
        cfg.theme.set_color(key, c);
    }
}

const INI_HEADER: &str = "\
; note.ini - settings for the note text editor.
;
; Created with these defaults on first run, in %APPDATA%\\note\\note.ini.
; Lines starting with ';' or '#' are comments. An inline comment needs a space before the
; ';' or '#'. A '#' that starts a hex colour is not a comment. Unknown keys are ignored and
; invalid values keep their default. Lines are applied top to bottom.
;
; [editor]  font, font_size, tab_width, caret_width and on/off options.
;           On/off values: true/false, yes/no, on/off, 1/0.
; [theme]   base = dark | light resets all colours to that palette. Put it BEFORE the colour
;           overrides. Colours: #rrggbb, rrggbb or #rgb. 'normal' is the same as 'fg'.
; [keys]    chord = action binds a key. chord = none removes a key.
;           Chords use nano notation (^ = Ctrl, M- = Alt, S- = Shift, so ^S-Z is Ctrl+Shift+Z)
;           or names: ctrl+shift+z, alt+up, f3, shift+delete, ctrl+plus, ctrl+_ (Ctrl+Shift+minus).
;           The last part is the key and the parts before it are modifiers.
";

const INI_EDITOR: &str = "
[editor]
; Typeface names, comma-separated, in order of preference. Each is at most 31 characters.
; The first one installed is used, then Consolas. Paper Mono is embedded in the exe.
; Default: Paper Mono
font = Paper Mono
; Font size in points, 6..72. Default: 11
font_size = 11
; Weight 100..900, or thin, extralight, light, regular, normal, medium, semibold, bold,
; extrabold, black. Default: 400
font_weight = 400
; Draw keywords and types in bold (weight 700). Cell widths stay the same. Default: false
bold_keywords = false
; Line pitch as a multiple of the font height, 1.0..2.5. Default: 1.2
line_height = 1.2
; Text anti-aliasing: cleartype, grayscale or none. Default: cleartype
font_smoothing = cleartype
; Columns per tab stop, 1..16. Default: 4
tab_width = 4
; Tab inserts spaces instead of a tab character. Default: false
insert_spaces = false
; Show the line-number gutter. Default: true
line_numbers = true
; Wrap long lines instead of scrolling sideways. Default: false
word_wrap = false
; Show the two-row nano shortcut bar. Default: true
show_hints = true
; Subtle background behind the caret's line. Default: true
highlight_current_line = true
; Syntax highlighting. Default: true
syntax = true
; New lines copy the indentation of the line above. Default: true
auto_indent = true
; Caret width in pixels, 1..10. Default: 2
caret_width = 2
";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn c(s: &str) -> Chord {
        Chord::parse(s).unwrap_or_else(|| panic!("chord should parse: {s}"))
    }

    /// Small deterministic PRNG for the garbage-input tests.
    struct XorShift(u64);
    impl XorShift {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    // ---- colours ----

    #[test]
    fn rgb_parse_accepts_all_forms() {
        assert_eq!(Rgb::parse("#1e1e1e"), Some(Rgb(0x1e, 0x1e, 0x1e)));
        assert_eq!(Rgb::parse("1E1E1E"), Some(Rgb(0x1e, 0x1e, 0x1e)));
        assert_eq!(Rgb::parse("#569cd6"), Some(Rgb(0x56, 0x9c, 0xd6)));
        assert_eq!(Rgb::parse("#abc"), Some(Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(Rgb::parse("fFf"), Some(Rgb(0xff, 0xff, 0xff)));
        assert_eq!(Rgb::parse("  #FfF  "), Some(Rgb(0xff, 0xff, 0xff)));
        assert_eq!(Rgb::parse("\t#000000\n"), Some(Rgb(0, 0, 0)));
    }

    #[test]
    fn rgb_parse_rejects_garbage() {
        for bad in [
            "", "#", "#12", "#12345", "#1234567", "##fff", "#gggggg", "#ééé", "red",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(Rgb(0x11, 0x22, 0x33).colorref(), 0x0033_2211);
        assert_eq!(Rgb(0xff, 0, 0).colorref(), 0x0000_00ff);
    }

    #[test]
    fn theme_token_zero_is_fg() {
        assert_eq!(Theme::dark().tok[0], Theme::dark().fg);
        assert_eq!(Theme::light().tok[0], Theme::light().fg);
        assert_eq!(TOK_COUNT, 10);
    }

    // ---- chords ----

    #[test]
    fn chord_parse_name_forms() {
        assert_eq!(
            c("ctrl+k"),
            Chord {
                vk: 0x4B,
                ctrl: true,
                alt: false,
                shift: false
            }
        );
        assert_eq!(
            c("ctrl+shift+z"),
            Chord {
                vk: 0x5A,
                ctrl: true,
                alt: false,
                shift: true
            }
        );
        assert_eq!(
            c("alt+u"),
            Chord {
                vk: 0x55,
                ctrl: false,
                alt: true,
                shift: false
            }
        );
        assert_eq!(c("F3").vk, VK_F1 + 2);
        assert_eq!(c("f12").vk, VK_F1 + 11);
        assert_eq!(c("ctrl+6").vk, 0x36);
        assert_eq!(c("ctrl+0").vk, 0x30);
        assert_eq!(c("CTRL+SHIFT+A"), c("ctrl+shift+a"));
        assert_eq!(c("control+meta+x"), c("ctrl+alt+x"));
        assert_eq!(c("c+m+s+a"), c("ctrl+alt+shift+a"));
    }

    #[test]
    fn chord_parse_punctuation_and_names() {
        assert_eq!(c("ctrl+plus").vk, VK_OEM_PLUS);
        assert_eq!(c("ctrl++"), c("ctrl+plus"));
        assert_eq!(c("ctrl+="), c("ctrl+plus"));
        assert_eq!(c("ctrl+minus").vk, VK_OEM_MINUS);
        assert!(!c("ctrl+minus").shift);
        assert_eq!(c("ctrl+_"), c("ctrl+shift+minus"));
        assert!(c("ctrl+_").shift);
        assert_eq!(c("ctrl+/").vk, VK_OEM_2);
        assert_eq!(c("ctrl+\\").vk, VK_OEM_5);
        assert_eq!(c("alt+,").vk, VK_OEM_COMMA);
        assert_eq!(c("alt+period").vk, VK_OEM_PERIOD);
        assert_eq!(c("pgup"), c("pageup"));
        assert_eq!(c("pgdn"), c("pagedown"));
        assert_eq!(c("ins"), c("insert"));
        assert_eq!(c("del"), c("delete"));
        assert_eq!(c("esc").vk, VK_ESCAPE);
        assert_eq!(c("backspace").vk, VK_BACK);
        assert_eq!(c("space").vk, VK_SPACE);
        assert_eq!(c("enter").vk, VK_RETURN);
    }

    #[test]
    fn chord_parse_nano_forms() {
        assert_eq!(c("^S-Z"), c("ctrl+shift+z"));
        assert_eq!(c("M-U"), c("alt+u"));
        assert_eq!(c("^_"), c("ctrl+_"));
        assert_eq!(c("^\\"), c("ctrl+\\"));
        assert_eq!(c("^+"), c("ctrl+plus"));
        assert_eq!(c("S-F3"), c("shift+f3"));
    }

    #[test]
    fn chord_parse_rejects_invalid() {
        for bad in [
            "",
            "   ",
            "ctrl+foo",
            "foo+k",
            "ctrl++shift+x",
            "ctrl+ctrl",
            "^",
            "M-",
            "f13",
            "f0",
            "f+3",
            "ctrl+k+j",
            "ctrl+shift",
        ] {
            assert_eq!(Chord::parse(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn chord_display_examples() {
        assert_eq!(c("ctrl+shift+z").display(), "^S-Z");
        assert_eq!(c("alt+u").display(), "M-U");
        assert_eq!(c("f3").display(), "F3");
        assert_eq!(c("ctrl+shift+minus").display(), "^_");
        assert_eq!(c("ctrl+\\").display(), "^\\");
        assert_eq!(c("ctrl+/").display(), "^/");
        assert_eq!(c("shift+delete").display(), "S-Del");
        assert_eq!(c("alt+up").display(), "M-Up");
        assert_eq!(c("ctrl+plus").display(), "^+");
        assert_eq!(c("ctrl+6").display(), "^6");
        assert_eq!(c("ctrl+end").display(), "^End");
    }

    #[test]
    fn chord_display_round_trips_every_key_and_modifier_combo() {
        let mut keys: Vec<u16> = (0x41..=0x5A).chain(0x30..=0x39).collect();
        keys.extend(VK_F1..VK_F1 + 12);
        keys.extend([
            VK_BACK,
            VK_TAB,
            VK_RETURN,
            VK_ESCAPE,
            VK_SPACE,
            VK_PRIOR,
            VK_NEXT,
            VK_END,
            VK_HOME,
            VK_LEFT,
            VK_UP,
            VK_RIGHT,
            VK_DOWN,
            VK_INSERT,
            VK_DELETE,
            VK_OEM_PLUS,
            VK_OEM_MINUS,
            VK_OEM_2,
            VK_OEM_5,
            VK_OEM_COMMA,
            VK_OEM_PERIOD,
        ]);
        for vk in keys {
            for mods in 0..8u8 {
                let chord = ch(vk, mods);
                let shown = chord.display();
                assert_eq!(
                    Chord::parse(&shown),
                    Some(chord),
                    "{shown:?} did not round trip"
                );
            }
        }
    }

    #[test]
    fn every_default_binding_round_trips_and_is_unique() {
        let keymap = Keymap::nano();
        let mut seen = HashSet::new();
        for (chord, _) in &keymap.binds {
            assert_eq!(Chord::parse(&chord.display()), Some(*chord));
            assert!(
                seen.insert(*chord),
                "duplicate default chord {}",
                chord.display()
            );
        }
        assert_eq!(seen.len(), NANO.len());
    }

    // ---- actions ----

    #[test]
    fn action_names_round_trip() {
        assert_eq!(Action::ALL.len(), 42);
        let mut names = HashSet::new();
        for &a in Action::ALL {
            let n = a.name();
            assert_eq!(Action::from_name(n), Some(a), "{n} should round trip");
            assert!(names.insert(n), "duplicate name {n}");
            assert!(
                n.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{n} is not snake_case"
            );
        }
        assert_eq!(Action::from_name("SAVE_AS"), Some(Action::SaveAs));
        assert_eq!(Action::from_name("  cut_line "), Some(Action::CutLine));
        assert_eq!(Action::from_name("nope"), None);
        assert_eq!(Action::from_name(""), None);
    }

    // ---- keymap ----

    #[test]
    fn nano_keymap_implements_default_table() {
        let k = Keymap::nano();
        let expect = [
            ("ctrl+o", Action::Save),
            ("ctrl+s", Action::Save),
            ("ctrl+x", Action::Exit),
            ("ctrl+r", Action::Open),
            ("ctrl+k", Action::CutLine),
            ("ctrl+u", Action::Paste),
            ("ctrl+w", Action::Find),
            ("ctrl+f", Action::Find),
            ("ctrl+\\", Action::Replace),
            ("ctrl+h", Action::Replace),
            ("ctrl+_", Action::GotoLine),
            ("ctrl+l", Action::GotoLine),
            ("ctrl+g", Action::Help),
            ("f1", Action::Help),
            ("ctrl+c", Action::ShowPosition),
            ("ctrl+a", Action::LineHome),
            ("ctrl+e", Action::LineEnd),
            ("ctrl+y", Action::PageUp),
            ("ctrl+v", Action::PageDown),
            ("alt+u", Action::Undo),
            ("ctrl+z", Action::Undo),
            ("alt+e", Action::Redo),
            ("ctrl+shift+z", Action::Redo),
            ("alt+w", Action::FindNext),
            ("f3", Action::FindNext),
            ("alt+q", Action::FindPrev),
            ("shift+f3", Action::FindPrev),
            ("ctrl+6", Action::MarkToggle),
            ("alt+a", Action::MarkToggle),
            ("alt+6", Action::Copy),
            ("ctrl+insert", Action::Copy),
            ("shift+delete", Action::Cut),
            ("shift+insert", Action::Paste),
            ("ctrl+n", Action::New),
            ("ctrl+shift+s", Action::SaveAs),
            ("alt+\\", Action::DocStart),
            ("ctrl+home", Action::DocStart),
            ("alt+/", Action::DocEnd),
            ("ctrl+end", Action::DocEnd),
            ("ctrl+left", Action::WordLeft),
            ("ctrl+right", Action::WordRight),
            ("ctrl+backspace", Action::DeleteWordLeft),
            ("ctrl+delete", Action::DeleteWordRight),
            ("alt+n", Action::ToggleLineNumbers),
            ("alt+l", Action::ToggleWrap),
            ("alt+x", Action::ToggleHints),
            ("ctrl+plus", Action::ZoomIn),
            ("ctrl+minus", Action::ZoomOut),
            ("ctrl+0", Action::ZoomReset),
            ("tab", Action::Indent),
            ("shift+tab", Action::Unindent),
            ("alt+3", Action::ToggleComment),
            ("alt+up", Action::MoveLineUp),
            ("alt+down", Action::MoveLineDown),
            ("ctrl+d", Action::DuplicateLine),
            ("ctrl+shift+a", Action::SelectAll),
        ];
        for (chord, action) in expect {
            assert_eq!(k.lookup(c(chord)), Some(action), "{chord}");
        }
        assert_eq!(k.binds.len(), expect.len());
        for &a in Action::ALL {
            assert!(k.chord_for(a).is_some(), "{} has no default key", a.name());
        }
        assert_eq!(k.chord_for(Action::Save), Some(c("ctrl+o")));
    }

    #[test]
    fn keymap_rebind_and_unbind() {
        let mut k = Keymap::nano();
        let count = k.binds.len();

        // Rebinding an existing chord replaces it without adding an entry.
        k.bind(c("ctrl+k"), Action::Save);
        assert_eq!(k.lookup(c("ctrl+k")), Some(Action::Save));
        assert_eq!(k.binds.len(), count);
        assert_eq!(k.chord_for(Action::CutLine), None);

        // A new chord adds an entry.
        k.bind(c("ctrl+j"), Action::CutLine);
        assert_eq!(k.lookup(c("ctrl+j")), Some(Action::CutLine));
        assert_eq!(k.binds.len(), count + 1);

        // Unbinding removes it. Unbinding twice is harmless.
        k.unbind(c("ctrl+j"));
        k.unbind(c("ctrl+j"));
        assert_eq!(k.lookup(c("ctrl+j")), None);
        assert_eq!(k.binds.len(), count);

        // Save still has its first (ctrl+o) binding for the hint bar.
        assert_eq!(k.chord_for(Action::Save), Some(c("ctrl+o")));
    }

    #[test]
    fn keys_section_rebinds_and_unbinds() {
        let cfg = Config::parse(
            "[keys]\n\
             ctrl+k = none\n\
             ctrl+j = cut_line\n\
             ctrl+shift+q = no_such_action\n\
             ctrl+= = zoom_in\n\
             ctrl+o = none ; drop save\n\
             bogus chord = save\n",
        );
        assert_eq!(cfg.keymap.lookup(c("ctrl+k")), None);
        assert_eq!(cfg.keymap.lookup(c("ctrl+j")), Some(Action::CutLine));
        assert_eq!(cfg.keymap.lookup(c("ctrl+shift+q")), None);
        assert_eq!(cfg.keymap.lookup(c("ctrl+o")), None);
        assert_eq!(cfg.keymap.chord_for(Action::Save), Some(c("ctrl+s")));
        assert_eq!(cfg.keymap.lookup(c("ctrl+plus")), Some(Action::ZoomIn));
    }

    // ---- parsing ----

    #[test]
    fn inline_comments_vs_colours() {
        let cfg = Config::parse(
            "[theme]\n\
             base = light\n\
             bg = #123456 ; inline comment\n\
             keyword = #abc # hash comment\n\
             comment=#6a9955\n\
             string = #ce9178;not a comment, so invalid\n\
             ; bg = #000000\n\
             # bg = #000000\n\
             [editor]\n\
             font = Consolas # mono face\n\
             font_size = 14 ; big\n\
             tab_width=2#tight\n",
        );
        let light = Theme::light();
        assert_eq!(cfg.theme.bg, Rgb(0x12, 0x34, 0x56));
        assert_eq!(cfg.theme.tok[1], Rgb(0xaa, 0xbb, 0xcc));
        assert_eq!(cfg.theme.tok[5], Rgb(0x6a, 0x99, 0x55));
        assert_eq!(cfg.theme.tok[3], light.tok[3]);
        assert_eq!(cfg.font_faces, vec!["Consolas"]);
        assert_eq!(cfg.font_size, 14);
        assert_eq!(
            cfg.tab_width, 4,
            "'2#tight' is not a number, so the default stays"
        );
    }

    #[test]
    fn hash_before_hex_is_colour_not_comment() {
        assert_eq!(strip_comment("bg = #1e1e1e"), "bg = #1e1e1e");
        assert_eq!(strip_comment("bg = #1e1e1e ; x"), "bg = #1e1e1e ");
        assert_eq!(strip_comment("bg = #fff # x"), "bg = #fff ");
        assert_eq!(strip_comment("bg = #1e1e1e1 x"), "bg = ");
        assert_eq!(strip_comment("font = a;b"), "font = a;b");
    }

    #[test]
    fn base_applies_in_file_order() {
        let first = Config::parse("[theme]\nbase = light\nbg = #000000\n");
        assert_eq!(first.theme.bg, Rgb(0, 0, 0));
        assert_eq!(first.theme.fg, Theme::light().fg);

        let later = Config::parse("[theme]\nbg = #000000\nbase = light\n");
        assert_eq!(
            later.theme.bg,
            Theme::light().bg,
            "base after an override resets it"
        );

        let dark = Config::parse("[theme]\nbase = DARK\n");
        assert_eq!(dark.theme, Theme::dark());
    }

    #[test]
    fn normal_token_sets_fg_too() {
        let cfg = Config::parse("[theme]\nnormal = #010203\n");
        assert_eq!(cfg.theme.fg, Rgb(1, 2, 3));
        assert_eq!(cfg.theme.tok[0], Rgb(1, 2, 3));
        let cfg = Config::parse("[theme]\nfg = #040506\n");
        assert_eq!(cfg.theme.tok[0], Rgb(4, 5, 6));
    }

    #[test]
    fn booleans_accept_all_spellings() {
        // word_wrap defaults to false, so these prove the value was actually read.
        for v in ["true", "yes", "1", "on", "TRUE"] {
            let cfg = Config::parse(&format!("[editor]\nword_wrap = {v}\n"));
            assert!(cfg.word_wrap, "{v}");
        }
        // line_numbers defaults to true, so these prove the same for false.
        for v in ["false", "no", "0", "off", "Off"] {
            let cfg = Config::parse(&format!("[editor]\nline_numbers = {v}\n"));
            assert!(!cfg.line_numbers, "{v}");
        }
        let cfg = Config::parse("[editor]\nline_numbers = maybe\nword_wrap = maybe\n");
        assert!(cfg.line_numbers);
        assert!(!cfg.word_wrap);
    }

    #[test]
    fn editor_keys_are_read() {
        let cfg = Config::parse(
            "[editor]\n\
             font = Consolas\n\
             insert_spaces = yes\n\
             word_wrap = on\n\
             show_hints = 0\n\
             highlight_current_line = no\n\
             syntax = off\n\
             auto_indent = false\n\
             caret_width = 3\n",
        );
        assert_eq!(cfg.font_faces, vec!["Consolas"]);
        assert!(cfg.insert_spaces);
        assert!(cfg.word_wrap);
        assert!(!cfg.show_hints);
        assert!(!cfg.highlight_current_line);
        assert!(!cfg.syntax);
        assert!(!cfg.auto_indent);
        assert_eq!(cfg.caret_width, 3);
    }

    #[test]
    fn keys_outside_their_section_are_ignored() {
        let cfg =
            Config::parse("font_size = 20\n[unknown]\ntab_width = 8\n[keys]\nfont_size = 30\n");
        assert_eq!(cfg.font_size, 11);
        assert_eq!(cfg.tab_width, 4);
    }

    #[test]
    fn numeric_values_are_clamped() {
        assert_eq!(Config::parse("[editor]\nfont_size = 1\n").font_size, 6);
        assert_eq!(Config::parse("[editor]\nfont_size = -3\n").font_size, 6);
        assert_eq!(Config::parse("[editor]\nfont_size = 500\n").font_size, 72);
        assert_eq!(Config::parse("[editor]\nfont_size = 14\n").font_size, 14);
        assert_eq!(Config::parse("[editor]\ntab_width = 0\n").tab_width, 1);
        assert_eq!(Config::parse("[editor]\ntab_width = -9\n").tab_width, 1);
        assert_eq!(Config::parse("[editor]\ntab_width = 99\n").tab_width, 16);
        assert_eq!(Config::parse("[editor]\ntab_width = 8\n").tab_width, 8);
        assert_eq!(Config::parse("[editor]\ncaret_width = 0\n").caret_width, 1);
        assert_eq!(
            Config::parse("[editor]\ncaret_width = 50\n").caret_width,
            10
        );
        assert_eq!(
            Config::parse("[editor]\nfont_size = 99999999999999999999999999\n").font_size,
            11,
            "overflowing numbers are ignored"
        );
    }

    #[test]
    fn font_name_is_truncated_to_logfont_limit() {
        let long = "x".repeat(50);
        let cfg = Config::parse(&format!("[editor]\nfont = {long}\n"));
        assert_eq!(cfg.font_faces[0].chars().count(), 31);
        assert_eq!(
            Config::parse("[editor]\nfont =\n").font_faces,
            vec!["Paper Mono"],
            "an empty value keeps the default"
        );
    }

    #[test]
    fn font_list_is_split_trimmed_and_capped() {
        let cfg = Config::parse("[editor]\nfont = Paper Mono, Cascadia Mono ,, Consolas,\n");
        assert_eq!(
            cfg.font_faces,
            vec!["Paper Mono", "Cascadia Mono", "Consolas"]
        );
        let many = (0..20)
            .map(|i| format!("F{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let cfg = Config::parse(&format!("[editor]\nfont = {many}\n"));
        assert_eq!(cfg.font_faces.len(), MAX_FONT_FACES);
        assert_eq!(Config::parse("[editor]\nfont = , ,\n").font_faces, vec!["Paper Mono"]);
    }

    #[test]
    fn font_weight_accepts_numbers_and_names() {
        assert_eq!(Config::default().font_weight, 400);
        assert_eq!(Config::parse("[editor]\nfont_weight = 300\n").font_weight, 300);
        assert_eq!(Config::parse("[editor]\nfont_weight = 900\n").font_weight, 900);
        for (name, w) in [
            ("thin", 100),
            ("ExtraLight", 200),
            ("light", 300),
            ("regular", 400),
            ("normal", 400),
            ("medium", 500),
            ("semibold", 600),
            ("bold", 700),
            ("extrabold", 800),
            ("black", 900),
        ] {
            let cfg = Config::parse(&format!("[editor]\nfont_weight = {name}\n"));
            assert_eq!(cfg.font_weight, w, "{name}");
        }
        for bad in ["0", "99", "901", "1000", "heavy", "", "-400"] {
            let cfg = Config::parse(&format!("[editor]\nfont_weight = {bad}\n"));
            assert_eq!(cfg.font_weight, 400, "{bad:?} keeps the default");
        }
    }

    #[test]
    fn line_height_is_clamped_and_rejects_non_numbers() {
        assert_eq!(Config::default().line_height, 1.2);
        assert_eq!(Config::parse("[editor]\nline_height = 1.5\n").line_height, 1.5);
        assert_eq!(Config::parse("[editor]\nline_height = 0.2\n").line_height, 1.0);
        assert_eq!(Config::parse("[editor]\nline_height = 9\n").line_height, 2.5);
        for bad in ["nan", "inf", "-inf", "tall", "1,5"] {
            let cfg = Config::parse(&format!("[editor]\nline_height = {bad}\n"));
            assert_eq!(cfg.line_height, 1.2, "{bad}");
        }
    }

    #[test]
    fn bold_keywords_and_smoothing_are_read() {
        assert!(!Config::default().bold_keywords);
        assert_eq!(Config::default().font_smoothing, FontSmoothing::ClearType);
        let cfg = Config::parse("[editor]\nbold_keywords = yes\nfont_smoothing = grayscale\n");
        assert!(cfg.bold_keywords);
        assert_eq!(cfg.font_smoothing, FontSmoothing::Grayscale);
        let cfg = Config::parse("[editor]\nfont_smoothing = NONE\n");
        assert_eq!(cfg.font_smoothing, FontSmoothing::Off);
        let cfg = Config::parse("[editor]\nfont_smoothing = fuzzy\n");
        assert_eq!(cfg.font_smoothing, FontSmoothing::ClearType);
    }

    #[test]
    fn empty_input_gives_defaults() {
        assert_eq!(Config::parse(""), Config::default());
        assert_eq!(
            Config::parse("; only comments\n# nothing\n"),
            Config::default()
        );
    }

    #[test]
    fn default_ini_round_trips() {
        let ini = Config::default_ini();
        assert_eq!(Config::parse(&ini), Config::default());
    }

    #[test]
    fn default_ini_documents_every_key_colour_and_binding() {
        let ini = Config::default_ini();
        for key in [
            "font",
            "font_size",
            "font_weight",
            "bold_keywords",
            "line_height",
            "font_smoothing",
            "tab_width",
            "insert_spaces",
            "line_numbers",
            "word_wrap",
            "show_hints",
            "highlight_current_line",
            "syntax",
            "auto_indent",
            "caret_width",
            "base",
        ] {
            assert!(ini.contains(key), "missing {key}");
        }
        for (name, rgb) in Theme::dark().entries() {
            let line = format!("; {name} = {}", rgb.hex());
            assert!(ini.contains(&line), "missing colour line {line:?}");
        }
        for (chord, action) in &Keymap::nano().binds {
            let line = format!("; {:<12} = {}", chord.display(), action.name());
            assert!(ini.contains(&line), "missing binding line {line:?}");
        }
        for &a in Action::ALL {
            assert!(ini.contains(a.name()), "missing action name {}", a.name());
        }
        // Every non-comment line must be a section header or a `key = value` pair.
        for line in ini.lines().map(str::trim).filter(|l| !l.is_empty()) {
            assert!(
                line.starts_with(';') || line.starts_with('[') || line.contains('='),
                "unexpected line {line:?}"
            );
        }
    }

    #[test]
    fn garbage_input_never_panics_and_keeps_limits() {
        let fixed = [
            "[",
            "]",
            "=",
            "==",
            "[[[]]]",
            "[editor]\n=\n==x",
            "[keys]\n=\n==\nctrl+ = \n+ = none",
            "[theme]\nbg = ##\nbase",
            "font_size = 99999999999999999999999999",
            "[editor]\nfont_size = -1\ntab_width = 0",
            "\u{0}\u{1}\u{ffff}",
            "é = ü",
            "[theme]\nkeyword = #é\nnormal = \u{1F600}",
            "[keys]\nM-=\n^\n^^^ = save",
            "[keys]\n^S- = \nS-^ = none",
            "[theme]\nbase = \nbase = light",
            "\r\n\r\n\t\t",
            "[editor]\nfont = \u{1F600}\u{1F600}",
            "#",
            ";",
            "  ;  ",
            "[keys]\nctrl+k = none=none",
        ];
        for s in fixed {
            assert_limits(&Config::parse(s));
        }

        let alphabet: Vec<char> = "[]=;#^+-_ \t\nabcfsmMkctrl0123456789éé\u{1F600}editorthemekeys:"
            .chars()
            .collect();
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        for _ in 0..3000 {
            let len = (rng.next_u64() % 200) as usize;
            let s: String = (0..len)
                .map(|_| alphabet[(rng.next_u64() % alphabet.len() as u64) as usize])
                .collect();
            assert_limits(&Config::parse(&s));
            let _ = Chord::parse(&s);
            let _ = Rgb::parse(&s);
            let _ = Action::from_name(&s);
        }
    }

    fn assert_limits(cfg: &Config) {
        assert!((6..=72).contains(&cfg.font_size));
        assert!((1..=16).contains(&cfg.tab_width));
        assert!((1..=10).contains(&cfg.caret_width));
        assert!(!cfg.font_faces.is_empty() && cfg.font_faces.len() <= MAX_FONT_FACES);
        assert!(cfg.font_faces.iter().all(|f| f.chars().count() <= 31));
        assert!((100..=900).contains(&cfg.font_weight));
        assert!((1.0..=2.5).contains(&cfg.line_height));
        assert_eq!(cfg.theme.tok[0], cfg.theme.fg);
    }

    #[test]
    fn colour_override_changes_only_its_slot() {
        let cfg = Config::parse("[theme]\nselection_bg = #ff0000\nhint_key_fg = #00ff00\n");
        let dark = Theme::dark();
        assert_eq!(cfg.theme.selection_bg, Rgb(0xff, 0, 0));
        assert_eq!(cfg.theme.hint_key_fg, Rgb(0, 0xff, 0));
        assert_eq!(cfg.theme.bg, dark.bg);
        assert_eq!(cfg.theme.tok, dark.tok);
    }
}
