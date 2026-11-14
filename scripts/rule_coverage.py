#!/usr/bin/env python3
"""Report which ADR "Rules for implementers" items are named by a test (ADR-0017 R3).

Lists every rule in docs/adr/ as `ADR-NNNN Rn` with the tests that name it, then the rules
no test names. A test names a rule when the rule appears in the test's name (for example
`adr_0012_r3_...`), in a comment or doc comment directly above it, or in a comment inside
its body. A list such as `ADR-0008 R5, R7` names each rule in it.

Tests are Rust functions marked `#[test]` (or any `#[...::test]` attribute) under crates/,
and Python `test_*` functions under scripts/.

Prints a one-line summary; --verbose prints the full list.

Report only: the exit status is 0 whatever the coverage. It is 1 only if the ADRs cannot
be read.

Usage: rule_coverage.py [--root DIR] [--verbose]
"""

import argparse
import re
import sys
from pathlib import Path

# Keep scripts/ free of __pycache__ when the shared lexer is imported.
sys.dont_write_bytecode = True
from rust_source import lex  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
ADR_DIR = Path("docs/adr")
RUST_DIRS = (Path("crates"),)
PYTHON_DIRS = (Path("scripts"),)
SKIP_DIRS = {"target", ".git", "node_modules", "__pycache__"}

ADR_FILE = re.compile(r"^(\d{4})-.*\.md$")
RULES_HEADING = re.compile(r"^##\s+Rules for implementers\s*$")
ANY_H2 = re.compile(r"^##\s")
RULE_ITEM = re.compile(r"^\s*[-*]\s+\*\*R(\d+)\.?\*\*")
REF = re.compile(r"\bADR-(\d{4})((?:\s*(?:,|and|&|/)?\s*R\d+\b)+)")
REF_RULE = re.compile(r"R(\d+)\b")
NAME_REF = re.compile(r"adr_?(\d{4})_r(\d+)(?![0-9])", re.IGNORECASE)

RUST_TEST_ATTR = re.compile(r"^\s*#\[\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*test\b")
RUST_FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\w+)")
# Lines above a test that still belong to it, once comments are blanked: attributes and
# lines that held only comments or nothing.
RUST_ABOVE = re.compile(r"^\s*(#\[|$)")
PY_TEST = re.compile(r"^(\s*)(?:async\s+)?def\s+(test\w*)\s*\(")


def adr_rules(root):
    """Return {adr_number: [rule_number, ...]} from every ADR except the template."""
    rules = {}
    for path in sorted((root / ADR_DIR).glob("*.md")):
        match = ADR_FILE.match(path.name)
        if not match or match.group(1) == "0000":
            continue
        found = []
        in_rules = False
        for line in path.read_text(encoding="utf-8").splitlines():
            if RULES_HEADING.match(line):
                in_rules = True
                continue
            if in_rules and ANY_H2.match(line):
                break
            if in_rules:
                item = RULE_ITEM.match(line)
                if item:
                    found.append(int(item.group(1)))
        rules[match.group(1)] = found
    return rules


def refs_in_text(text):
    """Yield (adr, rule) for every rule reference in a piece of prose."""
    for match in REF.finditer(text):
        for rule in REF_RULE.findall(match.group(2)):
            yield match.group(1), int(rule)


def refs_in_name(name):
    for adr, rule in NAME_REF.findall(name):
        yield adr, int(rule)


def source_files(root, dirs, suffix):
    for directory in dirs:
        base = root / directory
        if not base.is_dir():
            continue
        for path in sorted(base.rglob(f"*{suffix}")):
            if not SKIP_DIRS.intersection(path.relative_to(root).parts):
                yield path


def rust_tests(text):
    """Yield (line_number, name, text_naming_it) for each test function in Rust source.

    Structure (attributes, `fn`, braces) is read from the source with comments and string
    literals blanked out, so a brace inside a string, even one spanning lines, cannot move
    where a test ends. The text naming a test is the comments above it and in its body.
    """
    code, comments = lex(text)
    lines = code.split("\n")
    index = 0
    while index < len(lines):
        if not RUST_TEST_ATTR.match(lines[index]):
            index += 1
            continue
        fn_index = index
        while fn_index < len(lines) and not RUST_FN.match(lines[fn_index]):
            fn_index += 1
        if fn_index >= len(lines):
            return
        name = RUST_FN.match(lines[fn_index]).group(1)
        start = index
        while start > 0 and RUST_ABOVE.match(lines[start - 1]):
            start -= 1
        depth = 0
        opened = False
        end = fn_index
        for end in range(fn_index, len(lines)):
            depth += lines[end].count("{") - lines[end].count("}")
            opened = opened or "{" in lines[end]
            if opened and depth <= 0:
                break
        naming = [comments[i] for i in range(start, end + 1) if i in comments]
        yield fn_index + 1, name, "\n".join(naming)
        index = end + 1


def python_tests(text):
    """Yield (line_number, name, text_naming_it) for each test function in Python source."""
    lines = text.splitlines()
    for index, line in enumerate(lines):
        match = PY_TEST.match(line)
        if not match:
            continue
        indent = len(match.group(1))
        start = index
        while start > 0 and lines[start - 1].strip().startswith(("#", "@")):
            start -= 1
        text = [lines[i] for i in range(start, index) if lines[i].strip().startswith("#")]
        body = []
        for inner in lines[index + 1:]:
            if inner.strip() and len(inner) - len(inner.lstrip()) <= indent:
                break
            body.append(inner.strip())
        # Only the docstring and comments name a rule; string literals in the code (such as
        # fixtures and expected output) do not.
        code = [b for b in body if b]
        if code and code[0][:3] in ('"""', "'''"):
            quote = code[0][:3]
            for number, part in enumerate(code):
                text.append(part)
                if part.endswith(quote) and (number > 0 or len(part) >= 6):
                    break
        text.extend(b for b in body if b.startswith("#"))
        yield index + 1, match.group(2), "\n".join(text)


def find_tests(root):
    """Return {(adr, rule): [location, ...]} for every rule named by a test."""
    named = {}
    scanners = [(RUST_DIRS, ".rs", rust_tests), (PYTHON_DIRS, ".py", python_tests)]
    for dirs, suffix, scan in scanners:
        for path in source_files(root, dirs, suffix):
            source = path.read_text(encoding="utf-8")
            rel = path.relative_to(root)
            for number, name, text in scan(source):
                refs = set(refs_in_name(name)) | set(refs_in_text(text))
                for ref in refs:
                    named.setdefault(ref, []).append(f"{rel}:{number} {name}")
    return named


def report(root):
    """Return (lines, rule_count, uncovered_count) for the coverage report."""
    rules = adr_rules(root)
    named = find_tests(root)
    out = []
    uncovered = []
    total = 0
    for adr, numbers in rules.items():
        for rule in numbers:
            total += 1
            label = f"ADR-{adr} R{rule}"
            tests = named.get((adr, rule), [])
            if tests:
                out.append(f"{label}: {len(tests)} test(s)")
                out.extend(f"    {test}" for test in sorted(tests))
            else:
                uncovered.append(label)
    known = {(adr, rule) for adr, numbers in rules.items() for rule in numbers}
    unknown = sorted(ref for ref in named if ref not in known)
    out.append("")
    out.append(f"Rules with no test ({len(uncovered)}):")
    out.extend(f"    {label}" for label in uncovered)
    if unknown:
        out.append("")
        out.append("Tests naming a rule that does not exist:")
        for adr, rule in unknown:
            for test in sorted(named[(adr, rule)]):
                out.append(f"    ADR-{adr} R{rule}: {test}")
    summary = (f"{total - len(uncovered)} of {total} rule(s) named by a test, "
               f"{len(uncovered)} with none")
    if unknown:
        summary += f", {len(unknown)} named rule(s) that do not exist"
    out.append("")
    out.append(summary)
    return out, total, len(uncovered)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=str(ROOT), help="repository root (default: repo)")
    parser.add_argument("--verbose", action="store_true",
                        help="list every rule and its tests, not only the summary")
    args = parser.parse_args(argv)
    try:
        lines, _, _ = report(Path(args.root))
    except OSError as err:
        print(f"cannot read the ADRs: {err}")
        return 1
    if args.verbose:
        print("\n".join(lines))
    else:
        print(f"rule coverage: {lines[-1]} (--verbose lists them)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
