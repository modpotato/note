//! Rough throughput check: `cargo run --release --example bench -- <file>`
use note::search::{self, Mode, Query, Searcher};
use note::{buffer::Buffer, encoding, simd, syntax};
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench <file>");
    let bytes = std::fs::read(&path).expect("read");
    let mb = bytes.len() as f64 / 1e6;
    let t = Instant::now();
    let a = note::simd::is_ascii(&bytes);
    println!("is_ascii {:?} {a}", t.elapsed());
    let t = Instant::now();
    let v = bytes.to_vec();
    println!("to_vec   {:?} {}", t.elapsed(), v.len());
    let t = Instant::now();
    let dec = encoding::decode_owned(bytes);
    let d_decode = t.elapsed();
    let t = Instant::now();
    let mut buf = Buffer::from_text(dec.text);
    let d_index = t.elapsed();
    let t = Instant::now();
    let hits = simd::count_byte(buf.make_contiguous(), b'{');
    let d_count = t.elapsed();
    let t = Instant::now();
    let mut hl = syntax::Highlighter::new(syntax::Lang::from_path(&path));
    let last = buf.line_count() - 1;
    let st = hl.state_at(last, &mut |i, out| buf.slice_into(buf.line_start(i), buf.line_end(i), out));
    let d_lex = t.elapsed();
    println!("{mb:.1} MB, {} lines", buf.line_count());
    println!("decode   {d_decode:?}\nindex    {d_index:?}\ncount    {d_count:?} ({hits} hits)\nlex-all  {d_lex:?} (state {st})");

    // Find-all throughput for a few query shapes (count over the whole text).
    let text = buf.make_contiguous();
    for (find, mode, case) in [
        ("return", Mode::Plain, true),
        ("return", Mode::Plain, false),
        ("\\bfn\\s+\\w+", Mode::Regex, true),
        ("[A-Z][a-z]+Error", Mode::Regex, true),
        ("(?<=\\()\\w+(?=\\))", Mode::Regex, true),
        ("^\\s*//.*$", Mode::Regex, true),
    ] {
        let q = Query {
            find: find.to_string(),
            mode,
            match_case: case,
            ..Query::default()
        };
        let mut s = Searcher::new(&q).expect("query");
        let mut budget = u64::MAX;
        let t = Instant::now();
        let n = search::count(&mut s, text, (0, text.len()), &mut budget);
        let tag = if case { "" } else { " (ignore case)" };
        println!("find     {:?} {find}{tag}: {n:?}", t.elapsed());
    }
}
