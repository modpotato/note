#!/usr/bin/env python3
"""note against Wine's built-in Notepad, both run under the same Wine.

Both programs run in one Wine prefix on a virtual X display (Xvfb), so they share the same
translation layer, fonts and window system. For each file size the script records:

  window_s        launch until a top-level window of the app exists
  title_s         launch until the title shows the file name (the app's "file is open" signal)
  rss_kb          resident memory of the app process once the file is open
  typing_cpu_s    CPU seconds the app spends on a burst of keystrokes typed at the start
  typing_wall_s   wall seconds for the burst, including the wait until the app is quiet

Needs wine64 (or wine), Xvfb and xdotool, plus the Windows build made by run_benchmarks.py.
Writes benchmarks/results/wine.json. Run it after run_benchmarks.py so nothing competes.

    python3 benchmarks/wine_compare.py
"""

import json
import os
import re
import shutil
import signal
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRATCH = REPO / "target" / "bench-scratch"
DATA = REPO / "target" / "bench-data"
PREFIX = SCRATCH / "wineprefix"
OUT = REPO / "benchmarks" / "results" / "wine.json"
DISPLAY = ":99"
QUICK = "--quick" in sys.argv  # one repeat, 1 MiB only: a smoke test of the setup
REPEATS = 1 if QUICK else 3
TIMEOUT = 180.0
KEYS = "the quick brown fox jumps over the lazy dog "
KEYSTROKES = 300
CLK_TCK = os.sysconf("SC_CLK_TCK")

FILES = ["mixed-1m.txt"] if QUICK else ["mixed-1m.txt", "mixed-10m.txt", "mixed-100m.txt"]
# How long to wait for a typing burst to finish processing. A run that is still busy at the cap
# is recorded as not settled, so its CPU figure is a lower bound.
SETTLE_CAP = 300.0
REPEATS_BY_FILE = {None: 3, "mixed-1m.txt": 3, "mixed-10m.txt": 2, "mixed-100m.txt": 1}
APPS = {
    # Wine's windows carry no WM_CLASS, so the title is what identifies them: note writes
    # "<name> - note" and Wine's Notepad writes "<name> - Notepad".
    "note": {
        "label": "note (Windows build)",
        "exe": SCRATCH / "note-default.exe",
        "run": "C:\\bench\\note.exe",
        "title": " - note$",
        "exe_name": "note.exe",
    },
    "notepad": {
        "label": "Wine Notepad",
        "exe": None,
        "run": "notepad.exe",
        "title": " - Notepad$",
        "exe_name": "notepad.exe",
    },
}


def log(msg):
    print(f"[{datetime.now().strftime('%H:%M:%S')}] {msg}", flush=True)


def wine_bin():
    for name in ("wine64", "wine"):
        found = shutil.which(name)
        if found:
            return found
    sys.exit("wine64 (or wine) is not installed")


def env_for_wine():
    return {
        **os.environ,
        "DISPLAY": DISPLAY,
        "WINEPREFIX": str(PREFIX),
        "WINEARCH": "win64",
        "WINEDEBUG": "-all",
        # Keep Wine from trying to download Mono or Gecko, which this sandbox cannot reach.
        "WINEDLLOVERRIDES": "mscoree,mshtml=",
    }


def xdo(*args, timeout=10):
    return subprocess.run(
        ["xdotool", *args],
        env={**os.environ, "DISPLAY": DISPLAY},
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def start_display():
    if xdo("getmouselocation").returncode == 0:
        return None
    proc = subprocess.Popen(
        ["Xvfb", DISPLAY, "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    for _ in range(100):
        time.sleep(0.1)
        if xdo("getmouselocation").returncode == 0:
            return proc
    proc.kill()
    sys.exit("Xvfb did not start")


def init_prefix(wine, env):
    if (PREFIX / "system.reg").exists():
        return
    log("initialising the Wine prefix (first run only)")
    subprocess.run([wine, "wineboot", "--init"], env=env, timeout=900, capture_output=True)
    subprocess.run([shutil.which("wineserver") or "wineserver", "-w"], env=env, timeout=900)


def stage_files():
    bench = PREFIX / "drive_c" / "bench"
    bench.mkdir(parents=True, exist_ok=True)
    for name in FILES:
        src = DATA / name
        dst = bench / name
        if not dst.exists() or dst.stat().st_size != src.stat().st_size:
            shutil.copy(src, dst)
    note = APPS["note"]["exe"]
    shutil.copy(note, bench / "note.exe")


def pids_running(exe_name):
    """Processes whose command line names this Windows executable. Wine renames the Windows
    main thread, so the thread name cannot be used to find it."""
    found = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            argv = Path(f"/proc/{entry}/cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        names = {a.decode("utf-8", "replace").replace("\\", "/").rsplit("/", 1)[-1].lower() for a in argv if a}
        if exe_name.lower() in names:
            found.append(int(entry))
    return found


def status_kb(pid, key):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith(key + ":"):
                return int(line.split()[1])
    except OSError:
        pass
    return None


def cpu_seconds(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    fields = stat[stat.rindex(")") + 2 :].split()
    utime, stime = int(fields[11]), int(fields[12])
    return (utime + stime) / CLK_TCK


def main_pid(exe_name):
    """The Linux process that runs the Windows program: the largest one that names it."""
    pids = pids_running(exe_name)
    if not pids:
        return None
    return max(pids, key=lambda p: status_kb(p, "VmRSS") or 0)


def wait_for_window(title_re, t0):
    while time.perf_counter() - t0 < TIMEOUT:
        found = xdo("search", "--onlyvisible", "--name", title_re)
        ids = found.stdout.split()
        if found.returncode == 0 and ids:
            return ids[0], time.perf_counter() - t0
        time.sleep(0.02)
    return None, None


def wait_for_title(wid, file_name, t0):
    while time.perf_counter() - t0 < TIMEOUT:
        name = xdo("getwindowname", wid)
        if name.returncode == 0 and file_name in name.stdout:
            return time.perf_counter() - t0
        time.sleep(0.02)
    return None


def wait_until_quiet(pid, idle=0.5, cap=SETTLE_CAP):
    """True once the process has used no CPU for `idle` seconds; False if still busy at `cap`."""
    start = time.perf_counter()
    last = cpu_seconds(pid)
    quiet_since = time.perf_counter()
    while time.perf_counter() - start < cap:
        time.sleep(0.05)
        now = cpu_seconds(pid)
        if now is None:
            return False
        if now != last:
            last = now
            quiet_since = time.perf_counter()
        elif time.perf_counter() - quiet_since >= idle:
            return True
    return False


def kill_app(proc, exe_name):
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    for pid in pids_running(exe_name):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    proc.wait(timeout=30)
    time.sleep(0.5)


def run_once(app_key, file_name, wine, env):
    app = APPS[app_key]
    cmd = [wine, app["run"]]
    if file_name:
        cmd.append(f"C:\\bench\\{file_name}")
    record = {"app": app_key, "file": file_name or "(empty)", "status": "ok"}
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        wid, window_s = wait_for_window(app["title"], t0)
        if wid is None:
            record.update(status="timeout", error="no window within the limit")
            return record
        record["window_s"] = window_s
        if file_name:
            title_s = wait_for_title(wid, file_name, t0)
            if title_s is None:
                record.update(status="timeout", error="title never showed the file name")
                return record
            record["title_s"] = title_s

        time.sleep(0.5)  # let the app settle before reading its memory
        pid = main_pid(app["exe_name"])
        if pid is None:
            record.update(status="error", error="no process found for the program")
            return record
        record["rss_kb"] = status_kb(pid, "VmRSS")
        record["vmhwm_kb"] = status_kb(pid, "VmHWM")

        if file_name:
            xdo("windowfocus", "--sync", wid)
            xdo("key", "ctrl+Home")
            time.sleep(0.3)
            cpu0 = cpu_seconds(pid)
            t_type = time.perf_counter()
            text = (KEYS * (KEYSTROKES // len(KEYS) + 1))[:KEYSTROKES]
            xdo("type", "--delay", "2", text, timeout=300)
            settled = wait_until_quiet(pid)
            record["typing_settled"] = settled
            record["typing_wall_s"] = time.perf_counter() - t_type
            record["typing_cpu_s"] = cpu_seconds(pid) - cpu0
            record["keystrokes"] = KEYSTROKES
            record["rss_after_typing_kb"] = status_kb(pid, "VmRSS")
        return record
    except subprocess.TimeoutExpired as exc:
        record.update(status="timeout", error=f"xdotool: {exc}")
        return record
    finally:
        kill_app(proc, app["exe_name"])


def summarise(runs):
    summary = {}
    for app_key in APPS:
        for file_name in ["(empty)", *FILES]:
            rows = [r for r in runs if r["app"] == app_key and r["file"] == file_name]
            ok = [r for r in rows if r["status"] == "ok"]
            entry = {"runs": len(rows), "ok": len(ok)}
            entry["typing_settled"] = sum(1 for r in ok if r.get("typing_settled"))
            for key in ("window_s", "title_s", "rss_kb", "vmhwm_kb", "typing_cpu_s", "typing_wall_s", "rss_after_typing_kb"):
                values = [r[key] for r in ok if r.get(key) is not None]
                entry[key] = statistics.median(values) if values else None
            summary.setdefault(app_key, {})[file_name] = entry
    return summary


def main():
    wine = wine_bin()
    env = env_for_wine()
    display = start_display()
    try:
        init_prefix(wine, env)
        stage_files()
        notepad = PREFIX / "drive_c" / "windows" / "system32" / "notepad.exe"  # Wine's builtin, via symlink
        exe_size = {
            "note": APPS["note"]["exe"].stat().st_size,
            "notepad": notepad.stat().st_size if notepad.exists() else None,
        }

        log("warm-up launch (wine server start-up is excluded from the measurements)")
        run_once("note", None, wine, env)

        runs = []
        for file_name in [None, *FILES]:
            for app_key in APPS:
                for repeat in range(REPEATS if QUICK else REPEATS_BY_FILE[file_name]):
                    rec = run_once(app_key, file_name, wine, env)
                    rec["repeat"] = repeat
                    runs.append(rec)
                    log(
                        f"{app_key:<8} {file_name or '(empty)':<14} #{repeat}: {rec['status']}"
                        f" window={rec.get('window_s', 0) or 0:.2f}s"
                        f" title={rec.get('title_s') or 0:.2f}s"
                        f" rss={(rec.get('rss_kb') or 0) / 1024:.0f} MiB"
                    )
                    if rec["status"] != "ok" and file_name == FILES[-1]:
                        break  # the 100 MiB case is the expensive one; skip repeats after a failure
        result = {
            "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "wine": subprocess.run([wine, "--version"], env=env, capture_output=True, text=True).stdout.strip(),
            "xvfb_screen": "1280x800x24",
            "repeats": REPEATS,
            "keystrokes_per_burst": KEYSTROKES,
            "exe_bytes": exe_size,
            "apps": {k: v["label"] for k, v in APPS.items()},
            "runs": runs,
            "summary": summarise(runs),
        }
        OUT.parent.mkdir(parents=True, exist_ok=True)
        OUT.write_text(json.dumps(result, indent=1) + "\n")
        log(f"wrote {OUT}")
    finally:
        subprocess.run([shutil.which("wineserver") or "wineserver", "-k"], env=env, capture_output=True)
        if display is not None:
            display.kill()


if __name__ == "__main__":
    main()
