#!/usr/bin/env python3
"""Check the workspace against the structural rules in the architecture docs.

Two checks:
  - Crate dependencies (ADR-0004 R3, R4; ADR-0017 R1). Every direct dependency from one
    workspace crate to another, read from `cargo metadata`, must be an edge drawn in the
    crate diagram in docs/architecture/overview.md (the first Mermaid block under the
    "Crates" heading). `mydrive-graph-sim` may be a dev-dependency of any crate and a
    normal or build dependency of none, whatever the diagram draws. A path dependency on a
    crate that is not a workspace member fails, so an edge cannot be routed around the
    check through a crate outside the workspace.
  - Unsafe code. Every crate root (lib and bin targets) outside the crates allowed unsafe
    code has `#![forbid(unsafe_code)]` as a crate-level inner attribute, and no build
    script outside those crates contains `unsafe` in code. Integration-test, example and
    bench roots are not checked.

The allowed edges come from the diagram only, so changing the diagram changes what the
check allows. Exit status is 1 if anything is broken.

Usage: check_arch.py [--root DIR]
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

# Keep scripts/ free of __pycache__ when the shared lexer is imported.
sys.dont_write_bytecode = True
from rust_source import lex  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
OVERVIEW = Path("docs/architecture/overview.md")
CRATES_HEADING = "Crates"
# ADR-0004 R4, ADR-0017 R1. Fixed here rather than read from the diagram's "(dev only)"
# label, so editing the label cannot lift the rule.
DEV_ONLY = {"mydrive-graph-sim"}
# The overview and CLAUDE.md allow unsafe code in this crate only.
UNSAFE_ALLOWED = {"mydrive-vfs"}

HEADING = re.compile(r"^(#+)\s+(.*?)\s*$")
FENCE_OPEN = re.compile(r"^\s*(```|~~~)\s*mermaid\s*$")
FENCE_CLOSE = re.compile(r"^\s*(```|~~~)\s*$")
ARROW = re.compile(r"\s*-->(?:\|[^|]*\|)?\s*")
OTHER_LINK = re.compile(r"---|-\.-|==>|<--|--[ox]|~~~")
NODE = re.compile(
    r"^(?P<id>[A-Za-z_][A-Za-z0-9_-]*)"
    r"(?:\s*(?P<open>\[\(|\(\[|\[\[|\(\(|\[|\(|\{)"
    r"\s*(?P<label>\"[^\"]*\"|[^\"\]\)\}]*)\s*"
    r"(?P<close>\)\]|\]\)|\]\]|\)\)|\]|\)|\}))?$"
)
FORBID_UNSAFE = re.compile(r"^forbid\s*\(([^()]*)\)$")
ROOT_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro", "bin"}
UNSAFE_WORD = re.compile(r"\bunsafe\b")
DIRECTIVES = {"flowchart", "graph", "subgraph", "end", "direction", "classDef", "class",
              "style", "linkStyle", "click"}


class DiagramError(Exception):
    """The crate diagram is missing or uses syntax this parser does not understand."""


def crate_diagram_source(overview_text):
    """Return the lines of the first Mermaid block under the "Crates" heading."""
    lines = overview_text.splitlines()
    in_section = False
    section_level = 0
    for index, line in enumerate(lines):
        heading = HEADING.match(line)
        if heading:
            level = len(heading.group(1))
            if heading.group(2) == CRATES_HEADING:
                in_section, section_level = True, level
                continue
            if in_section and level <= section_level:
                break
        if in_section and FENCE_OPEN.match(line):
            block = []
            for number, inner in enumerate(lines[index + 1:], index + 2):
                if FENCE_CLOSE.match(inner):
                    return block
                block.append((number, inner))
            raise DiagramError(f"line {index + 1}: unclosed Mermaid block in the Crates section")
    raise DiagramError(f'no Mermaid block found under a "{CRATES_HEADING}" heading')


def parse_crate_diagram(overview_text):
    """Parse the crate diagram.

    Returns (crates, edges): crates maps node id to crate name and edges is a set of
    (from_crate, to_crate). A node's crate name is the first word of its label, so
    `sim["mydrive-graph-sim (dev only)"]` is the crate `mydrive-graph-sim`.
    """
    labels = {}
    id_edges = set()
    referenced = {}
    for number, raw in crate_diagram_source(overview_text):
        line = raw.split("%%", 1)[0].strip().rstrip(";").strip()
        if not line or line.split()[0] in DIRECTIVES:
            continue
        parts = ARROW.split(line)
        groups = []
        for part in parts:
            if OTHER_LINK.search(part):
                raise DiagramError(f"line {number}: unsupported link syntax: {raw.strip()}")
            group = []
            for token in part.split("&"):
                node = NODE.match(token.strip())
                if not node:
                    raise DiagramError(f"line {number}: cannot parse node: {token.strip()!r}")
                node_id = node.group("id")
                referenced.setdefault(node_id, number)
                if node.group("label") is not None:
                    label = node.group("label").strip().strip('"').strip()
                    if node_id in labels and labels[node_id] != label:
                        raise DiagramError(f"line {number}: node {node_id} has two labels")
                    labels[node_id] = label
                group.append(node_id)
            groups.append(group)
        for left, right in zip(groups, groups[1:]):
            for a in left:
                for b in right:
                    id_edges.add((a, b))
    crates = {}
    for node_id, number in referenced.items():
        label = labels.get(node_id)
        if not label:
            raise DiagramError(f"line {number}: node {node_id} has no label naming its crate")
        crates[node_id] = label.split()[0]
    edges = {(crates[a], crates[b]) for a, b in id_edges}
    return crates, edges


def cargo_metadata(root):
    """Run `cargo metadata` for the workspace at root, without resolving registry deps.

    Runs from this repository with --manifest-path, so the repository's pinned toolchain is
    used whichever workspace is checked.
    """
    manifest = Path(root).resolve() / "Cargo.toml"
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1",
         "--manifest-path", str(manifest)],
        cwd=ROOT, capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo metadata failed in {root}:\n{result.stderr.strip()}")
    return json.loads(result.stdout)


def workspace_packages(metadata):
    members = set(metadata["workspace_members"])
    return [p for p in metadata["packages"] if p["id"] in members]


def check_dependencies(metadata, edges, diagram_crates, dev_only=DEV_ONLY):
    """Return a problem for every workspace edge the diagram does not allow."""
    packages = workspace_packages(metadata)
    names = {p["name"] for p in packages}
    member_dirs = {Path(p["manifest_path"]).parent.resolve(): p["name"] for p in packages}
    problems = []
    for name in sorted(names - set(diagram_crates)):
        problems.append(f"crate {name} is not drawn in the crate diagram")
    for name in sorted(set(diagram_crates) - names):
        problems.append(f"the crate diagram draws {name}, which is not a workspace crate")
    for package in sorted(packages, key=lambda p: p["name"]):
        seen = set()
        for dep in package["dependencies"]:
            kind = dep.get("kind") or "normal"
            path = dep.get("path")
            if path is not None:
                target = member_dirs.get(Path(path).resolve())
                if target is None:
                    problems.append(
                        f"{package['name']} -> {dep['name']} ({kind} dependency) is a path "
                        f"dependency on {path}, which is not a workspace member")
                    continue
            elif dep["name"] in names:
                problems.append(
                    f"{package['name']} -> {dep['name']} ({kind} dependency) names a "
                    f"workspace crate but does not resolve to the workspace member")
                continue
            else:
                continue
            key = (target, kind)
            if key in seen:
                continue
            seen.add(key)
            edge = f"{package['name']} -> {target}"
            if target in dev_only:
                if kind != "dev":
                    problems.append(
                        f"{edge} ({kind} dependency): {target} is dev only and may appear "
                        f"only in [dev-dependencies]")
                continue
            if (package["name"], target) not in edges:
                problems.append(
                    f"{edge} ({kind} dependency) is not an edge in the crate diagram in "
                    f"{OVERVIEW}")
    return problems


def crate_inner_attributes(text):
    """Return the crate-level inner attributes of Rust source, as their inner text.

    Only attributes that start in column 0 and come before the first item count, with
    comments and string literals removed first, so `#![...]` inside a comment, a string or
    an inline `mod { }` is not a crate-level attribute.
    """
    code, _ = lex(text)
    attrs = []
    i, n = 0, len(code)
    while True:
        while i < n and code[i].isspace():
            i += 1
        if not code.startswith("#!", i):
            return attrs
        j = i + 2
        while j < n and code[j].isspace():
            j += 1
        if j >= n or code[j] != "[":
            return attrs
        depth, k = 0, j
        while k < n:
            if code[k] == "[":
                depth += 1
            elif code[k] == "]":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        if k >= n:
            return attrs
        if i == 0 or code[i - 1] == "\n":
            attrs.append(" ".join(code[j + 1:k].split()))
        i = k + 1


def forbids_unsafe(text):
    """True if the source has `#![forbid(..., unsafe_code, ...)]` at crate level."""
    for attr in crate_inner_attributes(text):
        match = FORBID_UNSAFE.match(attr)
        if match and "unsafe_code" in [a.strip() for a in match.group(1).split(",")]:
            return True
    return False


def check_forbid_unsafe(metadata, allowed=UNSAFE_ALLOWED):
    """Return a problem for every crate root without #![forbid(unsafe_code)], and for
    every build script that uses `unsafe`."""
    problems = []
    for package in sorted(workspace_packages(metadata), key=lambda p: p["name"]):
        if package["name"] in allowed:
            continue
        for target in package["targets"]:
            kinds = set(target["kind"])
            if not kinds & (ROOT_KINDS | {"custom-build"}):
                continue
            path = Path(target["src_path"])
            try:
                text = path.read_text(encoding="utf-8")
            except OSError as err:
                problems.append(f"{package['name']}: cannot read {path}: {err}")
                continue
            if "custom-build" in kinds:
                code, _ = lex(text)
                if UNSAFE_WORD.search(code):
                    problems.append(
                        f"{package['name']}: build script {path} contains unsafe code")
            elif not forbids_unsafe(text):
                problems.append(
                    f"{package['name']}: crate root {path} does not have "
                    f"#![forbid(unsafe_code)] as a crate-level attribute")
    return problems


def run(root):
    """Run both checks for the workspace at root and return the list of problems."""
    root = Path(root)
    overview = root / OVERVIEW
    try:
        crates, edges = parse_crate_diagram(overview.read_text(encoding="utf-8"))
    except (OSError, DiagramError) as err:
        return [f"{OVERVIEW}: {err}"]
    metadata = cargo_metadata(root)
    problems = check_dependencies(metadata, edges, set(crates.values()))
    problems += check_forbid_unsafe(metadata)
    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=str(ROOT), help="workspace root (default: repo)")
    args = parser.parse_args(argv)
    try:
        problems = run(args.root)
    except RuntimeError as err:
        print(err)
        return 1
    for problem in problems:
        print(problem)
    print(f"{len(problems)} architecture problem(s)" if problems else "architecture ok")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
