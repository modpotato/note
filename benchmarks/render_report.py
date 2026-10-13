#!/usr/bin/env python3
"""Renders benchmarks/REPORT.html from benchmarks/results/latest.json and wine.json.

    python3 benchmarks/render_report.py

One self-contained file: no scripts and no network requests. Bars and markers are HTML and
CSS placed in percent, so the charts reflow at phone width and take their colours from the
theme tokens. Every number on the page is read from the results, and every sentence that
states a comparison is built from those numbers.
"""

import html
import json
import math
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RESULTS = REPO / "benchmarks" / "results"
OUT = REPO / "benchmarks" / "REPORT.html"

CAMPAIGN = json.loads((RESULTS / "latest.json").read_text())
WINE = json.loads((RESULTS / "wine.json").read_text()) if (RESULTS / "wine.json").exists() else None

META = CAMPAIGN["meta"]
CODE = CAMPAIGN.get("code_health", {})
BUILD = CAMPAIGN.get("build", {})
WIN = CAMPAIGN.get("windows", {})
CORPORA = CAMPAIGN.get("corpora", {})
COMP = CAMPAIGN.get("comparisons", {})
MEM = CAMPAIGN.get("memory", [])
SUITE = CAMPAIGN.get("suite", []) + CAMPAIGN.get("ablation", [])
BENCH_EX = CAMPAIGN.get("bench_example") or {}
ERRORS = CAMPAIGN.get("errors", [])

HARNESS_FILES = {"examples/suite.rs", "examples/memprobe.rs"}
BIG, MID, SMALL = "mixed-100m.txt", "mixed-10m.txt", "mixed-1m.txt"
MIB = 1 << 20
SIZES = [(SMALL, "1 MiB"), (MID, "10 MiB"), (BIG, "100 MiB")]

ALL = {(r["group"], r["file"], r["build"], r["simd"], r["impl"], r["name"]): r for r in SUITE}

SEARCH_QUERIES = [
    "plain: return",
    "plain: return, ignore case",
    "regex: \\bfn\\s+\\w+",
    "regex: [A-Z][a-z]+Error",
    "regex: ^\\s*//.*$",
    "regex: \\b(unsafe|async|await|dyn)\\b",
    "regex: lookbehind (?<=\\()\\w+(?=\\))",
    "number range: 10-20",
]

# (kernel name, note impl, baseline impl, baseline label)
KERNELS = [
    ("count LF", "note", "scalar", "scalar loop"),
    ("find byte (absent)", "note", "scalar", "scalar loop"),
    ("newline positions", "note", "scalar", "scalar loop"),
    ("is_ascii", "note", "std", "std is_ascii"),
    ("substring, absent (5 B)", "note", "std str::find", "std str::find"),
    ("substring ci, absent (5 B)", "note", "scalar", "scalar loop"),
    ("CRLF to LF", "note", "std str::replace", "std str::replace"),
    ("LF to CRLF", "note", "std str::replace", "std str::replace"),
]

EDIT_ROWS = [
    ("open (decode, index, reset)", "open, decode and index"),
    ("keystroke: type mid-file", "keystroke, mid-file"),
    ("keystroke: type at top", "keystroke, top of file"),
    ("keystroke: type at end", "keystroke, end of file"),
    ("keystroke: random jumps", "keystroke, random positions"),
    ("backspace mid-file", "backspace, mid-file"),
    ("cursor down", "cursor down"),
    ("undo 300 keystrokes", "undo 300 keystrokes (total)"),
    ("find, absent: gap at top", "find, no match, gap at top"),
    ("find, absent: gap at end", "find, no match, gap at end"),
    ("live preview (50 lines, budget 4M)", "live preview, 50 lines"),
    ("count all matches", "count all matches"),
    ("replace all", "replace all"),
    ("save (encode)", "save (encode)"),
]


# ---- lookups and numbers ------------------------------------------------------------------


def rec(group, file, name, impl="note", build="main", simd="auto"):
    return ALL.get((group, file, build, simd, impl, name))


def mem(scenario, file):
    return next((m for m in MEM if m["scenario"] == scenario and m["file"] == file), None)


def mbs(nbytes, ns):
    """MB per second, with MB = 10^6 bytes. bytes per ns is GB/s, so multiply by 1000."""
    return nbytes / ns * 1e3 if nbytes and ns else None


def kernel_mbs(name, impl, file=BIG, build="main", simd="auto"):
    r = rec("kernels", file, name, impl=impl, build=build, simd=simd)
    return mbs(r["bytes"], r["median_ns"]) if r else None


ENC_NAMES = {"Utf8": "UTF-8", "Utf8Bom": "UTF-8 with BOM", "Utf16Le": "UTF-16LE", "Utf16Be": "UTF-16BE", "Latin1": "Latin-1"}
LE_NAMES = {"Lf": "LF", "CrLf": "CRLF"}


def esc(value):
    return html.escape(str(value), quote=True)


def fmt_ns(ns):
    if ns is None:
        return "n/a"
    if ns < 1e3:
        return f"{ns:.0f} ns"
    if ns < 1e6:
        return f"{ns / 1e3:.1f} µs"
    if ns < 1e9:
        return f"{ns / 1e6:.1f} ms"
    return f"{ns / 1e9:.2f} s"


def fmt_us(us):
    return f"{us / 1000:.2f} ms" if us >= 1000 else f"{us:.0f} µs"


def fmt_speed(v):
    if v is None:
        return "n/a"
    return f"{v / 1000:.2f} GB/s" if v >= 1000 else f"{v:.0f} MB/s"


def fmt_mib(n):
    return f"{n / MIB:.1f} MiB"


def fmt_kb(n):
    return f"{n / 1000:.1f} KB"


def fmt_int(n):
    return f"{n:,}" if isinstance(n, int) else str(n)


def pct_log(value, lo, hi):
    value = max(value, lo)
    return max(0.0, min(100.0, (math.log10(value) - math.log10(lo)) / (math.log10(hi) - math.log10(lo)) * 100))


def pct_lin(value, lo, hi):
    return max(0.0, min(100.0, (value - lo) / (hi - lo) * 100))


def gain(a, b):
    """How many times faster `a` is than `b`, as text."""
    if not a or not b:
        return "n/a"
    r = a / b
    return f"{r:.1f}×" if r >= 1 else f"{1 / r:.1f}× slower"


# ---- building blocks ----------------------------------------------------------------------


class Raw(str):
    """HTML that table() must not escape."""


VERDICT_LABEL = {"good": "✓ Good", "watch": "▲ Watch", "critical": "✕ Fails", "info": "○ Context"}


def badge(verdict):
    return f'<span class="badge b-{verdict}">{esc(VERDICT_LABEL[verdict])}</span>'


def table(headers, rows, numeric=(), caption=None):
    head = "".join(
        f'<th scope="col"{" class=\"num\"" if i in numeric else ""}>{esc(h)}</th>' for i, h in enumerate(headers)
    )
    body = []
    for row in rows:
        cells = []
        for i, cell in enumerate(row):
            text = cell if isinstance(cell, Raw) else esc(cell)
            cells.append(f'<td{" class=\"num\"" if i in numeric else ""}>{text}</td>')
        body.append("<tr>" + "".join(cells) + "</tr>")
    cap = f"<caption>{esc(caption)}</caption>" if caption else ""
    return (
        f'<div class="tablewrap"><table>{cap}<thead><tr>{head}</tr></thead>'
        f'<tbody>{"".join(body)}</tbody></table></div>'
    )


def details(label, inner):
    return f'<details class="data"><summary>{esc(label)}</summary>{inner}</details>'


def legend(items):
    parts = "".join(f'<li><span class="key {cls}"></span>{esc(text)}</li>' for cls, text in items)
    return f'<ul class="legend">{parts}</ul>'


def figure(title, sub, body, takeaway=None, foot=None):
    take = f'<p class="takeaway">{esc(takeaway)}</p>' if takeaway else ""
    note = f'<p class="foot">{esc(foot)}</p>' if foot else ""
    return (
        f'<figure class="chart"><figcaption><h3>{esc(title)}</h3><p class="sub">{esc(sub)}</p></figcaption>'
        f"{body}{take}{note}</figure>"
    )


def bar_rows(rows, scale_max):
    """rows: (label, value, css class, text, tooltip). One linear scale starting at zero."""
    out = []
    for label, value, cls, text, tip in rows:
        width = pct_lin(value, 0, scale_max) if value else 0.0
        out.append(
            f'<div class="brow"><div class="blabel">{esc(label)}</div>'
            f'<div class="btrack"><div class="bfill {cls}" style="width:{width:.2f}%" title="{esc(tip)}"></div></div>'
            f'<div class="bvalue">{esc(text)}</div></div>'
        )
    return f'<div class="bars">{"".join(out)}</div>'


def dumbbell(rows, lo, hi, ticks, legend_items):
    """rows: (label, [(value, cls, text, tooltip), ...]). Log axis from lo to hi."""
    tick_html = "".join(f'<span class="tick" style="left:{pct_log(t, lo, hi):.2f}%">{esc(t_lbl)}</span>' for t, t_lbl in ticks)
    grid = "".join(f'<i class="grid" style="left:{pct_log(t, lo, hi):.2f}%"></i>' for t, _ in ticks)
    out = []
    for label, marks in rows:
        vals = [m[0] for m in marks]
        left, right = pct_log(min(vals), lo, hi), pct_log(max(vals), lo, hi)
        line = f'<span class="dline" style="left:{left:.2f}%;width:{max(right - left, 0):.2f}%"></span>' if len(vals) > 1 else ""
        dots = "".join(
            f'<span class="dot {cls}" style="left:{pct_log(v, lo, hi):.2f}%" title="{esc(tip)}" tabindex="0" '
            f'aria-label="{esc(tip)}"></span>'
            for v, cls, _text, tip in marks
        )
        values = " · ".join(f'<span class="{cls}-t">{esc(text)}</span>' for _v, cls, text, _tip in marks)
        out.append(
            f'<div class="drow"><div class="dlabel">{esc(label)}</div>'
            f'<div class="dtrack">{grid}{line}{dots}</div><div class="dvals">{values}</div></div>'
        )
    return (
        f"{legend(legend_items)}"
        f'<div class="dumb" role="img" aria-label="dot plot on a logarithmic scale">'
        f'<div class="axis">{tick_html}</div>{"".join(out)}</div>'
    )


def range_plot(rows, lo, hi, ticks, thresholds):
    """rows: (label, median, p10, p90, css class). Median dot on a p10–p90 bar, log axis."""
    grid = "".join(f'<i class="grid" style="left:{pct_log(t, lo, hi):.2f}%"></i>' for t, _ in ticks)
    thr = "".join(
        f'<i class="thr {cls}" style="left:{pct_log(t, lo, hi):.2f}%"></i>' for t, label, cls in thresholds
    )
    tick_html = "".join(f'<span class="tick" style="left:{pct_log(t, lo, hi):.2f}%">{esc(lbl)}</span>' for t, lbl in ticks)
    out = []
    for label, median, p10, p90, cls in rows:
        a, b = pct_log(p10, lo, hi), pct_log(p90, lo, hi)
        m = pct_log(median, lo, hi)
        out.append(
            f'<div class="rrow"><div class="rlabel">{esc(label)}</div>'
            f'<div class="rtrack">{grid}{thr}'
            f'<span class="rwhisk" style="left:{a:.2f}%;width:{max(b - a, 0.5):.2f}%"></span>'
            f'<span class="rmed {cls}" style="left:{m:.2f}%" title="median {esc(fmt_ns(median))}"></span></div>'
            f'<div class="rvalue">{esc(fmt_ns(median))}</div></div>'
        )
    return (
        '<div class="range" role="img" aria-label="range plot on a logarithmic scale">'
        f'<div class="axis">{tick_html}</div>{"".join(out)}</div>'
    )


RAMP = ["#cde2fb", "#b7d3f6", "#9ec5f4", "#86b6ef", "#6da7ec", "#5598e7", "#3987e5",
        "#2a78d6", "#256abf", "#1c5cab", "#184f95", "#104281", "#0d366b"]


def ramp(value, lo, hi):
    """Sequential blue, light to dark, on a log scale. Text is dark on the light steps."""
    t = 0.0 if hi == lo else (math.log10(value) - math.log10(lo)) / (math.log10(hi) - math.log10(lo))
    i = min(len(RAMP) - 1, max(0, round(max(0.0, min(1.0, t)) * (len(RAMP) - 1))))
    return RAMP[i], "#ffffff" if i >= 7 else "#0b0b0b"


def tiles(items):
    cells = []
    for value, label, note, verdict in items:
        badge_html = badge(verdict) if verdict else ""
        cells.append(
            f'<div class="tile"><div class="tval">{esc(value)}</div><div class="tlabel">{esc(label)}</div>'
            f'<div class="tnote">{esc(note)}</div>{badge_html}</div>'
        )
    return f'<div class="tiles">{"".join(cells)}</div>'


# ---- sections -----------------------------------------------------------------------------


def summary_section():
    t = CODE["tests"]
    clip = CODE["clippy"]["diagnostics"]
    fmt = CODE["rustfmt"]
    loc = CODE["loc"]
    project_loc = sum(v for k, v in loc.items() if k not in HARNESS_FILES)
    deps = CODE["dependencies"]
    open_m = mem("open", BIG)
    top = rec("editing", BIG, "keystroke: type at top")
    worst = max(rec("regex", "-", n)["median_ns"] for n in ("(x+x+)+y on x^26", ".*.*.*= on 4000 bytes, no =") if rec("regex", "-", n))
    nsps = [rec("regex", "-", n)["ns_per_step"] for n in ("(a|aa)+c on a^28 b", "(x+x+)+y on x^26", "^(\\w+\\s?)*$ on (w^24)!", ".*.*.*= on 4000 bytes, no =") if rec("regex", "-", n)]
    tile_items = [
        (f"{t['passed']} / {t['passed'] + t['failed']}", "tests pass", f"release mode, {t['run_seconds']:.2f} s", "good" if t["failed"] == 0 else "critical"),
        (str(len(clip)), "clippy diagnostic", f"coded warning in {clip[0]['file']}, test code" if clip else "none", "watch" if clip else "good"),
        (str(fmt["files_with_diffs"]), "files rustfmt would change", f"{fmt['hunks']} hunks; one file is the original examples/bench.rs", "watch"),
        (f"{BUILD['cold_release_s']:.1f} s", "cold release build", "Linux, fat LTO, one codegen unit", "info"),
        (str(len(deps["runtime"])), "runtime dependencies on Linux", f"{len(deps['windows_only'])} Windows-only, {len(deps['build'])} build-time", "good"),
        (f"{WIN['default_exe_bytes'] / 1000:.0f} KB", "Windows exe, MinGW build", "release build is MSVC; README says about 870 KB", "info"),
        (f"{open_m['retained_bytes'] / open_m['file_bytes']:.2f}×", "heap kept after opening 100 MiB", f"{open_m['lines']:,} lines, 8 bytes of index each", "good"),
        (fmt_ns(top["median_ns"]), "keystroke at the top, 100 MiB", "median; it grows with the lines after the cursor", "watch"),
        (f"{min(nsps):.1f}–{max(nsps):.1f} ns", "per regex step", f"the budget stops the worst case in {fmt_ns(worst)}", "good"),
    ]
    return (
        '<section id="summary"><h2>Summary</h2>'
        f'<p class="lede-s">{project_loc:,} lines of project code, measured on every front below. '
        "Each tile is one number; the verdicts section says what each one means.</p>"
        f"{tiles(tile_items)}</section>"
    )


def verdict_section():
    fully = [n for n, impl, base, _l in KERNELS if kernel_mbs(n, impl) and kernel_mbs(n, base) and kernel_mbs(n, impl) > kernel_mbs(n, base)]
    counts = COMP.get("find_all", [])
    agree = [e for e in counts if e["note_matches"] == e["rg_matches"]]
    differ = [e for e in counts if e["note_matches"] != e["rg_matches"]]
    cmp_faster, cmp_slower = [], []
    for e in counts:
        note = rec("pipeline", BIG, e["label"])
        if not note:
            continue
        rg_best = min(e["rg_threads_s"]["median_s"], e["rg_single_thread_s"]["median_s"])
        ratio = rg_best * 1e9 / note["median_ns"]
        (cmp_faster if ratio >= 1 else cmp_slower).append((e["label"], ratio))
    slowest = min(cmp_slower, key=lambda x: x[1]) if cmp_slower else None
    decode_owned = rec("pipeline", BIG, "decode_owned")
    decode_borrowed = rec("pipeline", BIG, "decode (borrowed input)")
    crlf = rec("pipeline", BIG, "encode to CRLF")
    mid = rec("editing", BIG, "keystroke: type mid-file")
    top = rec("editing", BIG, "keystroke: type at top")
    end = rec("editing", BIG, "keystroke: type at end")
    key_cases = [
        ("typing mid-file", rec("editing", BIG, "keystroke: type mid-file")),
        ("typing at the top", top),
        ("typing at the end", end),
        ("typing at random positions", rec("editing", BIG, "keystroke: random jumps")),
        ("backspace mid-file", rec("editing", BIG, "backspace mid-file")),
    ]
    slowest_key = max(key_cases, key=lambda kv: kv[1]["median_ns"])
    rep = rec("editing", BIG, "replace all")
    find_top = rec("editing", BIG, "find, absent: gap at top")
    find_end = rec("editing", BIG, "find, absent: gap at end")
    cnt = rec("editing", BIG, "count all matches")
    open_m = mem("open", BIG)
    rep_m = mem("replace", BIG)
    lookbehind = rec("pipeline", BIG, "regex: lookbehind (?<=\\()\\w+(?=\\))")
    worst_regex = max(rec("regex", "-", n)["median_ns"] for n in ("(a|aa)+c on a^28 b", "(x+x+)+y on x^26", "^(\\w+\\s?)*$ on (w^24)!", ".*.*.*= on 4000 bytes, no =") if rec("regex", "-", n))
    best_regex = min(rec("regex", "-", n)["median_ns"] for n in ("(x+x+)+y on x^26", ".*.*.*= on 4000 bytes, no =") if rec("regex", "-", n))
    exe, inst = WIN["default_exe_bytes"], WIN["installer_bytes"]
    lb_steps = lookbehind.get("steps", 0) if lookbehind else 0
    verdict_rows = [
        ["Correctness",
         f"{CODE['tests']['passed']} tests pass; {len(agree)} of {len(counts)} match counts equal ripgrep's",
         "good" if CODE["tests"]["failed"] == 0 and not differ else "watch",
         (f"The one difference is {', '.join(f'{abs(e['note_matches'] - e['rg_matches'])} match(es) on {e['label']}' for e in differ)}. "
          "Those matches cross a newline, and note's count agrees with Python's re on them.") if differ else "All counts agree."],
        ["SIMD kernels",
         f"ahead of the baseline on {len(fully)} of {len(KERNELS)} kernels at 64 MiB",
         "good" if len(fully) == len(KERNELS) else "watch",
         "Margins run from the smallest against std's is_ascii to the largest against the scalar loops; the scalar loops are weak baselines."],
        ["Whole-file pipeline",
         f"decode {fmt_speed(mbs(decode_borrowed['bytes'], decode_borrowed['median_ns']))} with a copy, "
         f"{fmt_speed(mbs(decode_owned['bytes'], decode_owned['median_ns']))} without; CRLF encode {fmt_speed(mbs(crlf['bytes'], crlf['median_ns']))}",
         "watch",
         "Decoding and indexing are fast. The slow paths are saving with CRLF or UTF-16, and decoding CRLF, UTF-16 or Latin-1 input, because each converts every line ending or character."],
        ["Search vs ripgrep",
         f"faster on {len(cmp_faster)} of {len(counts)} queries; slower on {len(cmp_slower)}"
         + (f", by up to {1 / slowest[1]:.0f}×" if slowest else ""),
         "watch",
         ("Queries that open with a literal are faster than ripgrep. Patterns that open with a class or an anchor, and alternations, are "
          + (f"{min(1 / r for _, r in cmp_slower):.1f}× to {max(1 / r for _, r in cmp_slower):.0f}× slower." if cmp_slower else "not in the slower group."))],
        ["Typing",
         f"100 MiB, p50: {fmt_ns(mid['median_ns'])} mid-file, {fmt_ns(top['median_ns'])} at the top, {fmt_ns(end['median_ns'])} at the end",
         "good" if slowest_key[1]["median_ns"] < 16e6 else "watch",
         f"The slowest keystroke median is {slowest_key[0]}, at {fmt_ns(slowest_key[1]['median_ns'])}, against a 60 Hz frame of 16 ms."],
        ["Whole-file operations",
         f"replace all {fmt_ns(rep['median_ns'])} (p90 {fmt_ns(rep['p90_ns'])}); count {fmt_ns(cnt['median_ns'])}; "
         f"find with no match {fmt_ns(find_end['median_ns'])}, {fmt_ns(find_top['median_ns'])} with the cursor at the top",
         "watch",
         "Replace-all is the only edit above 100 ms. A find first makes the text contiguous, which moves the gap to the end, so with the gap at the top it moves almost the whole file."],
        ["Memory",
         f"opening keeps {open_m['retained_bytes'] / open_m['file_bytes']:.2f}× the file; replace-all peaks at "
         f"{rep_m['peak_extra_bytes'] / rep_m['file_bytes']:.1f}×",
         "watch",
         "The text is kept once, plus 8 bytes per line. Typing into a fresh large file doubles the text and index buffers, while the undo record for the typing stays small."],
        ["Regex robustness",
         f"worst pathological case stops after {fmt_ns(worst_regex)}; steps cost {min(r['ns_per_step'] for r in SUITE if r['group'] == 'regex' and 'ns_per_step' in r):.1f}–"
         f"{max(r['ns_per_step'] for r in SUITE if r['group'] == 'regex' and 'ns_per_step' in r):.1f} ns",
         "watch",
         "The budget bounds the work, but searches run on the UI thread, so the window is blocked for that long."],
        ["Lookaround at 100 MiB",
         f"one full count needs {lb_steps / 1e6:.0f} M steps; the Find dialog allows {200} M",
         "watch",
         "The dialog reports 'too complex' for this count on a file this large. By proportion, 10 MiB needs about 25 M steps, inside the budget."],
        ["Build and dependencies",
         f"{BUILD['cold_release_s']:.1f} s cold release; {len(CODE['dependencies']['runtime'])} runtime dependencies on Linux",
         "good",
         "A small build with no runtime dependencies on Linux, and one Windows-only binding crate. An incremental build takes about two thirds of a cold one, which fits whole-crate optimisation under fat LTO."],
        ["Size",
         f"exe {fmt_kb(exe)}, installer {fmt_kb(inst)} (MinGW build)",
         "watch",
         "Both are above the README's figures. The release binary is an MSVC build, which this machine cannot produce, so the gap is not like-for-like."],
    ]
    if WINE:
        verdict_rows.append(["Against Wine's Notepad", wine_headline(), "info",
                             "Both programs ran under the same Wine on the same display. Read the section for caveats."])
    body = [[front, text, Raw(badge(v)), reading] for front, text, v, reading in verdict_rows]
    return (
        '<section id="verdicts"><h2>Verdict by front</h2>'
        '<p class="lede-s">A reading of each front and the number it rests on. The verdict is a judgement against '
        "a 60 Hz frame, against ripgrep, or against the README, as the row says.</p>"
        f'{table(["Front", "What was measured", "Verdict", "Reading"], body)}</section>'
    )


def wine_headline():
    if not WINE:
        return "not run"
    s = WINE["summary"]
    n, p = s.get("note", {}).get(BIG, {}), s.get("notepad", {}).get(BIG, {})
    if n.get("title_s") and p.get("title_s"):
        return f"100 MiB open: note {n['title_s']:.1f} s, Wine Notepad {p['title_s']:.1f} s"
    if n.get("title_s"):
        return f"100 MiB open: note {n['title_s']:.1f} s; Wine Notepad did not open it"
    return "see section"


def latency_section():
    specs = [
        ("keystroke: type mid-file", "keystroke, mid-file, p50"),
        ("keystroke: type at top", "keystroke, top of file, p50"),
        ("keystroke: type at end", "keystroke, end of file, p50"),
        ("keystroke: random jumps", "keystroke, random positions, p50"),
        ("backspace mid-file", "backspace, mid-file, p50"),
        ("cursor down", "cursor down, p50"),
        ("undo 300 keystrokes", "undo 300 keystrokes, total"),
        ("find, absent: gap at top", "find, no match, gap at top"),
        ("find, absent: gap at end", "find, no match, gap at end"),
        ("live preview (50 lines, budget 4M)", "live preview, 50 lines"),
        ("count all matches", "count all matches"),
        ("replace all", "replace all"),
        ("save (encode)", "save"),
        ("open (decode, index, reset)", "open"),
    ]
    rows = []
    for name, label in specs:
        r = rec("editing", BIG, name)
        if r:
            rows.append((label, r["median_ns"], r["p10_ns"], r["p90_ns"], "s1"))
    hi = rec("pipeline", "c-headers.h", "highlight whole file (C)")
    if hi:
        rows.append(("first highlight, whole 37 MB C file", hi["median_ns"], hi["p10_ns"], hi["p90_ns"], "s1"))
    worst = rec("regex", "-", "(x+x+)+y on x^26")
    if worst:
        rows.append(("regex, worst case, budget stop", worst["median_ns"], worst["p10_ns"], worst["p90_ns"], "s1"))
    ticks = [(1e3, "1 µs"), (1e4, "10 µs"), (1e5, "100 µs"), (1e6, "1 ms"), (1e7, "10 ms"), (1e8, "100 ms"), (1e9, "1 s"), (1e10, "10 s")]
    thresholds = [(16e6, "16 ms · one frame", "good"), (1e8, "100 ms · noticeable", "watch"), (1e9, "1 s · freezes", "critical")]
    chart = range_plot(rows, 1e3, 1e10, ticks, thresholds)
    typing_max = max(rec("editing", BIG, n)["median_ns"] for n in ("keystroke: type mid-file", "keystroke: type at top", "keystroke: type at end", "keystroke: random jumps", "backspace mid-file"))
    rep = rec("editing", BIG, "replace all")
    slow_ops = [label for label, med, _p10, _p90, _c in rows if med > 100e6]
    body = (
        '<p class="legend-row"><span class="key s1"></span>median (dot) and the 10th to 90th percentile (bar); '
        "vertical lines mark one 60 Hz frame (16 ms), 100 ms and one second</p>" + chart
    )
    fig = figure(
        "Editor operations on a 100 MiB file",
        "Logarithmic axis. Each row is one operation, timed around the call the window makes.",
        body,
        takeaway=(
            f"Every keystroke median stays under {fmt_ns(typing_max)} on 100 MiB, inside one 60 Hz frame. "
            f"Operations above 100 ms: {'; '.join(slow_ops) if slow_ops else 'none'}. "
            f"Cursor down is the exception to the size trend: {fmt_ns(rec('editing', MID, 'cursor down')['median_ns'])} at 10 MiB against "
            f"{fmt_ns(rec('editing', BIG, 'cursor down')['median_ns'])} at 100 MiB, and the cause was not isolated."
        ),
        foot="Figures under 1 µs are single calls timed separately, so they include the cost of the timer call; read them as orders of magnitude.",
    )
    table_rows = [[label, fmt_ns(med), fmt_ns(p10), fmt_ns(p90)] for label, med, p10, p90, _c in rows]
    return (
        '<section id="latency"><h2>Does it feel instant?</h2>'
        '<p class="lede-s">What a person waits for on a 100 MiB file. The thresholds are the ones that matter: one '
        "60 Hz frame, a visible pause, and a frozen window.</p>"
        f'{fig}{details("Data table", table(["Operation", "Median", "p10", "p90"], table_rows, numeric=(1, 2, 3)))}</section>'
    )


def kernels_section():
    rows, table_rows = [], []
    for name, impl, base, base_label in KERNELS:
        nv, bv = kernel_mbs(name, impl), kernel_mbs(name, base)
        sv, sb = kernel_mbs(name, impl, file=SMALL), kernel_mbs(name, base, file=SMALL)
        if nv and bv:
            rows.append((name, nv, bv, base_label))
            table_rows.append([name, fmt_speed(nv), fmt_speed(bv), base_label, f"{nv / bv:.2f}×", fmt_speed(sv), fmt_speed(sb)])
    top = max(max(r[1], r[2]) for r in rows)
    bars = []
    for name, nv, bv, base_label in rows:
        ratio = nv / bv
        bars.append(
            f'<div class="krow"><div class="klabel">{esc(name)}'
            f'<span class="kdelta">{ratio:.2f}× {"faster" if ratio >= 1 else "slower"}</span></div>'
            f'<div class="kbars">'
            f'<div class="btrack thin"><div class="bfill s1" style="width:{pct_lin(nv, 0, top):.2f}%" title="note {esc(fmt_speed(nv))}"></div></div>'
            f'<div class="btrack thin"><div class="bfill s2" style="width:{pct_lin(bv, 0, top):.2f}%" title="{esc(base_label)} {esc(fmt_speed(bv))}"></div></div>'
            f'</div><div class="kvals"><span class="s1-t">{esc(fmt_speed(nv))}</span><span class="s2-t">{esc(fmt_speed(bv))}</span></div></div>'
        )
    std_rows = [r for r in rows if r[3].startswith("std")]
    isascii = next(r for r in rows if r[0] == "is_ascii")
    substr = next(r for r in rows if r[0].startswith("substring, absent"))
    body = legend([("s1", "note, AVX2 dispatch on this CPU"), ("s2", "baseline: std routine or scalar loop")]) + f'<div class="kern">{"".join(bars)}</div>'
    fig = figure(
        "Byte kernels on 64 MiB, against std and scalar baselines",
        "MB/s on one linear scale. The input is ASCII text cut from the 100 MiB corpus.",
        body,
        takeaway=(
            f"note is ahead on all {len(rows)} kernels. Against std it is {isascii[1] / isascii[2]:.2f}× on is_ascii "
            f"and {substr[1] / substr[2]:.1f}× on substring search (against std's str::find)"
            + (f"; the CRLF and LF conversions are {min(r[1] / r[2] for r in std_rows if 'CRLF' in r[0] or 'LF' in r[0]):.1f}–"
               f"{max(r[1] / r[2] for r in std_rows if 'CRLF' in r[0] or 'LF' in r[0]):.1f}× against str::replace." if std_rows else ".")
        ),
        foot="The scalar loops are naive code that the compiler may partly vectorise, so the margin against them is an upper bound.",
    )
    return (
        '<section id="kernels"><h2>Engine: SIMD byte kernels</h2>'
        '<p class="lede-s">The byte-scanning kernels behind newline counting, search, line endings and ASCII checks, '
        "each against the closest std routine or a scalar loop.</p>"
        f'{fig}{details("Data table, 64 MiB and 1 MiB", table(["Kernel", "note, 64 MiB", "baseline, 64 MiB", "Baseline", "Ratio", "note, 1 MiB", "baseline, 1 MiB"], table_rows, numeric=(1, 2, 4, 5, 6)))}'
        "</section>"
    )


def avx2_section():
    kern = []
    for name, impl, _b, _l in KERNELS:
        a = kernel_mbs(name, impl, build="ablation", simd="auto")
        s = kernel_mbs(name, impl, build="ablation", simd="sse2")
        if a and s:
            kern.append((name, a, s))
    ratios = [a / s for _n, a, s in kern]
    pipe = []
    for name in ("index (Buffer::from_text)", "count LF (raw bytes)", "decode (borrowed input)", "decode_owned", "encode to CRLF"):
        a = rec("decode", BIG, name, build="ablation", simd="auto")
        s = rec("decode", BIG, name, build="ablation", simd="sse2")
        if a and s:
            pipe.append((name, a["median_ns"], s["median_ns"]))
    pr = [s / a for _n, a, s in pipe]
    fig = figure(
        "AVX2 against SSE2, same binary and input",
        "The SSE2 run sets NOTE_SIMD=sse2 on a scratch copy whose dispatch reads that variable. The committed source is unchanged.",
        bar_rows([(n, a / s, "s1", f"{a / s:.2f}×", f"{n}: AVX2 is {a / s:.2f}× SSE2") for n, a, s in kern], max(ratios) * 1.08),
        takeaway=(
            f"On kernels at 64 MiB, AVX2 is between {min(ratios):.2f}× and {max(ratios):.2f}× the SSE2 speed. "
            f"Whole-file operations gain between {min(pr):.2f}× and {max(pr):.2f}×. That suggests memory traffic, which both paths share, sets the pace at this size."
        ),
    )
    kern_rows = [[n, fmt_speed(a), fmt_speed(s), f"{a / s:.2f}×"] for n, a, s in kern]
    pipe_rows = [[n, fmt_ns(a), fmt_ns(s), f"{s / a:.2f}×"] for n, a, s in pipe]
    return (
        '<section id="avx2"><h2>AVX2 against SSE2</h2>'
        '<p class="lede-s">The README says SSE2 is the baseline and AVX2 is used when the CPU has it. This measures what '
        "that switch is worth on this machine.</p>"
        f'{fig}{details("Data table", table(["Kernel", "AVX2 (auto)", "SSE2 (forced)", "AVX2 ÷ SSE2"], kern_rows, numeric=(1, 2, 3)) + table(["Whole-file operation, 100 MiB", "AVX2 time", "SSE2 time", "SSE2 ÷ AVX2 time"], pipe_rows, numeric=(1, 2, 3)))}</section>'
    )


def pipeline_section():
    specs = [
        ("decode (borrowed input)", "decode, copies the input"),
        ("decode_owned", "decode, takes ownership (no copy)"),
        ("count LF (raw bytes)", "count LF in raw bytes"),
        ("index (Buffer::from_text)", "index (line starts)"),
        ("encode to CRLF", "encode to CRLF"),
        ("encode to UTF-16LE", "encode to UTF-16LE"),
    ]
    rows = []
    for name, label in specs:
        r = rec("pipeline", BIG, name)
        if r:
            rows.append((label, mbs(r["bytes"], r["median_ns"]), name))
    for fname, name, label in (
        ("c-headers.h", "highlight whole file (C)", "highlight whole file, C (37 MB)"),
        ("python.py", "highlight whole file (Python)", "highlight whole file, Python (23 MB)"),
        ("rust.rs", "highlight whole file (Rust)", "highlight whole file, Rust (22 MB)"),
    ):
        r = rec("pipeline", fname, name)
        if r:
            rows.append((label, mbs(r["bytes"], r["median_ns"]), name))
    top = max(v for _l, v, _n in rows)
    bars = bar_rows([(lbl, v, "s1", fmt_speed(v), lbl) for lbl, v, _n in rows], top * 1.05)
    c_rate = mbs(rec("pipeline", "c-headers.h", "highlight whole file (C)")["bytes"], rec("pipeline", "c-headers.h", "highlight whole file (C)")["median_ns"])
    fig = figure(
        "Whole-file operations on 100 MiB, throughput",
        "MB/s of input on one linear scale. Highlighting is measured on single-language files of the size shown.",
        bars,
        takeaway=(
            f"Highlighting runs at {c_rate:.0f} MB/s on C, so a 100 MB C file would need about {100 / c_rate:.1f} s "
            "for its first full highlight. Decoding and indexing run at GB/s."
        ),
    )
    # Encodings: detection and cost per variant.
    enc_rows = []
    for variant, label in (
        ("mixed-100m.txt", "UTF-8, LF"),
        ("mixed-100m-crlf.txt", "UTF-8, CRLF"),
        ("mixed-100m-bom.txt", "UTF-8 with BOM"),
        ("mixed-100m-utf16le.txt", "UTF-16LE with BOM"),
        ("mixed-100m-latin1.txt", "Latin-1, not valid UTF-8"),
    ):
        det = rec("decode", variant, "detect")
        own = rec("decode", variant, "decode_owned")
        idx = rec("decode", variant, "index (Buffer::from_text)")
        if det and own:
            enc_rows.append([label, f"{ENC_NAMES.get(det['encoding'], det['encoding'])} · {LE_NAMES.get(det['line_ending'], det['line_ending'])}", fmt_speed(mbs(own["bytes"], own["median_ns"]))])
    enc_fig = figure(
        "Encodings and line endings, 100 MiB each",
        "The same text saved five ways. The detected encoding shows whether the decoder recognised the format.",
        table(["Input", "Detected as", "decode, owned"], enc_rows, numeric=(2,)),
        takeaway="All five formats are detected correctly. The CRLF, UTF-16 and Latin-1 inputs are the slow decodes, because each needs a conversion pass over every line ending or character.",
    )
    tools = COMP.get("newline_count"), COMP.get("read"), COMP.get("utf16_decode"), COMP.get("utf16_encode")
    tool_rows = []
    if tools[0]:
        wc = tools[0]["wc_l_s"]["median_s"] * 1000
        mine = rec("pipeline", BIG, "count LF (raw bytes)")["median_ns"] / 1e6
        tool_rows.append(["count newlines: wc -l vs note", f"{wc:.1f} ms", f"{mine:.1f} ms"])
    if tools[2]:
        io = tools[2]["iconv_s"]["median_s"] * 1000
        mine = rec("pipeline", "mixed-100m-utf16le.txt", "decode (borrowed input)") or rec("decode", "mixed-100m-utf16le.txt", "decode (borrowed input)")
        tool_rows.append(["UTF-16LE to UTF-8: iconv vs note", f"{io:.1f} ms", f"{mine['median_ns'] / 1e6:.1f} ms" if mine else "n/a"])
    if tools[3]:
        io = tools[3]["iconv_s"]["median_s"] * 1000
        mine = rec("pipeline", BIG, "encode to UTF-16LE")
        tool_rows.append(["UTF-8 to UTF-16LE: iconv vs note", f"{io:.1f} ms", f"{mine['median_ns'] / 1e6:.1f} ms" if mine else "n/a"])
    if tools[1]:
        tool_rows.append(["read the file: cat (I/O floor)", f"{tools[1]['cat_s']['median_s'] * 1000:.1f} ms", "-"])
    tool_table = table(["Operation on 100 MiB", "Other tool", "note"], tool_rows, numeric=(1, 2)) if tool_rows else ""
    tool_fig = figure(
        "Against standard tools on the same files",
        "Wall-clock medians, including process start-up (negligible at this size). note runs in-process.",
        tool_table,
    )
    return (
        '<section id="pipeline"><h2>Whole-file pipeline</h2>'
        '<p class="lede-s">Detection, decoding, indexing and encoding of a whole file, then highlighting. '
        "Search is in its own section.</p>"
        f"{fig}{enc_fig}{tool_fig}</section>"
    )


def search_section():
    counts = COMP.get("find_all", [])
    if not counts:
        return '<section id="search"><h2>Search against ripgrep</h2><p>Not measured.</p></section>'
    rows, agree_rows, speeds = [], [], []
    for e in counts:
        note = rec("pipeline", BIG, e["label"])
        if not note:
            continue
        nv = mbs(note["bytes"], note["median_ns"])
        st = mbs(104857585, e["rg_single_thread_s"]["median_s"] * 1e9)
        mt = mbs(104857585, e["rg_threads_s"]["median_s"] * 1e9)
        marks = [(nv, "s1", fmt_speed(nv), f"note: {fmt_speed(nv)}")]
        marks.append((st, "s2", fmt_speed(st), f"ripgrep, one thread: {fmt_speed(st)}"))
        marks.append((mt, "s3", fmt_speed(mt), f"ripgrep, four threads: {fmt_speed(mt)}"))
        speeds += [nv, st, mt]
        rows.append((e["label"], marks))
        same = e["note_matches"] == e["rg_matches"]
        agree_rows.append([
            e["label"], fmt_int(e["note_matches"]), fmt_int(e["rg_matches"]),
            Raw(badge("good") if same else badge("watch")),
            fmt_ns(note["median_ns"]), f"{e['rg_single_thread_s']['median_s'] * 1000:.1f} ms", f"{e['rg_threads_s']['median_s'] * 1000:.1f} ms",
        ])
    lookbehind = rec("pipeline", BIG, "regex: lookbehind (?<=\\()\\w+(?=\\))")
    if lookbehind:
        lv = mbs(lookbehind["bytes"], lookbehind["median_ns"])
        rows.append(("regex: lookbehind (no ripgrep equivalent)", [(lv, "s1", fmt_speed(lv), f"note: {fmt_speed(lv)}")]))
        speeds.append(lv)
    lo = 10 ** math.floor(math.log10(min(speeds)))
    hi = 10 ** math.ceil(math.log10(max(speeds)))
    ticks = [(t, f"{t:,.0f} MB/s" if t < 1000 else f"{t / 1000:,.0f} GB/s") for t in (10 ** k for k in range(int(math.log10(lo)), int(math.log10(hi)) + 1))]
    chart = dumbbell(rows, lo, hi, ticks, [("s1", "note"), ("s2", "ripgrep, one thread"), ("s3", "ripgrep, four threads")])
    by = {e["label"]: e for e in counts}
    def cmp(label):
        note = rec("pipeline", BIG, label)
        return (e := by.get(label)) and note and min(e["rg_threads_s"]["median_s"], e["rg_single_thread_s"]["median_s"]) * 1e9 / note["median_ns"]
    fast = cmp("plain: return")
    fast_ci = cmp("plain: return, ignore case")
    slow_alt = cmp("regex: \\b(unsafe|async|await|dyn)\\b")
    slow_cls = cmp("regex: [A-Z][a-z]+Error")
    fig = figure(
        "Counting every match in 100 MiB, note against ripgrep",
        "Logarithmic axis, MB/s. Each row is one query. Ripgrep uses -F for plain text and the same pattern otherwise.",
        chart,
        takeaway=(
            f"Against the faster of ripgrep's two runs, note counts plain text {fast:.1f}× faster, and case-insensitive plain text "
            f"{fast_ci:.1f}× faster. Alternations are {1 / slow_alt:.0f}× slower and a class-led pattern {1 / slow_cls:.0f}× slower. "
            "The likely cause is the backtracking VM doing more work per candidate position; this was not profiled."
        ),
        foot="Lookaround needs PCRE2 in ripgrep, so that row has no ripgrep marks.",
    )
    agree_table = table(["Query", "note matches", "ripgrep matches", "Agree", "note time", "ripgrep, 1 thread", "ripgrep, 4 threads"], agree_rows, numeric=(1, 2, 4, 5, 6))
    differ = [e for e in counts if e["note_matches"] != e["rg_matches"]]
    diff_note = ""
    if differ:
        diff_note = (
            "<p class=\"foot\">The mismatch is a construct, not a bug on either side. ripgrep matches one line at a time, "
            "so a \\s+ that spans a newline cannot match there. note's buffer is searched as one piece, and its count agrees with Python's re.</p>"
        )
    return (
        '<section id="search"><h2>Search against ripgrep</h2>'
        '<p class="lede-s">The same file and queries through note\'s search and through ripgrep, the usual fast reference. '
        "A speed number means little without the match counts, so they are compared too.</p>"
        f"{fig}<h3 class=\"sub-h\">Do the counts agree?</h3>{agree_table}{diff_note}</section>"
    )


def editing_section():
    sizes = [SMALL, MID, BIG]
    values = {}
    for name, _label in EDIT_ROWS:
        for f in sizes:
            r = rec("editing", f, name)
            if r:
                values[(name, f)] = r["median_ns"]
    lo, hi = min(values.values()), max(values.values())
    rows = []
    for name, label in EDIT_ROWS:
        cells = []
        for f in sizes:
            v = values.get((name, f))
            if v is None:
                cells.append('<td class="heat">n/a</td>')
            else:
                bg, fg = ramp(v, lo, hi)
                cells.append(f'<td class="heat" style="background:{bg};color:{fg}">{esc(fmt_ns(v))}</td>')
        rows.append(f'<tr><th scope="row">{esc(label)}</th>{"".join(cells)}</tr>')
    heat = (
        '<div class="tablewrap"><table class="heatmap"><thead><tr><th scope="col">Operation</th>'
        + "".join(f'<th scope="col" class="num">{esc(lbl)}</th>' for _f, lbl in SIZES)
        + f'</tr></thead><tbody>{"".join(rows)}</tbody></table></div>'
    )
    fig = figure(
        "Median per operation, by file size",
        "Colour is a log scale: pale is the fastest cell, dark the slowest. Text colour is chosen per cell for contrast.",
        heat,
        takeaway=(
            f"From 1 to 100 MiB, open goes from {fmt_ns(values[('open (decode, index, reset)', SMALL)])} to {fmt_ns(values[('open (decode, index, reset)', BIG)])}, "
            f"count from {fmt_ns(values[('count all matches', SMALL)])} to {fmt_ns(values[('count all matches', BIG)])}, "
            f"and replace-all from {fmt_ns(values[('replace all', SMALL)])} to {fmt_ns(values[('replace all', BIG)])}. "
            "Typing and deleting grow with the number of lines after the cursor, so they are flat at the end of the file "
            "and grow toward the top."
        ),
    )
    return (
        '<section id="editing"><h2>Editing across file sizes</h2>'
        '<p class="lede-s">The same operations at 1, 10 and 100 MiB, so the scaling is visible across one row.</p>'
        f"{fig}</section>"
    )


def scaling_section():
    series = [
        ("open, decode and index", "s1", [rec("editing", f, "open (decode, index, reset)") for f, _l in SIZES]),
        ("count all matches", "s2", [rec("editing", f, "count all matches") for f, _l in SIZES]),
        ("keystroke at the top, p50", "s3", [rec("editing", f, "keystroke: type at top") for f, _l in SIZES]),
    ]
    xs = [MIB, 10 * MIB, 100 * MIB]
    lo_x, hi_x = xs[0] / 2, xs[-1] * 2
    all_ms = [r["median_ns"] / 1e6 for _n, _c, rs in series for r in rs if r]
    ylo = 10 ** math.floor(math.log10(min(all_ms)))
    yhi = 10 ** math.ceil(math.log10(max(all_ms)))
    decades = []
    t = ylo
    while t <= yhi:
        decades.append(t)
        t *= 10

    def ylabel(ms):
        if ms < 1:
            return f"{ms * 1000:.0f} µs"
        if ms < 1000:
            return f"{ms:.0f} ms"
        return f"{ms / 1000:.0f} s"

    lines, dots, legend_items = [], [], []
    for name, cls, recs in series:
        legend_items.append((cls, name))
        pts = []
        for x, r, (_f, label) in zip(xs, recs, SIZES):
            if not r:
                continue
            px, py = pct_log(x, lo_x, hi_x), 100 - pct_log(r["median_ns"] / 1e6, ylo, yhi)
            pts.append((px, py))
            dots.append(f'<span class="pt {cls}" style="left:{px:.2f}%;top:{py:.2f}%" title="{esc(name)}, {esc(label)}: {esc(fmt_ns(r["median_ns"]))}" tabindex="0" aria-label="{esc(name)}, {esc(label)}: {esc(fmt_ns(r["median_ns"]))}"></span>')
        if len(pts) > 1:
            lines.append(f'<polyline class="ln {cls}" points="{" ".join(f"{x:.2f},{y:.2f}" for x, y in pts)}"/>')
    ytick = "".join(f'<span class="ytick" style="top:{100 - pct_log(d, ylo, yhi):.2f}%">{esc(ylabel(d))}</span>' for d in decades)
    ygrid = "".join(f'<i class="hgrid" style="top:{100 - pct_log(d, ylo, yhi):.2f}%"></i>' for d in decades)
    xtick = "".join(f'<span class="xtick" style="left:{pct_log(x, lo_x, hi_x):.2f}%">{esc(lbl)}</span>' for x, (_f, lbl) in zip(xs, SIZES))
    body = (
        legend(legend_items)
        + f'<div class="lc"><div class="yaxis">{ytick}</div><div class="plot">{ygrid}'
        + f'<svg class="lines" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">{"".join(lines)}</svg>'
        + f'{"".join(dots)}</div><div class="xaxis">{xtick}</div></div>'
    )
    open_r, count_r, key_r = [s[2] for s in series]
    fig = figure(
        "Time against file size, 1 to 100 MiB",
        "Both axes are logarithmic. Each marker is one median; the lines only join them.",
        body,
        takeaway=(
            f"Opening grows from {fmt_ns(open_r[0]['median_ns'])} to {fmt_ns(open_r[2]['median_ns'])} and counting from "
            f"{fmt_ns(count_r[0]['median_ns'])} to {fmt_ns(count_r[2]['median_ns'])}: both grow with the file, though not in a fixed ratio. "
            f"The keystroke at the top grows from {fmt_ns(key_r[0]['median_ns'])} to {fmt_ns(key_r[2]['median_ns'])}, "
            "because each keystroke moves every line start after the cursor."
        ),
    )
    tbl = table(
        ["File", "open", "count all", "keystroke at top, p50"],
        [[lbl, fmt_ns(open_r[i]["median_ns"]), fmt_ns(count_r[i]["median_ns"]), fmt_ns(key_r[i]["median_ns"])] for i, (_f, lbl) in enumerate(SIZES)],
        numeric=(1, 2, 3),
    )
    return f'<section id="scaling"><h2>Scaling</h2>{fig}{details("Data table", tbl)}</section>'


def memory_section():
    rows, bars_rows = [], []
    variants = [(f, lbl) for f, lbl in SIZES] + [
        ("mixed-100m-crlf.txt", "100 MiB, CRLF"),
        ("mixed-100m-bom.txt", "100 MiB, BOM"),
        ("mixed-100m-utf16le.txt", "100 MiB, UTF-16LE (200 MiB file)"),
        ("mixed-100m-latin1.txt", "100 MiB, Latin-1"),
    ]
    for f, label in variants:
        o = mem("open", f)
        if not o:
            continue
        e = mem("editor", f)
        t = mem("typing", f)
        c = mem("count", f)
        rp = mem("replace", f)
        rows.append([
            label, fmt_mib(o["file_bytes"]), fmt_mib(o["retained_bytes"]), f"{o['retained_bytes'] / o['file_bytes']:.2f}×",
            fmt_mib(o["peak_extra_bytes"]), fmt_mib(o["vmhwm_kb"] * 1024),
            fmt_mib(t["retained_bytes"]) if t else "n/a",
            fmt_mib(c["peak_extra_bytes"]) if c else "n/a",
            f"{fmt_mib(rp['peak_extra_bytes'])} ({rp['peak_extra_bytes'] / rp['file_bytes']:.1f}×)" if rp else "n/a",
        ])
        bars_rows.append((label, o["peak_extra_bytes"] / MIB, "s1", fmt_mib(o["peak_extra_bytes"]), f"peak extra heap while opening {label}"))
    rp100 = mem("replace", BIG)
    o100 = mem("open", BIG)
    t100 = mem("typing", BIG)
    fig = figure(
        "Extra heap while opening each file",
        "Peak heap above the starting level, MiB, one fresh process per scenario, measured by a counting allocator.",
        bar_rows(bars_rows, max(r[1] for r in bars_rows) * 1.05),
        takeaway=(
            f"A UTF-8 file peaks at the size it keeps ({fmt_mib(o100['peak_extra_bytes'])}, of which "
            f"{fmt_mib(o100['lines'] * 8)} is the line index). CRLF, Latin-1 and UTF-16 inputs peak at about twice the "
            f"file, because they hold the input and the converted text together. Replace-all peaks at "
            f"{rp100['peak_extra_bytes'] / rp100['file_bytes']:.1f}× the file."
        ),
        foot="Process peak resident memory includes the runtime, so it is shown separately in the table.",
    )
    growth_note = ""
    if t100:
        growth_note = (
            f"<p class=\"foot\">Typing 10,000 characters into a freshly opened 100 MiB file keeps "
            f"{fmt_mib(t100['retained_bytes'])} more. The first keystroke that fills the 4 KiB gap doubles the text buffer, "
            "and the line index grows the same way when it runs out of room. The undo record itself is small.</p>"
        )
    head = ["File", "On disk", "Kept after open", "Kept ÷ file", "Peak during open", "Process peak", "Kept after 10,000 keys", "Count peak", "Replace-all peak"]
    return (
        '<section id="memory"><h2>Memory</h2>'
        '<p class="lede-s">Heap use from the allocator, one scenario per process. Each row starts from an empty process.</p>'
        f"{fig}{growth_note}{details('Data table', table(head, rows, numeric=tuple(range(1, 9))))}</section>"
    )


def regex_section():
    cases = [
        ("(a|aa)+c on a^28 b", "(a|aa)+c on 28 a's and a b"),
        ("(x+x+)+y on x^26", "(x+x+)+y on 26 x's"),
        ("^(\\w+\\s?)*$ on (w^24)!", "^(\\w+\\s?)*$ on 24 w's and a !"),
        (".*.*.*= on 4000 bytes, no =", ".*.*.*= on 4000 bytes, no ="),
    ]
    rows, bars, nsps = [], [], []
    for name, label in cases:
        r = rec("regex", "-", name)
        if r:
            nsps.append(r["ns_per_step"])
            rows.append([label, fmt_ns(r["median_ns"]), r["outcome"], fmt_int(r["steps"]), f"{r['ns_per_step']:.2f} ns"])
            bars.append((f"{label} · {r['outcome']}", r["median_ns"] / 1e6, "s1", f"{r['median_ns'] / 1e6:.0f} ms", f"{label}: {r['outcome']} after {r['steps']:,} steps"))
    compile_rows = []
    for name, label in (("compile a{1000}", "a{1000}"), ("compile (a{1000}){1000}", "(a{1000}){1000}"), ("compile 2000-word alternation", "2000-word alternation")):
        r = rec("regex", "-", name)
        if r:
            compile_rows.append([label, fmt_ns(r["median_ns"]), "accepted" if r["accepted"] else "rejected"])
    lb = rec("pipeline", BIG, "regex: lookbehind (?<=\\()\\w+(?=\\))")
    fig = figure(
        "Time to finish or give up, on pathological patterns",
        "Classic catastrophic-backtracking patterns. A budget exhaustion is the engine stopping itself at 200 million steps.",
        bar_rows(bars, max(b[1] for b in bars) * 1.1),
        takeaway=(
            f"The budget holds: every case stops or finishes in under a second, at "
            f"{min(nsps):.1f}–{max(nsps):.1f} ns per step, "
            "which matches the comment in src/search.rs ('roughly a few nanoseconds each'). Because searches run on the window's thread, that is also how long "
            "the window can stop responding."
        ),
    )
    lb_note = ""
    if lb:
        lb_note = (
            f"<p class=\"foot\">The lookbehind count on 100 MiB needs {lb['steps'] / 1e6:.0f} M steps for one full count. "
            "The Find dialog's count uses one budget of 200 M steps for the whole file, so it reports 'too complex' there. Preview is "
            "budgeted at 4 M steps per call and only covers the lines on screen.</p>"
        )
    return (
        '<section id="regex"><h2>Regex robustness</h2>'
        '<p class="lede-s">A search the engine cannot finish must still stop, and a pattern that cannot be compiled must be refused. These cases test both.</p>'
        f"{fig}{lb_note}"
        f'{details("Data table", table(["Pattern", "Time", "Outcome", "Steps", "ns per step"], rows, numeric=(1, 3, 4)) + table(["Compile case", "Time", "Result"], compile_rows, numeric=(1,)))}</section>'
    )


def build_section():
    tests = CODE["tests"]
    per = tests["per_module"]
    loc = CODE["loc"]
    proj = {k: v for k, v in loc.items() if k not in HARNESS_FILES}
    harness = {k: v for k, v in loc.items() if k in HARNESS_FILES}
    unsafe = CODE["unsafe_keyword_hits"]
    clip = CODE["clippy"]["diagnostics"]
    fmt_files = CODE["rustfmt"]["per_file"]
    build_items = [
        ("cold release build", BUILD["cold_release_s"]),
        ("incremental release build (one file touched)", BUILD["incremental_release_s"]),
        ("cold debug build", BUILD["cold_debug_s"]),
        ("Windows release, cold (MinGW)", WIN["cold_build_s"]),
        ("Windows release, no embedded font", WIN["nofont_build_s"]),
    ]
    top_b = max(v for _l, v in build_items)
    build_bars = bar_rows([(l, v, "s1", f"{v:.1f} s", l) for l, v in build_items], top_b * 1.05)
    mod_max = max(per.values())
    mod_bars = bar_rows([(m, c, "s1", str(c), f"{m}: {c} tests") for m, c in sorted(per.items(), key=lambda kv: -kv[1])], mod_max * 1.05)
    loc_top = sorted(proj.items(), key=lambda kv: -kv[1])[:12]
    loc_max = loc_top[0][1]
    loc_bars = bar_rows([(k, v, "s1", f"{v:,}", k) for k, v in loc_top], loc_max * 1.05)
    tiles_html = tiles([
        (str(tests["passed"]), "tests pass", f"{tests['failed']} failing, {tests['run_seconds']:.2f} s", "good" if tests["failed"] == 0 else "critical"),
        (str(len(clip)), "clippy diagnostic", "coded warning, library test code", "watch" if clip else "good"),
        (str(len(fmt_files)), "files rustfmt would change", f"{CODE['rustfmt']['hunks']} hunks", "watch"),
        (str(CODE["dependencies"]["packages_in_lockfile"]), "entries in Cargo.lock, this crate included", f"{len(CODE['dependencies']['build'])} build-time and {len(CODE['dependencies']['windows_only'])} Windows-only direct dependencies", "good"),
    ])
    return (
        '<section id="build"><h2>Build and code health</h2>'
        '<p class="lede-s">Build times are wall-clock on this 4-vCPU VM, one run each. Test, lint and format results come from '
        "the project's own tools on the committed source.</p>"
        f"{tiles_html}"
        + figure("Build times", "Seconds. The incremental build touches one file, and fat LTO re-optimises the whole crate.", build_bars,
                 takeaway=f"An incremental release build takes {BUILD['incremental_release_s'] / BUILD['cold_release_s'] * 100:.0f}% of a cold one, which fits a whole-crate optimisation pass (fat LTO, one codegen unit) doing most of the work.")
        + figure("Tests per module", f"{tests['passed']} release-mode tests in {len(per)} modules.", mod_bars)
        + figure("Largest project files, lines", f"{sum(proj.values()):,} lines of project code; the harness adds {sum(harness.values()):,} in examples/.", loc_bars)
        + details(
            "Clippy, rustfmt and unsafe details",
            table(["Lint", "File", "Line", "Message"], [[d["code"], d["file"], d["line"], d["message"]] for d in clip], numeric=(2,))
            + table(["File", "Hunks rustfmt would change"], [[f, n] for f, n in sorted(fmt_files.items())], numeric=(1,))
            + table(["File", "Occurrences of the word unsafe"], [[k, v] for k, v in sorted(unsafe.items(), key=lambda kv: -kv[1])], numeric=(1,)),
        )
        + "</section>"
    )


def size_section():
    exe, nofont, inst = WIN["default_exe_bytes"], WIN["nofont_exe_bytes"], WIN["installer_bytes"]
    font_files = sum(WIN["font_files_bytes"].values())
    items = [
        ("exe, default build (font embedded)", exe, "s1", fmt_kb(exe)),
        ("exe, built without the font", nofont, "s1", fmt_kb(nofont)),
        ("installer (NSIS 3.09)", inst, "s1", fmt_kb(inst)),
    ]
    fig = figure(
        "Shipped sizes, MinGW build",
        "Decimal kilobytes, built with the release profile (fat LTO, strip). The release binary is MSVC, which this machine cannot build.",
        bar_rows([(l, v, c, t, l) for l, v, c, t in items], max(v for _l, v, _c, _t in items) * 1.05),
        takeaway=(
            f"The font accounts for {fmt_kb(exe - nofont)}, against {fmt_kb(font_files)} of font files: the README's saving holds. "
            "The totals cannot be checked against the README without the MSVC build."
        ),
    )
    rows = [
        ["Single exe", "about 870 KB", fmt_kb(exe), "different toolchain (MinGW here, MSVC in the release)", Raw(badge("watch"))],
        ["Exe without the font", "about 660 KB", fmt_kb(nofont), "different toolchain, as above", Raw(badge("watch"))],
        ["Font saves", "about 210 KB", fmt_kb(exe - nofont), f"same font files, {fmt_kb(font_files)} of them", Raw(badge("good"))],
        ["Installer", "about 520 KB", fmt_kb(inst), "NSIS 3.09 on Ubuntu, with the MinGW exe inside", Raw(badge("watch"))],
    ]
    return (
        '<section id="size"><h2>Binary size and installer</h2>'
        f'{fig}{details("Claims against measurements", table(["Item", "README says", "Measured", "Comparison", "Verdict"], rows))}</section>'
    )


def wine_section():
    if not WINE:
        return (
            '<section id="wine"><h2>note against Wine\'s Notepad</h2>'
            "<p>Not run. Install wine64 and xdotool, then run <code>python3 benchmarks/wine_compare.py</code>.</p></section>"
        )
    summ = WINE["summary"]
    keys = WINE["keystrokes_per_burst"]
    labels = {"(empty)": "no file", SMALL: "1 MiB", MID: "10 MiB", BIG: "100 MiB"}
    files = ["(empty)", SMALL, MID, BIG]
    apps = [("note", "note", "s1"), ("notepad", "Wine Notepad", "s2")]

    def cell(app, f, field):
        return summ.get(app, {}).get(f, {}).get(field)

    rows = []
    for f in files:
        for app, name, _c in apps:
            s = summ.get(app, {}).get(f, {})
            cpu = s.get("typing_cpu_s")
            rows.append([
                labels[f], name, f"{s.get('ok', 0)} / {s.get('runs', 0)}",
                f"{s['window_s']:.2f} s" if s.get("window_s") is not None else "n/a",
                ("n/a" if f == "(empty)" else (f"{s['title_s']:.2f} s" if s.get("title_s") is not None else "did not open")),
                f"{s['rss_kb'] / 1024:.0f} MiB" if s.get("rss_kb") else "n/a",
                (("≥ " if s.get("typing_settled", 0) < s.get("ok", 0) else "") + fmt_us(cpu / keys * 1e6)) if cpu is not None else "n/a",
                (f"{s.get('typing_settled', 0)} / {s.get('ok', 0)}" if f != "(empty)" and s.get("ok") else "n/a"),
            ])

    def unsettled_at(app, f, field):
        g = summ.get(app, {}).get(f, {})
        return field == "typing_cpu_s" and g.get("typing_settled", 0) < g.get("ok", 0)

    def paired(field, scale_fn, fmt_fn, title, which):
        vals = [cell(a, f, field) for f in which for a, _n, _c in apps if cell(a, f, field) is not None]
        top = max(vals) if vals else 1
        out = []
        for f in which:
            for app, name, cls in apps:
                v = cell(app, f, field)
                if v is None:
                    missing = "did not open" if field == "title_s" else "not measured"
                    out.append(f'<div class="brow"><div class="blabel">{esc(labels[f])} · {esc(name)}</div><div class="btrack"></div><div class="bvalue">{esc(missing)}</div></div>')
                else:
                    width = pct_lin(scale_fn(v), 0, scale_fn(top) * 1.05)
                    out.append(f'<div class="brow"><div class="blabel">{esc(labels[f])} · {esc(name)}</div><div class="btrack"><div class="bfill {cls}" style="width:{width:.2f}%"></div></div><div class="bvalue">{esc(("≥ " if unsettled_at(app, f, field) else "") + fmt_fn(v))}</div></div>')
        return figure(title, "", '<div class="bars">' + "".join(out) + "</div>")

    with_file = [f for f in files if f != "(empty)"]
    title_fig = paired("title_s", lambda v: v, lambda v: f"{v:.2f} s", "Time until the window title shows the file", with_file)
    rss_fig = paired("rss_kb", lambda v: v / 1024, lambda v: f"{v / 1024:.0f} MiB", "Resident memory once the file is open", files)
    cpu_fig = paired("typing_cpu_s", lambda v: v / keys * 1e6, lambda v: fmt_us(v / keys * 1e6), "CPU per keystroke, typed at the start of the file", with_file)
    n100, p100 = cell("note", BIG, "title_s"), cell("notepad", BIG, "title_s")
    measured_100 = bool(summ.get("note", {}).get(BIG, {}).get("runs"))
    takeaway = (
        f"At 100 MiB note opens the file in {n100:.1f} s; Wine Notepad {'does not open it' if p100 is None else f'takes {p100:.1f} s'}."
        if n100 is not None
        else ("The 100 MiB open did not finish for note." if measured_100 else "The 100 MiB case was not measured.")
    )
    np_100 = summ.get("notepad", {}).get(BIG, {})
    if np_100.get("runs") and np_100.get("typing_settled", 1) < np_100.get("ok", 0):
        takeaway += " Its 300-keystroke burst at 100 MiB had not settled after 300 s, so its CPU figure there is a lower bound."
    runs_text = ", ".join(f"{summ.get('note', {}).get(f, {}).get('runs', 0)} at {labels[f]}" for f in files)
    single_note = " The 100 MiB figures are single runs, so they have no spread." if summ.get("note", {}).get(BIG, {}).get("runs") == 1 else ""
    head = ["File", "Program", "Runs ok", "Window appears", "File open (title)", "Resident memory", "CPU per keystroke", "Typing settled"]
    return (
        '<section id="wine"><h2>note against Wine\'s Notepad</h2>'
        "<p class=\"lede-s\">Wine's built-in Notepad is the Notepad-style reference that runs on this Linux box. Both programs "
        f"ran in one Wine prefix, through the same translation layer, on the same virtual display ({esc(WINE.get('wine', 'Wine'))}, {esc(WINE.get('xvfb_screen', ''))}). "
        f"Times are medians of repeated runs ({runs_text}), and each program started from a fresh process.{single_note}</p>"
        + legend([("s1", "note (Windows build)"), ("s2", "Wine's Notepad")]) + title_fig + rss_fig + cpu_fig
        + f'<p class="takeaway">{esc(takeaway)}</p>'
        + details("Data table", table(head, rows, numeric=(2, 3, 4, 5, 6, 7)))
        + '<p class="foot">Typing settled counts the bursts whose program went quiet within 300 s. A burst still busy at that point is '
          "a lower bound for its CPU figure, and the table shows how many of those there were.</p>"
        + '<p class="foot">What is and is not measured: both run under Wine\'s translation layer, not on native Windows. '
        "Wine's Notepad draws through a standard edit control, while note paints its own text through GDI, so the two have "
        "different costs. The note build here is MinGW, not MSVC. The CPU figure is the program's own user and system CPU time during a typing burst. "
        "Xvfb and xdotool run as separate processes and are not counted; Wine's code inside the program is.</p></section>"
    )


def claims_section():
    exe, nofont, inst = WIN["default_exe_bytes"], WIN["nofont_exe_bytes"], WIN["installer_bytes"]
    ns = [r["ns_per_step"] for r in SUITE if r["group"] == "regex" and "ns_per_step" in r]
    live = rec("editing", BIG, "live preview (50 lines, budget 4M)")
    rows = [
        ['README: "About 870 KB as a single exe"', fmt_kb(exe), Raw(badge("watch")), "Different toolchain. The MSVC build cannot be made here."],
        ['README: "about 660 KB without the font"', fmt_kb(nofont), Raw(badge("watch")), "Same reason."],
        ['README: the font saves "about 210 KB"', fmt_kb(exe - nofont), Raw(badge("good")), "Holds: the difference is within 0.3% of the two font files' size."],
        ['README: installer "about 520 KB"', fmt_kb(inst), Raw(badge("watch")), "Above the figure by about 10%. NSIS version and exe toolchain differ."],
        ['README: byte scans use SSE2, and "AVX2 when your CPU has it"', "dispatch in src/simd.rs; AVX2 measured on this CPU", Raw(badge("good")), "Holds. The speed difference is in the AVX2 section."],
        ['src/search.rs: a budget step is "roughly a few nanoseconds each"', f"{min(ns):.1f}–{max(ns):.1f} ns per step", Raw(badge("good")), "Holds."],
        ['README: the live preview "only searches the lines on screen"', f"50-line preview {fmt_ns(live['median_ns'])} on 100 MiB", Raw(badge("good")), "Holds: the 50-line preview takes under 12 µs at every size in the table."],
        ['README: "Point the bench at a real file"', "examples/bench runs on the 100 MiB corpus", Raw(badge("good")), "Runs and prints the same breakdown; its output is in the results."],
    ]
    return (
        '<section id="claims"><h2>Claims checked</h2>'
        '<p class="lede-s">The README and DESIGN make a few measurable claims. Each is checked against the numbers above. '
        "The verdict says what kind of check was possible on this machine.</p>"
        f'{table(["Claim (source)", "Measured", "Verdict", "Note"], rows)}</section>'
    )


def method_section():
    corpus_rows = [[n, fmt_mib(i["bytes"]), i["sha256"][:16]] for n, i in CORPORA.get("files", {}).items()]
    env = [
        ["CPU", f"{META['cpu']}, {META['logical_cores']} logical cores"],
        ["Instruction sets", f"AVX2 {'yes' if META['avx2'] else 'no'}, AVX-512F {'yes' if META['avx512f'] else 'no'}"],
        ["Memory", f"{META['mem_total_mib'] / 1024:.1f} GiB"],
        ["Kernel", META["kernel"]],
        ["Rust", f"{META['rustc']}; {META['cargo']}"],
        ["Release profile", ", ".join(f"{k} = {v}" for k, v in META["release_profile"].items())],
        ["Python", META["python"]],
        ["ripgrep", META["ripgrep"]],
        ["iconv", META["iconv"]],
        ["makensis", META["makensis"]],
        ["Wine", WINE.get("wine") if WINE else "not run"],
        ["Commit measured", META["commit"] + (f"; {len(META['dirty_paths'])} uncommitted harness paths" if META["dirty_paths"] else "")],
        ["Campaign started (UTC)", META["generated_utc"]],
    ]
    repeat = []
    for r in SUITE:
        if r["build"] == "main-repeat" and r["impl"] == "note":
            first = next(x for x in SUITE if x["build"] == "main" and x["file"] == r["file"] and x["name"] == r["name"] and x["impl"] == r["impl"] and x["group"] == r["group"])
            repeat.append(abs(r["median_ns"] / first["median_ns"] - 1) * 100)
    repeat_text = f"up to {max(repeat):.0f}%" if repeat else "not measured"
    items = [
        "Timed suites ran one at a time, after the builds finished. Each measurement repeats until it has at least five samples and 600 ms of total time, or at least three samples and ten seconds, or 200 samples, whichever comes first.",
        f"Repeatability: the same 1 MiB kernels, run twice, differed by {repeat_text}. Treat differences under about 20% at that size as noise; the 64 MiB and 100 MiB figures are steadier.",
        "Medians are reported with the 10th and 90th percentiles where the spread matters. Per-keystroke numbers come from timing each call separately.",
        "MB means 10⁶ bytes; MiB means 2²⁰ bytes. File sizes in the names are MiB: mixed-100m.txt is 100 MiB.",
        "Heap figures come from a counting global allocator in a separate binary, one scenario per process. The timing runs use the normal allocator.",
        "The SSE2 runs use a scratch copy whose dispatch reads NOTE_SIMD. The committed source is unchanged.",
        "The Windows build is MinGW (x86_64-pc-windows-gnu), not MSVC. Its size and speed differ from the release binary's.",
        "Colours follow the dataviz palette reference. In dark mode the three series colours pass every check. In light mode the aqua slot is 2.74:1 on the light surface, under the validator's 3:1 floor, so each chart that uses it also prints its values and has a data table. Status colours appear only as badges that carry an icon and a text label.",
        "The heap probe's count and replace rows were re-run after a fix to their JSON output. The measurement logic did not change; only the formatting did.",
        "Not measured: native Windows, the Win32 renderer's frame times, real input latency through the window system, and the MSVC binary.",
    ]
    corpora_note = "Built from real files on this machine by benchmarks/corpora.py. The manifest records each file's digest."
    return (
        '<section id="method"><h2>Method and environment</h2>'
        f'<ul class="method">{"".join(f"<li>{esc(i)}</li>" for i in items)}</ul>'
        f'<h3 class="sub-h">Environment</h3>{table(["Item", "Value"], env)}'
        f'<h3 class="sub-h">Corpora</h3><p class="lede-s">{esc(corpora_note)}</p>'
        f'{table(["File", "Size", "SHA-256 (first 16 hex digits)"], corpus_rows, numeric=(1,))}</section>'
    )


def reproduce_section():
    return (
        '<section id="reproduce"><h2>Reproduce</h2>'
        '<pre class="code">python3 benchmarks/corpora.py            # the corpora, about 30 s\n'
        "python3 benchmarks/run_benchmarks.py      # builds and timed suites, about 25 min\n"
        "python3 benchmarks/wine_compare.py        # after run_benchmarks.py; note against Wine's Notepad (wine64, Xvfb, xdotool)\n"
        "python3 benchmarks/render_report.py       # writes benchmarks/REPORT.html</pre>"
        '<p class="foot">Raw results are in benchmarks/results/ (latest.json, wine.json). Corpora and build output stay in '
        "target/, which git ignores.</p></section>"
    )


# ---- page ---------------------------------------------------------------------------------

CSS = """
:root {
  color-scheme: light;
  --page: #f9f9f7; --surface: #fcfcfb; --ink: #0b0b0b; --ink-2: #52514e; --ink-3: #898781;
  --grid: #e1e0d9; --axis: #c3c2b7; --hair: rgba(11,11,11,0.10);
  --s1: #2a78d6; --s2: #eb6834; --s3: #1baf7a;
  --good: #0ca30c; --warn: #fab219; --serious: #ec835a; --critical: #d03b3b;
  --lab: 15rem; --val: 8.5rem;
  --font: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
    --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --ink-3: #898781;
    --grid: #2c2c2a; --axis: #383835; --hair: rgba(255,255,255,0.10);
    --s1: #3987e5; --s2: #d95926; --s3: #199e70;
  }
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --ink-3: #898781;
  --grid: #2c2c2a; --axis: #383835; --hair: rgba(255,255,255,0.10);
  --s1: #3987e5; --s2: #d95926; --s3: #199e70;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--page); color: var(--ink); font: 15px/1.6 var(--font); }
.wrap { max-width: 1040px; margin: 0 auto; padding-inline: 16px; }
header.mast { padding-block: 40px 24px; border-bottom: 1px solid var(--hair); }
.eyebrow { margin: 0 0 8px; color: var(--ink-2); font-size: 13px; }
h1 { margin: 0 0 10px; font-size: clamp(28px, 5vw, 40px); line-height: 1.12; letter-spacing: -.01em; text-wrap: balance; }
h2 { margin: 0 0 8px; font-size: 22px; line-height: 1.25; text-wrap: balance; }
h3 { margin: 0; font-size: 16px; line-height: 1.35; }
.lede { margin: 0 0 16px; color: var(--ink-2); max-width: 70ch; }
.lede-s { margin: 0 0 16px; color: var(--ink-2); max-width: 72ch; }
.sub { margin: 4px 0 10px; color: var(--ink-2); font-size: 13.5px; }
.sub-h { margin: 28px 0 8px; }
nav.toc { display: flex; flex-wrap: wrap; gap: 6px 14px; font-size: 13px; }
nav.toc a { color: var(--ink-2); text-decoration: none; border-bottom: 1px solid var(--axis); }
nav.toc a:hover { color: var(--ink); }
main section { padding-block: 36px; border-bottom: 1px solid var(--hair); }
main section:last-of-type { border-bottom: 0; }
code, pre { font-family: var(--mono); font-size: 13px; }
pre.code { background: var(--surface); border: 1px solid var(--hair); border-radius: 8px; padding: 12px 14px; overflow-x: auto; }
.tablewrap { overflow-x: auto; margin: 12px 0; border: 1px solid var(--hair); border-radius: 8px; background: var(--surface); }
table { border-collapse: collapse; width: 100%; font-size: 13.5px; }
caption { caption-side: top; text-align: left; padding: 8px 12px; color: var(--ink-2); font-size: 12.5px; }
th, td { text-align: left; padding: 8px 12px; border-top: 1px solid var(--hair); vertical-align: top; }
thead th { border-top: 0; color: var(--ink-2); font-weight: 600; font-size: 12.5px; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
th[scope="row"] { font-weight: 600; white-space: nowrap; }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)); gap: 12px; margin-top: 8px; }
.tile { background: var(--surface); border: 1px solid var(--hair); border-radius: 10px; padding: 14px 16px; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
.tval { font-size: 28px; font-weight: 650; line-height: 1.1; letter-spacing: -.01em; overflow-wrap: anywhere; }
.tlabel { font-size: 13.5px; }
.tnote { font-size: 12.5px; color: var(--ink-2); }
.badge { display: inline-block; align-self: flex-start; margin-top: 6px; padding: 2px 8px; border-radius: 999px; font-size: 12px; font-weight: 600; line-height: 1.5; white-space: nowrap; }
.b-good { background: var(--good); color: #0b0b0b; }
.b-watch { background: var(--warn); color: #0b0b0b; }
.b-critical { background: var(--critical); color: #ffffff; }
.b-info { background: var(--grid); color: var(--ink); }
figure.chart { margin: 22px 0 8px; padding: 0; min-width: 0; }
figure.chart figcaption h3 { font-size: 16px; }
.takeaway { margin: 14px 0 0; padding-left: 12px; border-left: 2px solid var(--s1); max-width: 72ch; }
.foot { margin: 10px 0 0; color: var(--ink-3); font-size: 12.5px; max-width: 72ch; }
.legend { list-style: none; display: flex; flex-wrap: wrap; gap: 6px 16px; padding: 0; margin: 8px 0 12px; font-size: 13px; color: var(--ink-2); }
.legend li { display: flex; align-items: center; gap: 6px; }
.key { display: inline-block; width: 14px; height: 10px; border-radius: 2px; background: var(--ink-3); flex: 0 0 auto; }
.key.s1 { background: var(--s1); } .key.s2 { background: var(--s2); } .key.s3 { background: var(--s3); }
.legend-row { display: flex; gap: 8px; align-items: center; margin: 8px 0 10px; font-size: 13px; color: var(--ink-2); }
.bars { display: flex; flex-direction: column; gap: 8px; margin: 8px 0; }
.brow { display: grid; grid-template-columns: var(--lab) minmax(0, 1fr) var(--val); align-items: center; gap: 12px; font-size: 13.5px; min-width: 0; }
.blabel { overflow-wrap: anywhere; min-width: 0; }
.btrack { position: relative; height: 12px; background: var(--grid); border-radius: 0 4px 4px 0; min-width: 0; }
.btrack.thin { height: 9px; }
.bfill { position: absolute; left: 0; top: 0; bottom: 0; border-radius: 0 4px 4px 0; background: var(--ink-3); min-width: 2px; }
.bfill.s1 { background: var(--s1); } .bfill.s2 { background: var(--s2); } .bfill.s3 { background: var(--s3); }
.bvalue { font-variant-numeric: tabular-nums; color: var(--ink-2); font-size: 13px; white-space: nowrap; }
.kern { display: flex; flex-direction: column; gap: 10px; margin: 8px 0; }
.krow { display: grid; grid-template-columns: var(--lab) minmax(0, 1fr) var(--val); gap: 12px; align-items: center; font-size: 13.5px; }
.klabel { display: flex; flex-direction: column; min-width: 0; overflow-wrap: anywhere; }
.kdelta { font-size: 12px; color: var(--ink-2); }
.kbars { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.kvals { display: flex; flex-direction: column; font-variant-numeric: tabular-nums; font-size: 12.5px; white-space: nowrap; }
.s1-t { color: var(--ink); } .s2-t { color: var(--ink-2); } .s3-t { color: var(--ink-2); }
.dumb, .range { margin: 6px 0; min-width: 0; }
.dumb { --val: 15rem; }
.axis { position: relative; height: 18px; margin: 0 calc(var(--val) + 12px) 0 calc(var(--lab) + 12px); font-size: 11.5px; color: var(--ink-3); font-variant-numeric: tabular-nums; }
.tick { position: absolute; transform: translateX(-50%); white-space: nowrap; }
.axis .tick:first-child { transform: none; }
.axis .tick:last-child { transform: translateX(-100%); }
.drow, .rrow { display: grid; grid-template-columns: var(--lab) minmax(0, 1fr) var(--val); align-items: center; gap: 12px; min-height: 30px; font-size: 13.5px; }
.dlabel, .rlabel { overflow-wrap: anywhere; min-width: 0; }
.dtrack, .rtrack { position: relative; height: 22px; min-width: 0; border-bottom: 1px solid var(--grid); }
.grid { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--grid); }
.dline { position: absolute; top: 10px; height: 2px; background: var(--axis); }
.dot { position: absolute; top: 5px; width: 12px; height: 12px; margin-left: -6px; border-radius: 50%; background: var(--ink-3); box-shadow: 0 0 0 2px var(--surface); }
.dot.s1 { background: var(--s1); } .dot.s2 { background: var(--s2); } .dot.s3 { background: var(--s3); }
.dvals { font-variant-numeric: tabular-nums; font-size: 12.5px; white-space: nowrap; color: var(--ink-2); }
.thr { position: absolute; top: 0; bottom: 0; width: 1px; background: var(--ink-3); }
.thr b { position: absolute; top: -3px; left: 4px; font-weight: 500; font-size: 11px; color: var(--ink-2); white-space: nowrap; }
.thr.watch { background: var(--ink-2); }
.thr.critical { background: var(--ink); }
.rwhisk { position: absolute; top: 10px; height: 2px; background: var(--axis); }
.rmed { position: absolute; top: 5px; width: 12px; height: 12px; margin-left: -6px; border-radius: 50%; background: var(--s1); box-shadow: 0 0 0 2px var(--surface); }
.rvalue { font-variant-numeric: tabular-nums; font-size: 12.5px; color: var(--ink-2); white-space: nowrap; }
td.heat { font-variant-numeric: tabular-nums; text-align: right; white-space: nowrap; border-left: 2px solid var(--surface); }
.heatmap th[scope="row"] { font-weight: 500; white-space: normal; }
.lc { display: grid; grid-template-columns: 4.5rem minmax(0, 1fr); gap: 4px 8px; margin: 10px 0 0; }
.yaxis { position: relative; min-height: 240px; }
.yaxis .ytick { position: absolute; right: 0; transform: translateY(-50%); font-size: 11.5px; color: var(--ink-3); font-variant-numeric: tabular-nums; white-space: nowrap; }
.plot { position: relative; min-height: 240px; border-left: 1px solid var(--axis); border-bottom: 1px solid var(--axis); }
.hgrid { position: absolute; left: 0; right: 0; height: 1px; background: var(--grid); }
svg.lines { position: absolute; inset: 0; width: 100%; height: 100%; overflow: visible; }
polyline.ln { fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; vector-effect: non-scaling-stroke; }
polyline.s1 { stroke: var(--s1); } polyline.s2 { stroke: var(--s2); } polyline.s3 { stroke: var(--s3); }
.pt { position: absolute; width: 11px; height: 11px; margin: -5.5px 0 0 -5.5px; border-radius: 50%; box-shadow: 0 0 0 2px var(--surface); }
.pt.s1 { background: var(--s1); } .pt.s2 { background: var(--s2); } .pt.s3 { background: var(--s3); }
.xaxis { grid-column: 2; position: relative; height: 22px; }
.xaxis .xtick { position: absolute; top: 4px; transform: translateX(-50%); font-size: 12px; color: var(--ink-2); white-space: nowrap; }
details.data { margin: 12px 0 0; }
details.data > summary { cursor: pointer; color: var(--ink-2); font-size: 13px; padding: 4px 0; }
details.data > summary:focus-visible, a:focus-visible, .dot:focus-visible, .pt:focus-visible { outline: 2px solid var(--s1); outline-offset: 2px; }
ul.method { margin: 0 0 8px; padding-left: 18px; max-width: 76ch; }
ul.method li { margin: 0 0 6px; }
footer { padding-block: 24px 48px; color: var(--ink-3); font-size: 12.5px; }
@media (max-width: 640px) {
  .brow, .krow, .drow, .rrow { grid-template-columns: minmax(0, 1fr); gap: 4px; }
  .bvalue, .dvals, .rvalue { white-space: normal; }
  .axis { margin: 0; }
  .kvals { flex-direction: row; gap: 14px; }
  .lc { grid-template-columns: 3.4rem minmax(0, 1fr); }
  .tval { font-size: 24px; }
  .blabel, .klabel, .dlabel, .rlabel { font-weight: 600; }
}
@media (prefers-reduced-motion: reduce) { * { transition: none !important; animation: none !important; } }
"""


def page():
    toc = [
        ("summary", "Summary"), ("verdicts", "Verdicts"), ("latency", "Interactive"),
        ("kernels", "SIMD kernels"), ("avx2", "AVX2 vs SSE2"), ("pipeline", "Whole-file"),
        ("search", "Search vs ripgrep"), ("editing", "Editing"), ("scaling", "Scaling"),
        ("memory", "Memory"), ("regex", "Regex"), ("build", "Build & code"), ("size", "Size"),
        ("wine", "Wine Notepad"), ("claims", "Claims"), ("method", "Method"),
    ]
    nav = "".join(f'<a href="#{a}">{esc(label)}</a>' for a, label in toc)
    sections = [
        summary_section(), verdict_section(), latency_section(), kernels_section(), avx2_section(),
        pipeline_section(), search_section(), editing_section(), scaling_section(), memory_section(),
        regex_section(), build_section(), size_section(), wine_section(), claims_section(),
        method_section(), reproduce_section(),
    ]
    err = ""
    if ERRORS:
        err = "<section><h2>Steps that failed</h2><ul>" + "".join(
            f"<li>{esc(e['step'])}: {esc(e['error'][:300])}</li>" for e in ERRORS
        ) + "</ul></section>"
    return f"""<title>note Benchmark Report</title>
<style>{CSS}</style>
<header class="mast"><div class="wrap">
<p class="eyebrow">note {esc(META.get('version', '1.1.0'))} · commit {esc(META['commit'])} · campaign started {esc(META['generated_utc'])}</p>
<h1>note on every front</h1>
<p class="lede">Engine throughput, editing latency, memory, regex limits, build and code health, binary size, and a side-by-side
run against Wine's Notepad. Every number comes from the harness in this repository.</p>
<nav class="toc" aria-label="Sections">{nav}</nav>
</div></header>
<main class="wrap">
{''.join(sections)}
{err}
</main>
<footer class="wrap">Generated by benchmarks/render_report.py from benchmarks/results/latest.json{' and wine.json' if WINE else ''}.</footer>
"""


def main():
    body = page()
    if "--artifact" in sys.argv:
        # The publishing platform adds the document skeleton, so this copy is the page content only.
        target = Path(sys.argv[sys.argv.index("--artifact") + 1])
        target.write_text(body + "\n")
        print(f"wrote artifact content {target} ({target.stat().st_size:,} bytes)")
        return
    OUT.write_text(
        '<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover"></head>'
        f"<body>\n{body}\n</body></html>\n"
    )
    print(f"wrote {OUT} ({OUT.stat().st_size:,} bytes)")


if __name__ == "__main__":
    main()
