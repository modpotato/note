//! Benchmark suite for the platform-independent core: buffer, search, regex, SIMD kernels,
//! syntax, encoding and the editor. The Win32 layer is not part of it.
//!
//!     cargo run --release --example suite -- <group> <args>
//!
//!     kernels  <file> <mb>   SIMD byte kernels against std and scalar baselines
//!     decode   <file>        detection, decoding, indexing and encoding of a whole file
//!     pipeline <file>        decode, then highlighting and search on the same file
//!     editing  <file>        editor and gap-buffer operations, latency per operation
//!     regex                  step budget under pathological patterns, compile limits
//!     config                 startup-path parsing
//!
//! Each measurement is one JSON object per line on stdout. Fresh input is built outside
//! the timed section, so cloning a file for a run is never counted.

use note::buffer::Buffer;
use note::config::{Config, Keymap};
use note::editor::{Editor, Move};
use note::encoding::{self, Encoding, LineEnding};
use note::regex::{Options, Regex};
use note::search::{self, Mode, Query, Searcher, FIND_BUDGET, PREVIEW_BUDGET};
use note::simd;
use note::syntax::{Highlighter, Lang};
use std::hint::black_box;
use std::time::{Duration, Instant};

const MIN_SAMPLES: usize = 5;
const MAX_SAMPLES: usize = 200;
const TARGET: Duration = Duration::from_millis(600);
const CAP: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct Stats {
    median: f64,
    min: f64,
    p10: f64,
    p90: f64,
    p99: f64,
    n: usize,
}

fn summarize(mut v: Vec<f64>) -> Stats {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let at = |q: f64| v[((n - 1) as f64 * q).round() as usize];
    Stats {
        median: at(0.5),
        min: v[0],
        p10: at(0.1),
        p90: at(0.9),
        p99: at(0.99),
        n,
    }
}

/// Times whole operations. `setup` builds fresh input (not timed), `f` consumes it.
fn bench<I, T>(mut setup: impl FnMut() -> I, mut f: impl FnMut(I) -> T) -> Stats {
    black_box(f(setup())); // warm-up: page faults, lazy statics, predictors
    let mut samples = Vec::new();
    let start = Instant::now();
    loop {
        let input = setup();
        let t = Instant::now();
        black_box(f(input));
        samples.push(t.elapsed().as_nanos() as f64);
        let n = samples.len();
        let elapsed = start.elapsed();
        if n >= MAX_SAMPLES || (n >= MIN_SAMPLES && elapsed >= TARGET) || (n >= 3 && elapsed >= CAP)
        {
            break;
        }
    }
    summarize(samples)
}

/// Times each of `n` calls separately, for per-keystroke latency distributions.
fn per_op<T>(n: usize, mut op: impl FnMut(usize) -> T) -> Stats {
    let v = (0..n)
        .map(|i| {
            let t = Instant::now();
            black_box(op(i));
            t.elapsed().as_nanos() as f64
        })
        .collect();
    summarize(v)
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One result line. `extra` is a raw JSON fragment without the leading comma.
fn emit(group: &str, name: &str, imp: &str, bytes: usize, s: Stats, extra: &str) {
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!(",{extra}")
    };
    println!(
        "{{\"group\":\"{group}\",\"name\":\"{}\",\"impl\":\"{}\",\"bytes\":{bytes},\"median_ns\":{:.1},\"min_ns\":{:.1},\"p10_ns\":{:.1},\"p90_ns\":{:.1},\"p99_ns\":{:.1},\"n\":{}{extra}}}",
        esc(name),
        esc(imp),
        s.median,
        s.min,
        s.p10,
        s.p90,
        s.p99,
        s.n,
    );
}

/// xorshift64: deterministic positions for the random-access scenarios.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn kernels(path: &str, mb: usize) {
    let raw = std::fs::read(path).expect("read input");
    let take = (mb << 20).min(raw.len());
    // ASCII only, so is_ascii scans the whole buffer instead of stopping at the first
    // multi-byte character.
    let text: Vec<u8> = raw[..take].iter().copied().filter(u8::is_ascii).collect();
    let size = text.len();
    let lf = String::from_utf8(text.clone()).expect("ascii");
    let crlf = lf.replace('\n', "\r\n");
    let crlf_bytes = crlf.as_bytes();
    let k = |name: &str, imp: &str, s: Stats| emit("kernels", name, imp, size, s, "");

    k(
        "count LF",
        "note",
        bench(|| (), |_| simd::count_byte(&text, b'\n')),
    );
    k(
        "count LF",
        "scalar",
        bench(|| (), |_| text.iter().filter(|&&b| b == b'\n').count()),
    );
    k(
        "find byte (absent)",
        "note",
        bench(|| (), |_| simd::find_byte(&text, 1)),
    );
    k(
        "find byte (absent)",
        "scalar",
        bench(|| (), |_| text.iter().position(|&b| b == 1)),
    );
    k(
        "newline positions",
        "note",
        bench(Vec::new, |mut out: Vec<usize>| {
            simd::newline_positions(&text, 0, &mut out);
            out.len()
        }),
    );
    k(
        "newline positions",
        "scalar",
        bench(Vec::new, |mut out: Vec<usize>| {
            for (i, &b) in text.iter().enumerate() {
                if b == b'\n' {
                    out.push(i);
                }
            }
            out.len()
        }),
    );
    k("is_ascii", "note", bench(|| (), |_| simd::is_ascii(&text)));
    k("is_ascii", "std", bench(|| (), |_| text.is_ascii()));
    k(
        "substring, absent (5 B)",
        "note",
        bench(|| (), |_| simd::find(&text, b"qzjxk")),
    );
    k(
        "substring, absent (5 B)",
        "std str::find",
        bench(|| (), |_| lf.find("qzjxk")),
    );
    k(
        "substring, absent (5 B)",
        "scalar",
        bench(|| (), |_| text.windows(5).position(|w| w == b"qzjxk")),
    );
    k(
        "substring ci, absent (5 B)",
        "note",
        bench(|| (), |_| simd::find_ascii_ci(&text, b"QZJXK")),
    );
    k(
        "substring ci, absent (5 B)",
        "scalar",
        bench(
            || (),
            |_| {
                text.windows(5)
                    .position(|w| w.eq_ignore_ascii_case(b"QZJXK"))
            },
        ),
    );
    k(
        "CRLF to LF",
        "note",
        bench(|| (), |_| simd::crlf_to_lf(crlf_bytes)),
    );
    k(
        "CRLF to LF",
        "std str::replace",
        bench(|| (), |_| crlf.replace("\r\n", "\n")),
    );
    k(
        "LF to CRLF",
        "note",
        bench(|| (), |_| simd::lf_to_crlf(&text)),
    );
    k(
        "LF to CRLF",
        "std str::replace",
        bench(|| (), |_| lf.replace('\n', "\r\n")),
    );
}

/// (label, query, mode, match case)
const QUERIES: &[(&str, &str, Mode, bool)] = &[
    ("plain: return", "return", Mode::Plain, true),
    ("plain: return, ignore case", "return", Mode::Plain, false),
    ("regex: \\bfn\\s+\\w+", "\\bfn\\s+\\w+", Mode::Regex, true),
    (
        "regex: [A-Z][a-z]+Error",
        "[A-Z][a-z]+Error",
        Mode::Regex,
        true,
    ),
    ("regex: ^\\s*//.*$", "^\\s*//.*$", Mode::Regex, true),
    (
        "regex: \\b(unsafe|async|await|dyn)\\b",
        "\\b(unsafe|async|await|dyn)\\b",
        Mode::Regex,
        true,
    ),
    (
        "regex: lookbehind (?<=\\()\\w+(?=\\))",
        "(?<=\\()\\w+(?=\\))",
        Mode::Regex,
        true,
    ),
    ("number range: 10-20", "10-20", Mode::NumberRange, true),
];

/// Detection, decoding, indexing and encoding of a whole file. Returns the decoded text.
fn decode_ops(g: &str, raw: &[u8]) -> Vec<u8> {
    let size = raw.len();
    let detected = encoding::decode(raw);
    let lines = detected.text.iter().filter(|&&b| b == b'\n').count() + 1;
    let extra = format!(
        "\"encoding\":\"{:?}\",\"line_ending\":\"{:?}\",\"lines\":{lines}",
        detected.encoding, detected.line_ending
    );
    emit(g, "detect", "note", size, bench(|| (), |_| 0), &extra);
    emit(
        g,
        "decode (borrowed input)",
        "note",
        size,
        bench(|| (), |_| encoding::decode(raw)),
        "",
    );
    emit(
        g,
        "decode_owned",
        "note",
        size,
        bench(|| raw.to_vec(), encoding::decode_owned),
        "",
    );
    emit(
        g,
        "count LF (raw bytes)",
        "note",
        size,
        bench(|| (), |_| simd::count_byte(raw, b'\n')),
        "",
    );
    let text = detected.text;
    emit(
        g,
        "index (Buffer::from_text)",
        "note",
        size,
        bench(|| text.clone(), Buffer::from_text),
        "",
    );
    emit(
        g,
        "encode to CRLF",
        "note",
        size,
        bench(
            || (),
            |_| encoding::encode(&text, Encoding::Utf8, LineEnding::CrLf),
        ),
        "",
    );
    emit(
        g,
        "encode to UTF-16LE",
        "note",
        size,
        bench(
            || (),
            |_| encoding::encode(&text, Encoding::Utf16Le, LineEnding::Lf),
        ),
        "",
    );
    text
}

fn decode_group(path: &str) {
    let raw = std::fs::read(path).expect("read input");
    decode_ops("decode", &raw);
}

fn pipeline(path: &str) {
    let raw = std::fs::read(path).expect("read input");
    let size = raw.len();
    let lang = Lang::from_path(path);
    let g = "pipeline";
    let text = decode_ops(g, &raw);

    if !matches!(lang, Lang::Plain) {
        emit(
            g,
            &format!("highlight whole file ({})", lang.name()),
            "note",
            size,
            bench(
                || (Buffer::from_text(text.clone()), Highlighter::new(lang)),
                |(buf, mut hl)| {
                    let last = buf.line_count() - 1;
                    hl.state_at(last, &mut |i, out| {
                        buf.slice_into(buf.line_start(i), buf.line_end(i), out)
                    })
                },
            ),
            "",
        );
    }

    // Counting all matches over the whole text. The budget is lifted so the timing is the
    // engine's, and the steps used are reported so the app's budget can be checked.
    for &(label, find, mode, match_case) in QUERIES {
        let q = Query {
            find: find.to_string(),
            mode,
            match_case,
            ..Query::default()
        };
        emit(
            g,
            &format!("compile: {label}"),
            "note",
            0,
            bench(|| (), |_| Searcher::new(&q).is_ok()),
            "",
        );
        let mut s = Searcher::new(&q).expect("valid query");
        let total = text.len();
        let mut budget = u64::MAX;
        let counted = search::count(&mut s, &text, (0, total), &mut budget);
        let steps = u64::MAX - budget;
        let extra = match counted {
            Ok(n) => format!(
                "\"matches\":{n},\"steps\":{steps},\"steps_per_byte\":{:.4},\"over_find_budget\":{}",
                steps as f64 / total.max(1) as f64,
                steps > FIND_BUDGET
            ),
            Err(_) => format!("\"matches\":null,\"steps\":{steps},\"over_find_budget\":true"),
        };
        emit(
            g,
            label,
            "note",
            total,
            bench(
                || (),
                |_| {
                    let mut b = u64::MAX;
                    search::count(&mut s, &text, (0, total), &mut b).ok()
                },
            ),
            &extra,
        );
    }
}

fn editing(path: &str) {
    let raw = std::fs::read(path).expect("read input");
    let size = raw.len();
    let cfg = Config::parse("");
    let g = "editing";
    let load = |bytes: Vec<u8>| {
        let mut ed = Editor::new(&cfg);
        ed.load_owned(path, bytes);
        ed
    };
    // One keystroke: insert, then the same scroll bookkeeping the window does per key.
    let key = |ed: &mut Editor, i: usize| {
        let ch = if i % 80 == 79 {
            '\n'
        } else {
            char::from(b'a' + (i % 26) as u8)
        };
        ed.insert_char(ch);
        ed.ensure_cursor_visible(40, 100);
    };

    emit(
        g,
        "open (decode, index, reset)",
        "note",
        size,
        bench(|| raw.clone(), |v| load(v).buf.len()),
        "",
    );

    let total = load(raw.clone()).buf.len();
    let mid = total / 2;
    for (label, at) in [
        ("keystroke: type mid-file", mid),
        ("keystroke: type at top", 0),
        ("keystroke: type at end", total),
    ] {
        let mut ed = load(raw.clone());
        ed.set_cursor(at, false);
        emit(g, label, "note", size, per_op(500, |i| key(&mut ed, i)), "");
    }

    let mut ed = load(raw.clone());
    let mut rng = Rng(0x5EED_0001);
    emit(
        g,
        "keystroke: random jumps",
        "note",
        size,
        per_op(300, |_| {
            let p = rng.below(total);
            ed.set_cursor(p, false);
            ed.insert_char('x');
        }),
        "",
    );

    let mut ed = load(raw.clone());
    ed.set_cursor(mid, false);
    emit(
        g,
        "backspace mid-file",
        "note",
        size,
        per_op(500, |_| {
            ed.backspace();
            ed.ensure_cursor_visible(40, 100);
        }),
        "",
    );

    let mut ed = load(raw.clone());
    ed.set_cursor(mid, false);
    emit(
        g,
        "cursor down",
        "note",
        size,
        per_op(1000, |_| ed.move_cursor(Move::Down, false)),
        "",
    );

    emit(
        g,
        "undo 300 keystrokes",
        "note",
        size,
        bench(
            || {
                let mut e = load(raw.clone());
                e.set_cursor(mid, false);
                for i in 0..300 {
                    key(&mut e, i);
                }
                e
            },
            |mut e| {
                for _ in 0..300 {
                    e.undo();
                }
                e.buf.len()
            },
        ),
        "",
    );

    // Gap buffer at the top, then a search: the search needs one contiguous slice, so the
    // gap has to travel to the end first.
    let absent = Query {
        find: "zzqxjv".into(),
        mode: Mode::Plain,
        match_case: true,
        ..Query::default()
    };
    let mut sr = Searcher::new(&absent).expect("query");
    emit(
        g,
        "find, absent: gap at top",
        "note",
        size,
        bench(
            || {
                let mut e = load(raw.clone());
                e.set_cursor(0, false);
                e.insert_char('x');
                e
            },
            |mut e| {
                let mut b = u64::MAX;
                e.find(&mut sr, true, true, None, &mut b).is_ok()
            },
        ),
        "",
    );
    emit(
        g,
        "find, absent: gap at end",
        "note",
        size,
        bench(
            || {
                let mut e = load(raw.clone());
                e.buf.make_contiguous();
                e
            },
            |mut e| {
                let mut b = u64::MAX;
                e.find(&mut sr, true, true, None, &mut b).is_ok()
            },
        ),
        "",
    );

    let common = Query {
        find: "return".into(),
        replace: "ret".into(),
        mode: Mode::Plain,
        match_case: true,
        ..Query::default()
    };
    let mut sr = Searcher::new(&common).expect("query");
    let mut ed = load(raw.clone());
    ed.buf.make_contiguous();
    let mid_line = ed.buf.line_count() / 2;
    emit(
        g,
        "live preview (50 lines, budget 4M)",
        "note",
        size,
        per_op(200, |_| {
            let mut b = PREVIEW_BUDGET;
            let hits = ed.hits(&mut sr, mid_line, mid_line + 50, 200, true, &mut b);
            hits.map(|h| ed.preview_lines(&h, 200).len()).ok()
        }),
        "",
    );
    emit(
        g,
        "count all matches",
        "note",
        size,
        bench(
            || load(raw.clone()),
            |mut e| {
                let mut b = u64::MAX;
                e.count_matches(&mut sr, None, &mut b).ok()
            },
        ),
        "",
    );
    emit(
        g,
        "replace all",
        "note",
        size,
        bench(
            || load(raw.clone()),
            |mut e| {
                let mut b = u64::MAX;
                e.replace_all(&mut sr, None, &mut b).map(|r| r.0).ok()
            },
        ),
        "",
    );
    emit(
        g,
        "save (encode)",
        "note",
        size,
        bench(|| load(raw.clone()), |mut e| e.save_bytes().len()),
        "",
    );

    // Raw buffer: where an insert lands decides how much of the line index moves.
    let base = encoding::decode(&raw).text;
    let mut buf = Buffer::from_text(base);
    let n_lines = buf.line_count();
    emit(
        g,
        "buffer insert at line 0",
        "note",
        size,
        per_op(200, |_| buf.insert(0, b"x")),
        &format!("\"lines\":{n_lines}"),
    );
    emit(
        g,
        "buffer insert at end",
        "note",
        size,
        per_op(200, |_| {
            let n = buf.len();
            buf.insert(n, b"x");
        }),
        "",
    );
    emit(
        g,
        "buffer insert mid-file",
        "note",
        size,
        per_op(200, |i| {
            let n = buf.len();
            buf.insert(n / 2 + i, b"x");
        }),
        "",
    );
}

fn regex_group() {
    let g = "regex";
    // Each case is run once for the step count, then timed. The engine must give up at the
    // budget, and the time to give up is the worst-case stall the user can see.
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "(a|aa)+c on a^28 b",
            "(a|aa)+c",
            format!("{}b", "a".repeat(28)),
        ),
        ("(x+x+)+y on x^26", "(x+x+)+y", "x".repeat(26)),
        (
            "^(\\w+\\s?)*$ on (w^24)!",
            "^(\\w+\\s?)*$",
            format!("{}!", "w".repeat(24)),
        ),
        (".*.*.*= on 4000 bytes, no =", ".*.*.*=", "a".repeat(4000)),
    ];
    for (name, pat, text) in &cases {
        let re = Regex::new(pat, Options::default()).expect("pattern");
        let t = text.as_bytes();
        let mut caps = re.captures();
        let mut b = FIND_BUDGET;
        let t0 = Instant::now();
        let r = re.find_at(t, 0, t.len(), &mut caps, &mut b);
        let dt = t0.elapsed().as_nanos() as f64;
        let steps = FIND_BUDGET - b;
        let outcome = match r {
            Ok(Some(_)) => "match",
            Ok(None) => "no match",
            Err(_) => "budget exhausted",
        };
        let extra = format!(
            "\"outcome\":\"{outcome}\",\"steps\":{steps},\"ns_per_step\":{:.3}",
            dt / steps.max(1) as f64
        );
        let stats = bench(
            || (),
            |_| {
                let mut caps = re.captures();
                let mut b = FIND_BUDGET;
                re.find_at(t, 0, t.len(), &mut caps, &mut b).is_ok()
            },
        );
        emit(g, name, "note", t.len(), stats, &extra);
    }

    let words: Vec<String> = (0..2000).map(|i| format!("word{i}")).collect();
    let alternation = words.join("|");
    let compile_cases = [
        ("compile a{1000}", "a{1000}".to_string()),
        ("compile (a{1000}){1000}", "(a{1000}){1000}".to_string()),
        ("compile 2000-word alternation", alternation),
    ];
    for (name, pat) in compile_cases {
        let accepted = Regex::new(&pat, Options::default()).is_ok();
        emit(
            g,
            name,
            "note",
            pat.len(),
            bench(|| (), |_| Regex::new(&pat, Options::default()).is_ok()),
            &format!("\"accepted\":{accepted}"),
        );
    }
}

fn config_group() {
    let g = "config";
    let ini = Config::default_ini();
    emit(
        g,
        "parse default config",
        "note",
        ini.len(),
        bench(|| (), |_| Config::parse(&ini)),
        "",
    );
    emit(
        g,
        "standard keymap",
        "note",
        0,
        bench(|| (), |_| Keymap::standard()),
        "",
    );
    let cfg = Config::parse("");
    emit(
        g,
        "new editor (empty)",
        "note",
        0,
        bench(|| (), |_| Editor::new(&cfg)),
        "",
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let group = args.get(1).map(String::as_str).unwrap_or("");
    let file = || args.get(2).expect("missing <file>").clone();
    match group {
        "kernels" => {
            let mb: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(16);
            kernels(&file(), mb)
        }
        "decode" => decode_group(&file()),
        "pipeline" => pipeline(&file()),
        "editing" => editing(&file()),
        "regex" => regex_group(),
        "config" => config_group(),
        _ => {
            eprintln!(
                "usage: suite <kernels <file> <mb> | decode <file> | pipeline <file> | editing <file> | regex | config>"
            );
            std::process::exit(2);
        }
    }
}
