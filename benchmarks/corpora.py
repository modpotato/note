#!/usr/bin/env python3
"""Builds the benchmark corpora from real files on this machine.

Output goes to target/bench-data, which git ignores. The build is deterministic: files are
taken in sorted path order, line endings are normalised to LF, and only valid UTF-8 text
without NUL bytes is kept. A manifest with sizes and SHA-256 digests is written beside them.

  mixed-{1m,10m,100m}.txt                    one stream cut at line boundaries (scaling series)
  mixed-100m-{crlf,bom,utf16le,latin1}.txt   encodings and line endings of the 100 MiB stream
  c-headers.h, python.py, rust.rs            single-language corpora for the highlighter

    python3 benchmarks/corpora.py [--force]
"""

import hashlib
import json
import os
import sys
from pathlib import Path

MIB = 1 << 20
REPO = Path(__file__).resolve().parent.parent
DATA = Path(os.environ.get("BENCH_DATA", REPO / "target" / "bench-data"))

MIXED_ROOTS = ["/usr/include", "/usr/lib", "/usr/share", "/usr/local", "/root/.cargo/registry"]
TEXT_EXTS = {
    ".txt", ".md", ".rst", ".h", ".hpp", ".hh", ".c", ".cc", ".cpp", ".cxx", ".rs", ".py",
    ".pyi", ".js", ".ts", ".json", ".xml", ".html", ".htm", ".css", ".pl", ".pm", ".sh",
    ".sql", ".go", ".lua", ".toml", ".yaml", ".yml", ".ini", ".cfg", ".conf", ".tex",
    ".cmake", ".mk", ".m4", ".in", ".def", ".idl",
}


def walk(roots, exts):
    """Sorted list of files under `roots` whose suffix is in `exts`. Each real file is listed
    once even when several roots reach it through a symlink."""
    found = {}
    for root in roots:
        if not os.path.isdir(root):
            continue
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames.sort()
            for name in filenames:
                if os.path.splitext(name)[1].lower() in exts:
                    path = os.path.join(dirpath, name)
                    found.setdefault(os.path.realpath(path), path)
    return sorted(found.values())


def read_text(path):
    """LF-normalised bytes of `path`, or None when it is not clean UTF-8 text."""
    try:
        data = Path(path).read_bytes()
    except OSError:
        return None
    if b"\x00" in data:
        return None
    try:
        data.decode("utf-8")
    except UnicodeDecodeError:
        return None
    data = data.replace(b"\r\n", b"\n")
    if data and not data.endswith(b"\n"):
        data += b"\n"
    return data


def collect(paths, limit=None):
    """Concatenates readable text files in order, stopping once `limit` bytes are reached."""
    out = bytearray()
    used = 0
    for p in paths:
        data = read_text(p)
        if data is None:
            continue
        out += data
        used += 1
        if limit is not None and len(out) >= limit:
            break
    return bytes(out), used


def cut_at_line(stream, size):
    """The first `size` bytes of `stream`, cut back to a line boundary."""
    end = stream.rfind(b"\n", 0, size) + 1
    return stream[:end]


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def main():
    force = "--force" in sys.argv
    DATA.mkdir(parents=True, exist_ok=True)
    manifest_path = DATA / "manifest.json"

    if manifest_path.exists() and not force:
        print(f"corpora already present in {DATA} (use --force to rebuild)")
        return

    paths = walk(MIXED_ROOTS, TEXT_EXTS)
    stream, used = collect(paths, limit=100 * MIB)
    if len(stream) < 100 * MIB:
        sys.exit(f"only {len(stream)} bytes of text available; the 100 MiB corpus needs more")

    outputs = {}
    outputs["mixed-1m.txt"] = cut_at_line(stream, 1 * MIB)
    outputs["mixed-10m.txt"] = cut_at_line(stream, 10 * MIB)
    outputs["mixed-100m.txt"] = cut_at_line(stream, 100 * MIB)
    big = outputs["mixed-100m.txt"]
    outputs["mixed-100m-crlf.txt"] = big.replace(b"\n", b"\r\n")
    outputs["mixed-100m-bom.txt"] = b"\xef\xbb\xbf" + big
    outputs["mixed-100m-utf16le.txt"] = b"\xff\xfe" + big.decode("utf-8").encode("utf-16-le")
    outputs["mixed-100m-latin1.txt"] = big.decode("utf-8").encode("latin-1", errors="replace")

    lang_sets = {
        "c-headers.h": walk(["/usr/include"], {".h"}),
        "python.py": walk(["/usr/lib/python3.13", "/usr/lib/python3"], {".py"}),
        "rust.rs": walk(MIXED_ROOTS, {".rs"}),
    }
    for name, files in lang_sets.items():
        outputs[name], _ = collect(files)

    for name, data in outputs.items():
        (DATA / name).write_bytes(data)

    manifest = {
        "mixed_stream": {
            "source_roots": MIXED_ROOTS,
            "extensions": sorted(TEXT_EXTS),
            "files_used": used,
            "stream_bytes": len(stream),
        },
        "files": {
            name: {"bytes": len(data), "sha256": sha256(data)} for name, data in outputs.items()
        },
        "language_sources": {name: len(files) for name, files in lang_sets.items()},
    }
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    for name, data in outputs.items():
        print(f"{len(data):>12,d}  {name}")
    print(f"manifest: {manifest_path}")


if __name__ == "__main__":
    main()
