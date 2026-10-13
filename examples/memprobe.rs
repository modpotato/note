//! Heap and peak-RSS probe. Each scenario runs in its own process, so the numbers are not
//! shared with earlier work. A counting global allocator tracks requested bytes.
//!
//!     cargo run --release --example memprobe -- <scenario> <file> [arg]
//!
//!     open <file>             read + decode + index (what opening a file costs)
//!     editor <file>           Editor::load_owned + highlight the last line
//!     typing <file> <n>       after opening, type n characters in the middle of the file
//!     count <file> <query>    count all plain matches
//!     replace <file> <query>  replace all plain matches with "ret"
//!
//! Prints one JSON object. `peak_extra_bytes` is the peak heap above the baseline reached
//! during the scenario, `retained_bytes` what is still held when it ends, and `vmhwm_kb`
//! the process's peak resident set, read from /proc.

use note::buffer::Buffer;
use note::config::Config;
use note::editor::Editor;
use note::encoding;
use note::search::{self, Mode, Query, Searcher};
use note::syntax::{Highlighter, Lang};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(bytes: usize) {
    let now = CURRENT.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(now, Relaxed);
}

// SAFETY: every call is forwarded unchanged to the system allocator; only counters are added.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            grow(l.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        CURRENT.fetch_sub(l.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let q = System.realloc(p, l, new_size);
        if !q.is_null() {
            if new_size >= l.size() {
                grow(new_size - l.size());
            } else {
                CURRENT.fetch_sub(l.size() - new_size, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn current() -> usize {
    CURRENT.load(Relaxed)
}

/// Resets the peak to the current heap level and returns that level.
fn start_probe() -> usize {
    let base = current();
    PEAK.store(base, Relaxed);
    base
}

fn vmhwm_kb() -> usize {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

/// JSON for an optional count: the number, or null.
fn json_opt(v: Option<usize>) -> String {
    v.map_or_else(|| "null".to_string(), |x| x.to_string())
}

fn report(scenario: &str, file_bytes: u64, base: usize, retained: usize, extra: &str) {
    let peak = PEAK.load(Relaxed).saturating_sub(base);
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!(",{extra}")
    };
    println!(
        "{{\"scenario\":\"{scenario}\",\"file_bytes\":{file_bytes},\"peak_extra_bytes\":{peak},\"retained_bytes\":{},\"vmhwm_kb\":{}{extra}}}",
        retained.saturating_sub(base),
        vmhwm_kb(),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let scenario = args.get(1).map(String::as_str).unwrap_or("");
    let path = args.get(2).expect("missing <file>").clone();
    let file_bytes = std::fs::metadata(&path).expect("stat").len();
    let cfg = Config::parse("");

    match scenario {
        "open" => {
            let base = start_probe();
            let bytes = std::fs::read(&path).expect("read");
            let decoded = encoding::decode_owned(bytes);
            let buf = Buffer::from_text(decoded.text);
            let lines = buf.line_count();
            report(
                "open",
                file_bytes,
                base,
                current(),
                &format!("\"lines\":{lines}"),
            );
            std::hint::black_box(&buf);
        }
        "editor" => {
            let base = start_probe();
            let bytes = std::fs::read(&path).expect("read");
            let mut ed = Editor::new(&cfg);
            ed.load_owned(&path, bytes);
            let mut hl = Highlighter::new(Lang::from_path(&path));
            let last = ed.buf.line_count() - 1;
            let buf = &ed.buf;
            hl.state_at(last, &mut |i, out| {
                buf.slice_into(buf.line_start(i), buf.line_end(i), out)
            });
            report("editor", file_bytes, base, current(), "");
            std::hint::black_box((&ed, &hl));
        }
        "typing" => {
            let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10_000);
            let mut ed = Editor::new(&cfg);
            ed.load_owned(&path, std::fs::read(&path).expect("read"));
            let mid = ed.buf.len() / 2;
            ed.set_cursor(mid, false);
            let base = start_probe();
            for i in 0..n {
                let ch = if i % 80 == 79 {
                    '\n'
                } else {
                    char::from(b'a' + (i % 26) as u8)
                };
                ed.insert_char(ch);
            }
            report(
                "typing",
                file_bytes,
                base,
                current(),
                &format!("\"keystrokes\":{n}"),
            );
            std::hint::black_box(&ed);
        }
        "count" | "replace" => {
            let query = args.get(3).expect("missing <query>").clone();
            let mut ed = Editor::new(&cfg);
            ed.load_owned(&path, std::fs::read(&path).expect("read"));
            let q = Query {
                find: query,
                replace: "ret".into(),
                mode: Mode::Plain,
                match_case: true,
                ..Query::default()
            };
            let mut sr = Searcher::new(&q).expect("query");
            let base = start_probe();
            let mut b = u64::MAX;
            if scenario == "count" {
                let len = ed.buf.len();
                let n = search::count(&mut sr, ed.buf.make_contiguous(), (0, len), &mut b);
                report(
                    "count",
                    file_bytes,
                    base,
                    current(),
                    &format!("\"matches\":{}", json_opt(n.ok())),
                );
            } else {
                let r = ed.replace_all(&mut sr, None, &mut b);
                report(
                    "replace",
                    file_bytes,
                    base,
                    current(),
                    &format!("\"replaced\":{}", json_opt(r.map(|x| x.0).ok())),
                );
            }
            std::hint::black_box(&ed);
        }
        _ => {
            eprintln!("usage: memprobe <open|editor|typing|count|replace> <file> [arg]");
            std::process::exit(2);
        }
    }
}
