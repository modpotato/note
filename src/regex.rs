//! Small backtracking regular-expression engine for find and replace. Pure std.
//!
//! A pattern is parsed to a tree, compiled to a short instruction list, and run by a
//! backtracking VM over UTF-8 bytes. Positions are byte offsets on char boundaries.
//!
//! Syntax: literals and escapes (`\t \n \r \xHH \x{H..} \uHHHH \Q..\E`), `.`, classes with
//! ranges, `\d \w \s \h` and their negations, POSIX names (`[[:alpha:]]`), anchors
//! `^ $ \A \z \Z \b \B \< \>`, groups (capturing, `(?:)`, named `(?<n>)`), alternation,
//! greedy / lazy / possessive quantifiers, back-references `\1 \k<n>`, lookahead, lookbehind
//! (any length), atomic groups, comments `(?#..)` and the inline flags `i s m x`.
//! `^` and `$` match at line breaks unless `(?-m)` is set. `\r\n` and `\R` match a line break.
//!
//! Every search takes a step budget, so a pathological pattern stops with `TooComplex`
//! instead of hanging the editor.

use std::fmt;

use crate::simd;

/// Compile options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub ignore_case: bool,
    /// The match must not start or end inside a word.
    pub whole_word: bool,
    /// The pattern is plain text, not regex syntax.
    pub literal: bool,
}

/// A pattern or replacement-template error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// Char index in the pattern where the problem was found.
    pub pos: usize,
    pub msg: String,
}

impl Error {
    fn new(pos: usize, msg: impl Into<String>) -> Error {
        Error {
            pos,
            msg: msg.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at {})", self.msg, self.pos + 1)
    }
}

/// The step budget ran out before the search finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooComplex;

const NONE: usize = usize::MAX;
const MAX_INSTS: usize = 200_000;
const MAX_REPEAT: u32 = 1000;
const MAX_DEPTH: usize = 200;

// ---------------------------------------------------------------------------
// Characters
// ---------------------------------------------------------------------------

/// The char at `pos` and its byte length. Invalid UTF-8 reads as U+FFFD, one byte long.
#[inline]
pub(crate) fn decode(t: &[u8], pos: usize) -> Option<(char, usize)> {
    let b = *t.get(pos)?;
    if b < 0x80 {
        return Some((b as char, 1));
    }
    let n = utf8_len(b);
    let s = t.get(pos..pos + n)?;
    match std::str::from_utf8(s) {
        Ok(s) => s.chars().next().map(|c| (c, n)),
        Err(_) => Some(('\u{FFFD}', 1)),
    }
}

/// The char that ends at `pos`.
#[inline]
pub(crate) fn decode_before(t: &[u8], pos: usize) -> Option<char> {
    if pos == 0 {
        return None;
    }
    let mut s = pos - 1;
    while s > 0 && pos - s < 4 && t[s] & 0xC0 == 0x80 {
        s -= 1;
    }
    match decode(t, s) {
        Some((c, n)) if s + n == pos => Some(c),
        _ => Some('\u{FFFD}'),
    }
}

#[inline]
fn utf8_len(b: u8) -> usize {
    match b {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

/// True at the start of a char (or at the end of the text).
#[inline]
fn is_boundary(t: &[u8], pos: usize) -> bool {
    pos >= t.len() || t[pos] & 0xC0 != 0x80
}

/// Simple case fold: the lowercase form when it is a single char.
#[inline]
pub(crate) fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn has_case(c: char) -> bool {
    fold(c) != c || c.to_uppercase().next() != Some(c)
}

#[inline]
pub(crate) fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_hspace(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Assert {
    LineStart,
    LineEnd,
    TextStart,
    TextEnd,
    /// End of text, or before a final '\n'.
    TextEndNewline,
    WordBoundary,
    NotWordBoundary,
    WordStart,
    WordEnd,
    /// Whole-word guard: a word char here must not continue a word on the other side.
    NotInsideWord,
}

#[derive(Clone, Copy, Debug)]
enum Item {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
    HSpace(bool),
}

impl Item {
    fn has(self, c: char) -> bool {
        match self {
            Item::Range(lo, hi) => lo <= c && c <= hi,
            Item::Digit(neg) => c.is_ascii_digit() != neg,
            Item::Word(neg) => is_word(c) != neg,
            Item::Space(neg) => c.is_whitespace() != neg,
            Item::HSpace(neg) => is_hspace(c) != neg,
        }
    }

    /// True when the item can match a char outside ASCII.
    fn wide(self) -> bool {
        match self {
            Item::Range(_, hi) => hi >= '\u{80}',
            Item::Digit(neg) => neg,
            _ => true,
        }
    }
}

#[derive(Clone, Debug)]
struct Class {
    items: Vec<Item>,
    negated: bool,
    ci: bool,
    /// Membership of each ASCII char, case folding and negation applied.
    ascii: u128,
}

impl Class {
    fn new(items: Vec<Item>, negated: bool, ci: bool) -> Class {
        let mut c = Class {
            items,
            negated,
            ci,
            ascii: 0,
        };
        for b in 0..128u8 {
            if c.slow(b as char) {
                c.ascii |= 1 << b;
            }
        }
        c
    }

    fn raw(&self, c: char) -> bool {
        self.items.iter().any(|it| it.has(c))
    }

    fn slow(&self, c: char) -> bool {
        let mut hit = self.raw(c);
        if !hit && self.ci {
            hit = c
                .to_lowercase()
                .chain(c.to_uppercase())
                .any(|v| self.raw(v))
                || (fold(c) != c && self.raw(fold(c)));
        }
        hit != self.negated
    }

    #[inline]
    fn matches(&self, c: char) -> bool {
        if (c as u32) < 128 {
            self.ascii >> (c as u32) & 1 == 1
        } else {
            self.slow(c)
        }
    }

    fn wide(&self) -> bool {
        self.negated || self.ci || self.items.iter().any(|it| it.wide())
    }
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    /// A char, and whether it matches case-insensitively.
    Char(char, bool),
    /// `.`; true when it also matches '\n'.
    Any(bool),
    Class(Box<Class>),
    Assert(Assert),
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        possessive: bool,
    },
    Backref(usize, bool),
    Look {
        node: Box<Node>,
        ahead: bool,
        negate: bool,
    },
    Atomic(Box<Node>),
}

/// `\r?\n`, for `\r\n` and `\R` in a pattern. Grouped, so a quantifier repeats all of it.
fn newline() -> Node {
    let n = Node::Concat(vec![
        Node::Repeat {
            node: Box::new(Node::Char('\r', false)),
            min: 0,
            max: Some(1),
            greedy: true,
            possessive: false,
        },
        Node::Char('\n', false),
    ]);
    Node::Group(Box::new(n), None)
}

/// Byte length range of what `n` can match. `None` max means unbounded.
fn len_range(n: &Node) -> (usize, Option<usize>) {
    match n {
        Node::Empty | Node::Assert(_) | Node::Look { .. } => (0, Some(0)),
        Node::Char(c, ci) if !(*ci && has_case(*c)) => (c.len_utf8(), Some(c.len_utf8())),
        Node::Char(..) | Node::Any(_) | Node::Class(_) => (1, Some(4)),
        Node::Group(inner, _) | Node::Atomic(inner) => len_range(inner),
        Node::Concat(items) => items.iter().fold((0, Some(0)), |(lo, hi), it| {
            let (a, b) = len_range(it);
            (lo + a, hi.zip(b).map(|(x, y)| x + y))
        }),
        Node::Alt(alts) => {
            let mut lo = usize::MAX;
            let mut hi = Some(0);
            for a in alts {
                let (x, y) = len_range(a);
                lo = lo.min(x);
                hi = hi.zip(y).map(|(p, q)| p.max(q));
            }
            (if alts.is_empty() { 0 } else { lo }, hi)
        }
        Node::Repeat { node, min, max, .. } => {
            let (a, b) = len_range(node);
            let hi = match (b, max) {
                (Some(0), _) => Some(0),
                (Some(b), Some(m)) => Some(b * *m as usize),
                _ => None,
            };
            (a * *min as usize, hi)
        }
        Node::Backref(..) => (0, None),
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Flags {
    i: bool,
    s: bool,
    m: bool,
    x: bool,
}

struct Parser {
    p: Vec<char>,
    i: usize,
    flags: Flags,
    groups: usize,
    names: Vec<(String, usize)>,
    /// Numeric back-references, checked against the group count at the end.
    refs: Vec<(usize, usize)>,
}

type PResult<T> = Result<T, Error>;

impl Parser {
    fn peek(&self) -> Option<char> {
        self.p.get(self.i).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.p.get(self.i + k).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn looking_at(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, c)| self.peek_at(k) == Some(c))
    }

    fn err<T>(&self, pos: usize, msg: &str) -> PResult<T> {
        Err(Error::new(pos, msg))
    }

    /// In `x` mode, skips whitespace and `#` comments outside classes.
    fn skip_x(&mut self) {
        if !self.flags.x {
            return;
        }
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.i += 1;
            } else if c == '#' {
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn alt(&mut self, depth: usize) -> PResult<Node> {
        if depth > MAX_DEPTH {
            return self.err(self.i, "pattern is nested too deeply");
        }
        let mut alts = vec![self.concat(depth)?];
        while self.eat('|') {
            alts.push(self.concat(depth)?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alt(alts)
        })
    }

    fn concat(&mut self, depth: usize) -> PResult<Node> {
        let mut items = Vec::new();
        loop {
            self.skip_x();
            match self.peek() {
                None | Some('|') => break,
                Some(')') if depth > 0 => break,
                Some(')') => return self.err(self.i, "unmatched )"),
                _ => {}
            }
            let Some(atom) = self.atom(depth)? else {
                continue;
            };
            let atom = self.quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap_or(Node::Empty),
            _ => Node::Concat(items),
        })
    }

    fn char_node(&self, c: char) -> Node {
        Node::Char(c, self.flags.i && has_case(c))
    }

    /// One atom, or None for something that matches nothing (a flag group, a comment).
    fn atom(&mut self, depth: usize) -> PResult<Option<Node>> {
        let start = self.i;
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        self.i += 1;
        let node = match c {
            '.' => Node::Any(self.flags.s),
            '^' => Node::Assert(if self.flags.m {
                Assert::LineStart
            } else {
                Assert::TextStart
            }),
            '$' => Node::Assert(if self.flags.m {
                Assert::LineEnd
            } else {
                Assert::TextEndNewline
            }),
            '[' => self.class(start)?,
            '(' => return self.group(start, depth),
            '\\' => return self.escape(start).map(Some),
            '*' | '+' | '?' => return self.err(start, "nothing to repeat"),
            '{' if self.brace(start).is_some() => return self.err(start, "nothing to repeat"),
            c => self.char_node(c),
        };
        Ok(Some(node))
    }

    /// Parses `{n}`, `{n,}`, `{n,m}` or `{,m}` starting at `at` (the '{') without consuming.
    /// Returns (min, max, chars used).
    fn brace(&self, at: usize) -> Option<(u32, Option<u32>, usize)> {
        let mut k = at + 1;
        let num = |k: &mut usize| -> Option<u32> {
            let s = *k;
            let mut v: u32 = 0;
            while let Some(d) = self.p.get(*k).and_then(|c| c.to_digit(10)) {
                v = v.saturating_mul(10).saturating_add(d);
                *k += 1;
            }
            (*k > s).then_some(v)
        };
        let min = num(&mut k);
        let (min, max) = if self.p.get(k) == Some(&',') {
            k += 1;
            let max = num(&mut k);
            if min.is_none() && max.is_none() {
                return None;
            }
            (min.unwrap_or(0), max)
        } else {
            let m = min?;
            (m, Some(m))
        };
        (self.p.get(k) == Some(&'}')).then_some((min, max, k + 1 - at))
    }

    fn quantifier(&mut self, atom: Node) -> PResult<Node> {
        self.skip_x();
        let start = self.i;
        let (min, max) = match self.peek() {
            Some('*') => {
                self.i += 1;
                (0, None)
            }
            Some('+') => {
                self.i += 1;
                (1, None)
            }
            Some('?') => {
                self.i += 1;
                (0, Some(1))
            }
            Some('{') => match self.brace(self.i) {
                Some((min, max, used)) => {
                    self.i += used;
                    (min, max)
                }
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        if let Node::Assert(_) = atom {
            return self.err(start, "nothing to repeat");
        }
        // A quantifier after \Q..\E repeats only the last quoted char, as in Perl.
        if let Node::Concat(mut items) = atom {
            let last = items.pop().unwrap_or(Node::Empty);
            self.i = start;
            items.push(self.quantifier(last)?);
            return Ok(Node::Concat(items));
        }
        if max.is_some_and(|m| m < min) {
            return self.err(start, "repetition range is backwards");
        }
        if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
            return self.err(start, "repetition count is over 1000");
        }
        let (mut greedy, mut possessive) = (true, false);
        if self.eat('?') {
            greedy = false;
        } else if self.eat('+') {
            possessive = true;
        }
        self.skip_x();
        if matches!(self.peek(), Some('*' | '+' | '?'))
            || (self.peek() == Some('{') && self.brace(self.i).is_some())
        {
            return self.err(self.i, "nested quantifier");
        }
        Ok(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
            possessive,
        })
    }

    fn name(&mut self, close: char) -> PResult<String> {
        let start = self.i;
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c == close {
                self.i += 1;
                let ok = s
                    .chars()
                    .next()
                    .is_some_and(|f| f.is_alphabetic() || f == '_')
                    && s.chars().all(|c| c.is_alphanumeric() || c == '_');
                return if ok {
                    Ok(s)
                } else {
                    self.err(start, "invalid group name")
                };
            }
            s.push(c);
            self.i += 1;
        }
        self.err(start, "unclosed group name")
    }

    fn group(&mut self, start: usize, depth: usize) -> PResult<Option<Node>> {
        enum Kind {
            Capture(usize),
            Plain,
            Look(bool, bool),
            Atomic,
        }
        let saved = self.flags;
        let kind = if self.eat('?') {
            let Some(c) = self.peek() else {
                return self.err(start, "unclosed (");
            };
            self.i += 1;
            match c {
                ':' => Kind::Plain,
                '=' => Kind::Look(true, false),
                '!' => Kind::Look(true, true),
                '>' => Kind::Atomic,
                '<' if self.eat('=') => Kind::Look(false, false),
                '<' if self.eat('!') => Kind::Look(false, true),
                '<' | '\'' | 'P' => {
                    if c == 'P' && !self.eat('<') {
                        return self.err(start, "unknown group type");
                    }
                    let close = if c == '\'' { '\'' } else { '>' };
                    let name = self.name(close)?;
                    if self.names.iter().any(|(n, _)| *n == name) {
                        return self.err(start, "duplicate group name");
                    }
                    self.groups += 1;
                    self.names.push((name, self.groups));
                    Kind::Capture(self.groups)
                }
                '#' => {
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        if c == ')' {
                            return Ok(None);
                        }
                    }
                    return self.err(start, "unclosed comment");
                }
                _ => {
                    // Inline flags: (?i) (?-i) (?ims-x) (?i:...)
                    self.i -= 1;
                    let mut on = true;
                    loop {
                        let Some(f) = self.peek() else {
                            return self.err(start, "unclosed (");
                        };
                        self.i += 1;
                        match f {
                            'i' => self.flags.i = on,
                            's' => self.flags.s = on,
                            'm' => self.flags.m = on,
                            'x' => self.flags.x = on,
                            '-' if on => on = false,
                            ')' => return Ok(None),
                            ':' => break,
                            _ => return self.err(self.i - 1, "unknown group flag"),
                        }
                    }
                    Kind::Plain
                }
            }
        } else {
            self.groups += 1;
            Kind::Capture(self.groups)
        };
        let inner = self.alt(depth + 1)?;
        if !self.eat(')') {
            return self.err(start, "unclosed (");
        }
        self.flags = saved;
        let inner = Box::new(inner);
        Ok(Some(match kind {
            Kind::Capture(n) => Node::Group(inner, Some(n)),
            Kind::Plain => Node::Group(inner, None),
            Kind::Look(ahead, negate) => Node::Look {
                node: inner,
                ahead,
                negate,
            },
            Kind::Atomic => Node::Atomic(inner),
        }))
    }

    /// `\xHH`, `\x{H..}`, `\uHHHH` after the letter.
    fn hex(&mut self, start: usize, letter: char) -> PResult<char> {
        let (digits, braced) = if letter == 'x' && self.eat('{') {
            (8, true)
        } else if letter == 'x' {
            (2, false)
        } else {
            (4, false)
        };
        let mut v: u32 = 0;
        let mut n = 0;
        while n < digits {
            match self.peek().and_then(|c| c.to_digit(16)) {
                Some(d) => {
                    v = v * 16 + d;
                    n += 1;
                    self.i += 1;
                }
                None => break,
            }
        }
        if n == 0 || (braced && !self.eat('}')) || (!braced && n < digits) {
            return self.err(start, "invalid hex escape");
        }
        char::from_u32(v).map_or_else(|| self.err(start, "invalid code point"), Ok)
    }

    /// Escapes that stand for one char in both patterns and classes.
    fn char_escape(&mut self, start: usize, c: char) -> PResult<Option<char>> {
        Ok(Some(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'f' => '\x0C',
            'v' => '\x0B',
            'a' => '\x07',
            'e' => '\x1B',
            '0' => '\0',
            'x' | 'u' => self.hex(start, c)?,
            c if c.is_ascii_alphanumeric() => return Ok(None),
            c => c,
        }))
    }

    fn perl_item(c: char) -> Option<Item> {
        Some(match c {
            'd' => Item::Digit(false),
            'D' => Item::Digit(true),
            'w' => Item::Word(false),
            'W' => Item::Word(true),
            's' => Item::Space(false),
            'S' => Item::Space(true),
            'h' => Item::HSpace(false),
            'H' => Item::HSpace(true),
            _ => return None,
        })
    }

    fn escape(&mut self, start: usize) -> PResult<Node> {
        let Some(c) = self.peek() else {
            return self.err(start, "pattern ends with \\");
        };
        self.i += 1;
        if let Some(item) = Parser::perl_item(c) {
            return Ok(Node::Class(Box::new(Class::new(vec![item], false, false))));
        }
        let a = |a| Ok(Node::Assert(a));
        match c {
            'b' => return a(Assert::WordBoundary),
            'B' => return a(Assert::NotWordBoundary),
            '<' => return a(Assert::WordStart),
            '>' => return a(Assert::WordEnd),
            'A' => return a(Assert::TextStart),
            'z' => return a(Assert::TextEnd),
            'Z' => return a(Assert::TextEndNewline),
            'R' => return Ok(newline()),
            'r' if self.looking_at("\\n") => {
                self.i += 2;
                return Ok(newline());
            }
            'E' => return Ok(Node::Empty),
            'Q' => {
                let mut items = Vec::new();
                while self.peek().is_some() && !self.looking_at("\\E") {
                    let ch = self.p[self.i];
                    items.push(self.char_node(ch));
                    self.i += 1;
                }
                self.i = (self.i + 2).min(self.p.len());
                return Ok(Node::Concat(items));
            }
            'k' => {
                let close = match self.peek() {
                    Some('<') => '>',
                    Some('{') => '}',
                    Some('\'') => '\'',
                    _ => return self.err(start, "expected \\k<name>"),
                };
                self.i += 1;
                let name = self.name(close)?;
                return match self.names.iter().find(|(n, _)| *n == name) {
                    Some(&(_, g)) => Ok(Node::Backref(g, self.flags.i)),
                    None => self.err(start, "unknown group name"),
                };
            }
            '1'..='9' => {
                let mut g = c.to_digit(10).unwrap_or(0) as usize;
                if let Some(d) = self.peek().and_then(|d| d.to_digit(10)) {
                    let two = g * 10 + d as usize;
                    if two <= self.groups {
                        g = two;
                        self.i += 1;
                    }
                }
                self.refs.push((g, start));
                return Ok(Node::Backref(g, self.flags.i));
            }
            _ => {}
        }
        match self.char_escape(start, c)? {
            Some(ch) => Ok(self.char_node(ch)),
            None => self.err(start, &format!("unknown escape \\{c}")),
        }
    }

    fn posix(&mut self, start: usize) -> PResult<Vec<Item>> {
        // At "[:"; reads "name:]".
        self.i += 1;
        let neg = self.eat('^');
        let mut name = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == ':' {
                if !self.eat(']') {
                    break;
                }
                let r = |a: char, b: char| Item::Range(a, b);
                let items = match name.as_str() {
                    "alpha" => vec![r('a', 'z'), r('A', 'Z')],
                    "digit" => vec![Item::Digit(false)],
                    "alnum" => vec![r('a', 'z'), r('A', 'Z'), r('0', '9')],
                    "upper" => vec![r('A', 'Z')],
                    "lower" => vec![r('a', 'z')],
                    "space" => vec![Item::Space(false)],
                    "blank" => vec![r(' ', ' '), r('\t', '\t')],
                    "punct" => vec![r('!', '/'), r(':', '@'), r('[', '`'), r('{', '~')],
                    "xdigit" => vec![r('0', '9'), r('a', 'f'), r('A', 'F')],
                    "word" => vec![Item::Word(false)],
                    "cntrl" => vec![r('\0', '\x1F'), r('\x7F', '\x7F')],
                    "print" => vec![r(' ', '~')],
                    "graph" => vec![r('!', '~')],
                    _ => return self.err(start, "unknown POSIX class"),
                };
                if neg {
                    // Only single-item names can be negated cheaply; others are rare.
                    return match items.as_slice() {
                        [Item::Digit(_)] => Ok(vec![Item::Digit(true)]),
                        [Item::Space(_)] => Ok(vec![Item::Space(true)]),
                        [Item::Word(_)] => Ok(vec![Item::Word(true)]),
                        _ => self.err(start, "negated POSIX class is not supported here"),
                    };
                }
                return Ok(items);
            }
            name.push(c);
        }
        self.err(start, "unclosed POSIX class")
    }

    fn class(&mut self, start: usize) -> PResult<Node> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let at = self.i;
            let Some(c) = self.peek() else {
                return self.err(start, "unclosed [");
            };
            self.i += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = match c {
                '[' if self.peek() == Some(':') => {
                    items.extend(self.posix(at)?);
                    continue;
                }
                '\\' => {
                    let Some(e) = self.peek() else {
                        return self.err(at, "unclosed [");
                    };
                    self.i += 1;
                    if let Some(item) = Parser::perl_item(e) {
                        items.push(item);
                        continue;
                    }
                    let ch = if e == 'b' {
                        Some('\x08')
                    } else {
                        self.char_escape(at, e)?
                    };
                    match ch {
                        Some(ch) => ch,
                        None => return self.err(at, &format!("unknown escape \\{e}")),
                    }
                }
                c => c,
            };
            if self.peek() == Some('-') && self.peek_at(1).is_some_and(|n| n != ']') {
                self.i += 1;
                let hat = self.i;
                let hc = self.p[self.i];
                self.i += 1;
                let hi = if hc == '\\' {
                    let Some(e) = self.peek() else {
                        return self.err(hat, "unclosed [");
                    };
                    self.i += 1;
                    match self.char_escape(hat, e)? {
                        Some(ch) if Parser::perl_item(e).is_none() => ch,
                        _ => return self.err(hat, "invalid range end"),
                    }
                } else {
                    hc
                };
                if hi < lo {
                    return self.err(at, "range out of order");
                }
                items.push(Item::Range(lo, hi));
            } else {
                items.push(Item::Range(lo, lo));
            }
        }
        Ok(Node::Class(Box::new(Class::new(
            items,
            negated,
            self.flags.i,
        ))))
    }
}

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Inst {
    Char(char),
    /// Matches any char whose fold equals this (already folded) char.
    Fold(char),
    Any,
    AnyNl,
    Class(usize),
    Assert(Assert),
    /// Try the first target, fall back to the second.
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    /// Records the loop entry position for the empty-iteration check.
    Mark(usize),
    /// Fails if the loop body consumed nothing since its Mark.
    Check(usize),
    Backref(usize, bool),
    /// Zero-width sub-match. The sub-program starts at the next instruction and ends with a
    /// Match at `end - 1`. Lookbehind tries starts from `pos - min` back to `pos - max`.
    Look {
        ahead: bool,
        negate: bool,
        end: usize,
        min: usize,
        max: Option<usize>,
        caps: bool,
    },
    /// Sub-match that is never backtracked into.
    Atomic {
        end: usize,
        caps: bool,
    },
    Match,
}

struct Compiler {
    prog: Vec<Inst>,
    classes: Vec<Class>,
    marks: usize,
}

impl Compiler {
    fn push(&mut self, i: Inst) -> PResult<usize> {
        if self.prog.len() >= MAX_INSTS {
            return Err(Error::new(0, "pattern is too large"));
        }
        self.prog.push(i);
        Ok(self.prog.len() - 1)
    }

    fn emit(&mut self, n: &Node) -> PResult<()> {
        match n {
            Node::Empty => {}
            Node::Char(c, ci) => {
                self.push(if *ci {
                    Inst::Fold(fold(*c))
                } else {
                    Inst::Char(*c)
                })?;
            }
            Node::Any(nl) => {
                self.push(if *nl { Inst::AnyNl } else { Inst::Any })?;
            }
            Node::Class(c) => {
                self.classes.push((**c).clone());
                self.push(Inst::Class(self.classes.len() - 1))?;
            }
            Node::Assert(a) => {
                self.push(Inst::Assert(*a))?;
            }
            Node::Group(inner, None) => self.emit(inner)?,
            Node::Group(inner, Some(g)) => {
                self.push(Inst::Save(2 * g))?;
                self.emit(inner)?;
                self.push(Inst::Save(2 * g + 1))?;
            }
            Node::Concat(items) => {
                for it in items {
                    self.emit(it)?;
                }
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (k, a) in alts.iter().enumerate() {
                    if k + 1 < alts.len() {
                        let split = self.push(Inst::Split(0, 0))?;
                        self.emit(a)?;
                        jumps.push(self.push(Inst::Jmp(0))?);
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.emit(a)?;
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
                possessive,
            } => {
                if *possessive {
                    let at = self.push(Inst::Atomic {
                        end: 0,
                        caps: false,
                    })?;
                    self.repeat(node, *min, *max, true)?;
                    self.push(Inst::Match)?;
                    let end = self.prog.len();
                    let caps = self.has_saves(at + 1, end);
                    self.prog[at] = Inst::Atomic { end, caps };
                } else {
                    self.repeat(node, *min, *max, *greedy)?;
                }
            }
            Node::Backref(g, ci) => {
                self.push(Inst::Backref(*g, *ci))?;
            }
            Node::Look {
                node,
                ahead,
                negate,
            } => {
                let (min, max) = len_range(node);
                let at = self.push(Inst::Match)?;
                self.emit(node)?;
                self.push(Inst::Match)?;
                let end = self.prog.len();
                let caps = self.has_saves(at + 1, end);
                self.prog[at] = Inst::Look {
                    ahead: *ahead,
                    negate: *negate,
                    end,
                    min,
                    max,
                    caps,
                };
            }
            Node::Atomic(inner) => {
                let at = self.push(Inst::Match)?;
                self.emit(inner)?;
                self.push(Inst::Match)?;
                let end = self.prog.len();
                let caps = self.has_saves(at + 1, end);
                self.prog[at] = Inst::Atomic { end, caps };
            }
        }
        Ok(())
    }

    fn has_saves(&self, from: usize, to: usize) -> bool {
        self.prog[from..to]
            .iter()
            .any(|i| matches!(i, Inst::Save(_)))
    }

    fn repeat(&mut self, node: &Node, min: u32, max: Option<u32>, greedy: bool) -> PResult<()> {
        for _ in 0..min {
            self.emit(node)?;
        }
        match max {
            None => {
                let nullable = len_range(node).0 == 0;
                let split = self.push(Inst::Split(0, 0))?;
                let slot = nullable.then(|| {
                    self.marks += 1;
                    self.marks - 1
                });
                if let Some(s) = slot {
                    self.push(Inst::Mark(s))?;
                }
                self.emit(node)?;
                if let Some(s) = slot {
                    self.push(Inst::Check(s))?;
                }
                self.push(Inst::Jmp(split))?;
                let end = self.prog.len();
                self.prog[split] = if greedy {
                    Inst::Split(split + 1, end)
                } else {
                    Inst::Split(end, split + 1)
                };
            }
            Some(max) => {
                // x{0,k} as nested optionals: once one copy is skipped, so are the rest.
                let mut splits = Vec::new();
                for _ in min..max {
                    splits.push(self.push(Inst::Split(0, 0))?);
                    self.emit(node)?;
                }
                let end = self.prog.len();
                for s in splits {
                    self.prog[s] = if greedy {
                        Inst::Split(s + 1, end)
                    } else {
                        Inst::Split(end, s + 1)
                    };
                }
            }
        }
        Ok(())
    }
}

/// Where a match can start, used to skip hopeless positions quickly.
#[derive(Clone, Debug)]
enum Hint {
    Anywhere,
    TextStart,
    LineStart,
    /// Every match starts with these bytes (ASCII case-insensitive when the flag is set).
    Prefix(Vec<u8>, bool),
    /// Every match starts with one of these bytes (only char-start bytes are listed).
    FirstByte(Box<[bool; 256]>),
}

/// Appends the literal bytes every match of `n` starts with. Returns false where the
/// literal run ends.
fn prefix(n: &Node, out: &mut Vec<u8>, ci: &mut bool) -> bool {
    match n {
        Node::Char(c, fold) => {
            if *fold && !c.is_ascii() {
                return false;
            }
            *ci |= *fold;
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            true
        }
        Node::Group(inner, _) => prefix(inner, out, ci),
        Node::Concat(items) => items.iter().all(|it| prefix(it, out, ci)),
        Node::Assert(_) | Node::Look { .. } | Node::Empty => true,
        _ => false,
    }
}

/// Adds the possible first bytes of `n` to `set`. Returns Ok(nullable), or Err when any byte
/// is possible.
fn first_bytes(n: &Node, set: &mut [bool; 256]) -> Result<bool, ()> {
    let lead = |set: &mut [bool; 256]| set[0xC0..].fill(true);
    match n {
        Node::Empty | Node::Assert(_) | Node::Look { .. } => Ok(true),
        Node::Char(c, ci) => {
            let mut b = [0u8; 4];
            set[c.encode_utf8(&mut b).as_bytes()[0] as usize] = true;
            if *ci {
                for v in c.to_lowercase().chain(c.to_uppercase()) {
                    set[v.encode_utf8(&mut b).as_bytes()[0] as usize] = true;
                }
                // Folds like Kelvin sign -> 'k' reach ASCII letters from outside ASCII.
                lead(set);
            }
            Ok(false)
        }
        Node::Any(nl) => {
            let newline = set[b'\n' as usize];
            set[..0x80].fill(true);
            set[b'\n' as usize] = newline || *nl;
            lead(set);
            Ok(false)
        }
        Node::Class(c) => {
            for b in 0..0x80u8 {
                if c.matches(b as char) {
                    set[b as usize] = true;
                }
            }
            if c.wide() {
                lead(set);
            }
            Ok(false)
        }
        Node::Group(inner, _) | Node::Atomic(inner) => first_bytes(inner, set),
        Node::Concat(items) => {
            for it in items {
                if !first_bytes(it, set)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Node::Alt(alts) => {
            let mut nullable = false;
            for a in alts {
                nullable |= first_bytes(a, set)?;
            }
            Ok(nullable)
        }
        Node::Repeat { node, min, .. } => Ok(first_bytes(node, set)? || *min == 0),
        Node::Backref(..) => Err(()),
    }
}

/// The first non-zero-width node of `n`, looking through groups and concatenations.
fn leading_assert(n: &Node) -> Option<Assert> {
    match n {
        Node::Assert(a) => Some(*a),
        Node::Group(inner, _) => leading_assert(inner),
        Node::Concat(items) => items.first().and_then(leading_assert),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Regex
// ---------------------------------------------------------------------------

/// A compiled pattern.
#[derive(Clone, Debug)]
pub struct Regex {
    prog: Vec<Inst>,
    classes: Vec<Class>,
    groups: usize,
    names: Vec<(String, usize)>,
    marks: usize,
    hint: Hint,
}

/// Capture positions of the last match, plus scratch space the VM reuses between runs.
#[derive(Clone, Debug, Default)]
pub struct Captures {
    slots: Vec<usize>,
    marks: Vec<usize>,
    stack: Vec<Frame>,
}

impl Captures {
    /// Byte range of capture group `g` (0 is the whole match), if it took part.
    pub fn get(&self, g: usize) -> Option<(usize, usize)> {
        let s = *self.slots.get(2 * g)?;
        let e = *self.slots.get(2 * g + 1)?;
        (s != NONE && e != NONE && s <= e).then_some((s, e))
    }
}

#[derive(Clone, Copy, Debug)]
enum Frame {
    Try(usize, usize),
    Cap(usize, usize),
    Mark(usize, usize),
}

impl Regex {
    pub fn new(pattern: &str, opts: Options) -> Result<Regex, Error> {
        let mut parser = Parser {
            p: pattern.chars().collect(),
            i: 0,
            flags: Flags {
                i: opts.ignore_case,
                s: false,
                m: true,
                x: false,
            },
            groups: 0,
            names: Vec::new(),
            refs: Vec::new(),
        };
        let mut tree = if opts.literal {
            Node::Concat(parser.p.iter().map(|&c| parser.char_node(c)).collect())
        } else {
            parser.alt(0)?
        };
        if let Some(&(g, at)) = parser.refs.iter().find(|&&(g, _)| g > parser.groups) {
            return Err(Error::new(at, format!("no group {g} to refer to")));
        }
        if opts.whole_word {
            tree = Node::Concat(vec![
                Node::Assert(Assert::NotInsideWord),
                Node::Group(Box::new(tree), None),
                Node::Assert(Assert::NotInsideWord),
            ]);
        }

        let mut c = Compiler {
            prog: Vec::new(),
            classes: Vec::new(),
            marks: 0,
        };
        c.push(Inst::Save(0))?;
        c.emit(&tree)?;
        c.push(Inst::Save(1))?;
        c.push(Inst::Match)?;

        let mut bytes = Vec::new();
        let mut ci = false;
        prefix(&tree, &mut bytes, &mut ci);
        let hint = match leading_assert(&tree) {
            Some(Assert::TextStart) => Hint::TextStart,
            _ if !bytes.is_empty() => Hint::Prefix(bytes, ci),
            Some(Assert::LineStart) => Hint::LineStart,
            _ => {
                let mut set = [false; 256];
                match first_bytes(&tree, &mut set) {
                    Ok(false) => Hint::FirstByte(Box::new(set)),
                    _ => Hint::Anywhere,
                }
            }
        };
        Ok(Regex {
            prog: c.prog,
            classes: c.classes,
            groups: parser.groups,
            names: parser.names,
            marks: c.marks,
            hint,
        })
    }

    /// Number of capture groups, not counting the whole match.
    pub fn group_count(&self) -> usize {
        self.groups
    }

    /// Named groups and their numbers, in pattern order.
    pub fn group_names(&self) -> impl Iterator<Item = (&str, usize)> {
        self.names.iter().map(|(n, g)| (n.as_str(), *g))
    }

    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.names.iter().find(|(n, _)| n == name).map(|&(_, g)| g)
    }

    pub fn captures(&self) -> Captures {
        Captures::default()
    }

    /// True when the pattern could start a match at `s`, judged cheaply.
    fn could_start(&self, t: &[u8], s: usize) -> bool {
        match &self.hint {
            Hint::Anywhere => true,
            Hint::TextStart => s == 0,
            Hint::LineStart => s == 0 || t[s - 1] == b'\n',
            Hint::Prefix(p, ci) => match t.get(s..s + p.len()) {
                Some(w) if *ci => w.eq_ignore_ascii_case(p),
                Some(w) => w == p.as_slice(),
                None => false,
            },
            Hint::FirstByte(set) => s < t.len() && set[t[s] as usize],
        }
    }

    /// Runs the program anchored at `s`. Returns the match end; `caps` holds the groups.
    fn exec(
        &self,
        t: &[u8],
        s: usize,
        caps: &mut Captures,
        budget: &mut u64,
    ) -> Result<Option<usize>, TooComplex> {
        caps.slots.clear();
        caps.slots.resize(2 * (self.groups + 1), NONE);
        caps.marks.clear();
        caps.marks.resize(self.marks, NONE);
        caps.stack.clear();
        let mut vm = Vm {
            re: self,
            t,
            slots: &mut caps.slots,
            marks: &mut caps.marks,
            stack: &mut caps.stack,
            left: *budget,
        };
        let r = vm.run(0, s, None);
        *budget = vm.left;
        r
    }

    /// Anchored match at `s`. Returns the end of the match.
    pub fn match_at(
        &self,
        t: &[u8],
        s: usize,
        caps: &mut Captures,
        budget: &mut u64,
    ) -> Result<Option<usize>, TooComplex> {
        if s > t.len() || !is_boundary(t, s) {
            return Ok(None);
        }
        self.exec(t, s, caps, budget)
    }

    /// First match that starts in `from..=limit`. `caps` holds its groups.
    pub fn find_at(
        &self,
        t: &[u8],
        from: usize,
        limit: usize,
        caps: &mut Captures,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, TooComplex> {
        let limit = limit.min(t.len());
        let mut s = from;
        while s <= limit {
            match &self.hint {
                Hint::Anywhere => {}
                Hint::TextStart if s > 0 => return Ok(None),
                Hint::TextStart => {}
                Hint::LineStart => {
                    if s > 0 && t[s - 1] != b'\n' {
                        let end = (limit + 1).min(t.len());
                        match simd::find_byte(&t[s..end], b'\n') {
                            Some(i) => s += i + 1,
                            None => return Ok(None),
                        }
                        continue;
                    }
                }
                Hint::Prefix(p, ci) => {
                    let end = (limit + p.len()).min(t.len());
                    let hay = &t[s..end];
                    let found = if *ci {
                        simd::find_ascii_ci(hay, p)
                    } else {
                        simd::find(hay, p)
                    };
                    match found {
                        Some(i) => s += i,
                        None => return Ok(None),
                    }
                }
                Hint::FirstByte(set) => {
                    while s < limit && !set[t[s] as usize] {
                        s += 1;
                    }
                    if s >= t.len() || !set[t[s] as usize] {
                        return Ok(None);
                    }
                }
            }
            if s > limit {
                return Ok(None);
            }
            if is_boundary(t, s) {
                if let Some(e) = self.exec(t, s, caps, budget)? {
                    return Ok(Some((s, e)));
                }
            }
            if s >= t.len() {
                break;
            }
            s += utf8_len(t[s]).max(1);
        }
        Ok(None)
    }

    /// Last match that starts in `lo..before`, trying starts from the right.
    pub fn rfind_before(
        &self,
        t: &[u8],
        lo: usize,
        before: usize,
        caps: &mut Captures,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, TooComplex> {
        let mut s = before.min(t.len() + 1);
        while s > lo {
            s -= 1;
            if !is_boundary(t, s) || !self.could_start(t, s) {
                continue;
            }
            if let Some(e) = self.exec(t, s, caps, budget)? {
                return Ok(Some((s, e)));
            }
        }
        Ok(None)
    }
}

struct Vm<'a> {
    re: &'a Regex,
    t: &'a [u8],
    slots: &'a mut Vec<usize>,
    marks: &'a mut Vec<usize>,
    stack: &'a mut Vec<Frame>,
    left: u64,
}

impl Vm<'_> {
    fn assert(&self, a: Assert, pos: usize) -> bool {
        let t = self.t;
        let word_before = || decode_before(t, pos).is_some_and(is_word);
        let word_after = || decode(t, pos).is_some_and(|(c, _)| is_word(c));
        match a {
            Assert::LineStart => pos == 0 || t[pos - 1] == b'\n',
            Assert::LineEnd => pos == t.len() || t[pos] == b'\n',
            Assert::TextStart => pos == 0,
            Assert::TextEnd => pos == t.len(),
            Assert::TextEndNewline => pos == t.len() || (pos + 1 == t.len() && t[pos] == b'\n'),
            Assert::WordBoundary => word_before() != word_after(),
            Assert::NotWordBoundary => word_before() == word_after(),
            Assert::WordStart => !word_before() && word_after(),
            Assert::WordEnd => word_before() && !word_after(),
            Assert::NotInsideWord => !(word_before() && word_after()),
        }
    }

    fn backref(&self, g: usize, ci: bool, pos: usize) -> Option<usize> {
        let (s, e) = (self.slots[2 * g], self.slots[2 * g + 1]);
        if s == NONE || e == NONE || s > e {
            return None;
        }
        let want = &self.t[s..e];
        if !ci {
            return self
                .t
                .get(pos..pos + want.len())
                .filter(|w| *w == want)
                .map(|_| pos + want.len());
        }
        let (mut i, mut p) = (0, pos);
        while i < want.len() {
            let (a, n) = decode(want, i)?;
            let (b, m) = decode(self.t, p)?;
            if fold(a) != fold(b) {
                return None;
            }
            i += n;
            p += m;
        }
        Some(p)
    }

    /// Restores the slots that a successful sub-match changed when the outer match backtracks.
    fn keep_caps(&mut self, before: &[usize]) {
        for (k, &old) in before.iter().enumerate() {
            if self.slots[k] != old {
                self.stack.push(Frame::Cap(k, old));
            }
        }
    }

    /// Runs from `pc` at `pos` until a Match. With `want_end`, Match succeeds only there.
    fn run(
        &mut self,
        pc: usize,
        pos: usize,
        want_end: Option<usize>,
    ) -> Result<Option<usize>, TooComplex> {
        let re = self.re;
        let base = self.stack.len();
        self.stack.push(Frame::Try(pc, pos));
        while self.stack.len() > base {
            let (mut pc, mut pos) = match self.stack.pop() {
                Some(Frame::Try(pc, pos)) => (pc, pos),
                Some(Frame::Cap(s, v)) => {
                    self.slots[s] = v;
                    continue;
                }
                Some(Frame::Mark(s, v)) => {
                    self.marks[s] = v;
                    continue;
                }
                None => break,
            };
            loop {
                if self.left == 0 {
                    self.stack.truncate(base);
                    return Err(TooComplex);
                }
                self.left -= 1;
                match &re.prog[pc] {
                    Inst::Char(c) => match decode(self.t, pos) {
                        Some((d, n)) if d == *c => {
                            pos += n;
                            pc += 1;
                        }
                        _ => break,
                    },
                    Inst::Fold(c) => match decode(self.t, pos) {
                        Some((d, n)) if fold(d) == *c => {
                            pos += n;
                            pc += 1;
                        }
                        _ => break,
                    },
                    Inst::Any => match decode(self.t, pos) {
                        Some((d, n)) if d != '\n' => {
                            pos += n;
                            pc += 1;
                        }
                        _ => break,
                    },
                    Inst::AnyNl => match decode(self.t, pos) {
                        Some((_, n)) => {
                            pos += n;
                            pc += 1;
                        }
                        None => break,
                    },
                    Inst::Class(k) => match decode(self.t, pos) {
                        Some((d, n)) if re.classes[*k].matches(d) => {
                            pos += n;
                            pc += 1;
                        }
                        _ => break,
                    },
                    Inst::Assert(a) => {
                        if !self.assert(*a, pos) {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Split(a, b) => {
                        self.stack.push(Frame::Try(*b, pos));
                        pc = *a;
                    }
                    Inst::Jmp(a) => pc = *a,
                    Inst::Save(s) => {
                        self.stack.push(Frame::Cap(*s, self.slots[*s]));
                        self.slots[*s] = pos;
                        pc += 1;
                    }
                    Inst::Mark(s) => {
                        self.stack.push(Frame::Mark(*s, self.marks[*s]));
                        self.marks[*s] = pos;
                        pc += 1;
                    }
                    Inst::Check(s) => {
                        if self.marks[*s] == pos {
                            break;
                        }
                        pc += 1;
                    }
                    Inst::Backref(g, ci) => match self.backref(*g, *ci, pos) {
                        Some(p) => {
                            pos = p;
                            pc += 1;
                        }
                        None => break,
                    },
                    &Inst::Look {
                        ahead,
                        negate,
                        end,
                        min,
                        max,
                        caps,
                    } => {
                        let before = caps.then(|| self.slots.clone());
                        let found = if ahead {
                            self.run(pc + 1, pos, None)?.is_some()
                        } else {
                            let lo = max.map_or(0, |m| pos.saturating_sub(m));
                            let mut hit = false;
                            if let Some(mut s) = pos.checked_sub(min) {
                                loop {
                                    if is_boundary(self.t, s)
                                        && self.run(pc + 1, s, Some(pos))?.is_some()
                                    {
                                        hit = true;
                                        break;
                                    }
                                    if s <= lo {
                                        break;
                                    }
                                    s -= 1;
                                }
                            }
                            hit
                        };
                        if let Some(before) = before {
                            if found && !negate {
                                self.keep_caps(&before);
                            } else if found {
                                self.slots.copy_from_slice(&before);
                            }
                        }
                        if found == negate {
                            break;
                        }
                        pc = end;
                    }
                    &Inst::Atomic { end, caps } => {
                        let before = caps.then(|| self.slots.clone());
                        match self.run(pc + 1, pos, None)? {
                            Some(p) => {
                                if let Some(before) = before {
                                    self.keep_caps(&before);
                                }
                                pos = p;
                                pc = end;
                            }
                            None => break,
                        }
                    }
                    Inst::Match => {
                        if want_end.is_some_and(|e| e != pos) {
                            break;
                        }
                        self.stack.truncate(base);
                        return Ok(Some(pos));
                    }
                }
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Replacement templates
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaseOp {
    Upper,
    Lower,
    End,
    NextUpper,
    NextLower,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Lit(String),
    Group(usize),
    Case(CaseOp),
}

/// A parsed replacement: literal text, group references and case conversions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template {
    pieces: Vec<Piece>,
}

impl Template {
    /// The replacement taken as plain text.
    pub fn literal(s: &str) -> Template {
        Template {
            pieces: vec![Piece::Lit(s.to_string())],
        }
    }

    /// Text with escapes only: `\n \t \\ \xHH \x{H..}`; `\r\n` becomes `\n`.
    pub fn escapes(s: &str) -> Result<Template, Error> {
        Template::parse_impl(s, None)
    }

    /// Regex replacement: `$0 $& $1 ${name} $$`, `\0..\9`, `\n \t \\ \xHH \x{H..}`, and the
    /// case conversions `\U \L \E \u \l`. Group references are checked against `re`.
    pub fn regex(s: &str, re: &Regex) -> Result<Template, Error> {
        Template::parse_impl(s, Some(re))
    }

    fn parse_impl(s: &str, re: Option<&Regex>) -> Result<Template, Error> {
        let p: Vec<char> = s.chars().collect();
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut i = 0;
        let flush = |lit: &mut String, pieces: &mut Vec<Piece>| {
            if !lit.is_empty() {
                pieces.push(Piece::Lit(std::mem::take(lit)));
            }
        };
        let group = |g: usize, at: usize| -> Result<Piece, Error> {
            match re {
                Some(re) if g <= re.group_count() => Ok(Piece::Group(g)),
                _ => Err(Error::new(at, format!("no group {g} in the pattern"))),
            }
        };
        while i < p.len() {
            let c = p[i];
            let at = i;
            i += 1;
            if c == '$' && re.is_some() {
                match p.get(i) {
                    Some('$') => {
                        lit.push('$');
                        i += 1;
                    }
                    Some('&') => {
                        flush(&mut lit, &mut pieces);
                        pieces.push(Piece::Group(0));
                        i += 1;
                    }
                    Some(d) if d.is_ascii_digit() => {
                        let mut g = d.to_digit(10).unwrap_or(0) as usize;
                        i += 1;
                        if let Some(d2) = p.get(i).and_then(|d| d.to_digit(10)) {
                            let two = g * 10 + d2 as usize;
                            if re.is_some_and(|r| two <= r.group_count()) {
                                g = two;
                                i += 1;
                            }
                        }
                        flush(&mut lit, &mut pieces);
                        pieces.push(group(g, at)?);
                    }
                    Some('{') => {
                        let close = p[i..].iter().position(|&c| c == '}').map(|k| i + k);
                        let Some(close) = close else {
                            return Err(Error::new(at, "unclosed ${"));
                        };
                        let name: String = p[i + 1..close].iter().collect();
                        let g = match name.parse::<usize>() {
                            Ok(g) => g,
                            Err(_) => match re.and_then(|r| r.group_index(&name)) {
                                Some(g) => g,
                                None => {
                                    return Err(Error::new(at, format!("no group named {name}")))
                                }
                            },
                        };
                        flush(&mut lit, &mut pieces);
                        pieces.push(group(g, at)?);
                        i = close + 1;
                    }
                    _ => lit.push('$'),
                }
                continue;
            }
            if c != '\\' {
                lit.push(c);
                continue;
            }
            let Some(&e) = p.get(i) else {
                lit.push('\\');
                break;
            };
            i += 1;
            let case = |op| Some(Piece::Case(op));
            let piece = match e {
                'U' if re.is_some() => case(CaseOp::Upper),
                'L' if re.is_some() => case(CaseOp::Lower),
                'E' if re.is_some() => case(CaseOp::End),
                'u' if re.is_some() => case(CaseOp::NextUpper),
                'l' if re.is_some() => case(CaseOp::NextLower),
                d if d.is_ascii_digit() && re.is_some() => {
                    Some(group(d.to_digit(10).unwrap_or(0) as usize, at)?)
                }
                _ => None,
            };
            if let Some(piece) = piece {
                flush(&mut lit, &mut pieces);
                pieces.push(piece);
                continue;
            }
            match e {
                'n' => lit.push('\n'),
                't' => lit.push('\t'),
                'r' if p.get(i) == Some(&'\\') && p.get(i + 1) == Some(&'n') => {
                    lit.push('\n');
                    i += 2;
                }
                'r' => lit.push('\r'),
                'f' => lit.push('\x0C'),
                'v' => lit.push('\x0B'),
                'a' => lit.push('\x07'),
                'e' => lit.push('\x1B'),
                '0' => lit.push('\0'),
                'x' => {
                    let braced = p.get(i) == Some(&'{');
                    let from = if braced { i + 1 } else { i };
                    let mut k = from;
                    while k < p.len() && p[k].is_ascii_hexdigit() && (braced || k - from < 2) {
                        k += 1;
                    }
                    let hex: String = p[from..k].iter().collect();
                    let ok = !hex.is_empty() && (!braced || p.get(k) == Some(&'}'));
                    let ch = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                    match ch {
                        Some(ch) if ok => lit.push(ch),
                        _ => return Err(Error::new(at, "invalid hex escape")),
                    }
                    i = if braced { k + 1 } else { k };
                }
                other => lit.push(other),
            }
        }
        flush(&mut lit, &mut pieces);
        Ok(Template { pieces })
    }

    /// The text of a template made only of literals (as `escapes` makes).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for p in &self.pieces {
            if let Piece::Lit(l) = p {
                s.push_str(l);
            }
        }
        s
    }

    /// True when the template has no group references or case conversions.
    pub fn is_literal(&self) -> bool {
        self.pieces.iter().all(|p| matches!(p, Piece::Lit(_)))
    }

    /// Appends the replacement for the match in `caps` (over text `t`) to `out`.
    pub fn expand(&self, t: &[u8], caps: &Captures, out: &mut Vec<u8>) {
        let mut mode: Option<CaseOp> = None;
        let mut next: Option<CaseOp> = None;
        for p in &self.pieces {
            match p {
                Piece::Lit(s) => push_cased(s, out, mode, &mut next),
                Piece::Group(g) => {
                    if let Some((s, e)) = caps.get(*g) {
                        push_cased(&String::from_utf8_lossy(&t[s..e]), out, mode, &mut next);
                    }
                }
                Piece::Case(op @ (CaseOp::Upper | CaseOp::Lower)) => mode = Some(*op),
                Piece::Case(CaseOp::End) => {
                    mode = None;
                    next = None;
                }
                Piece::Case(op) => next = Some(*op),
            }
        }
    }
}

/// Appends `s`, converting case per the running `mode` and the one-shot `next`.
fn push_cased(s: &str, out: &mut Vec<u8>, mode: Option<CaseOp>, next: &mut Option<CaseOp>) {
    if mode.is_none() && next.is_none() {
        out.extend_from_slice(s.as_bytes());
        return;
    }
    let mut b = [0u8; 4];
    for c in s.chars() {
        match next.take().or(mode) {
            Some(CaseOp::Upper | CaseOp::NextUpper) => {
                for u in c.to_uppercase() {
                    out.extend_from_slice(u.encode_utf8(&mut b).as_bytes());
                }
            }
            Some(CaseOp::Lower | CaseOp::NextLower) => {
                for l in c.to_lowercase() {
                    out.extend_from_slice(l.encode_utf8(&mut b).as_bytes());
                }
            }
            _ => out.extend_from_slice(c.encode_utf8(&mut b).as_bytes()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: u64 = 10_000_000;

    fn re(p: &str) -> Regex {
        Regex::new(p, Options::default()).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    fn re_ci(p: &str) -> Regex {
        let o = Options {
            ignore_case: true,
            ..Options::default()
        };
        Regex::new(p, o).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    /// First match as text.
    fn find(r: &Regex, t: &str) -> Option<String> {
        let mut caps = r.captures();
        let mut b = BUDGET;
        r.find_at(t.as_bytes(), 0, t.len(), &mut caps, &mut b)
            .unwrap()
            .map(|(s, e)| t[s..e].to_string())
    }

    /// Every non-overlapping match.
    fn all(r: &Regex, t: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut caps = r.captures();
        let mut b = BUDGET;
        let mut at = 0;
        while let Some((s, e)) = r
            .find_at(t.as_bytes(), at, t.len(), &mut caps, &mut b)
            .unwrap()
        {
            out.push(t[s..e].to_string());
            at = if e > s {
                e
            } else {
                e + t[e..].chars().next().map_or(1, |c| c.len_utf8())
            };
            if at > t.len() {
                break;
            }
        }
        out
    }

    fn group(r: &Regex, t: &str, g: usize) -> Option<String> {
        let mut caps = r.captures();
        let mut b = BUDGET;
        r.find_at(t.as_bytes(), 0, t.len(), &mut caps, &mut b)
            .unwrap()?;
        caps.get(g).map(|(s, e)| t[s..e].to_string())
    }

    fn replace(p: &str, rep: &str, t: &str) -> String {
        let r = re(p);
        let tpl = Template::regex(rep, &r).unwrap();
        let mut caps = r.captures();
        let mut b = BUDGET;
        let mut out = Vec::new();
        let mut at = 0;
        let mut last = 0;
        while let Some((s, e)) = r
            .find_at(t.as_bytes(), at, t.len(), &mut caps, &mut b)
            .unwrap()
        {
            out.extend_from_slice(&t.as_bytes()[last..s]);
            tpl.expand(t.as_bytes(), &caps, &mut out);
            last = e;
            at = if e > s { e } else { e + 1 };
            if at > t.len() {
                break;
            }
        }
        out.extend_from_slice(&t.as_bytes()[last..]);
        String::from_utf8(out).unwrap()
    }

    fn err(p: &str) -> String {
        Regex::new(p, Options::default()).unwrap_err().msg
    }

    #[test]
    fn literals_and_dot() {
        assert_eq!(find(&re("abc"), "xxabcxx").as_deref(), Some("abc"));
        assert_eq!(find(&re("a.c"), "abc a\nc").as_deref(), Some("abc"));
        assert_eq!(find(&re("a.c"), "a\nc"), None);
        assert_eq!(find(&re("(?s)a.c"), "a\nc").as_deref(), Some("a\nc"));
        assert_eq!(find(&re("é.ü"), "xéßü").as_deref(), Some("éßü"));
        assert_eq!(find(&re("a\\.b"), "axb a.b").as_deref(), Some("a.b"));
        assert_eq!(
            find(&re("\\x41\\x{1F600}\\u00e9"), "A😀é").as_deref(),
            Some("A😀é")
        );
        assert_eq!(find(&re("\\Qa.b*\\E+"), "a.b**").as_deref(), Some("a.b**"));
    }

    #[test]
    fn classes() {
        assert_eq!(all(&re("[a-c]+"), "xabcay bb"), ["abca", "bb"]);
        assert_eq!(all(&re("[^a-c\\s]+"), "abxyz cd"), ["xyz", "d"]);
        assert_eq!(all(&re("\\d+"), "a12b345"), ["12", "345"]);
        assert_eq!(all(&re("\\w+"), "héllo wörld_1 !"), ["héllo", "wörld_1"]);
        assert_eq!(
            all(&re("[[:upper:]][[:lower:]]+"), "Hello World x"),
            ["Hello", "World"]
        );
        assert_eq!(find(&re("[]a]+"), "x]a]").as_deref(), Some("]a]"));
        assert_eq!(find(&re("[a-]+"), "x-a-").as_deref(), Some("-a-"));
        assert_eq!(find(&re("[\\d.]+"), "v1.25").as_deref(), Some("1.25"));
        assert_eq!(find(&re("\\h+"), "a \t\nb").as_deref(), Some(" \t"));
        assert_eq!(find(&re("[^x]"), "\n").as_deref(), Some("\n"));
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(
            all(&re_ci("hello"), "Hello HELLO hello"),
            ["Hello", "HELLO", "hello"]
        );
        assert_eq!(find(&re_ci("ÄBC"), "xäbc").as_deref(), Some("äbc"));
        assert_eq!(find(&re_ci("[a-c]+"), "xABc").as_deref(), Some("ABc"));
        assert_eq!(
            find(&re("(?i)ab(?-i)C"), "ABC abc AbC").as_deref(),
            Some("ABC")
        );
        assert_eq!(find(&re("(?i:ab)c"), "ABc ABC").as_deref(), Some("ABc"));
        assert_eq!(find(&re("x(?i)y"), "xY Xy").as_deref(), Some("xY"));
    }

    #[test]
    fn anchors_are_per_line() {
        assert_eq!(all(&re("^\\w+"), "one two\nthree\n four"), ["one", "three"]);
        assert_eq!(all(&re("\\w+$"), "one two\nthree\nfour "), ["two", "three"]);
        assert_eq!(all(&re("(?-m)^\\w+"), "one\ntwo"), ["one"]);
        assert_eq!(all(&re("\\A\\w"), "ab\ncd"), ["a"]);
        assert_eq!(all(&re("\\w\\z"), "ab\ncd"), ["d"]);
        assert_eq!(all(&re("^$"), "a\n\nb\n"), ["", ""]);
    }

    #[test]
    fn word_boundaries() {
        assert_eq!(
            all(&re("\\bcat\\b"), "cat concat cat_ cat."),
            ["cat", "cat"]
        );
        assert_eq!(all(&re("\\Bcat"), "cat concat"), ["cat"]);
        assert_eq!(all(&re("\\<\\w"), "ab cd"), ["a", "c"]);
        assert_eq!(all(&re("\\w\\>"), "ab cd"), ["b", "d"]);
    }

    #[test]
    fn whole_word_option() {
        let o = Options {
            whole_word: true,
            literal: true,
            ..Options::default()
        };
        let r = Regex::new("foo", o).unwrap();
        assert_eq!(all(&r, "foo foobar barfoo (foo)"), ["foo", "foo"]);
        // A pattern that starts with punctuation is not held to word edges on that side.
        let r = Regex::new("-x", o).unwrap();
        assert_eq!(all(&r, "a-x -xy"), ["-x"]);
        // Backtracking finds the longer alternative that does fit.
        let r = Regex::new(
            "foo|foobar",
            Options {
                whole_word: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(all(&r, "foobar"), ["foobar"]);
    }

    #[test]
    fn alternation_and_groups() {
        assert_eq!(all(&re("cat|dog"), "dog cat cow"), ["dog", "cat"]);
        assert_eq!(
            group(&re("(\\w+)@(\\w+)"), "mail me@host now", 2).as_deref(),
            Some("host")
        );
        assert_eq!(group(&re("(?:a|b)+(c)"), "abbac", 1).as_deref(), Some("c"));
        assert_eq!(
            group(&re("(?<year>\\d{4})-(?<m>\\d\\d)"), "on 2024-05", 2).as_deref(),
            Some("05")
        );
        assert_eq!(re("(?<year>\\d{4})-(?<m>\\d\\d)").group_index("m"), Some(2));
        assert_eq!(group(&re("(a)|(b)"), "b", 1), None);
        assert_eq!(find(&re("a|"), "b").as_deref(), Some(""));
        // A '\n' allowed by one branch is not hidden by '.' in another.
        assert_eq!(find(&re("\n|.x"), "a\n").as_deref(), Some("\n"));
    }

    #[test]
    fn quantifiers() {
        assert_eq!(find(&re("a{2,3}"), "a aa aaaa").as_deref(), Some("aa"));
        assert_eq!(all(&re("a{2,3}"), "aaaaaaa"), ["aaa", "aaa"]);
        assert_eq!(find(&re("a{2}"), "aaa").as_deref(), Some("aa"));
        assert_eq!(find(&re("a{2,}"), "aaaa").as_deref(), Some("aaaa"));
        assert_eq!(find(&re("a{,2}b"), "aaab").as_deref(), Some("aab"));
        assert_eq!(find(&re("x{a}"), "x{a}").as_deref(), Some("x{a}"));
        assert_eq!(find(&re("<.+>"), "<a><b>").as_deref(), Some("<a><b>"));
        assert_eq!(find(&re("<.+?>"), "<a><b>").as_deref(), Some("<a>"));
        assert_eq!(find(&re("a??b"), "ab").as_deref(), Some("ab"));
        assert_eq!(find(&re("a{1,3}?"), "aaa").as_deref(), Some("a"));
        // Possessive: never gives back, so the trailing 'a' cannot match.
        assert_eq!(find(&re("a*+a"), "aaa"), None);
        assert_eq!(find(&re("(?>a*)b"), "aab").as_deref(), Some("aab"));
        // Empty iterations end instead of looping forever.
        assert_eq!(find(&re("(a*)*b"), "aab").as_deref(), Some("aab"));
        assert_eq!(find(&re("(a?)*?b"), "b").as_deref(), Some("b"));
        assert_eq!(find(&re("(|a)+b"), "ab").as_deref(), Some("ab"));
    }

    #[test]
    fn backreferences() {
        assert_eq!(find(&re("(\\w)\\1"), "abccd").as_deref(), Some("cc"));
        assert_eq!(
            find(&re("(?<q>['\"]).*?\\k<q>"), "say \"hi\" 'yo'").as_deref(),
            Some("\"hi\"")
        );
        assert_eq!(find(&re_ci("(a)\\1"), "aA").as_deref(), Some("aA"));
        assert_eq!(find(&re("(a)|\\1b"), "b"), None);
    }

    #[test]
    fn lookaround() {
        assert_eq!(all(&re("\\w+(?=\\()"), "f(x) g y(z)"), ["f", "y"]);
        assert_eq!(all(&re("\\b\\w+\\b(?!\\()"), "f(x) g"), ["x", "g"]);
        assert_eq!(all(&re("(?<=\\$)\\d+"), "$10 20 $30"), ["10", "30"]);
        assert_eq!(all(&re("(?<!\\$)\\b\\d+"), "$10 20 $30"), ["20"]);
        // Variable-length lookbehind.
        assert_eq!(all(&re("(?<=ab+)c"), "abbbc ac bc"), ["c"]);
        assert_eq!(find(&re("(?<=é)x"), "éx").as_deref(), Some("x"));
        // Groups set inside a positive lookahead are kept.
        assert_eq!(
            group(&re("(?=(\\w+))\\w"), "abc", 1).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn line_breaks() {
        assert_eq!(find(&re("a\\r\\nb"), "a\nb").as_deref(), Some("a\nb"));
        assert_eq!(find(&re("a\\Rb"), "a\r\nb").as_deref(), Some("a\r\nb"));
        assert_eq!(all(&re("\\n"), "a\nb\n"), ["\n", "\n"]);
        assert_eq!(find(&re("a\\s+b"), "a \n b").as_deref(), Some("a \n b"));
    }

    #[test]
    fn extended_mode_and_comments() {
        assert_eq!(
            find(&re("(?x) a  b # comment\n c"), "abc").as_deref(),
            Some("abc")
        );
        assert_eq!(find(&re("a(?#note)b"), "ab").as_deref(), Some("ab"));
    }

    #[test]
    fn errors_point_at_the_problem() {
        assert_eq!(err("(ab"), "unclosed (");
        assert_eq!(err("ab)"), "unmatched )");
        assert_eq!(err("[ab"), "unclosed [");
        assert_eq!(err("*a"), "nothing to repeat");
        assert_eq!(err("a**"), "nested quantifier");
        assert_eq!(err("a{3,2}"), "repetition range is backwards");
        assert_eq!(err("[z-a]"), "range out of order");
        assert_eq!(err("\\q"), "unknown escape \\q");
        assert_eq!(err("(a)\\2"), "no group 2 to refer to");
        assert_eq!(err("\\k<nope>"), "unknown group name");
        assert_eq!(err("a\\"), "pattern ends with \\");
        assert_eq!(Regex::new("ab(", Options::default()).unwrap_err().pos, 2);
        assert_eq!(err("(?<a>x)(?<a>y)"), "duplicate group name");
        assert_eq!(err("a{1001}"), "repetition count is over 1000");
    }

    #[test]
    fn literal_option_ignores_syntax() {
        let r = Regex::new(
            "a.b(",
            Options {
                literal: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(all(&r, "axb( a.b("), ["a.b("]);
    }

    #[test]
    fn catastrophic_patterns_hit_the_budget() {
        let r = re("(a*)*b");
        let t = "a".repeat(40);
        let mut caps = r.captures();
        let mut b = 100_000;
        // Either it finishes (thanks to the empty-loop check) or it reports the budget.
        let res = r.find_at(t.as_bytes(), 0, t.len(), &mut caps, &mut b);
        assert!(matches!(res, Ok(None) | Err(TooComplex)));
        let r = re("(x+x+)+y");
        let t = "x".repeat(30);
        let mut b = 100_000;
        assert_eq!(
            r.find_at(t.as_bytes(), 0, t.len(), &mut caps, &mut b),
            Err(TooComplex)
        );
    }

    #[test]
    fn find_respects_range_and_backward_search() {
        let r = re("ab");
        let t = b"ab ab ab";
        let mut caps = r.captures();
        let mut b = BUDGET;
        assert_eq!(
            r.find_at(t, 1, t.len(), &mut caps, &mut b).unwrap(),
            Some((3, 5))
        );
        assert_eq!(r.find_at(t, 1, 2, &mut caps, &mut b).unwrap(), None);
        assert_eq!(
            r.rfind_before(t, 0, 6, &mut caps, &mut b).unwrap(),
            Some((3, 5))
        );
        assert_eq!(
            r.rfind_before(t, 0, 3, &mut caps, &mut b).unwrap(),
            Some((0, 2))
        );
        assert_eq!(r.rfind_before(t, 0, 0, &mut caps, &mut b).unwrap(), None);
        // Line-start hint still finds later lines, and stops at the limit.
        let r = re("^b");
        let t = b"a\nb\nb";
        assert_eq!(
            r.find_at(t, 0, t.len(), &mut caps, &mut b).unwrap(),
            Some((2, 3))
        );
        assert_eq!(r.find_at(t, 0, 1, &mut caps, &mut b).unwrap(), None);
    }

    #[test]
    fn hints_are_chosen() {
        assert!(matches!(re("abc").hint, Hint::Prefix(..)));
        assert!(matches!(re_ci("abc").hint, Hint::Prefix(_, true)));
        assert!(matches!(re("^ab").hint, Hint::Prefix(..)));
        assert!(matches!(re("^\\d").hint, Hint::LineStart));
        assert!(matches!(re("\\Aa").hint, Hint::TextStart));
        assert!(matches!(re("[ab]c").hint, Hint::FirstByte(_)));
        assert!(matches!(re("a*").hint, Hint::Anywhere));
    }

    #[test]
    fn templates() {
        assert_eq!(
            replace("(\\w+)=(\\w+)", "$2=$1", "a=b, cc=dd"),
            "b=a, dd=cc"
        );
        assert_eq!(replace("(\\w+)=(\\w+)", "\\2:\\1", "a=b"), "b:a");
        assert_eq!(
            replace("(?<k>\\w+)=(?<v>\\w+)", "${v}<-${k}", "a=b"),
            "b<-a"
        );
        assert_eq!(replace("o", "[$&$0]", "foo"), "f[oo][oo]");
        assert_eq!(replace("x", "$$1", "x"), "$1");
        assert_eq!(replace("(\\w+)", "\\U$1\\E!", "hey you"), "HEY! YOU!");
        assert_eq!(
            replace("(\\w)(\\w*)", "\\u$1\\L$2", "hELLO wORLD"),
            "Hello World"
        );
        assert_eq!(replace("(\\w+)", "\\l$1", "ABC"), "aBC");
        assert_eq!(replace(",", "\\n\\t", "a,b"), "a\n\tb");
        assert_eq!(replace(",", "\\r\\n", "a,b"), "a\nb");
        assert_eq!(replace("a", "\\x41\\x{263A}", "a"), "A☺");
        assert_eq!(replace("a", "\\\\", "a"), "\\");
        // Missing groups expand to nothing.
        assert_eq!(replace("(a)|(b)", "[$2]", "ab"), "[][b]");
        let r = re("(a)");
        assert_eq!(
            Template::regex("$2", &r).unwrap_err().msg,
            "no group 2 in the pattern"
        );
        assert_eq!(
            Template::regex("${x}", &r).unwrap_err().msg,
            "no group named x"
        );
        // Escape-only templates leave '$' and '\1' alone.
        let t = Template::escapes("$1\\t\\1").unwrap();
        let mut out = Vec::new();
        t.expand(b"", &r.captures(), &mut out);
        assert_eq!(out, b"$1\t1");
    }
}
