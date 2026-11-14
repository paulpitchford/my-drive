"""Tests for check_arch.py. Each builds a temporary fixture workspace; none edits the tree.

Run with `just scripts-test`. Needs `cargo` on PATH.
"""

import io
import tempfile
import textwrap
import unittest
from contextlib import redirect_stdout
from pathlib import Path

import check_arch

# The crate diagram as drawn when this test was written. Tests copy it into fixtures so they
# do not depend on the live overview, which may change.
DIAGRAM = """\
flowchart TB
    cli["mydrive-cli (bin: mydrive)"] --> ipc
    daemon["mydrive-daemon (bin: mydrived)"] --> ipc
    daemon --> vfs
    daemon --> engine
    vfs["mydrive-vfs"] --> core
    engine["mydrive-engine"] --> store
    engine --> graphcrate
    engine --> core
    ipc["mydrive-ipc"] --> core
    store["mydrive-store"] --> core
    graphcrate["mydrive-graph"] --> core
    sim["mydrive-graph-sim (dev only)"] --> core
    core["mydrive-core"]
"""

BINS = {"mydrive-cli", "mydrive-daemon"}
CRATES = ["mydrive-core", "mydrive-graph", "mydrive-store", "mydrive-engine", "mydrive-vfs",
          "mydrive-ipc", "mydrive-graph-sim", "mydrive-cli", "mydrive-daemon"]
# The dependencies the diagram draws, as the real workspace will have them.
DRAWN = {
    "mydrive-cli": ["mydrive-ipc"],
    "mydrive-daemon": ["mydrive-ipc", "mydrive-vfs", "mydrive-engine"],
    "mydrive-vfs": ["mydrive-core"],
    "mydrive-engine": ["mydrive-store", "mydrive-graph", "mydrive-core"],
    "mydrive-ipc": ["mydrive-core"],
    "mydrive-store": ["mydrive-core"],
    "mydrive-graph": ["mydrive-core"],
    "mydrive-graph-sim": ["mydrive-core"],
}
FORBID = "//! Fixture crate.\n\n#![forbid(unsafe_code)]\n"


def overview(diagram):
    return textwrap.dedent("""\
        # Architecture overview

        ## Components

        ```mermaid
        flowchart TB
            a["not a crate"] --> b["also not a crate"]
        ```

        ### Crates

        One Cargo workspace.

        ```mermaid
        """) + diagram + "```\n\n## Vocabulary\n"


class Fixture:
    """A temporary Cargo workspace with the nine crates, editable before checking."""

    def __init__(self, testcase):
        self._dir = tempfile.TemporaryDirectory()
        testcase.addCleanup(self._dir.cleanup)
        self.root = Path(self._dir.name)
        self.deps = {name: list(DRAWN.get(name, [])) for name in CRATES}
        self.dev_deps = {name: [] for name in CRATES}
        self.build_deps = {name: [] for name in CRATES}
        self.roots = {name: FORBID for name in CRATES}
        self.diagram = DIAGRAM

    def write(self):
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n', encoding="utf-8")
        docs = self.root / "docs" / "architecture"
        docs.mkdir(parents=True, exist_ok=True)
        (docs / "overview.md").write_text(overview(self.diagram), encoding="utf-8")
        for name in CRATES:
            crate = self.root / "crates" / name
            (crate / "src").mkdir(parents=True, exist_ok=True)
            manifest = [f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\n']
            for section, deps in (("dependencies", self.deps[name]),
                                  ("dev-dependencies", self.dev_deps[name]),
                                  ("build-dependencies", self.build_deps[name])):
                manifest.append(f"\n[{section}]\n")
                manifest.extend(f'{dep} = {{ path = "../{dep}" }}\n' for dep in deps)
            (crate / "Cargo.toml").write_text("".join(manifest), encoding="utf-8")
            root_file = "main.rs" if name in BINS else "lib.rs"
            (crate / "src" / root_file).write_text(self.roots[name], encoding="utf-8")
        return self

    def problems(self):
        return check_arch.run(self.write().root)


class DiagramParsing(unittest.TestCase):
    def test_ids_resolve_to_crate_names_through_labels(self):
        crates, edges, dev_only = check_arch.parse_crate_diagram(overview(DIAGRAM))
        self.assertEqual(crates["graphcrate"], "mydrive-graph")
        self.assertEqual(crates["sim"], "mydrive-graph-sim")
        self.assertEqual(crates["cli"], "mydrive-cli")
        self.assertEqual(dev_only, {"mydrive-graph-sim"})
        self.assertIn(("mydrive-engine", "mydrive-graph"), edges)
        self.assertIn(("mydrive-graph-sim", "mydrive-core"), edges)
        self.assertEqual(len(edges), 12)

    def test_only_the_crates_section_is_read(self):
        crates, _, _ = check_arch.parse_crate_diagram(overview(DIAGRAM))
        self.assertNotIn("a", crates)

    def test_chains_edge_labels_and_ampersands(self):
        diagram = textwrap.dedent("""\
            flowchart TB
                a["x-a"] --> b["x-b"] -->|uses| c["x-c"]
                a & b --> d["x-d"]  %% comment
                subgraph g["group"]
                    e("x-e") --> d;
                end
            """)
        _, edges, _ = check_arch.parse_crate_diagram(overview(diagram))
        self.assertEqual(edges, {("x-a", "x-b"), ("x-b", "x-c"), ("x-a", "x-d"),
                                 ("x-b", "x-d"), ("x-e", "x-d")})

    def test_unsupported_link_syntax_is_an_error_not_ignored(self):
        diagram = 'flowchart TB\n    a["x-a"] -.-> b["x-b"]\n'
        with self.assertRaises(check_arch.DiagramError):
            check_arch.parse_crate_diagram(overview(diagram))

    def test_node_without_label_is_an_error(self):
        diagram = 'flowchart TB\n    a["x-a"] --> b\n'
        with self.assertRaises(check_arch.DiagramError):
            check_arch.parse_crate_diagram(overview(diagram))

    def test_missing_crates_section_is_an_error(self):
        with self.assertRaises(check_arch.DiagramError):
            check_arch.parse_crate_diagram("# Overview\n\n## Components\n")


class DependencyCheck(unittest.TestCase):
    def test_drawn_edges_pass(self):
        self.assertEqual(Fixture(self).problems(), [])

    def test_ac1_store_to_graph_fails_and_names_the_edge(self):
        """AC1, ADR-0004 R3: an edge not in the diagram fails and is named."""
        fixture = Fixture(self)
        fixture.deps["mydrive-store"].append("mydrive-graph")
        problems = fixture.problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("mydrive-store -> mydrive-graph", problems[0])

    def test_adr_0004_r3_cli_depends_on_ipc_only(self):
        """ADR-0004 R3: mydrive-cli depends directly on mydrive-ipc only."""
        fixture = Fixture(self)
        fixture.deps["mydrive-cli"].append("mydrive-core")
        self.assertIn("mydrive-cli -> mydrive-core", "\n".join(fixture.problems()))

    def test_adr_0004_r3_nothing_depends_on_daemon_or_cli(self):
        """ADR-0004 R3: no crate depends on mydrive-daemon or mydrive-cli."""
        fixture = Fixture(self)
        fixture.deps["mydrive-engine"].append("mydrive-daemon")
        fixture.dev_deps["mydrive-ipc"].append("mydrive-cli")
        text = "\n".join(fixture.problems())
        self.assertIn("mydrive-engine -> mydrive-daemon", text)
        self.assertIn("mydrive-ipc -> mydrive-cli", text)

    def test_adr_0004_r4_graph_sim_as_dev_dependency_of_any_crate_passes(self):
        """ADR-0004 R4, ADR-0017 R1: mydrive-graph-sim is allowed in [dev-dependencies]."""
        fixture = Fixture(self)
        for name in ("mydrive-engine", "mydrive-store", "mydrive-cli"):
            fixture.dev_deps[name].append("mydrive-graph-sim")
        self.assertEqual(fixture.problems(), [])

    def test_adr_0004_r4_graph_sim_as_normal_dependency_fails(self):
        """ADR-0004 R4, ADR-0017 R1: mydrive-graph-sim is a normal dependency of none."""
        fixture = Fixture(self)
        fixture.deps["mydrive-engine"].append("mydrive-graph-sim")
        fixture.build_deps["mydrive-store"].append("mydrive-graph-sim")
        text = "\n".join(fixture.problems())
        self.assertIn("mydrive-engine -> mydrive-graph-sim (normal dependency)", text)
        self.assertIn("mydrive-store -> mydrive-graph-sim (build dependency)", text)

    def test_graph_sim_stays_dev_only_even_if_an_edge_to_it_is_drawn(self):
        fixture = Fixture(self)
        fixture.diagram = DIAGRAM + "    engine --> sim\n"
        fixture.deps["mydrive-engine"].append("mydrive-graph-sim")
        self.assertIn("mydrive-engine -> mydrive-graph-sim", "\n".join(fixture.problems()))

    def test_dev_dependency_along_an_undrawn_edge_fails(self):
        fixture = Fixture(self)
        fixture.dev_deps["mydrive-ipc"].append("mydrive-store")
        self.assertIn("mydrive-ipc -> mydrive-store (dev dependency)",
                      "\n".join(fixture.problems()))

    def test_crate_missing_from_diagram_fails(self):
        fixture = Fixture(self)
        fixture.diagram = DIAGRAM.replace('    cli["mydrive-cli (bin: mydrive)"] --> ipc\n', "")
        self.assertIn("crate mydrive-cli is not drawn", "\n".join(fixture.problems()))

    def test_ac3_changing_the_diagram_changes_what_is_allowed(self):
        """AC3: the allowed edges come from the diagram, with no script change."""
        fixture = Fixture(self)
        fixture.deps["mydrive-store"].append("mydrive-graph")
        self.assertNotEqual(fixture.problems(), [])
        fixture.diagram = DIAGRAM + "    store --> graphcrate\n"
        self.assertEqual(fixture.problems(), [])
        fixture.diagram = DIAGRAM.replace("    engine --> graphcrate\n", "")
        self.assertIn("mydrive-engine -> mydrive-graph", "\n".join(fixture.problems()))


class UnsafeCheck(unittest.TestCase):
    def test_ac2_removing_forbid_from_a_library_root_fails(self):
        """AC2: a crate root without #![forbid(unsafe_code)] fails."""
        fixture = Fixture(self)
        fixture.roots["mydrive-store"] = "//! Fixture crate.\n"
        problems = fixture.problems()
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("mydrive-store", problems[0])
        self.assertIn("forbid(unsafe_code)", problems[0])

    def test_ac2_removing_forbid_from_a_binary_root_fails(self):
        fixture = Fixture(self)
        fixture.roots["mydrive-daemon"] = "fn main() {}\n"
        self.assertIn("mydrive-daemon", "\n".join(fixture.problems()))

    def test_forbid_in_a_comment_or_deny_does_not_count(self):
        fixture = Fixture(self)
        fixture.roots["mydrive-core"] = "// #![forbid(unsafe_code)]\n#![deny(unsafe_code)]\n"
        self.assertIn("mydrive-core", "\n".join(fixture.problems()))

    def test_forbid_with_other_lints_counts(self):
        fixture = Fixture(self)
        fixture.roots["mydrive-core"] = "#![forbid(missing_docs, unsafe_code)]\n"
        self.assertEqual(fixture.problems(), [])

    def test_vfs_is_exempt(self):
        fixture = Fixture(self)
        fixture.roots["mydrive-vfs"] = "#![deny(unsafe_code)]\n"
        self.assertEqual(fixture.problems(), [])


class Main(unittest.TestCase):
    def test_exit_status(self):
        fixture = Fixture(self).write()
        with redirect_stdout(io.StringIO()) as out:
            self.assertEqual(check_arch.main(["--root", str(fixture.root)]), 0)
        self.assertIn("architecture ok", out.getvalue())
        fixture.deps["mydrive-store"].append("mydrive-graph")
        fixture.write()
        with redirect_stdout(io.StringIO()) as out:
            self.assertEqual(check_arch.main(["--root", str(fixture.root)]), 1)
        self.assertIn("mydrive-store -> mydrive-graph", out.getvalue())


if __name__ == "__main__":
    unittest.main()
