# Benchmarks

These scripts measure note's engine on every front the project can reach on Linux, and run the
Windows build against Wine's built-in Notepad. `REPORT.html` is rendered from `results/`.

## Reproduce

The committed results were measured on Ubuntu 24.04 with wine-9.0, ripgrep 14.1.0 and NSIS 3.09.
Install the tools, then run the steps from the repository root:

```sh
sudo apt install ripgrep libc-bin xvfb xdotool wine wine64 gcc-mingw-w64-x86-64 nsis
rustup target add x86_64-pc-windows-gnu

python3 benchmarks/run_benchmarks.py --check   # which tools are present, and what each enables
python3 benchmarks/corpora.py --verify         # the corpora regenerate to the committed digests
python3 benchmarks/run_benchmarks.py           # builds, tests, timed suites, memory: about 25 minutes
python3 benchmarks/wine_compare.py             # note against Wine's Notepad: about 15 minutes
python3 benchmarks/crosscheck_search.py        # Python re cross-check of one search count: seconds
python3 benchmarks/render_report.py            # writes benchmarks/REPORT.html
```

`run_benchmarks.py` stops before it measures anything when a required tool is missing, and
refuses to run on corpora that differ from `results/corpora.json`. Optional tools (MinGW, NSIS, the
Windows Rust target) skip only their own phases, and the report says so. `wine_compare.py` needs the
Windows build that `run_benchmarks.py` makes, so run them in that order.

## What is deterministic

- **Corpora:** bit-identical on every machine. `corpora.py --verify` checks them against
  `results/corpora.json`.
- **Counts:** test results, lint and format results, source line counts, and search match counts
  depend only on the commit and the toolchain.
- **Timings** depend on the machine and on what else is running. Compare ratios between runs on
  one machine, not absolute figures across machines. The report shows the spread of repeated runs.
- **Sizes** of the Windows binaries depend on the rustc and linker versions, which the results
  record.

## Files

| File | What it does |
| --- | --- |
| `corpora.py` | Generates the corpora from a fixed seed. `--verify` checks them, `--record` writes the digests. |
| `run_benchmarks.py` | The campaign: tests, clippy, rustfmt, builds, timed suites, a SIMD ablation, memory probes, and ripgrep, wc and iconv comparisons. `--check` runs only the preflight. |
| `wine_compare.py` | note and Wine's Notepad under Wine on a virtual display: window, title, memory, and CPU per keystroke. |
| `crosscheck_search.py` | Counts one search query with Python's `re`, over the whole text and line by line. |
| `render_report.py` | Renders `REPORT.html` from `results/`. `--artifact PATH` writes only the body. |
| `results/` | `latest.json`, `wine.json` and `crosscheck.json` hold the raw results. `corpora.json` holds the corpus digests. |
| `../examples/suite.rs`, `../examples/memprobe.rs` | The timed harness and the heap probe that the campaign runs. |

## Limits

- Wine is a translation layer. The Wine figures compare two programs under Wine. They do not
  measure Windows Notepad on Windows.
- The Windows build is MinGW (`x86_64-pc-windows-gnu`). The release binary is MSVC, which Linux
  cannot build, so size comparisons with the README are not like-for-like.
- The corpora are synthetic. `REPORT.html` compares their shape with real code in this repository.
  To time a real file, run the suite on it directly: `target/release/examples/suite pipeline FILE`.
- Not measured: frame times of the Win32 renderer, input latency through the window system, and
  native Windows behaviour.

## Clean up

`target/bench-data` holds the corpora (about 0.7 GB). `target/bench-scratch` holds the scratch
builds and a Wine prefix (about 1 GB). `target/bench-ablation` holds the SIMD ablation build.
`rm -rf target/bench-data target/bench-scratch target/bench-ablation` removes them. The next run
rebuilds what it needs.
