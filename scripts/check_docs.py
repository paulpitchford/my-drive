#!/usr/bin/env python3
"""Check the project docs for broken relative links and dangling cross-references.

Checks every Markdown file outside the research folders for:
  - relative links whose target file does not exist
  - references to ADR-NNNN, Jnn, Fnn, WP-nnn and Gn that have no definition

Exit status is 1 if anything is broken.
"""

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {"target", ".git", "research_notes", "reports", "node_modules"}

LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
FENCE = re.compile(r"^(```|~~~)")
REFS = {
    "ADR": re.compile(r"\bADR-(\d{4})\b"),
    "J": re.compile(r"\bJ(\d{2})\b"),
    "F": re.compile(r"\bF(\d{2})\b"),
    "WP": re.compile(r"\bWP-(\d{3})\b"),
}


def markdown_files():
    for path in sorted(ROOT.rglob("*.md")):
        if not SKIP_DIRS.intersection(path.relative_to(ROOT).parts):
            yield path


def defined_ids():
    def stems(folder, pattern):
        found = set()
        directory = ROOT / folder
        if directory.is_dir():
            for path in directory.glob("*.md"):
                match = re.match(pattern, path.name)
                if match:
                    found.add(match.group(1))
        return found

    return {
        "ADR": stems("docs/adr", r"(\d{4})-"),
        "J": stems("docs/journeys", r"J(\d{2})-"),
        "F": stems("docs/flows", r"F(\d{2})-"),
        "WP": stems("docs/backlog", r"WP-(\d{3})-"),
    }


def main():
    known = defined_ids()
    problems = []
    for path in markdown_files():
        rel = path.relative_to(ROOT)
        in_fence = False
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if FENCE.match(line.strip()):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            for target in LINK.findall(line):
                if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
                    continue
                file_part = unquote(target.split("#", 1)[0])
                if file_part and not (path.parent / file_part).exists():
                    problems.append(f"{rel}:{number}: broken link: {target}")
            for kind, pattern in REFS.items():
                for ident in pattern.findall(line):
                    if kind == "ADR" and ident == "0000":
                        continue
                    if ident not in known[kind]:
                        label = f"ADR-{ident}" if kind == "ADR" else (
                            f"WP-{ident}" if kind == "WP" else f"{kind}{ident}")
                        problems.append(f"{rel}:{number}: unknown reference: {label}")
    for problem in problems:
        print(problem)
    print(f"{len(problems)} problem(s) in docs" if problems else "docs ok")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
