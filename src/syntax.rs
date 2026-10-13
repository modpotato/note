//! Incremental per-line syntax highlighter. See DESIGN.md (syntax.rs).
//!
//! # LineState encoding
//!
//! A `LineState` is a `u8`. The low 3 bits are a *kind*, the high 5 bits an *aux* value.
//! Kinds are interpreted per language. `0` always means "no construct open at end of line".
//!
//! | kind | meaning                                                    | aux                              |
//! |------|------------------------------------------------------------|----------------------------------|
//! | 0    | normal                                                     | -                                |
//! | 1    | block comment (`/* */`, `<# #>`, `--[[ ]]`, JSONC `/* */`) | nesting depth (Rust nests)       |
//! | 2    | Python `"""` string                                        | -                                |
//! | 3    | Python `'''` string                                        | -                                |
//! | 4    | backtick string (JS/TS template, Go raw string)            | -                                |
//! | 5    | Rust raw string `r#"..."#` / C# verbatim `@"..."`          | Rust: hash count; C#: 0          |
//! | 6    | Markdown fenced code block / HTML: inside a tag            | Markdown: 1 for a `~~~` fence    |
//! | 7    | HTML comment `<!-- -->`                                    | -                                |

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Tok {
    Normal = 0,
    Keyword = 1,
    Type = 2,
    String = 3,
    Number = 4,
    Comment = 5,
    Preproc = 6,
    Function = 7,
    Punct = 8,
    Heading = 9,
}
pub const TOK_COUNT: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Plain,
    Rust,
    C,
    Cpp,
    CSharp,
    Python,
    JavaScript,
    TypeScript,
    Json,
    Toml,
    Ini,
    Shell,
    PowerShell,
    Batch,
    Markdown,
    Html,
    Css,
    Go,
    Lua,
    Sql,
    Zig,
}

const EXT_MAP: &[(&str, Lang)] = &[
    ("rs", Lang::Rust),
    ("c", Lang::C),
    ("h", Lang::C),
    ("cpp", Lang::Cpp),
    ("cc", Lang::Cpp),
    ("cxx", Lang::Cpp),
    ("hpp", Lang::Cpp),
    ("hh", Lang::Cpp),
    ("cs", Lang::CSharp),
    ("py", Lang::Python),
    ("pyw", Lang::Python),
    ("js", Lang::JavaScript),
    ("mjs", Lang::JavaScript),
    ("cjs", Lang::JavaScript),
    ("jsx", Lang::JavaScript),
    ("ts", Lang::TypeScript),
    ("tsx", Lang::TypeScript),
    ("json", Lang::Json),
    ("jsonc", Lang::Json),
    ("toml", Lang::Toml),
    ("ini", Lang::Ini),
    ("cfg", Lang::Ini),
    ("conf", Lang::Ini),
    ("reg", Lang::Ini),
    ("sh", Lang::Shell),
    ("bash", Lang::Shell),
    ("zsh", Lang::Shell),
    ("ps1", Lang::PowerShell),
    ("psm1", Lang::PowerShell),
    ("bat", Lang::Batch),
    ("cmd", Lang::Batch),
    ("md", Lang::Markdown),
    ("markdown", Lang::Markdown),
    ("html", Lang::Html),
    ("htm", Lang::Html),
    ("xml", Lang::Html),
    ("svg", Lang::Html),
    ("css", Lang::Css),
    ("go", Lang::Go),
    ("lua", Lang::Lua),
    ("sql", Lang::Sql),
    ("zig", Lang::Zig),
];

impl Lang {
    /// Language from a file path. Extension is matched case-insensitively. `Makefile` and
    /// `Dockerfile` (any directory) map to Shell. Anything else is Plain.
    pub fn from_path(path: &str) -> Lang {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if name.eq_ignore_ascii_case("Makefile") || name.eq_ignore_ascii_case("Dockerfile") {
            return Lang::Shell;
        }
        let Some(dot) = name.rfind('.') else {
            return Lang::Plain;
        };
        let ext = &name[dot + 1..];
        EXT_MAP
            .iter()
            .find(|(e, _)| e.eq_ignore_ascii_case(ext))
            .map_or(Lang::Plain, |&(_, l)| l)
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Plain => "Plain text",
            Lang::Rust => "Rust",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
            Lang::Python => "Python",
            Lang::JavaScript => "JavaScript",
            Lang::TypeScript => "TypeScript",
            Lang::Json => "JSON",
            Lang::Toml => "TOML",
            Lang::Ini => "INI",
            Lang::Shell => "Shell",
            Lang::PowerShell => "PowerShell",
            Lang::Batch => "Batch",
            Lang::Markdown => "Markdown",
            Lang::Html => "HTML",
            Lang::Css => "CSS",
            Lang::Go => "Go",
            Lang::Lua => "Lua",
            Lang::Sql => "SQL",
            Lang::Zig => "Zig",
        }
    }
}

/// Lexer state carried across lines. See the module docs for the encoding.
pub type LineState = u8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub start: u32,
    pub len: u32,
    pub tok: Tok,
}

/// Tokenise ONE line (no trailing '\n'). Clears and fills `out` with spans covering only
/// non-Normal regions (gaps are Normal). Returns the state at end of line.
pub fn highlight_line(
    lang: Lang,
    line: &[u8],
    state_in: LineState,
    out: &mut Vec<Span>,
) -> LineState {
    out.clear();
    match lang {
        Lang::Plain => 0,
        Lang::Markdown => lex_markdown(line, state_in, out),
        Lang::Json => lex_json(line, state_in, out),
        Lang::Ini => {
            lex_ini(line, false, out);
            0
        }
        Lang::Toml => {
            lex_ini(line, true, out);
            0
        }
        Lang::Html => lex_html(line, state_in, out),
        _ => match lang_def(lang) {
            Some(d) => lex_generic(d, line, state_in, out),
            None => 0,
        },
    }
}

/// Cache of line-start states so rendering line N only lexes from the nearest valid entry.
pub struct Highlighter {
    lang: Lang,
    /// `states[i]` is the state at the start of line `i`; `states.len() == valid_upto`.
    states: Vec<LineState>,
    valid_upto: usize,
    scratch: Vec<u8>,
    spans: Vec<Span>,
}

impl Highlighter {
    pub fn new(lang: Lang) -> Self {
        Highlighter {
            lang,
            states: vec![0],
            valid_upto: 1,
            scratch: Vec::new(),
            spans: Vec::new(),
        }
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Changes the language and invalidates every cached state.
    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
        self.invalidate_from(0);
    }

    /// Line `line` changed. Its start state is still valid; everything after it is not.
    pub fn invalidate_from(&mut self, line: usize) {
        self.valid_upto = self.valid_upto.min(line.saturating_add(1));
        self.states.truncate(self.valid_upto);
    }

    /// State at the start of `line`. Lexes forward from the last valid cached line, calling
    /// `get_line(i, buf)` to fill `buf` with the bytes of line `i` (buf is cleared first).
    pub fn state_at(
        &mut self,
        line: usize,
        get_line: &mut dyn FnMut(usize, &mut Vec<u8>),
    ) -> LineState {
        if self.lang == Lang::Plain {
            return 0;
        }
        while self.valid_upto <= line {
            let cur = self.valid_upto - 1;
            let st_in = self.states.last().copied().unwrap_or(0);
            self.scratch.clear();
            get_line(cur, &mut self.scratch);
            let st_out = highlight_line(self.lang, &self.scratch, st_in, &mut self.spans);
            self.states.push(st_out);
            self.valid_upto += 1;
        }
        self.states.get(line).copied().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

#[inline]
const fn enc(kind: u8, aux: u8) -> u8 {
    (kind & 7) | ((aux & 31) << 3)
}

#[inline]
fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

#[inline]
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// True if `pat` occurs at `i`.
#[inline]
fn starts(s: &[u8], i: usize, pat: &str) -> bool {
    s.get(i..).is_some_and(|t| t.starts_with(pat.as_bytes()))
}

/// Index of the first occurrence of `pat` at or after `from`.
fn find_from(s: &[u8], from: usize, pat: &str) -> Option<usize> {
    let p = pat.as_bytes();
    if p.is_empty() {
        return Some(from);
    }
    let t = s.get(from..)?;
    t.windows(p.len()).position(|w| w == p).map(|k| from + k)
}

/// Number of consecutive bytes equal to `c` starting at `i`.
fn run_len(s: &[u8], i: usize, c: u8) -> usize {
    s.get(i..)
        .map_or(0, |t| t.iter().take_while(|&&b| b == c).count())
}

/// Appends a span, merging with the previous one when it has the same token and touches it.
/// Empty spans and Normal spans are dropped. Never panics.
#[inline]
fn push(out: &mut Vec<Span>, start: usize, end: usize, tok: Tok) {
    if tok == Tok::Normal {
        return;
    }
    let end = end.min(u32::MAX as usize);
    let start = start.min(end);
    if start >= end {
        return;
    }
    let s = start as u32;
    let l = (end - start) as u32;
    if let Some(last) = out.last_mut() {
        if last.tok == tok && last.start.saturating_add(last.len) == s {
            last.len = last.len.saturating_add(l);
            return;
        }
    }
    out.push(Span {
        start: s,
        len: l,
        tok,
    });
}

/// Binary search of `word` in a sorted keyword list. With `ci`, `word` is ASCII-lowercased
/// into a stack buffer first and the list must be lowercase.
fn in_list(list: &[&str], ci: bool, word: &[u8]) -> bool {
    if !ci {
        return list.binary_search_by(|k| k.as_bytes().cmp(word)).is_ok();
    }
    let mut buf = [0u8; 16];
    if word.len() > buf.len() {
        return false;
    }
    let low = &mut buf[..word.len()];
    for (d, &b) in low.iter_mut().zip(word) {
        *d = b.to_ascii_lowercase();
    }
    let low: &[u8] = low;
    list.binary_search_by(|k| k.as_bytes().cmp(low)).is_ok()
}

fn scan_number(s: &[u8], i: usize) -> usize {
    let n = s.len();
    let mut j = i;
    if s[i] == b'0'
        && matches!(
            s.get(i + 1).copied(),
            Some(b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
        )
    {
        j = i + 2;
        while j < n && (s[j].is_ascii_hexdigit() || s[j] == b'_') {
            j += 1;
        }
    } else {
        while j < n && (s[j].is_ascii_digit() || s[j] == b'_') {
            j += 1;
        }
        if j + 1 < n && s[j] == b'.' && s[j + 1].is_ascii_digit() {
            j += 1;
            while j < n && (s[j].is_ascii_digit() || s[j] == b'_') {
                j += 1;
            }
        }
        if j < n && (s[j] == b'e' || s[j] == b'E') {
            let mut k = j + 1;
            if k < n && (s[k] == b'+' || s[k] == b'-') {
                k += 1;
            }
            if k < n && s[k].is_ascii_digit() {
                j = k;
                while j < n && s[j].is_ascii_digit() {
                    j += 1;
                }
            }
        }
    }
    // Type suffix such as u32, f64, L, ULL.
    while j < n && (s[j].is_ascii_alphanumeric() || s[j] == b'_') {
        j += 1;
    }
    j
}

// ---------------------------------------------------------------------------
// Generic C-like lexer, driven by a static per-language definition
// ---------------------------------------------------------------------------

struct LangDef {
    kw: &'static [&'static str],
    ty: &'static [&'static str],
    /// Keywords and types are matched case-insensitively (lists must be lowercase).
    ci: bool,
    /// Line comment prefixes.
    lines: &'static [&'static str],
    /// Block comment open/close.
    block: Option<(&'static str, &'static str)>,
    nested: bool,
    /// Single-line string quote bytes.
    quotes: &'static [u8],
    /// Python triple-quoted strings.
    triple: bool,
    /// Backtick strings, may span lines.
    backtick: bool,
    /// Rust `r#"..."#`.
    raw_rust: bool,
    /// C# `@"..."`.
    verbatim: bool,
    /// `#` at line start is a preprocessor line.
    preproc: bool,
    /// Uppercase-initial identifiers are types.
    upper_ty: bool,
    /// Variable sigil (`$`) for shell and PowerShell, 0 if none.
    var: u8,
    /// Batch: `REM` and `::` comments at statement start.
    batch: bool,
    /// Rust: lifetimes, char literals, macros, attributes.
    rust: bool,
}

const BASE: LangDef = LangDef {
    kw: &[],
    ty: &[],
    ci: false,
    lines: &[],
    block: None,
    nested: false,
    quotes: b"\"",
    triple: false,
    backtick: false,
    raw_rust: false,
    verbatim: false,
    preproc: false,
    upper_ty: false,
    var: 0,
    batch: false,
    rust: false,
};

static RUST: LangDef = LangDef {
    kw: RUST_KW,
    ty: RUST_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    nested: true,
    raw_rust: true,
    upper_ty: true,
    rust: true,
    ..BASE
};
static C: LangDef = LangDef {
    kw: C_KW,
    ty: C_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    preproc: true,
    ..BASE
};
static CPP: LangDef = LangDef {
    kw: CPP_KW,
    ty: CPP_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    preproc: true,
    upper_ty: true,
    ..BASE
};
static CSHARP: LangDef = LangDef {
    kw: CS_KW,
    ty: CS_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    verbatim: true,
    preproc: true,
    upper_ty: true,
    ..BASE
};
static PYTHON: LangDef = LangDef {
    kw: PY_KW,
    ty: PY_TY,
    lines: &["#"],
    quotes: b"\"'",
    triple: true,
    ..BASE
};
static JAVASCRIPT: LangDef = LangDef {
    kw: JS_KW,
    ty: JS_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    backtick: true,
    ..BASE
};
static TYPESCRIPT: LangDef = LangDef {
    kw: TS_KW,
    ty: TS_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    backtick: true,
    upper_ty: true,
    ..BASE
};
static GO: LangDef = LangDef {
    kw: GO_KW,
    ty: GO_TY,
    lines: &["//"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    backtick: true,
    upper_ty: true,
    ..BASE
};
static LUA: LangDef = LangDef {
    kw: LUA_KW,
    lines: &["--"],
    block: Some(("--[[", "]]")),
    quotes: b"\"'",
    ..BASE
};
static SQL: LangDef = LangDef {
    kw: SQL_KW,
    ty: SQL_TY,
    ci: true,
    lines: &["--"],
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    ..BASE
};
static SHELL: LangDef = LangDef {
    kw: SH_KW,
    lines: &["#"],
    quotes: b"\"'",
    var: b'$',
    ..BASE
};
static POWERSHELL: LangDef = LangDef {
    kw: PS_KW,
    ci: true,
    lines: &["#"],
    block: Some(("<#", "#>")),
    quotes: b"\"'",
    var: b'$',
    ..BASE
};
static BATCH: LangDef = LangDef {
    kw: BAT_KW,
    ci: true,
    batch: true,
    ..BASE
};
static ZIG: LangDef = LangDef {
    kw: ZIG_KW,
    ty: ZIG_TY,
    lines: &["//"],
    quotes: b"\"'",
    upper_ty: true,
    ..BASE
};
static CSS: LangDef = LangDef {
    kw: CSS_KW,
    block: Some(("/*", "*/")),
    quotes: b"\"'",
    ..BASE
};

fn lang_def(lang: Lang) -> Option<&'static LangDef> {
    Some(match lang {
        Lang::Rust => &RUST,
        Lang::C => &C,
        Lang::Cpp => &CPP,
        Lang::CSharp => &CSHARP,
        Lang::Python => &PYTHON,
        Lang::JavaScript => &JAVASCRIPT,
        Lang::TypeScript => &TYPESCRIPT,
        Lang::Go => &GO,
        Lang::Lua => &LUA,
        Lang::Sql => &SQL,
        Lang::Shell => &SHELL,
        Lang::PowerShell => &POWERSHELL,
        Lang::Batch => &BATCH,
        Lang::Zig => &ZIG,
        Lang::Css => &CSS,
        Lang::Plain | Lang::Json | Lang::Toml | Lang::Ini | Lang::Markdown | Lang::Html => {
            return None
        }
    })
}

/// Scans the body of a construct that may span lines. `seg` is where the emitted span starts,
/// `i` is where scanning starts. Returns the index after the construct (or the line end when
/// it is still open) and the state at end of line (0 when closed).
fn scan_multi(
    d: &LangDef,
    s: &[u8],
    seg: usize,
    mut i: usize,
    kind: u8,
    aux: u8,
    out: &mut Vec<Span>,
) -> (usize, u8) {
    let n = s.len();
    match kind {
        1 => {
            let Some((open, close)) = d.block else {
                return (i, 0);
            };
            let mut depth = usize::from(aux.clamp(1, 31));
            while i < n {
                if starts(s, i, close) {
                    i += close.len();
                    depth -= 1;
                    if depth == 0 {
                        push(out, seg, i, Tok::Comment);
                        return (i, 0);
                    }
                } else if d.nested && starts(s, i, open) {
                    depth = (depth + 1).min(31);
                    i += open.len();
                } else {
                    i += 1;
                }
            }
            push(out, seg, n, Tok::Comment);
            (n, enc(1, depth as u8))
        }
        2..=4 => {
            let close = match kind {
                2 => "\"\"\"",
                3 => "'''",
                _ => "`",
            };
            while i < n {
                if s[i] == b'\\' {
                    i += 2;
                } else if starts(s, i, close) {
                    i += close.len();
                    push(out, seg, i, Tok::String);
                    return (i, 0);
                } else {
                    i += 1;
                }
            }
            push(out, seg, n, Tok::String);
            (n, enc(kind, 0))
        }
        5 if d.verbatim => {
            while i < n {
                if s[i] == b'"' {
                    if s.get(i + 1) == Some(&b'"') {
                        i += 2;
                    } else {
                        i += 1;
                        push(out, seg, i, Tok::String);
                        return (i, 0);
                    }
                } else {
                    i += 1;
                }
            }
            push(out, seg, n, Tok::String);
            (n, enc(5, 0))
        }
        5 if d.raw_rust => {
            let h = usize::from(aux.min(31));
            while i < n {
                if s[i] == b'"' && (1..=h).all(|k| s.get(i + k) == Some(&b'#')) {
                    i += 1 + h;
                    push(out, seg, i, Tok::String);
                    return (i, 0);
                }
                i += 1;
            }
            push(out, seg, n, Tok::String);
            (n, enc(5, h as u8))
        }
        _ => (i, 0),
    }
}

/// Rust `'`: char literal, or lifetime when it is not closed within 2 chars. None means a
/// plain punctuation byte.
fn rust_quote(s: &[u8], i: usize) -> Option<(usize, Tok)> {
    let n = s.len();
    let c1 = *s.get(i + 1)?;
    if c1 == b'\\' {
        let mut j = i + 2;
        while j < n && s[j] != b'\'' {
            j += 1;
        }
        return Some(((j + 1).min(n), Tok::String));
    }
    if c1 != b'\'' && s.get(i + 2) == Some(&b'\'') {
        return Some((i + 3, Tok::String));
    }
    if c1 >= 0x80 {
        let lim = (i + 6).min(n);
        return (i + 2..lim)
            .find(|&k| s[k] == b'\'')
            .map(|k| (k + 1, Tok::String));
    }
    if is_ident_start(c1) {
        let mut j = i + 2;
        while j < n && is_ident_byte(s[j]) {
            j += 1;
        }
        return Some((j, Tok::Type));
    }
    None
}

/// True if an identifier ending at `j` is followed by a call: `(`, or Rust `!(` / `![` / `!{`.
fn call_follows(s: &[u8], mut j: usize, rust: bool) -> bool {
    if rust && s.get(j) == Some(&b'!') {
        return matches!(s.get(j + 1).copied(), Some(b'(' | b'[' | b'{'));
    }
    while matches!(s.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    s.get(j) == Some(&b'(')
}

fn classify(d: &LangDef, s: &[u8], start: usize, end: usize) -> Tok {
    let w = &s[start..end];
    if in_list(d.kw, d.ci, w) {
        return Tok::Keyword;
    }
    if in_list(d.ty, d.ci, w) {
        return Tok::Type;
    }
    if call_follows(s, end, d.rust) {
        return Tok::Function;
    }
    if d.upper_ty && w.first().is_some_and(|b| b.is_ascii_uppercase()) {
        return Tok::Type;
    }
    Tok::Normal
}

fn lex_generic(d: &LangDef, s: &[u8], state_in: u8, out: &mut Vec<Span>) -> u8 {
    let n = s.len();
    let mut i = 0usize;
    let kind = state_in & 7;
    if (1..=5).contains(&kind) {
        let (j, st) = scan_multi(d, s, 0, 0, kind, state_in >> 3, out);
        if st != 0 {
            return st;
        }
        i = j;
    }

    let lead = s.iter().position(|&b| !is_space(b)).unwrap_or(n);
    // Whole line is preprocessor (C-like `#`, Rust `#[`): non-string, non-comment bytes are Preproc.
    let pp = (d.preproc && s.get(lead) == Some(&b'#'))
        || (d.rust && s.get(lead..).is_some_and(|t| t.starts_with(b"#[")));

    if d.batch {
        let j = if s.get(lead) == Some(&b'@') {
            lead + 1
        } else {
            lead
        };
        let rem = s
            .get(j..j + 3)
            .is_some_and(|t| t.eq_ignore_ascii_case(b"rem"))
            && s.get(j + 3).is_none_or(|&b| is_space(b));
        if rem || starts(s, j, "::") {
            push(out, lead, n, Tok::Comment);
            return 0;
        }
    }

    while i < n {
        let c = s[i];
        if is_space(c) {
            if pp {
                push(out, i, i + 1, Tok::Preproc);
            }
            i += 1;
            continue;
        }
        if let Some((open, _)) = d.block {
            if starts(s, i, open) {
                let (j, st) = scan_multi(d, s, i, i + open.len(), 1, 0, out);
                if st != 0 {
                    return st;
                }
                i = j;
                continue;
            }
        }
        if d.lines.iter().any(|p| starts(s, i, p)) {
            push(out, i, n, Tok::Comment);
            return 0;
        }
        if d.raw_rust && c == b'r' {
            let mut j = i + 1;
            let mut h = 0usize;
            while h < 31 && s.get(j) == Some(&b'#') {
                h += 1;
                j += 1;
            }
            if s.get(j) == Some(&b'"') {
                let (k, st) = scan_multi(d, s, i, j + 1, 5, h as u8, out);
                if st != 0 {
                    return st;
                }
                i = k;
                continue;
            }
        }
        if d.verbatim && c == b'@' && s.get(i + 1) == Some(&b'"') {
            let (k, st) = scan_multi(d, s, i, i + 2, 5, 0, out);
            if st != 0 {
                return st;
            }
            i = k;
            continue;
        }
        if d.triple && (c == b'"' || c == b'\'') {
            let tq = if c == b'"' { "\"\"\"" } else { "'''" };
            if starts(s, i, tq) {
                let kind = if c == b'"' { 2 } else { 3 };
                let (k, st) = scan_multi(d, s, i, i + 3, kind, 0, out);
                if st != 0 {
                    return st;
                }
                i = k;
                continue;
            }
        }
        if d.backtick && c == b'`' {
            let (k, st) = scan_multi(d, s, i, i + 1, 4, 0, out);
            if st != 0 {
                return st;
            }
            i = k;
            continue;
        }
        if d.rust && c == b'\'' {
            if let Some((e, tok)) = rust_quote(s, i) {
                push(out, i, e, tok);
                i = e;
                continue;
            }
        }
        if d.quotes.contains(&c) {
            let mut j = i + 1;
            while j < n {
                let b = s[j];
                j += 1;
                if b == b'\\' {
                    j += 1;
                } else if b == c {
                    break;
                }
            }
            let e = j.min(n);
            push(out, i, e, Tok::String);
            i = e;
            continue;
        }
        if c.is_ascii_digit() {
            let e = scan_number(s, i);
            push(out, i, e, if pp { Tok::Preproc } else { Tok::Number });
            i = e;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < n && is_ident_byte(s[j]) {
                j += 1;
            }
            let tok = if pp {
                Tok::Preproc
            } else {
                classify(d, s, i, j)
            };
            push(out, i, j, tok);
            i = j;
            continue;
        }
        if d.var != 0 && c == d.var && s.get(i + 1).is_some_and(|&b| is_ident_start(b)) {
            let mut j = i + 2;
            while j < n && is_ident_byte(s[j]) {
                j += 1;
            }
            push(out, i, j, if pp { Tok::Preproc } else { Tok::Type });
            i = j;
            continue;
        }
        if c.is_ascii_graphic() {
            push(out, i, i + 1, if pp { Tok::Preproc } else { Tok::Punct });
        }
        i += 1;
    }
    0
}

// ---------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------

/// Search window for inline markers, so pathological lines stay linear-ish.
const INLINE_WINDOW: usize = 512;

fn lex_markdown(s: &[u8], state_in: u8, out: &mut Vec<Span>) -> u8 {
    let n = s.len();
    let lead = s.iter().take_while(|&&b| is_space(b)).count();
    if state_in & 7 == 6 {
        let fence = if state_in >> 3 != 0 { b'~' } else { b'`' };
        push(out, 0, n, Tok::String);
        let run = run_len(s, lead, fence);
        let closes = lead <= 3
            && run >= 3
            && s.get(lead + run..)
                .is_some_and(|t| t.iter().all(|&b| is_space(b)));
        return if closes { 0 } else { state_in };
    }
    if lead <= 3 {
        let fc = s.get(lead).copied().unwrap_or(0);
        if (fc == b'`' || fc == b'~') && run_len(s, lead, fc) >= 3 {
            push(out, lead, n, Tok::String);
            return enc(6, u8::from(fc == b'~'));
        }
        if fc == b'#' {
            let h = run_len(s, lead, b'#');
            if (1..=6).contains(&h) && s.get(lead + h).is_none_or(|&b| is_space(b)) {
                push(out, 0, n, Tok::Heading);
                return 0;
            }
        }
    }

    let mut i = 0usize;
    while i < n {
        let c = s[i];
        match c {
            b'`' => {
                let run = run_len(s, i, b'`');
                let lim = (i + INLINE_WINDOW).min(n);
                let mut j = i + run;
                let mut close = None;
                while j < lim {
                    if s[j] == b'`' {
                        let r = run_len(s, j, b'`');
                        if r == run {
                            close = Some(j + r);
                            break;
                        }
                        j += r;
                    } else {
                        j += 1;
                    }
                }
                match close {
                    Some(e) => {
                        push(out, i, e, Tok::String);
                        i = e;
                    }
                    None => i += run,
                }
            }
            b'*' | b'_' => {
                let run = run_len(s, i, c);
                let opener_ok = run <= 2
                    && s.get(i + run).is_some_and(|&b| !is_space(b))
                    && (c == b'*' || i == 0 || !is_ident_byte(s[i - 1]));
                let mut found = None;
                if opener_ok {
                    let lim = (i + INLINE_WINDOW).min(n);
                    let mut j = i + run;
                    while j < lim {
                        if s[j] == c {
                            let r = run_len(s, j, c);
                            if r == run && !is_space(s[j - 1]) {
                                found = Some(j + r);
                                break;
                            }
                            j += r;
                        } else {
                            j += 1;
                        }
                    }
                }
                match found {
                    Some(e) => {
                        push(out, i, e, Tok::Keyword);
                        i = e;
                    }
                    None => i += run,
                }
            }
            b'[' => {
                let lim = (i + INLINE_WINDOW).min(n);
                let mut j = i + 1;
                while j < lim && s[j] != b']' {
                    j += 1;
                }
                if j + 1 < n && s[j] == b']' && s[j + 1] == b'(' {
                    let mut k = j + 2;
                    while k < lim && s[k] != b')' {
                        k += 1;
                    }
                    if k < lim {
                        push(out, i, k + 1, Tok::Function);
                        i = k + 1;
                        continue;
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    0
}

// ---------------------------------------------------------------------------
// INI / TOML
// ---------------------------------------------------------------------------

fn lex_ini(s: &[u8], toml: bool, out: &mut Vec<Span>) {
    let n = s.len();
    let lead = s.iter().take_while(|&&b| is_space(b)).count();
    if lead >= n {
        return;
    }
    match s[lead] {
        b';' | b'#' => {
            push(out, lead, n, Tok::Comment);
            return;
        }
        b'[' => {
            let e = s[lead..]
                .iter()
                .position(|&b| b == b']')
                .map_or(n, |p| lead + p + 1);
            push(out, lead, e, Tok::Keyword);
            scan_value(s, e, toml, out);
            return;
        }
        _ => {}
    }
    let sep = s[lead..]
        .iter()
        .position(|&b| b == b'=' || (!toml && b == b':'))
        .map(|p| lead + p);
    if let Some(sep) = sep {
        let mut ke = sep;
        while ke > lead && is_space(s[ke - 1]) {
            ke -= 1;
        }
        push(out, lead, ke, Tok::Type);
        scan_value(s, sep + 1, toml, out);
    }
}

fn scan_value(s: &[u8], mut i: usize, toml: bool, out: &mut Vec<Span>) {
    let n = s.len();
    while i < n {
        let c = s[i];
        if c == b';' || (toml && c == b'#') {
            push(out, i, n, Tok::Comment);
            return;
        }
        if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < n {
                if s[j] == b'\\' && c == b'"' {
                    j += 2;
                } else if s[j] == c {
                    j += 1;
                    break;
                } else {
                    j += 1;
                }
            }
            let e = j.min(n);
            push(out, i, e, Tok::String);
            i = e;
            continue;
        }
        if !toml && c == b'#' && s.get(i + 1).is_some_and(|b| b.is_ascii_hexdigit()) {
            let mut j = i + 1;
            while j < n && s[j].is_ascii_hexdigit() {
                j += 1;
            }
            push(out, i, j, Tok::Number);
            i = j;
            continue;
        }
        if c.is_ascii_digit()
            || ((c == b'-' || c == b'+') && s.get(i + 1).is_some_and(|b| b.is_ascii_digit()))
        {
            let mut j = i + 1;
            while j < n
                && (s[j].is_ascii_alphanumeric()
                    || matches!(s[j], b'.' | b'_' | b':' | b'-' | b'+'))
            {
                j += 1;
            }
            push(out, i, j, Tok::Number);
            i = j;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < n && is_ident_byte(s[j]) {
                j += 1;
            }
            let w = &s[i..j];
            if w == b"true" || w == b"false" {
                push(out, i, j, Tok::Keyword);
            }
            i = j;
            continue;
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// JSON (also JSONC comments)
// ---------------------------------------------------------------------------

fn lex_json(s: &[u8], state_in: u8, out: &mut Vec<Span>) -> u8 {
    let n = s.len();
    let mut i = 0usize;
    if state_in & 7 == 1 {
        match find_from(s, 0, "*/") {
            Some(p) => {
                push(out, 0, p + 2, Tok::Comment);
                i = p + 2;
            }
            None => {
                push(out, 0, n, Tok::Comment);
                return 1;
            }
        }
    }
    while i < n {
        let c = s[i];
        if is_space(c) {
            i += 1;
        } else if c == b'"' {
            let mut j = i + 1;
            while j < n {
                if s[j] == b'\\' {
                    j += 2;
                } else if s[j] == b'"' {
                    j += 1;
                    break;
                } else {
                    j += 1;
                }
            }
            let e = j.min(n);
            let after = s[e..].iter().position(|&b| !is_space(b)).map(|p| e + p);
            let is_key = after.is_some_and(|k| s[k] == b':');
            push(out, i, e, if is_key { Tok::Type } else { Tok::String });
            i = e;
        } else if c == b'/' && s.get(i + 1) == Some(&b'/') {
            push(out, i, n, Tok::Comment);
            return 0;
        } else if c == b'/' && s.get(i + 1) == Some(&b'*') {
            match find_from(s, i + 2, "*/") {
                Some(p) => {
                    push(out, i, p + 2, Tok::Comment);
                    i = p + 2;
                }
                None => {
                    push(out, i, n, Tok::Comment);
                    return 1;
                }
            }
        } else if c.is_ascii_digit()
            || (c == b'-' && s.get(i + 1).is_some_and(|b| b.is_ascii_digit()))
        {
            let mut j = i + 1;
            while j < n && (s[j].is_ascii_alphanumeric() || matches!(s[j], b'.' | b'+' | b'-')) {
                j += 1;
            }
            push(out, i, j, Tok::Number);
            i = j;
        } else if c.is_ascii_alphabetic() {
            let mut j = i + 1;
            while j < n && s[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let w = &s[i..j];
            if w == b"true" || w == b"false" || w == b"null" {
                push(out, i, j, Tok::Keyword);
            }
            i = j;
        } else {
            if matches!(c, b'{' | b'}' | b'[' | b']' | b',' | b':') {
                push(out, i, i + 1, Tok::Punct);
            }
            i += 1;
        }
    }
    0
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

fn lex_html(s: &[u8], state_in: u8, out: &mut Vec<Span>) -> u8 {
    let n = s.len();
    let mut i = 0usize;
    let mut in_tag = false;
    if state_in & 7 == 7 {
        match find_from(s, 0, "-->") {
            Some(p) => {
                push(out, 0, p + 3, Tok::Comment);
                i = p + 3;
            }
            None => {
                push(out, 0, n, Tok::Comment);
                return 7;
            }
        }
    } else {
        in_tag = state_in & 7 == 6;
    }
    while i < n {
        let c = s[i];
        if in_tag {
            match c {
                b'>' => {
                    push(out, i, i + 1, Tok::Keyword);
                    in_tag = false;
                    i += 1;
                }
                b'"' | b'\'' => {
                    let mut j = i + 1;
                    while j < n && s[j] != c {
                        j += 1;
                    }
                    let e = (j + 1).min(n);
                    push(out, i, e, Tok::String);
                    i = e;
                }
                b'=' | b'/' | b'?' => {
                    push(out, i, i + 1, Tok::Punct);
                    i += 1;
                }
                _ if c.is_ascii_alphabetic() || c == b'_' => {
                    let mut j = i + 1;
                    while j < n && (is_ident_byte(s[j]) || matches!(s[j], b'-' | b':' | b'.')) {
                        j += 1;
                    }
                    push(out, i, j, Tok::Type);
                    i = j;
                }
                _ => i += 1,
            }
        } else if c == b'<' {
            if starts(s, i, "<!--") {
                match find_from(s, i + 4, "-->") {
                    Some(p) => {
                        push(out, i, p + 3, Tok::Comment);
                        i = p + 3;
                    }
                    None => {
                        push(out, i, n, Tok::Comment);
                        return 7;
                    }
                }
            } else if s.get(i + 1).is_some_and(|&b| b == b'!' || b == b'?') {
                let e = s[i..]
                    .iter()
                    .position(|&b| b == b'>')
                    .map_or(n, |p| i + p + 1);
                push(out, i, e, Tok::Preproc);
                i = e;
            } else {
                let mut j = i + 1;
                if s.get(j) == Some(&b'/') {
                    j += 1;
                }
                if s.get(j).is_some_and(|b| b.is_ascii_alphabetic()) {
                    while j < n && (is_ident_byte(s[j]) || matches!(s[j], b'-' | b':' | b'.')) {
                        j += 1;
                    }
                    push(out, i, j, Tok::Keyword);
                    i = j;
                    in_tag = true;
                } else {
                    push(out, i, i + 1, Tok::Punct);
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    if in_tag {
        enc(6, 0)
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Keyword tables. Each list is sorted (byte order) for binary_search; lowercase when `ci`.
// A unit test checks the ordering.
// ---------------------------------------------------------------------------

const RUST_KW: &[&str] = &[
    "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const RUST_TY: &[&str] = &[
    "bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128", "u16",
    "u32", "u64", "u8", "usize",
];
const C_KW: &[&str] = &[
    "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern", "for",
    "goto", "if", "inline", "register", "restrict", "return", "sizeof", "static", "struct",
    "switch", "typedef", "union", "volatile", "while",
];
const C_TY: &[&str] = &[
    "_Bool",
    "bool",
    "char",
    "double",
    "float",
    "int",
    "int16_t",
    "int32_t",
    "int64_t",
    "int8_t",
    "intptr_t",
    "long",
    "short",
    "signed",
    "size_t",
    "ssize_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "uint8_t",
    "uintptr_t",
    "unsigned",
    "void",
];
const CPP_KW: &[&str] = &[
    "auto",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "const_cast",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "namespace",
    "new",
    "noexcept",
    "nullptr",
    "operator",
    "private",
    "protected",
    "public",
    "reinterpret_cast",
    "return",
    "sizeof",
    "static",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typename",
    "union",
    "using",
    "virtual",
    "volatile",
    "while",
];
const CPP_TY: &[&str] = &[
    "bool", "char", "char16_t", "char32_t", "double", "float", "int", "long", "short", "signed",
    "size_t", "unsigned", "void", "wchar_t",
];
const CS_KW: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "checked",
    "class",
    "const",
    "continue",
    "default",
    "delegate",
    "do",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "interface",
    "internal",
    "is",
    "lock",
    "namespace",
    "new",
    "null",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sealed",
    "sizeof",
    "stackalloc",
    "static",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "unchecked",
    "unsafe",
    "using",
    "var",
    "virtual",
    "volatile",
    "while",
    "yield",
];
const CS_TY: &[&str] = &[
    "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "nint",
    "nuint", "object", "sbyte", "short", "string", "uint", "ulong", "ushort", "void",
];
const PY_KW: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];
const PY_TY: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "complex",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "object",
    "set",
    "str",
    "tuple",
    "type",
];
const JS_KW: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const JS_TY: &[&str] = &[
    "Array", "Boolean", "Date", "Error", "Map", "Number", "Object", "Promise", "RegExp", "Set",
    "String", "Symbol",
];
const TS_KW: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "module",
    "namespace",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const TS_TY: &[&str] = &[
    "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown",
];
const GO_KW: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "iota",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const GO_TY: &[&str] = &[
    "any",
    "bool",
    "byte",
    "complex128",
    "complex64",
    "error",
    "float32",
    "float64",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "rune",
    "string",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];
const LUA_KW: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];
const SQL_KW: &[&str] = &[
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "check",
    "commit",
    "create",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "foreign",
    "from",
    "group",
    "having",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "references",
    "returning",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "union",
    "unique",
    "update",
    "values",
    "view",
    "when",
    "where",
    "with",
];
const SQL_TY: &[&str] = &[
    "bigint",
    "blob",
    "boolean",
    "char",
    "date",
    "datetime",
    "decimal",
    "double",
    "float",
    "int",
    "integer",
    "numeric",
    "real",
    "smallint",
    "text",
    "timestamp",
    "varchar",
];
const SH_KW: &[&str] = &[
    "alias", "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
    "in", "local", "readonly", "return", "select", "shift", "source", "then", "time", "unset",
    "until", "while",
];
const PS_KW: &[&str] = &[
    "begin",
    "break",
    "catch",
    "class",
    "continue",
    "data",
    "define",
    "do",
    "dynamicparam",
    "else",
    "elseif",
    "end",
    "exit",
    "filter",
    "finally",
    "for",
    "foreach",
    "from",
    "function",
    "if",
    "in",
    "param",
    "process",
    "return",
    "switch",
    "throw",
    "trap",
    "try",
    "until",
    "using",
    "var",
    "while",
    "workflow",
];
const BAT_KW: &[&str] = &[
    "call",
    "cd",
    "cls",
    "copy",
    "defined",
    "del",
    "dir",
    "do",
    "echo",
    "else",
    "endlocal",
    "equ",
    "errorlevel",
    "exist",
    "exit",
    "for",
    "geq",
    "goto",
    "gtr",
    "if",
    "in",
    "leq",
    "lss",
    "md",
    "mkdir",
    "neq",
    "not",
    "pause",
    "rd",
    "rem",
    "ren",
    "set",
    "setlocal",
    "shift",
    "start",
    "type",
];
const ZIG_KW: &[&str] = &[
    "align",
    "and",
    "anyframe",
    "anytype",
    "asm",
    "break",
    "callconv",
    "catch",
    "comptime",
    "const",
    "continue",
    "defer",
    "else",
    "enum",
    "errdefer",
    "error",
    "export",
    "extern",
    "false",
    "fn",
    "for",
    "if",
    "inline",
    "noalias",
    "null",
    "or",
    "orelse",
    "packed",
    "pub",
    "resume",
    "return",
    "struct",
    "suspend",
    "switch",
    "test",
    "true",
    "try",
    "undefined",
    "union",
    "unreachable",
    "usingnamespace",
    "var",
    "volatile",
    "while",
];
const ZIG_TY: &[&str] = &[
    "bool", "f128", "f16", "f32", "f64", "f80", "i128", "i16", "i32", "i64", "i8", "isize",
    "noreturn", "type", "u128", "u16", "u32", "u64", "u8", "usize", "void",
];
const CSS_KW: &[&str] = &[
    "absolute",
    "auto",
    "block",
    "fixed",
    "flex",
    "grid",
    "hidden",
    "inherit",
    "initial",
    "inline",
    "none",
    "relative",
    "transparent",
    "unset",
];

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_LANGS: [Lang; 21] = [
        Lang::Plain,
        Lang::Rust,
        Lang::C,
        Lang::Cpp,
        Lang::CSharp,
        Lang::Python,
        Lang::JavaScript,
        Lang::TypeScript,
        Lang::Json,
        Lang::Toml,
        Lang::Ini,
        Lang::Shell,
        Lang::PowerShell,
        Lang::Batch,
        Lang::Markdown,
        Lang::Html,
        Lang::Css,
        Lang::Go,
        Lang::Lua,
        Lang::Sql,
        Lang::Zig,
    ];

    /// Spans as (text, tok) for an ASCII line, starting from state 0.
    fn toks(lang: Lang, line: &str) -> Vec<(&str, Tok)> {
        toks_st(lang, line, 0).0
    }

    fn toks_st(lang: Lang, line: &str, st: u8) -> (Vec<(&str, Tok)>, u8) {
        let mut out = Vec::new();
        let ret = highlight_line(lang, line.as_bytes(), st, &mut out);
        let v = out
            .iter()
            .map(|sp| {
                (
                    &line[sp.start as usize..(sp.start + sp.len) as usize],
                    sp.tok,
                )
            })
            .collect();
        (v, ret)
    }

    fn check_spans(line: &[u8], spans: &[Span]) {
        let mut prev_end = 0usize;
        let mut prev_tok: Option<Tok> = None;
        for sp in spans {
            assert!(sp.len > 0, "zero-length span");
            assert_ne!(sp.tok, Tok::Normal, "Normal span emitted");
            let start = sp.start as usize;
            let end = start + sp.len as usize;
            assert!(end <= line.len(), "span out of bounds");
            assert!(start >= prev_end, "spans overlap or are unordered");
            assert!(
                !(start == prev_end && prev_tok == Some(sp.tok)),
                "adjacent same-tok spans not merged"
            );
            prev_end = end;
            prev_tok = Some(sp.tok);
        }
    }

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
    }

    #[test]
    fn keyword_lists_are_sorted() {
        let lists: &[(&str, &[&str], bool)] = &[
            ("RUST_KW", RUST_KW, false),
            ("RUST_TY", RUST_TY, false),
            ("C_KW", C_KW, false),
            ("C_TY", C_TY, false),
            ("CPP_KW", CPP_KW, false),
            ("CPP_TY", CPP_TY, false),
            ("CS_KW", CS_KW, false),
            ("CS_TY", CS_TY, false),
            ("PY_KW", PY_KW, false),
            ("PY_TY", PY_TY, false),
            ("JS_KW", JS_KW, false),
            ("JS_TY", JS_TY, false),
            ("TS_KW", TS_KW, false),
            ("TS_TY", TS_TY, false),
            ("GO_KW", GO_KW, false),
            ("GO_TY", GO_TY, false),
            ("LUA_KW", LUA_KW, false),
            ("SQL_KW", SQL_KW, true),
            ("SQL_TY", SQL_TY, true),
            ("SH_KW", SH_KW, false),
            ("PS_KW", PS_KW, true),
            ("BAT_KW", BAT_KW, true),
            ("ZIG_KW", ZIG_KW, false),
            ("ZIG_TY", ZIG_TY, false),
            ("CSS_KW", CSS_KW, false),
        ];
        for (name, list, ci) in lists {
            assert!(
                list.windows(2).all(|w| w[0] < w[1]),
                "{name} is not strictly sorted"
            );
            if *ci {
                assert!(
                    list.iter().all(|k| *k == k.to_ascii_lowercase()),
                    "{name} must be lowercase"
                );
            }
        }
    }

    #[test]
    fn from_path_maps_extensions() {
        assert_eq!(Lang::from_path("src/main.RS"), Lang::Rust);
        assert_eq!(Lang::from_path("a\\b\\x.Cpp"), Lang::Cpp);
        assert_eq!(Lang::from_path("x.h"), Lang::C);
        assert_eq!(Lang::from_path("x.cs"), Lang::CSharp);
        assert_eq!(Lang::from_path("x.PYW"), Lang::Python);
        assert_eq!(Lang::from_path("x.mjs"), Lang::JavaScript);
        assert_eq!(Lang::from_path("x.tsx"), Lang::TypeScript);
        assert_eq!(Lang::from_path("x.jsonc"), Lang::Json);
        assert_eq!(Lang::from_path("x.toml"), Lang::Toml);
        assert_eq!(Lang::from_path("x.reg"), Lang::Ini);
        assert_eq!(Lang::from_path("x.zsh"), Lang::Shell);
        assert_eq!(Lang::from_path("x.psm1"), Lang::PowerShell);
        assert_eq!(Lang::from_path("x.CMD"), Lang::Batch);
        assert_eq!(Lang::from_path("x.markdown"), Lang::Markdown);
        assert_eq!(Lang::from_path("x.svg"), Lang::Html);
        assert_eq!(Lang::from_path("x.css"), Lang::Css);
        assert_eq!(Lang::from_path("x.go"), Lang::Go);
        assert_eq!(Lang::from_path("x.lua"), Lang::Lua);
        assert_eq!(Lang::from_path("x.sql"), Lang::Sql);
        assert_eq!(Lang::from_path("x.zig"), Lang::Zig);
        assert_eq!(Lang::from_path("C:\\proj\\Makefile"), Lang::Shell);
        assert_eq!(Lang::from_path("Dockerfile"), Lang::Shell);
        assert_eq!(Lang::from_path("notes.txt"), Lang::Plain);
        assert_eq!(Lang::from_path("noext"), Lang::Plain);
        assert_eq!(Lang::from_path("trailing."), Lang::Plain);
        assert_eq!(Lang::from_path(""), Lang::Plain);
    }

    #[test]
    fn plain_has_no_spans() {
        assert!(toks(Lang::Plain, "fn main() { \"x\" // c 42").is_empty());
        let mut h = Highlighter::new(Lang::Plain);
        let mut calls = 0;
        assert_eq!(h.state_at(5, &mut |_, _| calls += 1), 0);
        assert_eq!(calls, 0);
    }

    #[test]
    fn rust_basic_line() {
        assert_eq!(
            toks(Lang::Rust, "let s = \"hi\"; // c"),
            vec![
                ("let", Tok::Keyword),
                ("=", Tok::Punct),
                ("\"hi\"", Tok::String),
                (";", Tok::Punct),
                ("// c", Tok::Comment)
            ]
        );
    }

    #[test]
    fn rust_types_functions_numbers() {
        let t = toks(
            Lang::Rust,
            "pub fn add(a: u32) -> Option<u32> { Some(a) } let n = 1_000u64 + 0xFFu8;",
        );
        for want in [
            ("pub", Tok::Keyword),
            ("fn", Tok::Keyword),
            ("add", Tok::Function),
            ("u32", Tok::Type),
            ("Option", Tok::Type),
            ("Some", Tok::Function),
            ("1_000u64", Tok::Number),
            ("0xFFu8", Tok::Number),
        ] {
            assert!(t.contains(&want), "missing {want:?} in {t:?}");
        }
    }

    #[test]
    fn rust_lifetime_is_not_a_string() {
        let t = toks(Lang::Rust, "fn f<'a>(x: &'a str) -> char { 'x' }");
        assert!(t.contains(&("'a", Tok::Type)));
        assert!(t.contains(&("'x'", Tok::String)));
        assert!(!t.iter().any(|(s, k)| *k == Tok::String && s.contains("'a")));
    }

    #[test]
    fn rust_raw_string_single_line() {
        assert_eq!(
            toks(Lang::Rust, r##"let r = r#"a"b"#;"##),
            vec![
                ("let", Tok::Keyword),
                ("=", Tok::Punct),
                ("r#\"a\"b\"#", Tok::String),
                (";", Tok::Punct)
            ]
        );
    }

    #[test]
    fn rust_raw_string_spans_lines_with_hashes() {
        let (t1, st1) = toks_st(Lang::Rust, r##"let s = r##"start"##, 0);
        assert_eq!(
            t1,
            vec![
                ("let", Tok::Keyword),
                ("=", Tok::Punct),
                ("r##\"start", Tok::String)
            ]
        );
        assert_eq!(st1, enc(5, 2));
        // A single `"#` does not close a two-hash raw string; `"##` does.
        let (t2, st2) = toks_st(Lang::Rust, r###"inner "# still"##;"###, st1);
        assert_eq!(
            t2,
            vec![("inner \"# still\"##", Tok::String), (";", Tok::Punct)]
        );
        assert_eq!(st2, 0);
    }

    #[test]
    fn rust_nested_block_comment_carries_depth() {
        let (t, st) = toks_st(Lang::Rust, "/* a /* b", 0);
        assert_eq!(t, vec![("/* a /* b", Tok::Comment)]);
        assert_eq!(st, enc(1, 2));
        let (t, st) = toks_st(Lang::Rust, "c */ x", st);
        assert_eq!(t, vec![("c */ x", Tok::Comment)]);
        assert_eq!(st, enc(1, 1));
        let (t, st) = toks_st(Lang::Rust, "*/ let", st);
        assert_eq!(t, vec![("*/", Tok::Comment), ("let", Tok::Keyword)]);
        assert_eq!(st, 0);
    }

    #[test]
    fn rust_attribute_is_preproc() {
        let t = toks(Lang::Rust, "#[derive(Debug)]");
        assert_eq!(t, vec![("#[derive(Debug)]", Tok::Preproc)]);
    }

    #[test]
    fn python_basic_line() {
        assert_eq!(
            toks(Lang::Python, "def f(x): return 0x1F  # note"),
            vec![
                ("def", Tok::Keyword),
                ("f", Tok::Function),
                ("(", Tok::Punct),
                ("):", Tok::Punct),
                ("return", Tok::Keyword),
                ("0x1F", Tok::Number),
                ("# note", Tok::Comment),
            ]
        );
    }

    #[test]
    fn python_triple_quotes_span_lines() {
        let (t1, st1) = toks_st(Lang::Python, "x = \"\"\"doc", 0);
        assert_eq!(t1, vec![("=", Tok::Punct), ("\"\"\"doc", Tok::String)]);
        assert_eq!(st1, 2);
        let (t2, st2) = toks_st(Lang::Python, "more\"\"\" + 1", st1);
        assert_eq!(
            t2,
            vec![
                ("more\"\"\"", Tok::String),
                ("+", Tok::Punct),
                ("1", Tok::Number)
            ]
        );
        assert_eq!(st2, 0);
        let (_, st3) = toks_st(Lang::Python, "s = '''a", 0);
        assert_eq!(st3, 3);
    }

    #[test]
    fn c_preprocessor_line() {
        assert_eq!(
            toks(Lang::C, "#include <stdio.h>"),
            vec![("#include <stdio.h>", Tok::Preproc)]
        );
        assert_eq!(
            toks(Lang::C, "#define N 10 /* x */"),
            vec![("#define N 10 ", Tok::Preproc), ("/* x */", Tok::Comment)]
        );
    }

    #[test]
    fn c_line_and_types() {
        let t = toks(Lang::C, "int main(void) { return 0; } // done");
        assert!(t.contains(&("int", Tok::Type)));
        assert!(t.contains(&("main", Tok::Function)));
        assert!(t.contains(&("void", Tok::Type)));
        assert!(t.contains(&("return", Tok::Keyword)));
        assert!(t.contains(&("// done", Tok::Comment)));
    }

    #[test]
    fn c_block_comment_multiline() {
        let (t1, st1) = toks_st(Lang::C, "/* start", 0);
        assert_eq!(t1, vec![("/* start", Tok::Comment)]);
        assert_eq!(st1, enc(1, 1));
        let (t2, st2) = toks_st(Lang::C, "end */ int x;", st1);
        assert_eq!(
            t2,
            vec![
                ("end */", Tok::Comment),
                ("int", Tok::Type),
                (";", Tok::Punct)
            ]
        );
        assert_eq!(st2, 0);
    }

    #[test]
    fn cpp_and_csharp_samples() {
        let t = toks(Lang::Cpp, "template <typename T> class Foo; // x");
        assert!(t.contains(&("template", Tok::Keyword)));
        assert!(t.contains(&("Foo", Tok::Type)));
        let t = toks(Lang::CSharp, "public static readonly int N = 5;");
        assert!(t.contains(&("public", Tok::Keyword)));
        assert!(t.contains(&("int", Tok::Type)));
        assert!(t.contains(&("5", Tok::Number)));
        let (t, st) = toks_st(Lang::CSharp, "var p = @\"c:\\dir", 0);
        assert_eq!(
            t,
            vec![
                ("var", Tok::Keyword),
                ("=", Tok::Punct),
                ("@\"c:\\dir", Tok::String)
            ]
        );
        assert_eq!(st, enc(5, 0));
    }

    #[test]
    fn javascript_template_and_strings() {
        let (t, st) = toks_st(Lang::JavaScript, "const s = `a", 0);
        assert_eq!(
            t,
            vec![
                ("const", Tok::Keyword),
                ("=", Tok::Punct),
                ("`a", Tok::String)
            ]
        );
        assert_eq!(st, 4);
        assert_eq!(
            toks(Lang::JavaScript, "let v = 'q' + 2.5e3;"),
            vec![
                ("let", Tok::Keyword),
                ("=", Tok::Punct),
                ("'q'", Tok::String),
                ("+", Tok::Punct),
                ("2.5e3", Tok::Number),
                (";", Tok::Punct)
            ]
        );
    }

    #[test]
    fn typescript_types_and_calls() {
        assert_eq!(
            toks(Lang::TypeScript, "const x: Array<number> = new Map();"),
            vec![
                ("const", Tok::Keyword),
                (":", Tok::Punct),
                ("Array", Tok::Type),
                ("<", Tok::Punct),
                ("number", Tok::Type),
                (">", Tok::Punct),
                ("=", Tok::Punct),
                ("new", Tok::Keyword),
                ("Map", Tok::Function),
                ("();", Tok::Punct),
            ]
        );
    }

    #[test]
    fn go_calls_and_raw_strings() {
        assert_eq!(
            toks(Lang::Go, "x := fmt.Sprintf(\"%d\", 42) // ok"),
            vec![
                (":=", Tok::Punct),
                (".", Tok::Punct),
                ("Sprintf", Tok::Function),
                ("(", Tok::Punct),
                ("\"%d\"", Tok::String),
                (",", Tok::Punct),
                ("42", Tok::Number),
                (")", Tok::Punct),
                ("// ok", Tok::Comment),
            ]
        );
        let (t, st) = toks_st(Lang::Go, "s := `raw", 0);
        assert_eq!(t, vec![(":=", Tok::Punct), ("`raw", Tok::String)]);
        assert_eq!(st, 4);
    }

    #[test]
    fn lua_block_and_line_comments() {
        assert_eq!(
            toks(Lang::Lua, "local t = --[[ x ]] 5 -- end"),
            vec![
                ("local", Tok::Keyword),
                ("=", Tok::Punct),
                ("--[[ x ]]", Tok::Comment),
                ("5", Tok::Number),
                ("-- end", Tok::Comment),
            ]
        );
    }

    #[test]
    fn sql_case_insensitive() {
        assert_eq!(
            toks(Lang::Sql, "SELECT name FROM users WHERE id = 1; -- done"),
            vec![
                ("SELECT", Tok::Keyword),
                ("FROM", Tok::Keyword),
                ("WHERE", Tok::Keyword),
                ("=", Tok::Punct),
                ("1", Tok::Number),
                (";", Tok::Punct),
                ("-- done", Tok::Comment),
            ]
        );
        assert!(toks(Lang::Sql, "CREATE TABLE t (id INT);").contains(&("INT", Tok::Type)));
    }

    #[test]
    fn shell_strings_comments_vars() {
        assert_eq!(
            toks(Lang::Shell, "echo \"$HOME\" # done"),
            vec![("\"$HOME\"", Tok::String), ("# done", Tok::Comment)]
        );
        let t = toks(Lang::Shell, "export PATH=$HOME/bin");
        assert!(t.contains(&("export", Tok::Keyword)));
        assert!(t.contains(&("$HOME", Tok::Type)));
        assert_eq!(
            toks(Lang::Shell, "Makefile # x"),
            vec![("# x", Tok::Comment)]
        );
    }

    #[test]
    fn powershell_case_insensitive_and_block() {
        assert_eq!(
            toks(Lang::PowerShell, "$x = 1 # hi"),
            vec![
                ("$x", Tok::Type),
                ("=", Tok::Punct),
                ("1", Tok::Number),
                ("# hi", Tok::Comment)
            ]
        );
        let t = toks(Lang::PowerShell, "IF ($a) { RETURN 1 }");
        assert!(t.contains(&("IF", Tok::Keyword)));
        assert!(t.contains(&("RETURN", Tok::Keyword)));
        let (t, st) = toks_st(Lang::PowerShell, "<# multi", 0);
        assert_eq!(t, vec![("<# multi", Tok::Comment)]);
        assert_eq!(st, enc(1, 1));
        assert_eq!(toks_st(Lang::PowerShell, "line #> x", st).1, 0);
    }

    #[test]
    fn batch_rem_and_labels() {
        assert_eq!(
            toks(Lang::Batch, "@echo off"),
            vec![("@", Tok::Punct), ("echo", Tok::Keyword)]
        );
        assert_eq!(
            toks(Lang::Batch, "REM this is"),
            vec![("REM this is", Tok::Comment)]
        );
        assert_eq!(
            toks(Lang::Batch, ":: label"),
            vec![(":: label", Tok::Comment)]
        );
        assert_eq!(toks(Lang::Batch, "rem"), vec![("rem", Tok::Comment)]);
        assert!(toks(Lang::Batch, "SET X=1").contains(&("SET", Tok::Keyword)));
        // "remove" is not a REM comment.
        assert!(!toks(Lang::Batch, "remove x")
            .iter()
            .any(|(_, k)| *k == Tok::Comment));
    }

    #[test]
    fn zig_builtins_and_calls() {
        assert_eq!(
            toks(Lang::Zig, "const std = @import(\"std\");"),
            vec![
                ("const", Tok::Keyword),
                ("=", Tok::Punct),
                ("@", Tok::Punct),
                ("import", Tok::Function),
                ("(", Tok::Punct),
                ("\"std\"", Tok::String),
                (");", Tok::Punct),
            ]
        );
        assert!(toks(Lang::Zig, "var x: u8 = 0;").contains(&("u8", Tok::Type)));
    }

    #[test]
    fn css_block_comment() {
        let t = toks(Lang::Css, "/* c */ a { display: flex }");
        assert_eq!(t[0], ("/* c */", Tok::Comment));
        assert!(t.contains(&("flex", Tok::Keyword)));
        assert!(t.contains(&("{", Tok::Punct)));
    }

    #[test]
    fn json_keys_literals_numbers() {
        assert_eq!(
            toks(Lang::Json, "{\"name\": \"x\", \"n\": -1.5e3, \"ok\": true}"),
            vec![
                ("{", Tok::Punct),
                ("\"name\"", Tok::Type),
                (":", Tok::Punct),
                ("\"x\"", Tok::String),
                (",", Tok::Punct),
                ("\"n\"", Tok::Type),
                (":", Tok::Punct),
                ("-1.5e3", Tok::Number),
                (",", Tok::Punct),
                ("\"ok\"", Tok::Type),
                (":", Tok::Punct),
                ("true", Tok::Keyword),
                ("}", Tok::Punct),
            ]
        );
    }

    #[test]
    fn jsonc_block_comment_spans_lines() {
        let (t1, st1) = toks_st(Lang::Json, "/* a", 0);
        assert_eq!(t1, vec![("/* a", Tok::Comment)]);
        assert_eq!(st1, 1);
        let (t2, st2) = toks_st(Lang::Json, "b */ 2", st1);
        assert_eq!(t2, vec![("b */", Tok::Comment), ("2", Tok::Number)]);
        assert_eq!(st2, 0);
    }

    #[test]
    fn ini_sections_keys_values() {
        assert_eq!(toks(Lang::Ini, "[core]"), vec![("[core]", Tok::Keyword)]);
        assert_eq!(
            toks(Lang::Ini, "name = \"x\" ; note"),
            vec![
                ("name", Tok::Type),
                ("\"x\"", Tok::String),
                ("; note", Tok::Comment)
            ]
        );
        assert_eq!(toks(Lang::Ini, "; whole"), vec![("; whole", Tok::Comment)]);
        // '#' in a value is a hex colour, not a comment, in INI.
        assert_eq!(
            toks(Lang::Ini, "bg = #1e1e1e"),
            vec![("bg", Tok::Type), ("#1e1e1e", Tok::Number)]
        );
        assert_eq!(
            toks(Lang::Ini, "\"name\"=\"v\""),
            vec![("\"name\"", Tok::Type), ("\"v\"", Tok::String)]
        );
    }

    #[test]
    fn toml_comments_and_literals() {
        assert_eq!(
            toks(Lang::Toml, "port = 8080 # http"),
            vec![
                ("port", Tok::Type),
                ("8080", Tok::Number),
                ("# http", Tok::Comment)
            ]
        );
        assert_eq!(
            toks(Lang::Toml, "enabled = true"),
            vec![("enabled", Tok::Type), ("true", Tok::Keyword)]
        );
        assert_eq!(
            toks(Lang::Toml, "[server.tls]"),
            vec![("[server.tls]", Tok::Keyword)]
        );
    }

    #[test]
    fn markdown_headings_and_inline() {
        assert_eq!(
            toks(Lang::Markdown, "# Title"),
            vec![("# Title", Tok::Heading)]
        );
        assert_eq!(
            toks(Lang::Markdown, "## Sub"),
            vec![("## Sub", Tok::Heading)]
        );
        assert!(toks(Lang::Markdown, "#notheading").is_empty());
        assert_eq!(
            toks(Lang::Markdown, "Use `code` and **bold** and *em*."),
            vec![
                ("`code`", Tok::String),
                ("**bold**", Tok::Keyword),
                ("*em*", Tok::Keyword)
            ]
        );
        assert_eq!(
            toks(Lang::Markdown, "see [docs](https://x.y) now"),
            vec![("[docs](https://x.y)", Tok::Function)]
        );
        // A list bullet is not emphasis.
        assert!(toks(Lang::Markdown, "* item").is_empty());
    }

    #[test]
    fn markdown_fenced_block_carries_state() {
        assert_eq!(
            toks_st(Lang::Markdown, "```rust", 0),
            (vec![("```rust", Tok::String)], enc(6, 0))
        );
        assert_eq!(
            toks_st(Lang::Markdown, "let x = *y;", enc(6, 0)),
            (vec![("let x = *y;", Tok::String)], enc(6, 0))
        );
        assert_eq!(
            toks_st(Lang::Markdown, "```", enc(6, 0)),
            (vec![("```", Tok::String)], 0)
        );
        // A ~~~ fence is only closed by ~~~.
        assert_eq!(toks_st(Lang::Markdown, "~~~", 0).1, enc(6, 1));
        assert_eq!(toks_st(Lang::Markdown, "```", enc(6, 1)).1, enc(6, 1));
    }

    #[test]
    fn html_tags_attrs_strings() {
        assert_eq!(
            toks(Lang::Html, "<div class=\"a\">x</div>"),
            vec![
                ("<div", Tok::Keyword),
                ("class", Tok::Type),
                ("=", Tok::Punct),
                ("\"a\"", Tok::String),
                (">", Tok::Keyword),
                ("</div>", Tok::Keyword),
            ]
        );
    }

    #[test]
    fn html_comment_spans_lines() {
        let (t1, st1) = toks_st(Lang::Html, "<!-- start", 0);
        assert_eq!(t1, vec![("<!-- start", Tok::Comment)]);
        assert_eq!(st1, 7);
        let (t2, st2) = toks_st(Lang::Html, "end --> <b>", st1);
        assert_eq!(t2, vec![("end -->", Tok::Comment), ("<b>", Tok::Keyword)]);
        assert_eq!(st2, 0);
    }

    #[test]
    fn html_tag_spans_lines() {
        let (t1, st1) = toks_st(Lang::Html, "<img src=\"a.png\"", 0);
        assert_eq!(
            t1,
            vec![
                ("<img", Tok::Keyword),
                ("src", Tok::Type),
                ("=", Tok::Punct),
                ("\"a.png\"", Tok::String)
            ]
        );
        assert_eq!(st1, enc(6, 0));
        let (t2, st2) = toks_st(Lang::Html, "alt=\"x\">", st1);
        assert_eq!(
            t2,
            vec![
                ("alt", Tok::Type),
                ("=", Tok::Punct),
                ("\"x\"", Tok::String),
                (">", Tok::Keyword)
            ]
        );
        assert_eq!(st2, 0);
    }

    #[test]
    fn highlighter_cache_and_invalidation() {
        let mut lines: Vec<&str> = vec!["x = \"\"\"", "body", "\"\"\" ok", "y = 1"];
        let calls = std::cell::Cell::new(0usize);
        let mut h = Highlighter::new(Lang::Python);
        {
            let lines = &lines;
            let mut get = |i: usize, buf: &mut Vec<u8>| {
                calls.set(calls.get() + 1);
                buf.extend_from_slice(lines.get(i).copied().unwrap_or("").as_bytes());
            };
            assert_eq!(h.state_at(1, &mut get), 2);
            assert_eq!(h.state_at(3, &mut get), 0);
            assert_eq!(calls.get(), 3, "lines 0..=2 lexed once each");
            assert_eq!(h.state_at(2, &mut get), 2);
            assert_eq!(calls.get(), 3, "cached lookups do not re-lex");
        }

        // Edit line 0 so the string no longer opens; invalidate from that line.
        lines[0] = "x = 1";
        h.invalidate_from(0);
        calls.set(0);
        {
            let lines = &lines;
            let mut get = |i: usize, buf: &mut Vec<u8>| {
                calls.set(calls.get() + 1);
                buf.extend_from_slice(lines.get(i).copied().unwrap_or("").as_bytes());
            };
            assert_eq!(h.state_at(2, &mut get), 0);
            assert_eq!(
                h.state_at(3, &mut get),
                2,
                "line 2 now opens a triple quote"
            );
        }

        // Invalidating below the cached frontier only re-lexes from that line.
        calls.set(0);
        h.invalidate_from(1);
        {
            let lines = &lines;
            let mut get = |i: usize, buf: &mut Vec<u8>| {
                calls.set(calls.get() + 1);
                buf.extend_from_slice(lines.get(i).copied().unwrap_or("").as_bytes());
            };
            assert_eq!(h.state_at(3, &mut get), 2);
        }
        assert_eq!(calls.get(), 2, "only lines 1 and 2 are re-lexed");

        // Invalidating past the frontier changes nothing.
        calls.set(0);
        h.invalidate_from(10);
        {
            let mut get = |_: usize, _: &mut Vec<u8>| calls.set(calls.get() + 1);
            assert_eq!(h.state_at(3, &mut get), 2);
        }
        assert_eq!(calls.get(), 0);

        // Changing the language discards everything.
        h.set_lang(Lang::Plain);
        assert_eq!(h.lang(), Lang::Plain);
        assert_eq!(h.state_at(3, &mut |_, _| panic!("plain must not lex")), 0);
    }

    #[test]
    fn fuzz_no_panic_and_span_invariants() {
        let interesting = b"\"'`#/*-<>!@$[]()=:;\\rR\t 0123456789.xXeE_aA{}|%&~r\x00\xff\xc3\xa9";
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut out = Vec::new();
        for &lang in ALL_LANGS.iter() {
            let mut state = 0u8;
            for k in 0..5000 {
                let len = (rng.next_u64() % 96) as usize;
                let line: Vec<u8> = (0..len)
                    .map(|_| {
                        let r = rng.next_u64();
                        if r & 1 == 0 {
                            interesting[(r >> 8) as usize % interesting.len()]
                        } else {
                            let b = (r >> 16) as u8;
                            if b == b'\n' {
                                b'x'
                            } else {
                                b
                            }
                        }
                    })
                    .collect();
                if k % 7 == 0 {
                    state = (rng.next_u64() & 0xFF) as u8;
                }
                state = highlight_line(lang, &line, state, &mut out);
                check_spans(&line, &out);
                if k % 97 == 0 {
                    state = 0;
                }
            }
        }
    }

    #[test]
    fn long_line_stays_bounded() {
        let line = vec![b'['; 100_000];
        let mut out = Vec::new();
        highlight_line(Lang::Markdown, &line, 0, &mut out);
        check_spans(&line, &out);
        let line = vec![b'`'; 50_000];
        highlight_line(Lang::Markdown, &line, 0, &mut out);
        check_spans(&line, &out);
    }
}
