"""Regenerate ATTRIBUTION.md from the scores' own headers.

The file has always claimed to be generated from `@by` and `@origin`; this
is that generator, so adding a score updates the credits by rerunning it.
"""

import pathlib
import re
from collections import defaultdict

# The script lives beside the scores it reads, whatever the checkout.
ROOT = pathlib.Path(__file__).resolve().parent
SCORES = ROOT / "scores"

PREAMBLE = """# Where these scores came from

Every score under `scores/` carries its own `@by` and `@origin` in a
header comment, and this file is generated from those headers.

Scores gathered from the Strudel community remain the work of their
authors and are included here to test an independent implementation
against them. Upstream documentation examples are part of the Strudel
project and carry its AGPL-3.0 licence.
"""


def header(source):
    """The lines of the leading block comment, before its `*/`."""
    out = []
    for line in source.splitlines():
        if "*/" in line:
            break
        out.append(line)
    return "\n".join(out)


def main():
    by_origin = defaultdict(set)
    counts = defaultdict(int)
    for path in sorted(SCORES.rglob("*.strudel")):
        head = header(path.read_text(encoding="utf-8", errors="replace"))
        origin = re.search(r"@origin\s+(.+)", head)
        author = re.search(r"@by\s+(.+)", head)
        if not origin:
            raise SystemExit(f"{path} has no @origin")
        origin = origin.group(1).strip()
        counts[origin] += 1
        if author:
            by_origin[origin].add(author.group(1).strip())

    lines = [PREAMBLE]
    for origin in sorted(counts):
        lines.append(f"## {origin} - {counts[origin]} score(s)\n")
        for author in sorted(by_origin[origin], key=str.lower):
            lines.append(f"- {author}")
        lines.append("")
    (ROOT / "ATTRIBUTION.md").write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8")
    for origin in sorted(counts):
        print(f"{origin:<20} {counts[origin]:>4}  ({len(by_origin[origin])} named)")


if __name__ == "__main__":
    main()
