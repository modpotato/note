#!/usr/bin/env python3
"""Runs the whole benchmark campaign and writes benchmarks/results/latest.json.

    python3 benchmarks/run_benchmarks.py --check    which tools are present, and what they enable
    python3 benchmarks/run_benchmarks.py            everything (about 25 minutes)
    python3 benchmarks/run_benchmarks.py --quick    skips the 100 MiB encoding variants

Before anything runs, the corpora in target/bench-data must match the digests committed in
benchmarks/results/corpora.json. Missing corpora are generated; mismatched ones stop the run.
Order matters: every build happens before the first timed run, and the timed runs go one at a
time, so no compile competes with a measurement. A failing step is recorded in the results
rather than aborting the campaign. Then run wine_compare.py and render_report.py.
"""

import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path

import corpora  # the seeded corpora and their digests, in this directory

REPO = Path(__file__).resolve().parent.parent
DATA = Path(os.environ.get("BENCH_DATA", REPO / "target" / "bench-data"))
SCRATCH = REPO / "target" / "bench-scratch"
ABLATION = REPO / "target" / "bench-ablation"
RESULTS = REPO / "benchmarks" / "results"
EXAMPLES = REPO / "target" / "release" / "examples"
WIN = "x86_64-pc-windows-gnu"
QUICK = "--quick" in sys.argv

FIND_ALL = [
    # (label, ripgrep flags, pattern) for the queries that ripgrep can run the same way.
    ("plain: return", ["-F"], "return"),
    ("plain: return, ignore case", ["-F", "-i"], "return"),
    ("regex: \\bfn\\s+\\w+", [], "\\bfn\\s+\\w+"),
    ("regex: [A-Z][a-z]+Error", [], "[A-Z][a-z]+Error"),
    ("regex: ^\\s*//.*$", [], "^\\s*//.*$"),
    ("regex: \\b(unsafe|async|await|dyn)\\b", [], "\\b(unsafe|async|await|dyn)\\b"),
]


def log(msg):
    print(f"[{datetime.now().strftime('%H:%M:%S')}] {msg}", flush=True)


def sh(cmd, cwd=REPO, env=None, timeout=3600, discard=False):
    """Runs a command. Returns (returncode, stdout, stderr, seconds)."""
    full_env = {**os.environ, **(env or {})}
    t0 = time.perf_counter()
    p = subprocess.run(
        cmd,
        cwd=cwd,
        env=full_env,
        capture_output=not discard,
        stdout=subprocess.DEVNULL if discard else None,
        timeout=timeout,
    )
    dt = time.perf_counter() - t0
    out = "" if discard else p.stdout.decode("utf-8", "replace")
    err = "" if discard else p.stderr.decode("utf-8", "replace")
    return p.returncode, out, err, dt


def must(cmd, **kw):
    rc, out, err, dt = sh(cmd, **kw)
    if rc != 0:
        raise RuntimeError(f"{' '.join(map(str, cmd[:4]))} exited {rc}: {(err or out)[-1500:]}")
    return out, err, dt


def tool_version(cmd):
    try:
        rc, out, err, _ = sh(cmd, timeout=30)
        text = (out or err).strip().splitlines()
        return text[0] if text else "unknown"
    except (OSError, subprocess.TimeoutExpired):
        return "not installed"


def environment():
    cpuinfo = Path("/proc/cpuinfo").read_text()
    flags = set(re.search(r"flags\s*:\s*(.*)", cpuinfo).group(1).split())
    meminfo = Path("/proc/meminfo").read_text()
    commit = sh(["git", "rev-parse", "--short", "HEAD"])[1].strip()
    dirty = sh(["git", "status", "--porcelain"])[1].splitlines()
    return {
        "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "commit": commit,
        "dirty_paths": dirty,
        "cpu": re.search(r"model name\s*:\s*(.*)", cpuinfo).group(1).strip(),
        "logical_cores": cpuinfo.count("processor\t:"),
        "avx2": "avx2" in flags,
        "avx512f": "avx512f" in flags,
        "mem_total_mib": int(re.search(r"MemTotal:\s+(\d+)", meminfo).group(1)) // 1024,
        "kernel": platform.release(),
        "rustc": tool_version(["rustc", "-V"]),
        "cargo": tool_version(["cargo", "-V"]),
        "python": platform.python_version(),
        "ripgrep": tool_version(["rg", "--version"]),
        "iconv": tool_version(["iconv", "--version"]),
        "makensis": tool_version(["makensis", "-VERSION"]),
        "release_profile": tomllib.loads((REPO / "Cargo.toml").read_text())["profile"]["release"],
    }


# ---- preflight ----------------------------------------------------------------------------

REQUIRED = {  # tool -> how to get it
    "cargo": "install Rust with rustup: https://rustup.rs",
    "rustc": "install Rust with rustup: https://rustup.rs",
    "rg": "apt install ripgrep",
    "iconv": "apt install libc-bin",
    "wc": "coreutils (preinstalled on Linux)",
    "cat": "coreutils (preinstalled on Linux)",
    "git": "apt install git",
}
OPTIONAL = {  # tool -> (what it enables, how to get it)
    "x86_64-w64-mingw32-gcc": ("the Windows build and its sizes", "apt install gcc-mingw-w64-x86-64"),
    "makensis": ("the installer size", "apt install nsis"),
}


def preflight():
    """What this machine can run, as (rows, missing required tools). Optional gaps only skip phases."""
    rows, missing = [], []
    for name, hint in REQUIRED.items():
        present = shutil.which(name) is not None
        rows.append({"tool": name, "present": present, "version": tool_version([name, "--version"]) if present else None, "hint": hint})
        if not present:
            missing.append(name)
    for name, (enables, hint) in OPTIONAL.items():
        rows.append({"tool": name, "present": shutil.which(name) is not None, "enables": enables, "hint": hint})
    installed = sh(["rustup", "target", "list", "--installed"], timeout=60)[1].split() if shutil.which("rustup") else []
    rows.append({"tool": f"rustup target {WIN}", "present": WIN in installed, "enables": "the Windows build", "hint": f"rustup target add {WIN}"})
    return rows, missing


def print_rows(rows):
    for row in rows:
        if row["present"]:
            print(f"  ok       {row['tool']:<28} {row.get('version') or row.get('enables', '')}")
        else:
            print(f"  MISSING  {row['tool']:<28} {row.get('enables', 'required')}; {row['hint']}")


def check_only():
    rows, missing = preflight()
    print_rows(rows)
    if (DATA / "manifest.json").exists():
        problems = corpora.check_files()
        print("  corpora " + ("match benchmarks/results/corpora.json" if not problems else "differ: " + "; ".join(problems)))
    else:
        print("  corpora not built yet; the run builds them")
    if missing:
        print(f"\nmissing required tools: {', '.join(missing)}")
        return 1
    return 0


# ---- code health ----------------------------------------------------------------------


def code_health():
    health = {}

    _, out, err, _ = sh(["cargo", "test", "--release"], timeout=1800)
    passed = failed = ignored = 0
    seconds = 0.0
    for m in re.finditer(
        r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored.*?finished in ([\d.]+)s",
        out,
    ):
        passed += int(m.group(1))
        failed += int(m.group(2))
        ignored += int(m.group(3))
        seconds += float(m.group(4))
    _, listing, _, _ = sh(["cargo", "test", "--release", "--", "--list"], timeout=600)
    per_module = {}
    for line in listing.splitlines():
        m = re.match(r"^([a-z_]+)::.*: test$", line)
        if m:
            per_module[m.group(1)] = per_module.get(m.group(1), 0) + 1
    health["tests"] = {
        "passed": passed,
        "failed": failed,
        "ignored": ignored,
        "run_seconds": seconds,
        "per_module": dict(sorted(per_module.items())),
    }
    log(f"tests: {passed} passed, {failed} failed in {seconds:.2f}s")

    _, out, err, dt = sh(["cargo", "clippy", "--all-targets", "--message-format=json"], timeout=1800)
    lints = []
    for line in out.splitlines():
        if not line.startswith("{"):
            continue
        obj = json.loads(line)
        if obj.get("reason") != "compiler-message":
            continue
        msg = obj["message"]
        if msg["level"] not in ("warning", "error"):
            continue
        code = (msg.get("code") or {}).get("code")
        if code is None:
            continue
        span = next((s for s in msg["spans"] if s.get("is_primary")), {})
        lints.append(
            {
                "level": msg["level"],
                "code": code,
                "message": msg["message"],
                "file": span.get("file_name", ""),
                "line": span.get("line_start", 0),
            }
        )
    health["clippy"] = {"diagnostics": lints, "seconds": dt}
    log(f"clippy: {len(lints)} coded diagnostics")

    _, out, err, _ = sh(["cargo", "fmt", "--check"], timeout=600)
    per_file = {}
    for m in re.finditer(r"^Diff in (\S+?)(?::\d+)?:", out, re.M):
        path = os.path.relpath(m.group(1), REPO)
        per_file[path] = per_file.get(path, 0) + 1
    health["rustfmt"] = {
        "files_with_diffs": len(per_file),
        "hunks": sum(per_file.values()),
        "per_file": dict(sorted(per_file.items())),
    }
    log(f"rustfmt: {len(per_file)} files differ")

    loc, unsafe = {}, {}
    for path in sorted([*REPO.glob("src/**/*.rs"), *REPO.glob("examples/*.rs"), REPO / "build.rs"]):
        rel = str(path.relative_to(REPO))
        text = path.read_text(encoding="utf-8")
        loc[rel] = text.count("\n")
        n_unsafe = len(re.findall(r"\bunsafe\b", text))
        if n_unsafe:
            unsafe[rel] = n_unsafe
    health["loc"] = loc
    health["unsafe_keyword_hits"] = unsafe

    cargo_toml = tomllib.loads((REPO / "Cargo.toml").read_text())
    target_deps = cargo_toml.get("target", {})
    windows_only = sorted(
        name for t in target_deps.values() for name in t.get("dependencies", {})
    )
    lock = (REPO / "Cargo.lock").read_text()
    health["dependencies"] = {
        "runtime": sorted(cargo_toml.get("dependencies", {})),
        "build": sorted(cargo_toml.get("build-dependencies", {})),
        "windows_only": windows_only,
        "packages_in_lockfile": lock.count("[[package]]"),
    }
    return health


def build_times():
    """Cold and incremental release builds, and a cold debug build. The incremental build
    touches src/editor.rs, which changes only its modification time."""
    times = {}
    shutil.rmtree(SCRATCH / "cold-release", ignore_errors=True)
    _, _, dt = must(
        ["cargo", "build", "--release", "--locked"],
        env={"CARGO_TARGET_DIR": str(SCRATCH / "cold-release")},
        timeout=1800,
    )
    times["cold_release_s"] = dt
    log(f"cold release build: {dt:.1f}s")

    (REPO / "src" / "editor.rs").touch()
    _, _, dt = must(["cargo", "build", "--release", "--locked"], timeout=1800)
    times["incremental_release_s"] = dt
    log(f"incremental release build (touch src/editor.rs): {dt:.1f}s")

    shutil.rmtree(SCRATCH / "cold-debug", ignore_errors=True)
    _, _, dt = must(
        ["cargo", "build", "--locked"],
        env={"CARGO_TARGET_DIR": str(SCRATCH / "cold-debug")},
        timeout=1800,
    )
    times["cold_debug_s"] = dt
    log(f"cold debug build: {dt:.1f}s")
    return times


def windows_sizes():
    out = {"available": False}
    if not shutil.which("x86_64-w64-mingw32-gcc") or not shutil.which("makensis"):
        out["reason"] = "MinGW or NSIS missing"
        return out
    env = {"CARGO_TARGET_DIR": str(SCRATCH / "win")}
    shutil.rmtree(SCRATCH / "win", ignore_errors=True)
    _, _, cold = must(["cargo", "build", "--release", "--locked", "--target", WIN], env=env, timeout=1800)
    default_exe = SCRATCH / "note-default.exe"
    shutil.copy(SCRATCH / "win" / WIN / "release" / "note.exe", default_exe)
    _, _, nofont = must(
        ["cargo", "build", "--release", "--locked", "--target", WIN, "--no-default-features"],
        env=env,
        timeout=1800,
    )
    nofont_exe = SCRATCH / "note-nofont.exe"
    shutil.copy(SCRATCH / "win" / WIN / "release" / "note.exe", nofont_exe)

    installer = SCRATCH / "note-setup.exe"
    installer.unlink(missing_ok=True)
    must(
        [
            "makensis",
            "-V3",
            f"-DVERSION={tomllib.loads((REPO / 'Cargo.toml').read_text())['package']['version']}",
            f"-DEXE={default_exe}",
            f"-DOUTFILE={installer}",
            str(REPO / "installer" / "note.nsi"),
        ],
        timeout=600,
    )
    out.update(
        {
            "available": True,
            "default_exe_bytes": default_exe.stat().st_size,
            "nofont_exe_bytes": nofont_exe.stat().st_size,
            "installer_bytes": installer.stat().st_size,
            "cold_build_s": cold,
            "nofont_build_s": nofont,
            "font_files_bytes": {
                p.name: p.stat().st_size for p in sorted((REPO / "assets" / "fonts").glob("*.ttf"))
            },
        }
    )
    log(
        f"windows: default {out['default_exe_bytes']:,} B, no font {out['nofont_exe_bytes']:,} B, "
        f"installer {out['installer_bytes']:,} B"
    )
    return out


# ---- timed runs --------------------------------------------------------------------------


def suite_run(binary, args, file_label, build="main", simd="auto", env=None):
    _, out, err, _ = sh([str(binary), *args], env=env, timeout=7200)
    recs = [json.loads(line) for line in out.splitlines() if line.startswith("{")]
    if not recs:
        raise RuntimeError(f"{binary.name} {args}: no results; stderr: {err[-800:]}")
    for r in recs:
        r.update({"file": file_label, "build": build, "simd": simd, "mode": args[0]})
    log(f"  {binary.name} {args[0]} {file_label} [{build}/{simd}]: {len(recs)} measurements")
    return recs


def memprobe_run(scenario, file_label, *extra):
    binary = EXAMPLES / "memprobe"
    _, out, err, _ = sh([str(binary), scenario, str(DATA / file_label), *extra], timeout=1800)
    line = next((l for l in out.splitlines() if l.startswith("{")), None)
    if line is None:
        raise RuntimeError(f"memprobe {scenario} {file_label}: {err[-500:]}")
    rec = json.loads(line)
    rec["file"] = file_label
    return rec


def time_cmd(cmd, runs=5, discard=False):
    times = []
    for _ in range(runs):
        t0 = time.perf_counter()
        if discard:
            subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        else:
            subprocess.run(cmd, capture_output=True, check=True)
        times.append(time.perf_counter() - t0)
    return {"median_s": statistics.median(times), "min_s": min(times), "max_s": max(times), "runs": runs}


def comparisons(suite_records):
    """ripgrep, wc, iconv and cat on the same files as the in-process measurements."""
    big = DATA / "mixed-100m.txt"
    result = {"find_all": [], "newline_count": None, "read": None, "utf16_decode": None, "utf16_encode": None}

    note_counts = {
        r["name"]: r.get("matches")
        for r in suite_records
        if r.get("mode") == "pipeline" and r.get("file") == "mixed-100m.txt" and r.get("impl") == "note"
    }
    for label, flags, pattern in FIND_ALL:
        rg = ["rg", "--no-config", "--no-messages", "--count-matches", "--no-filename", *flags, "--", pattern, str(big)]
        rg1 = ["rg", "--no-config", "--no-messages", "--count-matches", "--no-filename", "-j1", *flags, "--", pattern, str(big)]
        count = int(subprocess.run(rg, capture_output=True, text=True, check=True).stdout.strip())
        entry = {
            "label": label,
            "rg_matches": count,
            "note_matches": note_counts.get(label),
            "rg_threads_s": time_cmd(rg, runs=5),
            "rg_single_thread_s": time_cmd(rg1, runs=5),
        }
        result["find_all"].append(entry)
        log(f"  rg '{label}': {count} matches, {entry['rg_threads_s']['median_s']:.3f}s")

    result["newline_count"] = {"wc_l_s": time_cmd(["wc", "-l", str(big)], runs=5), "bytes": big.stat().st_size}
    result["read"] = {"cat_s": time_cmd(["cat", str(big)], runs=5, discard=True), "bytes": big.stat().st_size}
    u16 = DATA / "mixed-100m-utf16le.txt"
    result["utf16_decode"] = {
        "iconv_s": time_cmd(["iconv", "-f", "UTF-16", "-t", "UTF-8", str(u16)], runs=3, discard=True),
        "bytes": u16.stat().st_size,
    }
    result["utf16_encode"] = {
        "iconv_s": time_cmd(["iconv", "-f", "UTF-8", "-t", "UTF-16LE", str(big)], runs=3, discard=True),
        "bytes": big.stat().st_size,
    }
    return result


def build_ablation():
    """A scratch copy of the same source with one change: the SIMD level can be forced to SSE2
    through NOTE_SIMD=sse2. The committed source is not touched."""
    shutil.rmtree(ABLATION / "src", ignore_errors=True)
    ABLATION.mkdir(parents=True, exist_ok=True)
    for name in ("src", "examples", "assets", ".cargo"):
        shutil.copytree(REPO / name, ABLATION / name, dirs_exist_ok=True)
    for name in ("Cargo.toml", "Cargo.lock", "build.rs"):
        shutil.copy(REPO / name, ABLATION / name)
    simd = ABLATION / "src" / "simd.rs"
    text = simd.read_text()
    old = 'let avx2 = std::is_x86_feature_detected!("avx2");'
    new = (
        'let avx2 = std::is_x86_feature_detected!("avx2") '
        '&& std::env::var("NOTE_SIMD").map_or(true, |v| v != "sse2");'
    )
    assert text.count(old) == 1, "dispatch line not found exactly once"
    simd.write_text(text.replace(old, new))
    must(
        ["cargo", "build", "--release", "--example", "suite"],
        cwd=ABLATION,
        env={"CARGO_TARGET_DIR": str(ABLATION / "target")},
        timeout=1800,
    )
    return ABLATION / "target" / "release" / "examples" / "suite"


def main():
    if "--check" in sys.argv:
        sys.exit(check_only())
    rows, missing = preflight()
    if missing:
        sys.exit(f"missing required tools: {', '.join(missing)}; see python3 benchmarks/run_benchmarks.py --check")
    RESULTS.mkdir(parents=True, exist_ok=True)
    SCRATCH.mkdir(parents=True, exist_ok=True)
    campaign = {"meta": environment(), "preflight": rows, "errors": []}
    log(f"commit {campaign['meta']['commit']}, {len(campaign['meta']['dirty_paths'])} uncommitted paths")

    def attempt(name, fn):
        try:
            return fn()
        except Exception as exc:  # record and keep going
            campaign["errors"].append({"step": name, "error": str(exc)[-2000:]})
            log(f"ERROR in {name}: {str(exc)[-300:]}")
            return None

    if not (DATA / "manifest.json").exists():
        corpora.build()
    problems = corpora.check_files()
    if problems:
        sys.exit("corpora differ from benchmarks/results/corpora.json: " + "; ".join(problems) + "\n  run: python3 benchmarks/corpora.py")
    campaign["corpora"] = json.loads(corpora.RECORD.read_text())

    campaign["code_health"] = attempt("code health", code_health) or {}
    log("code health done")
    must(["cargo", "build", "--release", "--examples"], timeout=1800)
    campaign["build"] = attempt("build times", build_times) or {}
    must(["cargo", "build", "--release", "--examples"], timeout=1800)  # restore the warm tree
    campaign["windows"] = attempt("windows sizes", windows_sizes) or {"available": False}
    ablation_bin = attempt("ablation build", build_ablation)

    suite_bin = EXAMPLES / "suite"
    suite = []
    mb = lambda name: DATA / name  # noqa: E731

    def timed(name, fn):
        log(f"== {name}")
        return attempt(name, fn) or []

    suite += timed("kernels", lambda: suite_run(suite_bin, ["kernels", str(mb("mixed-1m.txt")), "1"], "mixed-1m.txt") + suite_run(suite_bin, ["kernels", str(mb("mixed-100m.txt")), "64"], "mixed-100m.txt"))

    decode_files = ["mixed-1m.txt", "mixed-10m.txt", "mixed-100m.txt"]
    if not QUICK:
        decode_files += ["mixed-100m-crlf.txt", "mixed-100m-bom.txt", "mixed-100m-utf16le.txt", "mixed-100m-latin1.txt"]
    for f in decode_files:
        suite += timed(f"decode {f}", lambda f=f: suite_run(suite_bin, ["decode", str(mb(f))], f))

    for f in ["mixed-1m.txt", "mixed-10m.txt", "mixed-100m.txt", "c-headers.h", "python.py", "rust.rs"]:
        suite += timed(f"pipeline {f}", lambda f=f: suite_run(suite_bin, ["pipeline", str(mb(f))], f))

    for f in ["mixed-1m.txt", "mixed-10m.txt", "mixed-100m.txt"]:
        suite += timed(f"editing {f}", lambda f=f: suite_run(suite_bin, ["editing", str(mb(f))], f))

    suite += timed("regex", lambda: suite_run(suite_bin, ["regex"], "-"))
    suite += timed("config", lambda: suite_run(suite_bin, ["config"], "-"))
    suite += timed("repeat kernels (1 MiB)", lambda: suite_run(suite_bin, ["kernels", str(mb("mixed-1m.txt")), "1"], "mixed-1m.txt", build="main-repeat"))
    campaign["suite"] = suite

    ablation = []
    if ablation_bin:
        for simd, env in (("auto", {}), ("sse2", {"NOTE_SIMD": "sse2"})):
            ablation += timed(
                f"ablation kernels {simd}",
                lambda simd=simd, env=env: suite_run(ablation_bin, ["kernels", str(mb("mixed-100m.txt")), "64"], "mixed-100m.txt", build="ablation", simd=simd, env=env)
                + suite_run(ablation_bin, ["kernels", str(mb("mixed-1m.txt")), "1"], "mixed-1m.txt", build="ablation", simd=simd, env=env),
            )
            ablation += timed(
                f"ablation decode {simd}",
                lambda simd=simd, env=env: suite_run(ablation_bin, ["decode", str(mb("mixed-100m.txt"))], "mixed-100m.txt", build="ablation", simd=simd, env=env)
                + suite_run(ablation_bin, ["decode", str(mb("mixed-100m-utf16le.txt"))], "mixed-100m-utf16le.txt", build="ablation", simd=simd, env=env)
                if not QUICK
                else suite_run(ablation_bin, ["decode", str(mb("mixed-100m.txt"))], "mixed-100m.txt", build="ablation", simd=simd, env=env),
            )
    campaign["ablation"] = ablation

    memory = []
    mem_files = ["mixed-1m.txt", "mixed-10m.txt", "mixed-100m.txt"]
    for f in mem_files:
        for scenario in ("open", "editor"):
            memory.append(attempt(f"mem {scenario} {f}", lambda s=scenario, f=f: memprobe_run(s, f)))
        memory.append(attempt(f"mem typing {f}", lambda f=f: memprobe_run("typing", f, "10000")))
        memory.append(attempt(f"mem count {f}", lambda f=f: memprobe_run("count", f, "return")))
        memory.append(attempt(f"mem replace {f}", lambda f=f: memprobe_run("replace", f, "return")))
    if not QUICK:
        for f in ["mixed-100m-crlf.txt", "mixed-100m-bom.txt", "mixed-100m-utf16le.txt", "mixed-100m-latin1.txt"]:
            memory.append(attempt(f"mem open {f}", lambda f=f: memprobe_run("open", f)))
    campaign["memory"] = [m for m in memory if m]
    log(f"memory probes: {len(campaign['memory'])}")

    campaign["comparisons"] = attempt("comparisons", lambda: comparisons(suite)) or {}

    bench_example = REPO / "target" / "release" / "examples" / "bench"
    campaign["bench_example"] = attempt(
        "bench example",
        lambda: {"command": f"cargo run --release --example bench -- {DATA / 'mixed-100m.txt'}", "output": sh([str(bench_example), str(DATA / "mixed-100m.txt")], timeout=1800)[1]},
    )

    out = RESULTS / "latest.json"
    out.write_text(json.dumps(campaign, indent=1) + "\n")
    log(f"wrote {out} ({len(suite)} suite measurements, {len(campaign['errors'])} errors)")


if __name__ == "__main__":
    main()
