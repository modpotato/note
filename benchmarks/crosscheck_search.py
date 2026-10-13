"""Cross-check one search count with Python's re, on the 100 MiB corpus.

note searches the whole buffer, so a \\s+ can match across a newline. ripgrep
matches one line at a time, so it cannot. This script counts one query both
ways, and counts the matches that span a newline, so the difference between
the two tools is checked rather than assumed.

Run after benchmarks/corpora.py. Writes benchmarks/results/crosscheck.json.
"""

import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CORPUS = REPO / "target" / "bench-data" / "mixed-100m.txt"
RESULTS = REPO / "benchmarks" / "results" / "crosscheck.json"
PATTERN = r"\bfn\s+\w+"


def main():
    pattern = re.compile(PATTERN)
    text = CORPUS.read_bytes().decode("utf-8")
    whole = list(pattern.finditer(text))
    spanning = sum(1 for m in whole if "\n" in m.group(0))
    per_line = sum(len(pattern.findall(line)) for line in text.split("\n"))
    out = {
        "file": CORPUS.name,
        "pattern": PATTERN,
        "python": sys.version.split()[0],
        "whole_text_matches": len(whole),
        "per_line_matches": per_line,
        "spanning_newline": spanning,
    }
    RESULTS.write_text(json.dumps(out, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(out))


if __name__ == "__main__":
    main()
