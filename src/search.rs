//! Find and replace: the query typed in the Find/Replace dialog, compiled into a matcher,
//! and the scans built on it (find next / previous with wrap and scope, count, replace all,
//! and the hits for the live preview).
//!
//! Modes: plain text, escape sequences (`\n`, `\t`, `\xHH`), regular expressions (see
//! `regex`) and number ranges (`10-20`, `1.5..3`, `>=100`). Options: match case, whole word,
//! fuzzy (plain and escape modes: about one typo per four chars) and preserve case (the
//! replacement follows the capitalisation of the text it replaces).

use crate::regex::{self, Captures, Regex, Template, TooComplex};

/// How the Find text is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Plain,
    Escapes,
    Regex,
    NumberRange,
}

/// Everything the user set up in the dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub find: String,
    pub replace: String,
    pub mode: Mode,
    pub match_case: bool,
    pub whole_word: bool,
    /// Plain and escape modes only.
    pub fuzzy: bool,
    pub preserve_case: bool,
}

/// The box an error belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Find,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError {
    pub field: Field,
    /// Char index in that box.
    pub pos: usize,
    pub msg: String,
}

impl QueryError {
    fn new(field: Field, e: regex::Error) -> QueryError {
        QueryError {
            field,
            pos: e.pos,
            msg: e.msg,
        }
    }
}

/// An optional byte range to search in ("In the Selection Only"); None is the whole text.
pub type Scope = Option<(usize, usize)>;

/// Step budgets. A step is one instruction of the regex VM, one number token or one DP cell
/// of the fuzzy matcher; roughly a few nanoseconds each.
pub const FIND_BUDGET: u64 = 200_000_000;
pub const PREVIEW_BUDGET: u64 = 4_000_000;

/// A compiled query.
pub struct Searcher {
    matcher: Matcher,
    template: Template,
    replace_error: Option<QueryError>,
    preserve_case: bool,
    caps: Captures,
    /// The last match, which `expand` replaces.
    last: (usize, usize),
}

enum Matcher {
    Regex(Regex),
    Number(NumRange),
    Fuzzy(Fuzzy),
}

impl Searcher {
    /// Compiles `q`. An empty or invalid Find text is an error; an invalid Replace text is
    /// kept in `replace_error` so searching still works.
    pub fn new(q: &Query) -> Result<Searcher, QueryError> {
        if q.find.is_empty() {
            return Err(QueryError {
                field: Field::Find,
                pos: 0,
                msg: "Nothing to find".to_string(),
            });
        }
        let find_err = |e| QueryError::new(Field::Find, e);
        let literal = match q.mode {
            Mode::Plain => Some(q.find.clone()),
            Mode::Escapes => Some(Template::escapes(&q.find).map_err(find_err)?.text()),
            _ => None,
        };
        let matcher = match (q.mode, literal) {
            (Mode::NumberRange, _) => {
                Matcher::Number(NumRange::parse(&q.find).map_err(|msg| QueryError {
                    field: Field::Find,
                    pos: 0,
                    msg,
                })?)
            }
            (_, Some(text)) if q.fuzzy => {
                Matcher::Fuzzy(Fuzzy::new(&text, q.match_case, q.whole_word))
            }
            (_, lit) => {
                let opts = regex::Options {
                    ignore_case: !q.match_case,
                    whole_word: q.whole_word,
                    literal: lit.is_some(),
                };
                let pattern = lit.as_deref().unwrap_or(&q.find);
                if pattern.is_empty() {
                    return Err(QueryError {
                        field: Field::Find,
                        pos: 0,
                        msg: "Nothing to find".to_string(),
                    });
                }
                Matcher::Regex(Regex::new(pattern, opts).map_err(find_err)?)
            }
        };
        let template = match (&matcher, q.mode) {
            (Matcher::Regex(re), Mode::Regex) => Template::regex(&q.replace, re),
            (_, Mode::Escapes | Mode::Regex) => Template::escapes(&q.replace),
            _ => Ok(Template::literal(&q.replace)),
        };
        let (template, replace_error) = match template {
            Ok(t) => (t, None),
            Err(e) => (
                Template::default(),
                Some(QueryError::new(Field::Replace, e)),
            ),
        };
        Ok(Searcher {
            matcher,
            template,
            replace_error,
            preserve_case: q.preserve_case,
            caps: Captures::default(),
            last: (0, 0),
        })
    }

    /// The problem with the Replace text, if any. Replacing is refused while there is one.
    pub fn replace_error(&self) -> Option<&QueryError> {
        self.replace_error.as_ref()
    }

    /// The compiled regex in regex mode (for group names in the dialog's completions).
    pub fn regex(&self) -> Option<&Regex> {
        match &self.matcher {
            Matcher::Regex(r) => Some(r),
            _ => None,
        }
    }

    /// First match that starts in `from..=limit`.
    pub fn find(
        &mut self,
        t: &[u8],
        from: usize,
        limit: usize,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, TooComplex> {
        let r = match &self.matcher {
            Matcher::Regex(re) => re.find_at(t, from, limit, &mut self.caps, budget)?,
            Matcher::Number(n) => n.find(t, from, limit, budget)?,
            Matcher::Fuzzy(f) => f.find(t, from, limit, budget)?,
        };
        if let Some(m) = r {
            self.last = m;
        }
        Ok(r)
    }

    /// Last match that starts in `lo..before`.
    pub fn rfind(
        &mut self,
        t: &[u8],
        lo: usize,
        before: usize,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, TooComplex> {
        if let Matcher::Regex(re) = &self.matcher {
            let r = re.rfind_before(t, lo, before, &mut self.caps, budget)?;
            if let Some(m) = r {
                self.last = m;
            }
            return Ok(r);
        }
        // Number and fuzzy matches are scanned forward; the last one before `before` wins.
        let mut at = lo;
        let mut best = None;
        while at < before {
            match self.find(t, at, before - 1, budget)? {
                Some(m) => {
                    best = Some(m);
                    at = next_from(t, m);
                }
                None => break,
            }
        }
        if let Some(m) = best {
            self.last = m;
        }
        Ok(best)
    }

    /// The match that starts exactly at `s` and ends at `e`, if that is one.
    pub fn matches_exactly(
        &mut self,
        t: &[u8],
        s: usize,
        e: usize,
        budget: &mut u64,
    ) -> Result<bool, TooComplex> {
        Ok(self.find(t, s, s, budget)? == Some((s, e)))
    }

    /// Appends the replacement for the last match (in text `t`) to `out`.
    pub fn expand(&self, t: &[u8], out: &mut Vec<u8>) {
        let (s, e) = self.last;
        let from = out.len();
        self.template.expand(t, &self.caps, out);
        if self.preserve_case {
            let matched = String::from_utf8_lossy(&t[s..e.min(t.len())]);
            let rep = String::from_utf8_lossy(&out[from..]).into_owned();
            let fixed = preserve_case(&matched, &rep);
            out.truncate(from);
            out.extend_from_slice(fixed.as_bytes());
        }
    }
}

/// Where to look after the match `m`: its end, or one char on for an empty match.
pub fn next_from(t: &[u8], m: (usize, usize)) -> usize {
    if m.1 > m.0 {
        m.1
    } else {
        m.1 + t.get(m.1).map_or(1, |&b| utf8_len(b))
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

/// `rep` re-cased like `matched`: ALL CAPS, Capitalised or all lower.
pub fn preserve_case(matched: &str, rep: &str) -> String {
    let letters: Vec<char> = matched.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return rep.to_string();
    }
    let upper = letters.iter().all(|c| c.is_uppercase());
    let lower = letters.iter().all(|c| c.is_lowercase());
    if upper && letters.len() > 1 {
        rep.to_uppercase()
    } else if lower {
        rep.to_lowercase()
    } else if letters[0].is_uppercase() {
        let mut out = String::with_capacity(rep.len());
        let mut chars = rep.chars();
        if let Some(f) = chars.next() {
            out.extend(f.to_uppercase());
        }
        out.extend(chars);
        out
    } else {
        rep.to_string()
    }
}

// ---------------------------------------------------------------------------
// Scans over a scope
// ---------------------------------------------------------------------------

/// A match found by `find_in`, in buffer offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    /// The search went past the end (or start) of the scope and came round.
    pub wrapped: bool,
}

/// Find next / previous inside `scope` (a byte range of `t`). Forward search starts at
/// `from` (an empty match exactly at `skip_empty` is passed over); backward search finds
/// the last match starting before `from`. With `wrap`, the rest of the scope is searched
/// from the other end.
pub fn find_in(
    s: &mut Searcher,
    t: &[u8],
    scope: (usize, usize),
    from: usize,
    forward: bool,
    wrap: bool,
    budget: &mut u64,
) -> Result<Option<Found>, TooComplex> {
    let (lo, hi) = scope;
    let sub = &t[lo..hi];
    let from = from.clamp(lo, hi) - lo;
    let found = |m: (usize, usize), wrapped| Found {
        start: m.0 + lo,
        end: m.1 + lo,
        wrapped,
    };
    if forward {
        let mut at = from;
        // An empty match where the cursor already is would be found forever.
        let first = loop {
            match s.find(sub, at, sub.len(), budget)? {
                Some(m) if m.0 == m.1 && m.0 == from => at = next_from(sub, m),
                other => break other,
            }
            if at > sub.len() {
                break None;
            }
        };
        if let Some(m) = first {
            return Ok(Some(found(m, false)));
        }
        if wrap && from > 0 {
            if let Some(m) = s.find(sub, 0, from - 1, budget)? {
                return Ok(Some(found(m, true)));
            }
        }
    } else {
        if let Some(m) = s.rfind(sub, 0, from, budget)? {
            return Ok(Some(found(m, false)));
        }
        if wrap {
            if let Some(m) = s.rfind(sub, from, sub.len() + 1, budget)? {
                return Ok(Some(found(m, true)));
            }
        }
    }
    Ok(None)
}

/// Number of matches in `scope`.
pub fn count(
    s: &mut Searcher,
    t: &[u8],
    scope: (usize, usize),
    budget: &mut u64,
) -> Result<usize, TooComplex> {
    let sub = &t[scope.0..scope.1];
    let mut n = 0;
    let mut at = 0;
    while at <= sub.len() {
        match s.find(sub, at, sub.len(), budget)? {
            Some(m) => {
                n += 1;
                at = next_from(sub, m);
            }
            None => break,
        }
    }
    Ok(n)
}

/// The result of replacing every match in a scope: bytes `start..end` of the text become
/// `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replaced {
    pub start: usize,
    pub end: usize,
    pub text: Vec<u8>,
    pub count: usize,
}

/// Replaces every match in `scope`. Only the span from the first match to the last one is
/// rebuilt, so the edit stays small for a few matches in a big file. Each position in
/// `track` is moved to where it ends up: shifted past earlier replacements, or to the end
/// of the replacement when it was inside a match.
pub fn replace_all(
    s: &mut Searcher,
    t: &[u8],
    scope: (usize, usize),
    track: &mut [usize],
    budget: &mut u64,
) -> Result<Option<Replaced>, TooComplex> {
    let (lo, hi) = scope;
    let sub = &t[lo..hi];
    let mut out = Vec::new();
    let mut first = None;
    let mut last = 0;
    let mut count = 0;
    let mut at = 0;
    let mut delta: isize = 0;
    let mut mapped: Vec<Option<usize>> = vec![None; track.len()];
    while at <= sub.len() {
        let Some(m) = s.find(sub, at, sub.len(), budget)? else {
            break;
        };
        if first.is_none() {
            first = Some(m.0);
        } else {
            out.extend_from_slice(&sub[last..m.0]);
        }
        let before = out.len();
        s.expand(sub, &mut out);
        let rep_len = out.len() - before;
        let (ms, me) = (m.0 + lo, m.1 + lo);
        for (k, &p) in track.iter().enumerate() {
            if mapped[k].is_some() {
                continue;
            }
            if p <= ms {
                mapped[k] = Some((p as isize + delta) as usize);
            } else if p < me {
                mapped[k] = Some((ms as isize + delta) as usize + rep_len);
            }
        }
        delta += rep_len as isize - (m.1 - m.0) as isize;
        last = m.1;
        count += 1;
        at = next_from(sub, m);
    }
    for (k, p) in track.iter_mut().enumerate() {
        *p = mapped[k].unwrap_or_else(|| (*p as isize + delta).max(0) as usize);
    }
    Ok(first.map(|f| Replaced {
        start: f + lo,
        end: last + lo,
        text: out,
        count,
    }))
}

/// One match for the live preview, with its replacement when asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub start: usize,
    pub end: usize,
    pub rep: Option<Vec<u8>>,
}

/// Matches that start in `from..=limit` (at most `max`), for the live preview.
pub fn hits(
    s: &mut Searcher,
    t: &[u8],
    from: usize,
    limit: usize,
    max: usize,
    with_rep: bool,
    budget: &mut u64,
) -> Result<Vec<Hit>, TooComplex> {
    let mut out = Vec::new();
    let mut at = from;
    while at <= limit && out.len() < max {
        let Some(m) = s.find(t, at, limit, budget)? else {
            break;
        };
        let rep = with_rep.then(|| {
            let mut r = Vec::new();
            s.expand(t, &mut r);
            r
        });
        out.push(Hit {
            start: m.0,
            end: m.1,
            rep,
        });
        at = next_from(t, m);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Number ranges
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct NumRange {
    lo: Option<(f64, bool)>,
    hi: Option<(f64, bool)>,
}

const RANGE_HELP: &str = "Enter a range like 10-20, 1.5..3, >=100, <0 or 42";

impl NumRange {
    fn num(s: &str) -> Result<f64, String> {
        let s = s.trim();
        s.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && !s.is_empty())
            .ok_or_else(|| RANGE_HELP.to_string())
    }

    fn parse(s: &str) -> Result<NumRange, String> {
        let s = s.trim();
        for (op, lo, incl) in [
            (">=", true, true),
            ("<=", false, true),
            (">", true, false),
            ("<", false, false),
        ] {
            if let Some(rest) = s.strip_prefix(op) {
                let v = Some((NumRange::num(rest)?, incl));
                return Ok(if lo {
                    NumRange { lo: v, hi: None }
                } else {
                    NumRange { lo: None, hi: v }
                });
            }
        }
        if let Some(rest) = s.strip_prefix('=') {
            let v = NumRange::num(rest)?;
            return Ok(NumRange {
                lo: Some((v, true)),
                hi: Some((v, true)),
            });
        }
        let (a, b) = if let Some(i) = s.find("..") {
            (&s[..i], &s[i + 2..])
        } else if let Some(i) = s
            .char_indices()
            .skip(1)
            .find(|&(i, c)| c == '-' && s[..i].trim_end().ends_with(|p: char| p.is_ascii_digit()))
            .map(|(i, _)| i)
        {
            (&s[..i], &s[i + 1..])
        } else {
            let v = NumRange::num(s)?;
            return Ok(NumRange {
                lo: Some((v, true)),
                hi: Some((v, true)),
            });
        };
        let lo = if a.trim().is_empty() {
            None
        } else {
            Some((NumRange::num(a)?, true))
        };
        let hi = if b.trim().is_empty() {
            None
        } else {
            Some((NumRange::num(b)?, true))
        };
        if lo.is_none() && hi.is_none() {
            return Err(RANGE_HELP.to_string());
        }
        if let (Some((x, _)), Some((y, _))) = (lo, hi) {
            if x > y {
                return Ok(NumRange { lo: hi, hi: lo });
            }
        }
        Ok(NumRange { lo, hi })
    }

    fn contains(&self, v: f64) -> bool {
        let lo_ok = self
            .lo
            .is_none_or(|(x, incl)| if incl { v >= x } else { v > x });
        let hi_ok = self
            .hi
            .is_none_or(|(x, incl)| if incl { v <= x } else { v < x });
        lo_ok && hi_ok
    }

    /// The number token starting at `i`, if one starts there: an optional '-', digits, and
    /// an optional fraction. A token never starts inside another number.
    fn token(t: &[u8], i: usize) -> Option<usize> {
        let digit = |k: usize| t.get(k).is_some_and(|b| b.is_ascii_digit());
        let prev = |k: usize| if k == 0 { None } else { Some(t[k - 1]) };
        let mut j = i;
        if t.get(i) == Some(&b'-') {
            if !digit(i + 1) || prev(i).is_some_and(|p| p.is_ascii_alphanumeric() || p == b'.') {
                return None;
            }
            j += 1;
        } else if !digit(i) || prev(i).is_some_and(|p| p.is_ascii_digit() || p == b'.') {
            return None;
        }
        while digit(j) {
            j += 1;
        }
        if t.get(j) == Some(&b'.') && digit(j + 1) {
            j += 1;
            while digit(j) {
                j += 1;
            }
        }
        Some(j)
    }

    fn find(
        &self,
        t: &[u8],
        from: usize,
        limit: usize,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, TooComplex> {
        let mut i = from;
        let limit = limit.min(t.len());
        while i <= limit && i < t.len() {
            if *budget == 0 {
                return Err(TooComplex);
            }
            *budget -= 1;
            match NumRange::token(t, i) {
                Some(e) => {
                    let v = std::str::from_utf8(&t[i..e])
                        .ok()
                        .and_then(|s| s.parse::<f64>().ok());
                    if v.is_some_and(|v| self.contains(v)) {
                        return Ok(Some((i, e)));
                    }
                    i = e;
                }
                None => i += 1,
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Fuzzy matching
// ---------------------------------------------------------------------------

/// Approximate search: the best substring within `k` edits of the needle. An edit is an
/// inserted, deleted or changed char, or two neighbours swapped ("recieve"). Runs an
/// edit-distance column over the text (Sellers' algorithm, with the swap rule).
#[derive(Clone, Debug)]
struct Fuzzy {
    needle: Vec<char>,
    rev: Vec<char>,
    ci: bool,
    whole_word: bool,
    k: usize,
}

/// Edit-distance columns. `prev` is the latest one after each step; row i is the distance
/// of needle[..i] to the text read so far (ending here).
#[derive(Clone)]
struct Dp {
    prev2: Vec<usize>,
    prev: Vec<usize>,
    cur: Vec<usize>,
    last: Option<char>,
    /// Text chars cost one each before the needle starts (anchored), or nothing (free start).
    anchored: bool,
}

impl Dp {
    fn new(m: usize, anchored: bool) -> Dp {
        let col: Vec<usize> = (0..=m).collect();
        Dp {
            prev2: col.clone(),
            prev: col,
            cur: vec![0; m + 1],
            last: None,
            anchored,
        }
    }

    fn step(&mut self, needle: &[char], c: char) {
        self.cur[0] = if self.anchored { self.prev[0] + 1 } else { 0 };
        for i in 1..=needle.len() {
            let cost = usize::from(needle[i - 1] != c);
            let mut v = (self.prev[i] + 1)
                .min(self.cur[i - 1] + 1)
                .min(self.prev[i - 1] + cost);
            if i > 1 && self.last == Some(needle[i - 1]) && c == needle[i - 2] && needle[i - 1] != c
            {
                v = v.min(self.prev2[i - 2] + 1);
            }
            self.cur[i] = v;
        }
        std::mem::swap(&mut self.prev2, &mut self.prev);
        std::mem::swap(&mut self.prev, &mut self.cur);
        self.last = Some(c);
    }

    fn dist(&self) -> usize {
        self.prev[self.prev.len() - 1]
    }
}

impl Fuzzy {
    fn new(text: &str, match_case: bool, whole_word: bool) -> Fuzzy {
        let ci = !match_case;
        let needle: Vec<char> = text
            .chars()
            .map(|c| if ci { regex::fold(c) } else { c })
            .collect();
        let rev = needle.iter().rev().copied().collect();
        // One edit per four chars, so short words still have to match exactly.
        let k = (needle.len() / 4).min(8);
        Fuzzy {
            needle,
            rev,
            ci,
            whole_word,
            k,
        }
    }

    fn char_at(&self, t: &[u8], p: usize) -> Option<(char, usize)> {
        let (c, n) = regex::decode(t, p)?;
        Some((if self.ci { regex::fold(c) } else { c }, n))
    }

    /// The best start for a match ending at `end`: an anchored edit-distance run backwards
    /// from `end`, over at most needle+k chars and never before `floor`.
    fn start_of(&self, t: &[u8], floor: usize, end: usize) -> usize {
        let m = self.needle.len();
        let mut dp = Dp::new(m, true);
        let mut best = (usize::MAX, end);
        let mut p = end;
        for _ in 0..m + self.k {
            if p <= floor {
                break;
            }
            let mut s = p - 1;
            while s > floor && t[s] & 0xC0 == 0x80 {
                s -= 1;
            }
            let Some((c, _)) = self.char_at(t, s) else {
                break;
            };
            dp.step(&self.rev, c);
            p = s;
            if dp.dist() < best.0 {
                best = (dp.dist(), p);
            }
        }
        best.1
    }

    fn word_ok(&self, t: &[u8], s: usize, e: usize) -> bool {
        if !self.whole_word {
            return true;
        }
        let before = regex::decode_before(t, s);
        let after = regex::decode(t, e).map(|(c, _)| c);
        !before.is_some_and(regex::is_word) && !after.is_some_and(regex::is_word)
    }

    fn find(
        &self,
        t: &[u8],
        from: usize,
        limit: usize,
        budget: &mut u64,
    ) -> Result<Option<(usize, usize)>, TooComplex> {
        let m = self.needle.len();
        if m == 0 {
            return Ok(None);
        }
        let limit = limit.min(t.len());
        // A match that starts by `limit` ends within this many bytes of it.
        let stop = limit.saturating_add(4 * (m + self.k)).min(t.len());
        let mut dp = Dp::new(m, false);
        let mut p = from;
        while p < stop {
            let Some((c, n)) = self.char_at(t, p) else {
                break;
            };
            let cost = m as u64;
            if *budget < cost {
                return Err(TooComplex);
            }
            *budget -= cost;
            dp.step(&self.needle, c);
            p += n;
            if dp.dist() > self.k {
                continue;
            }
            // Extend while the distance keeps dropping: "hell" -> "hello".
            let mut best = (dp.dist(), p);
            let mut look = dp.clone();
            let mut q = p;
            while let Some((c2, n2)) = self.char_at(t, q) {
                look.step(&self.needle, c2);
                q += n2;
                if look.dist() < best.0 {
                    best = (look.dist(), q);
                } else {
                    break;
                }
            }
            let end = best.1;
            let start = self.start_of(t, from, end);
            if start > limit {
                return Ok(None);
            }
            if self.word_ok(t, start, end) {
                return Ok(Some((start, end)));
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Regex help: menu tokens and completions
// ---------------------------------------------------------------------------

/// A token the dialog can insert. `back` is how many chars the caret moves back from the
/// end afterwards (to land inside brackets).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub text: &'static str,
    pub back: usize,
    pub label: &'static str,
}

const fn tk(text: &'static str, back: usize, label: &'static str) -> Token {
    Token { text, back, label }
}

/// Find-box tokens for the ">" menu, in separated groups.
pub const FIND_TOKENS: &[&[Token]] = &[
    &[
        tk(".", 0, "Any character"),
        tk("\\d", 0, "Digit"),
        tk("\\w", 0, "Word character"),
        tk("\\s", 0, "Whitespace"),
        tk("\\h", 0, "Space or tab"),
        tk("[]", 1, "One of these characters"),
        tk("[^]", 1, "None of these characters"),
    ],
    &[
        tk("^", 0, "Start of line"),
        tk("$", 0, "End of line"),
        tk("\\b", 0, "Word boundary"),
        tk("\\<", 0, "Start of word"),
        tk("\\>", 0, "End of word"),
    ],
    &[
        tk("*", 0, "0 or more times"),
        tk("+", 0, "1 or more times"),
        tk("?", 0, "0 or 1 time"),
        tk("{,}", 2, "Between n and m times"),
        tk("*?", 0, "0 or more (lazy)"),
    ],
    &[
        tk("()", 1, "Group"),
        tk("(?:)", 1, "Group without capture"),
        tk("(?<>)", 2, "Named group"),
        tk("|", 0, "Or"),
        tk("\\1", 0, "Text of group 1"),
    ],
    &[
        tk("(?=)", 1, "Followed by"),
        tk("(?!)", 1, "Not followed by"),
        tk("(?<=)", 1, "Preceded by"),
        tk("(?<!)", 1, "Not preceded by"),
    ],
    &[
        tk("\\n", 0, "Line break"),
        tk("\\t", 0, "Tab"),
        tk("\\x{}", 1, "Character by hex code"),
        tk("(?i)", 0, "Ignore case from here"),
    ],
];

/// Replace-box tokens for the ">" menu.
pub const REPLACE_TOKENS: &[&[Token]] = &[
    &[
        tk("$0", 0, "Whole match"),
        tk("$1", 0, "Group 1"),
        tk("${}", 1, "Named group"),
    ],
    &[
        tk("\\U", 0, "Uppercase from here"),
        tk("\\L", 0, "Lowercase from here"),
        tk("\\E", 0, "End case change"),
        tk("\\u", 0, "Uppercase next character"),
        tk("\\l", 0, "Lowercase next character"),
    ],
    &[
        tk("\\n", 0, "Line break"),
        tk("\\t", 0, "Tab"),
        tk("$$", 0, "Dollar sign"),
    ],
];

const ESCAPES: &[(&str, &str)] = &[
    ("\\d", "digit"),
    ("\\D", "not a digit"),
    ("\\w", "word character"),
    ("\\W", "not a word character"),
    ("\\s", "whitespace"),
    ("\\S", "not whitespace"),
    ("\\h", "space or tab"),
    ("\\H", "not space or tab"),
    ("\\b", "word boundary"),
    ("\\B", "not a word boundary"),
    ("\\<", "start of word"),
    ("\\>", "end of word"),
    ("\\A", "start of text"),
    ("\\z", "end of text"),
    ("\\n", "line break"),
    ("\\R", "any line break"),
    ("\\t", "tab"),
    ("\\x{", "character by hex code"),
    ("\\Q", "quote until \\E"),
];

const GROUPS: &[(&str, &str)] = &[
    ("(?:", "group without capture"),
    ("(?<", "named group (?<name>...)"),
    ("(?=", "followed by"),
    ("(?!", "not followed by"),
    ("(?<=", "preceded by"),
    ("(?<!", "not preceded by"),
    ("(?>", "atomic group"),
    ("(?i)", "ignore case from here"),
    ("(?-i)", "match case from here"),
    ("(?s)", ". matches line breaks"),
    ("(?x)", "ignore spaces in the pattern"),
    ("(?#", "comment"),
];

const POSIX: &[&str] = &[
    "alpha", "digit", "alnum", "upper", "lower", "space", "blank", "punct", "xdigit", "word",
];

const CASES: &[(&str, &str)] = &[
    ("\\U", "uppercase from here"),
    ("\\L", "lowercase from here"),
    ("\\E", "end case change"),
    ("\\u", "uppercase next character"),
    ("\\l", "lowercase next character"),
    ("\\n", "line break"),
    ("\\t", "tab"),
];

/// One entry of the completion list: the full token, and what it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub insert: String,
    pub label: String,
}

fn item(insert: impl Into<String>, label: impl Into<String>) -> Completion {
    Completion {
        insert: insert.into(),
        label: label.into(),
    }
}

/// True when the backslash at byte `i` of `s` is not itself escaped.
fn live_backslash(s: &str, i: usize) -> bool {
    s.as_bytes()[..i]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
        % 2
        == 0
}

/// Completions for regex-mode text typed so far (`before` is the box text up to the caret).
/// Returns the byte offset where the token being typed starts, and the matching entries.
/// `groups` and `names` come from the Find pattern, for back-references and group names.
pub fn completions(
    before: &str,
    field: Field,
    groups: usize,
    names: &[String],
) -> Option<(usize, Vec<Completion>)> {
    let mut list = Vec::new();
    let start;
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    match field {
        Field::Find => {
            if let Some(i) = before
                .rfind("[:")
                .filter(|&i| before[i + 2..].chars().all(|c| c.is_ascii_lowercase()))
            {
                start = i;
                for p in POSIX {
                    list.push(item(format!("[:{p}:]"), format!("{p} characters")));
                }
            } else if let Some(i) = before.rfind("(?").filter(|&i| {
                before[i + 2..]
                    .chars()
                    .all(|c| matches!(c, '<' | '=' | '!' | ':' | '>' | '#' | '-' | 'i' | 's' | 'x'))
            }) {
                start = i;
                for (t, l) in GROUPS {
                    list.push(item(*t, *l));
                }
            } else {
                let i = before.rfind('\\')?;
                if !live_backslash(before, i)
                    || !before[i + 1..]
                        .chars()
                        .all(|c| word(c) || c == '<' || c == '{')
                {
                    return None;
                }
                start = i;
                for (t, l) in ESCAPES {
                    list.push(item(*t, *l));
                }
                for g in 1..=groups.min(9) {
                    list.push(item(format!("\\{g}"), format!("text of group {g}")));
                }
                for n in names {
                    list.push(item(format!("\\k<{n}>"), format!("text of group {n}")));
                }
            }
        }
        Field::Replace => {
            let d = before.rfind('$');
            let b = before.rfind('\\').filter(|&i| live_backslash(before, i));
            match (d, b) {
                (Some(i), b) if b.is_none_or(|b| b < i) => {
                    if !before[i + 1..]
                        .chars()
                        .all(|c| word(c) || c == '{' || c == '&')
                    {
                        return None;
                    }
                    start = i;
                    list.push(item("$0", "whole match"));
                    for g in 1..=groups.min(9) {
                        list.push(item(format!("${g}"), format!("group {g}")));
                    }
                    for n in names {
                        list.push(item(format!("${{{n}}}"), format!("group {n}")));
                    }
                    list.push(item("$$", "dollar sign"));
                }
                (_, Some(i)) => {
                    if !before[i + 1..].chars().all(word) {
                        return None;
                    }
                    start = i;
                    for g in 0..=groups.min(9) {
                        let what = if g == 0 {
                            "whole match".to_string()
                        } else {
                            format!("group {g}")
                        };
                        list.push(item(format!("\\{g}"), what));
                    }
                    for (t, l) in CASES {
                        list.push(item(*t, *l));
                    }
                }
                _ => return None,
            }
        }
    }
    let typed = &before[start..];
    list.retain(|c| c.insert.starts_with(typed) && c.insert != typed);
    (!list.is_empty()).then_some((start, list))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(find: &str, mode: Mode) -> Query {
        Query {
            find: find.to_string(),
            mode,
            ..Query::default()
        }
    }

    fn matches(query: &Query, t: &str) -> Vec<String> {
        let mut s = Searcher::new(query).unwrap();
        let mut b = FIND_BUDGET;
        hits(&mut s, t.as_bytes(), 0, t.len(), 1000, false, &mut b)
            .unwrap()
            .into_iter()
            .map(|h| t[h.start..h.end].to_string())
            .collect()
    }

    fn replaced(query: &Query, t: &str) -> String {
        let mut s = Searcher::new(query).unwrap();
        let mut b = FIND_BUDGET;
        match replace_all(&mut s, t.as_bytes(), (0, t.len()), &mut [], &mut b).unwrap() {
            Some(r) => {
                let mut out = t.as_bytes()[..r.start].to_vec();
                out.extend_from_slice(&r.text);
                out.extend_from_slice(&t.as_bytes()[r.end..]);
                String::from_utf8(out).unwrap()
            }
            None => t.to_string(),
        }
    }

    #[test]
    fn plain_mode_is_literal_and_case_insensitive_by_default() {
        assert_eq!(
            matches(&q("a.b", Mode::Plain), "a.b axb A.B"),
            ["a.b", "A.B"]
        );
        let mut cs = q("A.B", Mode::Plain);
        cs.match_case = true;
        assert_eq!(matches(&cs, "a.b A.B"), ["A.B"]);
        let mut ww = q("cat", Mode::Plain);
        ww.whole_word = true;
        assert_eq!(matches(&ww, "cat concat cats cat"), ["cat", "cat"]);
    }

    #[test]
    fn escape_mode_reads_escapes() {
        assert_eq!(matches(&q("a\\tb", Mode::Escapes), "a\tb a\\tb"), ["a\tb"]);
        assert_eq!(matches(&q("x\\r\\ny", Mode::Escapes), "x\ny"), ["x\ny"]);
        let mut r = q(",", Mode::Escapes);
        r.replace = "\\n".to_string();
        assert_eq!(replaced(&r, "a,b"), "a\nb");
    }

    #[test]
    fn regex_mode_with_groups() {
        let mut r = q("(\\w+)@(\\w+)", Mode::Regex);
        r.replace = "$2 at $1".to_string();
        assert_eq!(replaced(&r, "me@home, you@work"), "home at me, work at you");
        let mut bad = q("(", Mode::Regex);
        assert_eq!(
            Searcher::new(&bad).err().map(|e| e.field),
            Some(Field::Find)
        );
        bad.find = "a".to_string();
        bad.replace = "$3".to_string();
        let s = Searcher::new(&bad).unwrap();
        assert_eq!(s.replace_error().map(|e| e.field), Some(Field::Replace));
    }

    #[test]
    fn empty_find_is_an_error() {
        assert!(Searcher::new(&q("", Mode::Plain)).is_err());
        assert!(Searcher::new(&q("", Mode::Regex)).is_err());
    }

    #[test]
    fn number_ranges() {
        let t = "a 5 b 10, c 15.5 -3 x86 1.2.3 v-7 20";
        assert_eq!(matches(&q("5-15", Mode::NumberRange), t), ["5", "10", "7"]);
        assert_eq!(
            matches(&q("5..20", Mode::NumberRange), t),
            ["5", "10", "15.5", "7", "20"]
        );
        assert_eq!(matches(&q(">=86", Mode::NumberRange), t), ["86"]);
        assert_eq!(matches(&q("<0", Mode::NumberRange), t), ["-3"]);
        assert_eq!(matches(&q("1.2", Mode::NumberRange), t), ["1.2"]);
        assert_eq!(matches(&q("7", Mode::NumberRange), t), ["7"]);
        assert_eq!(matches(&q("20-5", Mode::NumberRange), "3 7 21"), ["7"]);
        assert!(Searcher::new(&q("abc", Mode::NumberRange)).is_err());
        assert!(Searcher::new(&q("..", Mode::NumberRange)).is_err());
    }

    #[test]
    fn fuzzy_tolerates_typos() {
        let mut f = q("receive", Mode::Plain);
        f.fuzzy = true;
        assert_eq!(
            matches(&f, "we recieve it; we receive it"),
            ["recieve", "receive"]
        );
        let mut f = q("hello", Mode::Plain);
        f.fuzzy = true;
        assert_eq!(matches(&f, "say helo and hello"), ["helo", "hello"]);
        // Short words must match exactly.
        let mut f = q("cat", Mode::Plain);
        f.fuzzy = true;
        assert_eq!(matches(&f, "cut cat"), ["cat"]);
    }

    #[test]
    fn preserve_case_follows_the_match() {
        assert_eq!(preserve_case("HELLO", "world"), "WORLD");
        assert_eq!(preserve_case("Hello", "world"), "World");
        assert_eq!(preserve_case("hello", "World"), "world");
        assert_eq!(preserve_case("hELLo", "world"), "world");
        assert_eq!(preserve_case("A", "bee"), "Bee");
        assert_eq!(preserve_case("42", "x"), "x");
        let mut r = q("cat", Mode::Plain);
        r.replace = "dog".to_string();
        r.preserve_case = true;
        assert_eq!(replaced(&r, "cat Cat CAT"), "dog Dog DOG");
    }

    #[test]
    fn find_in_wraps_and_respects_scope() {
        let t = b"ab ab ab";
        let mut s = Searcher::new(&q("ab", Mode::Plain)).unwrap();
        let mut b = FIND_BUDGET;
        let all = (0, t.len());
        let f = find_in(&mut s, t, all, 4, true, true, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!((f.start, f.wrapped), (6, false));
        let f = find_in(&mut s, t, all, 7, true, true, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!((f.start, f.wrapped), (0, true));
        assert_eq!(
            find_in(&mut s, t, all, 7, true, false, &mut b).unwrap(),
            None
        );
        let f = find_in(&mut s, t, all, 3, false, true, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!((f.start, f.wrapped), (0, false));
        let f = find_in(&mut s, t, all, 0, false, true, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!((f.start, f.wrapped), (6, true));
        // Scope: only the middle "ab".
        let f = find_in(&mut s, t, (2, 6), 0, true, true, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!((f.start, f.end), (3, 5));
        assert_eq!(count(&mut s, t, (2, 6), &mut b).unwrap(), 1);
        assert_eq!(count(&mut s, t, all, &mut b).unwrap(), 3);
    }

    #[test]
    fn empty_matches_do_not_stick() {
        let t = b"a\nb\nc";
        let mut s = Searcher::new(&q("^", Mode::Regex)).unwrap();
        let mut b = FIND_BUDGET;
        let all = (0, t.len());
        let f = find_in(&mut s, t, all, 0, true, true, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!(f.start, 2);
        assert_eq!(count(&mut s, t, all, &mut b).unwrap(), 3);
        let mut r = q("^", Mode::Regex);
        r.replace = "> ".to_string();
        assert_eq!(replaced(&r, "a\nb\nc"), "> a\n> b\n> c");
    }

    #[test]
    fn replace_all_rebuilds_only_the_matched_span() {
        let mut r = q("x", Mode::Plain);
        r.replace = "yy".to_string();
        let t = b"aaxbbxcc";
        let mut s = Searcher::new(&r).unwrap();
        let mut b = FIND_BUDGET;
        // Positions: before the first match, inside one, between, and after the last.
        let mut track = [1, 2, 3, 4, 7];
        let got = replace_all(&mut s, t, (0, t.len()), &mut track, &mut b)
            .unwrap()
            .unwrap();
        assert_eq!((got.start, got.end, got.count), (2, 6, 2));
        assert_eq!(got.text, b"yybbyy");
        assert_eq!(track, [1, 2, 4, 5, 9]);
        // Lookbehind sees the original text, not earlier replacements.
        let mut r = q("(?<=a)a", Mode::Regex);
        r.replace = "b".to_string();
        assert_eq!(replaced(&r, "aaaa"), "abbb");
    }

    #[test]
    fn completions_follow_what_is_typed() {
        let names = vec!["year".to_string()];
        let (at, list) = completions("ab\\", Field::Find, 2, &names).unwrap();
        assert_eq!(at, 2);
        assert!(list.iter().any(|c| c.insert == "\\d"));
        assert!(list.iter().any(|c| c.insert == "\\2"));
        assert!(list.iter().any(|c| c.insert == "\\k<year>"));
        let (_, list) = completions("\\d", Field::Find, 0, &[]).unwrap_or((0, Vec::new()));
        assert!(list.is_empty(), "a finished token offers nothing more");
        let (_, list) = completions("x\\k", Field::Find, 1, &names).unwrap();
        assert_eq!(
            list.iter().map(|c| c.insert.as_str()).collect::<Vec<_>>(),
            ["\\k<year>"]
        );
        // An escaped backslash starts nothing.
        assert_eq!(completions("\\\\", Field::Find, 0, &[]), None);
        let (at, list) = completions("x(?<", Field::Find, 0, &[]).unwrap();
        assert_eq!(at, 1);
        assert!(list.iter().any(|c| c.insert == "(?<="));
        let (_, list) = completions("[[:al", Field::Find, 0, &[]).unwrap();
        assert_eq!(
            list.iter().map(|c| c.insert.as_str()).collect::<Vec<_>>(),
            ["[:alpha:]", "[:alnum:]"]
        );
        let (at, list) = completions("a $", Field::Replace, 1, &names).unwrap();
        assert_eq!(at, 2);
        let got: Vec<&str> = list.iter().map(|c| c.insert.as_str()).collect();
        assert_eq!(got, ["$0", "$1", "${year}", "$$"]);
        let (_, list) = completions("\\", Field::Replace, 0, &[]).unwrap();
        assert!(list.iter().any(|c| c.insert == "\\U"));
        assert_eq!(completions("plain text", Field::Replace, 0, &[]), None);
    }

    #[test]
    fn hits_carry_replacements() {
        let mut r = q("(\\d+)", Mode::Regex);
        r.replace = "<$1>".to_string();
        let mut s = Searcher::new(&r).unwrap();
        let mut b = PREVIEW_BUDGET;
        let t = b"a1 b22 c333";
        let h = hits(&mut s, t, 0, 5, 10, true, &mut b).unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].rep.as_deref(), Some(&b"<22>"[..]));
    }
}
