"""Tests for rule_coverage.py, against temporary fixture trees. Run with `just scripts-test`."""

import io
import tempfile
import textwrap
import unittest
from contextlib import redirect_stdout
from pathlib import Path

import rule_coverage

ADR_ONE = """\
# ADR-0001: One

## Decision

- **R9.** Not a rule: outside the rules section.

## Rules for implementers

- **R1.** First rule.
- **R2.** Second rule, which wraps
  onto a second line.
- **R3.** Third rule.

## Revisit when

- **R4.** Not a rule either.
"""

ADR_TWO = """\
# ADR-0002: Two

## Rules for implementers

- **R1.** Only rule.
"""

TEMPLATE = """\
# ADR-0000: Template

## Rules for implementers

- **R1.** Template rule, not a real one.
"""

RUST = """\
//! Module docs mention ADR-0002 R1, but that is not a test.

/// ADR-0001 R3 on a function that is not a test.
pub fn helper() {}

#[cfg(test)]
mod tests {
    /// ADR-0001 R1: a doc comment on a test names the rule.
    #[test]
    fn first() {
        assert!(true);
    }

    #[test]
    fn adr_0001_r2_named_in_the_test_name() {}

    // ADR-0001 R1, R2: a plain comment naming two rules.
    #[tokio::test]
    async fn listed() {
        let s = "{";
    }

    #[test]
    fn comment_in_body() {
        // ADR-0007 R9 does not exist.
    }
}
"""

# Spelled TEST_ so the coverage scan of this file does not take the fixture for a real test.
PYTHON = '''\
import unittest


class T(unittest.TestCase):
    def TEST_python(self):
        """ADR-0002 R1 named in a Python test docstring."""

    def helper(self):
        """ADR-0001 R3 is not a test."""
'''.replace("TEST_", "test_")


def fixture(testcase):
    directory = tempfile.TemporaryDirectory()
    testcase.addCleanup(directory.cleanup)
    root = Path(directory.name)
    files = {
        "docs/adr/0000-template.md": TEMPLATE,
        "docs/adr/0001-one.md": ADR_ONE,
        "docs/adr/0002-two.md": ADR_TWO,
        "docs/adr/README.md": "Not an ADR.\n",
        "crates/demo/src/lib.rs": RUST,
        "scripts/test_demo.py": PYTHON,
    }
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return root


class RuleCoverage(unittest.TestCase):
    def test_ac4_lists_every_rule_in_every_adr(self):
        """AC4, ADR-0017 R3: every rule in every ADR is listed, covered or not."""
        root = fixture(self)
        self.assertEqual(rule_coverage.adr_rules(root), {"0001": [1, 2, 3], "0002": [1]})
        lines, total, uncovered = rule_coverage.report(root)
        text = "\n".join(lines)
        self.assertEqual((total, uncovered), (4, 1))
        for label in ("ADR-0001 R1", "ADR-0001 R2", "ADR-0001 R3", "ADR-0002 R1"):
            self.assertIn(label, text)
        self.assertNotIn("ADR-0001 R4", text)
        self.assertNotIn("ADR-0001 R9", text)

    def test_ac4_finds_a_rule_named_in_a_test_doc_comment(self):
        """AC4, ADR-0017 R3: a rule named in a test doc comment is found."""
        named = rule_coverage.find_tests(fixture(self))
        self.assertIn("crates/demo/src/lib.rs:10 first", named[("0001", 1)])

    def test_name_list_and_python_forms_are_found(self):
        named = rule_coverage.find_tests(fixture(self))
        self.assertIn("crates/demo/src/lib.rs:15 adr_0001_r2_named_in_the_test_name",
                      named[("0001", 2)])
        self.assertIn("crates/demo/src/lib.rs:19 listed", named[("0001", 1)])
        self.assertIn("crates/demo/src/lib.rs:19 listed", named[("0001", 2)])
        self.assertEqual(named[("0002", 1)], ["scripts/test_demo.py:5 test_python"])

    def test_references_outside_tests_do_not_count(self):
        lines, _, _ = rule_coverage.report(fixture(self))
        uncovered = lines[lines.index("Rules with no test (1):") + 1]
        self.assertEqual(uncovered.strip(), "ADR-0001 R3")

    def test_reference_to_a_missing_rule_is_reported(self):
        lines, _, _ = rule_coverage.report(fixture(self))
        self.assertIn("    ADR-0007 R9: crates/demo/src/lib.rs:24 comment_in_body", lines)

    def test_report_only_exits_zero_with_uncovered_rules(self):
        with redirect_stdout(io.StringIO()) as out:
            status = rule_coverage.main(["--root", str(fixture(self))])
        self.assertEqual(status, 0)
        self.assertIn("3 of 4 rule(s) named by a test", out.getvalue())

    def test_default_output_is_the_one_line_summary(self):
        with redirect_stdout(io.StringIO()) as out:
            rule_coverage.main(["--root", str(fixture(self))])
        self.assertEqual(out.getvalue().splitlines(), [
            "rule coverage: 3 of 4 rule(s) named by a test, 1 with none, 1 named rule(s) "
            "that do not exist (--verbose lists them)"])

    def test_verbose_lists_every_rule_and_test(self):
        root = fixture(self)
        with redirect_stdout(io.StringIO()) as out:
            rule_coverage.main(["--root", str(root), "--verbose"])
        lines, _, _ = rule_coverage.report(root)
        self.assertEqual(out.getvalue().splitlines(), lines)
        self.assertIn("    ADR-0001 R3", lines)

    def test_multi_line_string_with_unbalanced_brace_does_not_merge_tests(self):
        """The reviewer's case: a string spanning lines with an unbalanced brace in test
        `a` must not swallow tests `b` and `d`, or credit their rules to `a`."""
        source = textwrap.dedent("""\
            /// ADR-0001 R1
            #[test]
            fn a() {
                let s = "multi
            line {";
            }

            /// ADR-0001 R2
            #[test]
            fn b() {
                let c = '{';
            }

            /// ADR-0002 R1
            #[test]
            fn d() {
                let r = r#"}"#;
            }
            """)
        tests = list(rule_coverage.rust_tests(source))
        self.assertEqual([name for _, name, _ in tests], ["a", "b", "d"])
        credited = {name: set(rule_coverage.refs_in_text(text)) for _, name, text in tests}
        self.assertEqual(credited, {"a": {("0001", 1)}, "b": {("0001", 2)},
                                    "d": {("0002", 1)}})

    def test_reference_forms(self):
        text = textwrap.dedent("""\
            ADR-0008 R5, R7 and ADR-0012 R3 and R4; ADR-0015 "Names"; ADR-0001 R10
            """)
        self.assertEqual(sorted(rule_coverage.refs_in_text(text)),
                         [("0001", 10), ("0008", 5), ("0008", 7), ("0012", 3), ("0012", 4)])


if __name__ == "__main__":
    unittest.main()
