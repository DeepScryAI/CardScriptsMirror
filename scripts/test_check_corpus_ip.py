"""Contract tests for the nightly scanner invocation and failure handling."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("check_corpus_ip", Path(__file__).with_name("check_corpus_ip.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class CorpusGateTests(unittest.TestCase):
    def exercise(self, *, code=0, changes=None, missing=False, revision="pinned", stale=False, dirty=""):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            compiler = root / "compiler"
            (compiler / "scripts").mkdir(parents=True)
            (compiler / "scripts/scan_scryfall_ip.rs").touch()
            (compiler / "ip_allowlist.tsv").touch()
            (root / ".card-compiler-revision").write_text("pinned\n")
            reports = root / "reports"
            reports.mkdir()
            if stale:
                (reports / "nightly-ip-cards.json").write_text('{"hit_pairs":0}')
            calls = []
            def run(command, check):
                calls.append(command)
                data = dict(target="card-scripts-mirror", submodules_included=False,
                            text_files_scanned=3, hit_pairs=0, omitted_hit_pairs=0,
                            mirror_scopes_applied=True, configured_mirror_scope_rules=1)
                data.update(changes or {})
                if not missing:
                    Path(command[command.index("--report") + 1]).write_text(json.dumps(data))
                return subprocess.CompletedProcess(command, code)
            with patch.object(gate.subprocess, "check_output", side_effect=[revision, dirty]), patch.object(gate.subprocess, "run", side_effect=run):
                gate.check(root, compiler, root / "cache.json", reports)
            self.assertEqual([c[c.index("--path-prefix") + 1] for c in calls], ["cards", "tokens"])
            self.assertTrue(all(str(compiler / "ip_allowlist.tsv") in c for c in calls))
            self.assertTrue(all("--exclude-submodules" in c for c in calls))
            self.assertTrue(all(c[:2] == ["rust-script", "--force"] for c in calls))
            self.assertTrue((reports / "nightly-ip-tokens.json").exists())

    def test_clean_corpus_checks_both_scopes(self):
        self.exercise()

    def test_older_scanner_report_rejects(self):
        with self.assertRaisesRegex(RuntimeError, "scoped-policy contract"):
            self.exercise(changes={"mirror_scopes_applied": None})

    def test_invalid_scope_rule_count_rejects(self):
        for value in (None, True, -1):
            with self.subTest(value=value), self.assertRaisesRegex(RuntimeError, "scoped-policy contract"):
                self.exercise(changes={"configured_mirror_scope_rules": value})

    def test_zero_scope_rules_remains_valid(self):
        self.exercise(changes={"configured_mirror_scope_rules": 0})

    def test_real_findings_reject(self):
        with self.assertRaisesRegex(RuntimeError, "rejected"):
            self.exercise(code=1, changes={"hit_pairs":1})

    def test_tool_failure_rejects(self):
        with self.assertRaisesRegex(RuntimeError, "rejected"):
            self.exercise(code=2)

    def test_old_report_cannot_hide_missing_new_report(self):
        with self.assertRaisesRegex(RuntimeError, "fresh report"):
            self.exercise(missing=True, stale=True)

    def test_empty_corpus_rejects(self):
        with self.assertRaisesRegex(RuntimeError, "empty scan"):
            self.exercise(changes={"text_files_scanned":0})

    def test_findings_reject_even_if_scanner_returns_zero(self):
        with self.assertRaisesRegex(RuntimeError, "findings remain"):
            self.exercise(changes={"hit_pairs":1})

    def test_truncated_report_rejects(self):
        with self.assertRaisesRegex(RuntimeError, "findings remain"):
            self.exercise(changes={"omitted_hit_pairs":1})

    def test_wrong_scope_rejects(self):
        with self.assertRaisesRegex(RuntimeError, "scope"):
            self.exercise(changes={"target":"deep-scry"})

    def test_dirty_scanner_rejects_before_scan(self):
        with self.assertRaisesRegex(RuntimeError, "uncommitted"):
            self.exercise(dirty=" M scripts/scan_scryfall_ip.rs")

    def test_wrong_revision_rejects_before_scan(self):
        with self.assertRaisesRegex(RuntimeError, "revision mismatch"):
            self.exercise(revision="unexpected")


if __name__ == "__main__":
    unittest.main()
