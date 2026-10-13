#!/usr/bin/env python3
"""Generates the benchmark corpora from a fixed seed, so every clone writes the same bytes.

The text is synthetic. The generators below write C headers, Python and Rust source, Markdown,
config files and prose, each corpus from its own seed. Nothing is read from the machine, so the
digests are the same everywhere. benchmarks/results/corpora.json records them, and --verify
checks them.

  mixed-{1m,10m,100m}.txt                    one mixed stream, cut at a line boundary
  mixed-100m-{crlf,bom,utf16le,latin1}.txt   encodings and line endings of the 100 MiB stream
  c-headers.h, python.py, rust.rs            single-language corpora for the highlighter

    python3 benchmarks/corpora.py             build into target/bench-data
    python3 benchmarks/corpora.py --verify    regenerate in memory and compare with the record
    python3 benchmarks/corpora.py --record    write this generator's digests to the record

Choices use only random.Random.getrandbits, whose output CPython keeps stable for a given seed.
Changing a generator changes its digests: run --record and commit the record with the change.
"""

import hashlib
import json
import os
import platform
import random
import sys
from pathlib import Path

MIB = 1 << 20
SEED = 20261013
GENERATOR = "synthetic-v1"
REPO = Path(__file__).resolve().parent.parent
DATA = Path(os.environ.get("BENCH_DATA", REPO / "target" / "bench-data"))
RECORD = REPO / "benchmarks" / "results" / "corpora.json"


class Choices:
    """Seeded choices. Each draw is one getrandbits call, so the sequence depends only on the seed."""

    def __init__(self, seed):
        self.bits = random.Random(seed).getrandbits

    def n(self, k):
        """An integer in [0, k)."""
        return self.bits(32) % k

    def between(self, lo, hi):
        """An integer in [lo, hi]."""
        return lo + self.bits(32) % (hi - lo + 1)

    def p(self, prob):
        """True with probability prob."""
        return self.bits(32) < int(prob * (1 << 32))

    def pick(self, seq):
        return seq[self.bits(32) % len(seq)]


def seed_for(name):
    """A seed derived from a corpus name, so each corpus is independent of the others."""
    return int.from_bytes(hashlib.sha256(f"{SEED}/{name}".encode()).digest()[:8], "big")


COMMON = (
    "count size index value result error buffer length offset start end line text name state "
    "config path file node item list map key data input output source target flags mode next "
    "prev head tail limit total width height color style cache queue stack token parser lexer "
    "reader writer handle context option module point range span window frame event timer "
    "counter pivot chunk block record field entry header footer layout cursor selection"
).split()
FUNCTION_WORDS = (
    "the of and to in is that for with on as be this by not from or an are it at which when "
    "each into only also more than then there their use can will has have"
).split()
# Latin-1 letters only, so the Latin-1 corpus is an exact re-encoding of the UTF-8 one.
ACCENTED = ["café", "naïve", "résumé", "Zürich", "señor", "façade", "crème", "über", "piñata", "garçon"]
CONSONANTS, VOWELS = "bcdfghklmnprstvz", "aeiou"


class Text:
    """Identifiers, prose and literals, drawn from a vocabulary built from the same choices."""

    def __init__(self, c):
        self.c = c
        self.words = list(COMMON)
        seen = set(self.words)
        while len(self.words) < 1500:
            word = "".join(c.pick(CONSONANTS) + c.pick(VOWELS) for _ in range(c.between(2, 3)))
            if word not in seen:
                seen.add(word)
                self.words.append(word)

    def word(self):
        return self.c.pick(self.words)

    def ident(self):
        return "_".join(self.word() for _ in range(self.c.between(1, 3)))

    def upper(self):
        return self.ident().upper()

    def camel(self):
        return "".join(self.word().capitalize() for _ in range(self.c.between(1, 3)))

    def sentence(self, lo=4, hi=14):
        c = self.c
        n = c.between(lo, hi)
        words = [c.pick(FUNCTION_WORDS) if c.p(0.45) else self.word() for _ in range(n)]
        if c.p(0.04):
            words[c.n(n)] = c.pick(ACCENTED)
        words[0] = words[0].capitalize()
        return " ".join(words) + c.pick([".", ".", ".", ",", ":", ""])

    def number(self):
        c = self.c
        k = c.n(6)
        if k == 0:
            return f"0x{c.bits(16):04X}"
        if k == 1:
            return str(1 << c.n(16))
        return str(c.n(1000))

    def literal(self):
        c = self.c
        body = " ".join(self.word() for _ in range(c.between(1, 5)))
        if c.p(0.25):
            body += c.pick(["\\n", " %d", ": {}"])
        return f'"{body}"'

    def expr(self, depth=0):
        c = self.c
        k = c.n(10)
        if depth > 1 or k < 4:
            return self.ident() if c.p(0.6) else self.number()
        if k < 6:
            return f"{self.ident()}({self.expr(depth + 1)}, {self.expr(depth + 1)})"
        op = c.pick(["+", "-", "*", "&&", "||", "<", ">=", "==", "%", "|", "<<", "/"])
        return f"{self.expr(depth + 1)} {op} {self.expr(depth + 1)}"


class Writer:
    """Collects lines and their UTF-8 size, so a generator can stop at a target size."""

    def __init__(self):
        self.lines = []
        self.size = 0

    def add(self, *lines):
        for line in lines:
            self.lines.append(line)
            self.size += len(line.encode("utf-8")) + 1

    def text(self):
        return "\n".join(self.lines) + "\n"


C_TYPES = ["int", "unsigned int", "size_t", "const char *", "void *", "uint32_t", "double", "long", "bool"]


def c_params(t):
    return ", ".join(f"{t.c.pick(C_TYPES)} {t.ident()}" for _ in range(t.c.between(0, 4))) or "void"


def c_header(t, target):
    c = t.c
    w = Writer()
    guard = f"{t.upper()}_H"
    w.add(f"#ifndef {guard}", f"#define {guard}", "")
    w.add(*(f"#include <{t.word()}.h>" for _ in range(c.between(1, 4))), "")
    while w.size < target:
        k = c.n(100)
        if k < 14:
            w.add(f"{c.pick(C_TYPES)} {t.ident()}({c_params(t)});", "")
        elif k < 26:
            name = t.camel()
            w.add(f"typedef struct {name} {{")
            for _ in range(c.between(2, 6)):
                w.add(f"    {c.pick(C_TYPES)} {t.ident()}{c.pick(['', '[16]', ' : 3'])};")
            w.add(f"}} {name};", "")
        elif k < 32:
            name = t.camel()
            w.add(f"enum {name} {{")
            for i in range(c.between(2, 7)):
                w.add(f"    {name.upper()}_{t.upper()} = {i},")
            w.add("};", "")
        elif k < 48:
            w.add(f"#define {t.upper()} {t.number()}")
        elif k < 52:
            w.add("/**", f" * {t.sentence()}", *(f" * {t.sentence()}" for _ in range(c.between(0, 3))), " */")
            w.add(f"{c.pick(C_TYPES)} {t.ident()}({c_params(t)});", "")
        elif k < 66:
            w.add(f"#define {t.upper()}(x) (({t.word()}) > 0 ? (x) : {t.number()})")
        elif k < 84:
            w.add(f"/* {t.sentence()} */")
        else:
            w.add(f"#define {t.upper()}")
    w.add("", f"#endif /* {guard} */")
    return w.text()


PY_MODULES = ["os", "sys", "re", "json", "math", "time", "pathlib", "collections", "itertools", "functools"]
PY_BASES = ["object", "Base", "Exception", "dict"]


def py_body(t, w, depth, n):
    c = t.c
    pad = "    " * depth
    for _ in range(n):
        k = c.n(100)
        if depth >= 4 and 22 <= k < 66:
            k = c.n(22)  # no deeper nesting
        if k < 22:
            w.add(f"{pad}{t.ident()} = {t.expr(1)}")
        elif k < 34:
            w.add(f"{pad}if {t.expr(1)}:")
            py_body(t, w, depth + 1, c.between(1, 3))
            if c.p(0.4):
                w.add(f"{pad}else:")
                py_body(t, w, depth + 1, c.between(1, 2))
        elif k < 44:
            w.add(f"{pad}for {t.ident()} in range({t.expr(1)}):")
            py_body(t, w, depth + 1, c.between(1, 3))
        elif k < 50:
            w.add(f"{pad}with open({t.literal()}) as {t.ident()}:")
            py_body(t, w, depth + 1, 1)
        elif k < 56:
            w.add(f"{pad}try:")
            py_body(t, w, depth + 1, 1)
            w.add(f"{pad}except {c.pick(['ValueError', 'KeyError', 'OSError'])} as exc:")
            w.add(f"{pad}    raise RuntimeError({t.literal()}) from exc")
        elif k < 66:
            w.add(f"{pad}{t.ident()} = [{t.ident()} for {t.ident()} in {t.ident()} if {t.expr(1)}]")
        elif k < 74:
            w.add(f"{pad}print({t.literal()}, {t.expr(1)})")
        elif k < 80:
            w.add(f"{pad}{t.ident()}({t.expr(1)})  # {t.sentence()}")
        elif k < 88:
            w.add(f'{pad}message = f"{{{t.ident()}}} {t.word()} {{{t.ident()}}}"')
        elif k < 92:
            w.add(f"{pad}# {t.sentence()}")
        else:
            w.add(f"{pad}return {t.expr(1)}")


def py_function(t, w, depth):
    c = t.c
    pad = "    " * depth
    params = [t.ident() for _ in range(c.between(0, 3))]
    if depth:
        params.insert(0, "self")
    w.add(f"{pad}def {t.ident()}({', '.join(params)}):", f'{pad}    """{t.sentence()}"""')
    py_body(t, w, depth + 1, c.between(3, 10))


def py_module(t, target):
    c = t.c
    w = Writer()
    w.add('"""' + t.sentence(6, 16), t.sentence(), '"""', "")
    w.add(*(f"import {c.pick(PY_MODULES)}" for _ in range(c.between(1, 4))), "")
    while w.size < target:
        k = c.n(100)
        if k < 35:
            w.add(f"class {t.camel()}({c.pick(PY_BASES)}):", f'    """{t.sentence()}"""', "")
            for _ in range(c.between(2, 4)):
                py_function(t, w, 1)
            w.add("")
        elif k < 75:
            py_function(t, w, 0)
            w.add("", "")
        elif k < 85:
            w.add(f"{t.upper()} = {t.number()}  # {t.sentence()}")
        else:
            w.add(f"# {t.sentence()}")
    return w.text()


RS_TYPES = ["u32", "usize", "i64", "f64", "bool", "String", "Vec<u8>", "Option<usize>", "&str", "PathBuf",
            "Box<dyn std::error::Error>"]
RS_RETURNS = [" -> ()", " -> bool", " -> Result<(), Error>", " -> Option<usize>", " -> Self", " -> String",
              " -> Result<(), Box<dyn std::error::Error>>"]


def rs_body(t, w, depth, n):
    c = t.c
    pad = "    " * depth
    for _ in range(n):
        k = c.n(100)
        if depth >= 4 and 20 <= k < 60:
            k = c.n(20)  # no deeper nesting
        if k < 20:
            w.add(f"{pad}let {'mut ' if c.p(0.3) else ''}{t.ident()} = {t.expr(1)};")
        elif k < 32:
            w.add(f"{pad}if {t.expr(1)} {{")
            rs_body(t, w, depth + 1, c.between(1, 3))
            if c.p(0.35):
                w.add(f"{pad}}} else {{")
                rs_body(t, w, depth + 1, 1)
            w.add(f"{pad}}}")
        elif k < 42:
            w.add(f"{pad}for {t.ident()} in 0..{t.number()} {{")
            rs_body(t, w, depth + 1, c.between(1, 3))
            w.add(f"{pad}}}")
        elif k < 48:
            w.add(f"{pad}match {t.ident()} {{", f"{pad}    Some(v) => v + {c.n(9)},", f"{pad}    None => {t.number()},", f"{pad}}}")
        elif k < 56:
            w.add(f"{pad}if let Some({t.ident()}) = {t.ident()}.get({c.n(8)}) {{")
            rs_body(t, w, depth + 1, 1)
            w.add(f"{pad}}}")
        elif k < 64:
            w.add(f"{pad}assert_eq!({t.expr(1)}, {t.number()});")
        elif k < 70:
            w.add(f"{pad}println!({t.literal()}, {t.ident()});")
        elif k < 78:
            w.add(f"{pad}{t.ident()}.{t.ident()}({t.expr(1)})?;")
        elif k < 84:
            w.add(f"{pad}let {t.ident()} = |{t.ident()}| {t.ident()} + {c.n(9)};")
        elif k < 90:
            w.add(f"{pad}// {t.sentence()}")
        elif k == 99 and c.p(0.5):
            w.add(f"{pad}unsafe {{ {t.ident()}({t.expr(1)}) }}")
        elif k == 98 and c.p(0.5):
            w.add(f"{pad}let {t.ident()} = {t.ident()}({t.expr(1)}).await?;")
        else:
            w.add(f"{pad}return {t.expr(1)};")


def rs_fn(t, w, depth):
    c = t.c
    pad = "    " * depth
    params = [f"{t.ident()}: {c.pick(RS_TYPES)}" for _ in range(c.between(0, 3))]
    if depth:
        params.insert(0, "&self")
    w.add(f"{pad}/// {t.sentence()}")
    w.add(f"{pad}pub {'async ' if c.p(0.04) else ''}fn {t.ident()}({', '.join(params)}){c.pick(RS_RETURNS)} {{")
    rs_body(t, w, depth + 1, c.between(3, 10))
    w.add(f"{pad}}}")


def rs_module(t, target):
    c = t.c
    w = Writer()
    w.add(f"//! {t.sentence(6, 14)}", f"//! {t.sentence()}", "")
    w.add(*(f"use std::{{{t.word()}, {t.word()}}};" for _ in range(c.between(1, 4))), "")
    while w.size < target:
        k = c.n(100)
        if k < 22:
            w.add("#[derive(Debug, Clone, PartialEq)]", f"pub struct {t.camel()} {{")
            for _ in range(c.between(2, 6)):
                w.add(f"    pub {t.ident()}: {c.pick(RS_TYPES)},")
            w.add("}", "")
        elif k < 30:
            w.add("#[derive(Debug, Clone, Copy)]", f"pub enum {t.camel()} {{")
            for _ in range(c.between(2, 5)):
                w.add(f"    {t.camel()},")
            w.add("}", "")
        elif k < 60:
            w.add(f"impl {t.camel()} {{")
            for _ in range(c.between(1, 4)):
                rs_fn(t, w, 1)
            w.add("}", "")
        elif k < 70:
            rs_fn(t, w, 0)
            w.add("")
        elif k < 76:
            w.add("#[cfg(test)]", "mod tests {", "    use super::*;", "")
            for _ in range(c.between(1, 3)):
                w.add("    #[test]", f"    fn {t.ident()}() {{")
                rs_body(t, w, 2, c.between(2, 5))
                w.add("    }", "")
            w.add("}", "")
        elif k < 84:
            w.add(f"/// {t.sentence()}", f"pub const {t.upper()}: {c.pick(['u32', 'usize', 'f64'])} = {t.number()};")
        else:
            w.add(f"// {t.sentence()}")
    return w.text()


def markdown(t, target):
    c = t.c
    w = Writer()
    w.add(f"# {t.camel()} {t.word()}", "")
    while w.size < target:
        k = c.n(100)
        if k < 12:
            w.add(f"## {t.word().capitalize()} {t.word()}", "")
        elif k < 60:
            w.add(" ".join(t.sentence(8, 24) for _ in range(c.between(1, 3))), "")
        elif k < 80:
            w.add(*(f"- {t.sentence()}" for _ in range(c.between(2, 5))), "")
        else:
            w.add("```c", f"    int {t.ident()} = {t.expr(1)};", "```", "")
    return w.text()


def config(t, target):
    c = t.c
    w = Writer()
    while w.size < target:
        k = c.n(100)
        if k < 15:
            w.add("", f"[{t.word()}.{t.word()}]")
        elif k < 55:
            w.add(f'{t.ident()} = "{t.word()} {t.word()}"')
        elif k < 80:
            w.add(f"{t.ident()} = {t.number()}")
        else:
            w.add(f'  "{t.ident()}": [{c.n(90)}, {c.n(90)}],')
    return w.text()


def plain(t, target):
    c = t.c
    w = Writer()
    while w.size < target:
        w.add(" ".join(t.sentence() for _ in range(c.between(2, 5))), "")
    return w.text()


# Weights are per file, and file sizes are drawn between 4 KiB and 64 KiB.
SEGMENTS = [(c_header, 25), (rs_module, 25), (py_module, 20), (markdown, 14), (config, 6), (plain, 10)]
SINGLE = [("c-headers.h", c_header, 35 * MIB), ("python.py", py_module, 22 * MIB), ("rust.rs", rs_module, 21 * MIB)]
ASCII_BYTES = bytes(range(128))
STATS = {"mixed-1m.txt", "mixed-10m.txt", "mixed-100m.txt", "c-headers.h", "python.py", "rust.rs"}


def mixed(t, target):
    """Files of several kinds, one after another, until the stream reaches target bytes."""
    c = t.c
    total = sum(weight for _, weight in SEGMENTS)
    parts, size = [], 0
    while size < target:
        k = c.n(total)
        for make, weight in SEGMENTS:
            if k < weight:
                break
            k -= weight
        part = make(t, c.between(4 * 1024, 64 * 1024))
        parts.append(part)
        size += len(part.encode("utf-8"))
    return "".join(parts)


def cut_at_line(stream, size):
    """The first `size` bytes of `stream`, cut back to a line boundary."""
    end = stream.rfind(b"\n", 0, size) + 1
    return stream[:end]


def line_stats(data):
    """Shape of a text corpus: the figures a reader needs to judge whether it is realistic."""
    lines = data.split(b"\n")
    if lines[-1] == b"":
        lines.pop()
    marks = (b"//", b"/*", b"* ", b"*/", b"# ")  # "# " is a Python comment; "#define" and "#[" are not
    blank = comment = longest = 0
    for line in lines:
        s = line.strip()
        if not s:
            blank += 1
        elif s.startswith(marks):
            comment += 1
        longest = max(longest, len(line))
    n = len(lines)
    return {
        "lines": n,
        "avg_line_bytes": round(len(data) / n, 1),
        "blank_pct": round(100 * blank / n, 1),
        "comment_pct": round(100 * comment / n, 1),
        "nonascii_pct": round(100 * len(data.translate(None, ASCII_BYTES)) / len(data), 2),
        "longest_line": longest,
    }


def build_outputs():
    stream = mixed(Text(Choices(seed_for("mixed"))), 100 * MIB).encode("utf-8")
    out = {
        "mixed-1m.txt": cut_at_line(stream, MIB),
        "mixed-10m.txt": cut_at_line(stream, 10 * MIB),
        "mixed-100m.txt": cut_at_line(stream, 100 * MIB),
    }
    big = out["mixed-100m.txt"]
    out["mixed-100m-crlf.txt"] = big.replace(b"\n", b"\r\n")
    out["mixed-100m-bom.txt"] = b"\xef\xbb\xbf" + big
    out["mixed-100m-utf16le.txt"] = b"\xff\xfe" + big.decode("utf-8").encode("utf-16-le")
    out["mixed-100m-latin1.txt"] = big.decode("utf-8").encode("latin-1")
    for name, make, size in SINGLE:
        out[name] = make(Text(Choices(seed_for(name))), size).encode("utf-8")
    return out


def manifest(outputs):
    files = {}
    for name, data in outputs.items():
        entry = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        if name in STATS:
            entry["stats"] = line_stats(data)
        files[name] = entry
    return {"generator": GENERATOR, "seed": SEED, "python": platform.python_version(), "files": files}


def committed():
    return json.loads(RECORD.read_text())["files"] if RECORD.exists() else None


def build():
    outputs = build_outputs()
    man = manifest(outputs)
    DATA.mkdir(parents=True, exist_ok=True)
    for name, data in outputs.items():
        (DATA / name).write_bytes(data)
    (DATA / "manifest.json").write_text(json.dumps(man, indent=2) + "\n")
    for name, entry in man["files"].items():
        print(f"{entry['bytes']:>12,d}  {entry['sha256'][:16]}  {name}")
    record_files = committed()
    same = record_files is not None and all(
        record_files.get(name, {}).get("sha256") == entry["sha256"] for name, entry in man["files"].items()
    )
    print(f"wrote {DATA}; digests {'match' if same else 'do NOT match'} benchmarks/results/corpora.json")
    return man


def check_files():
    """Problems with target/bench-data against the record. An empty list means they match it."""
    problems = []
    for name, entry in (committed() or {}).items():
        path = DATA / name
        if not path.exists():
            problems.append(f"{name}: missing")
        elif hashlib.sha256(path.read_bytes()).hexdigest() != entry["sha256"]:
            problems.append(f"{name}: differs from the record")
    return problems


def verify():
    man = manifest(build_outputs())
    record_files = committed()
    if record_files is None:
        print(f"no record at {RECORD}; run with --record first")
        return 1
    bad = []
    for name, entry in man["files"].items():
        ok = record_files.get(name, {}).get("sha256") == entry["sha256"]
        print(f"{'ok      ' if ok else 'MISMATCH'}  {name}")
        if not ok:
            bad.append(name)
    if bad:
        print(f"{len(bad)} digest(s) differ from {RECORD.relative_to(REPO)}")
        return 1
    print(f"all digests match {RECORD.relative_to(REPO)}")
    return 0


def record():
    man = manifest(build_outputs())
    RECORD.parent.mkdir(parents=True, exist_ok=True)
    RECORD.write_text(json.dumps(man, indent=2) + "\n")
    print(f"wrote {RECORD.relative_to(REPO)}")


if __name__ == "__main__":
    if "--verify" in sys.argv:
        sys.exit(verify())
    if "--record" in sys.argv:
        record()
    else:
        build()
