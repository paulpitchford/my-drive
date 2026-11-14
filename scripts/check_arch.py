#!/usr/bin/env python3
"""Check the workspace against the structural rules in the architecture docs.

Two checks:
  - Crate dependencies (ADR-0004 R3, R4; ADR-0017 R1). Every direct dependency from one
    workspace crate to another, read from `cargo metadata`, must be an edge drawn in the
    crate diagram in docs/architecture/overview.md (the first Mermaid block under the
    "Crates" heading). A crate whose diagram label says "(dev only)" may be a
    dev-dependency of any crate and a normal or build dependency of none.
  - Unsafe code. Every crate root (lib and bin targets) outside the crates allowed unsafe
    code contains `#![forbid(unsafe_code)]`.

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

ROOT = Path(__file__).resolve().parent.parent
OVERVIEW = Path("docs/architecture/overview.md")
CRATES_HEADING = "Crates"
DEV_ONLY_MARK = "(dev only)"
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
FORBID_UNSAFE = re.compile(r"^\s*#!\[\s*forbid\s*\(([^)]*)\)\s*\]")
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

    Returns (crates, edges, dev_only): crates maps node id to crate name, edges is a set of
    (from_crate, to_crate) and dev_only is the set of crates whose label marks them dev only.
    A node's crate name is the first word of its label, so `sim["mydrive-graph-sim (dev
    only)"]` is the crate `mydrive-graph-sim`.
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
    dev_only = set()
    for node_id, number in referenced.items():
        label = labels.get(node_id)
        if not label:
            raise DiagramError(f"line {number}: node {node_id} has no label naming its crate")
        crate = label.split()[0]
        crates[node_id] = crate
        if DEV_ONLY_MARK in label:
            dev_only.add(crate)
    edges = {(crates[a], crates[b]) for a, b in id_edges}
    return crates, edges, dev_only


def cargo_metadata(root):
    """Run `cargo metadata` for the workspace at root, without resolving registry deps."""
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=root, capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo metadata failed in {root}:\n{result.stderr.strip()}")
    return json.loads(result.stdout)


def workspace_packages(metadata):
    members = set(metadata["workspace_members"])
    return [p for p in metadata["packages"] if p["id"] in members]


def check_dependencies(metadata, edges, diagram_crates, dev_only):
    """Return a problem for every workspace edge the diagram does not allow."""
    packages = workspace_packages(metadata)
    names = {p["name"] for p in packages}
    problems = []
    for name in sorted(names - set(diagram_crates)):
        problems.append(f"crate {name} is not drawn in the crate diagram")
    for name in sorted(set(diagram_crates) - names):
        problems.append(f"the crate diagram draws {name}, which is not a workspace crate")
    for package in sorted(packages, key=lambda p: p["name"]):
        seen = set()
        for dep in package["dependencies"]:
            target = dep["name"]
            if target not in names:
                continue
            kind = dep.get("kind") or "normal"
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


def check_forbid_unsafe(metadata, allowed=UNSAFE_ALLOWED):
    """Return a problem for every crate root that lacks #![forbid(unsafe_code)]."""
    problems = []
    for package in sorted(workspace_packages(metadata), key=lambda p: p["name"]):
        if package["name"] in allowed:
            continue
        for target in package["targets"]:
            if not set(target["kind"]) & {"lib", "rlib", "dylib", "cdylib", "staticlib",
                                          "proc-macro", "bin"}:
                continue
            path = Path(target["src_path"])
            try:
                text = path.read_text(encoding="utf-8")
            except OSError as err:
                problems.append(f"{package['name']}: cannot read crate root {path}: {err}")
                continue
            if not any(
                "unsafe_code" in [a.strip() for a in m.group(1).split(",")]
                for m in map(FORBID_UNSAFE.match, text.splitlines()) if m
            ):
                problems.append(
                    f"{package['name']}: crate root {path} does not contain "
                    f"#![forbid(unsafe_code)]")
    return problems


def run(root):
    """Run both checks for the workspace at root and return the list of problems."""
    root = Path(root)
    overview = root / OVERVIEW
    try:
        crates, edges, dev_only = parse_crate_diagram(overview.read_text(encoding="utf-8"))
    except (OSError, DiagramError) as err:
        return [f"{OVERVIEW}: {err}"]
    metadata = cargo_metadata(root)
    problems = check_dependencies(metadata, edges, set(crates.values()), dev_only)
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
