"""Identity fixtures never start endpoints or read an actual inbox."""

import contextlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from conductor import a2a_session_start


class IdentityRegression(unittest.TestCase):
    modules = (a2a_session_start,)

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="codex hook fixture ")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.default = self.root / ".agents/a2a/default_identity"
        self.default.parent.mkdir(parents=True)
        self.default.write_text("fixture-agent\nignored-second-line\n")
        for m in self.modules:
            p = patch.object(m, "ROOT", self.root)
            p.start()
            self.addCleanup(p.stop)

    def test_default_file_uses_first_line(self):
        for m in self.modules:
            self.assertEqual(m.resolve_identity(None, {}), "fixture-agent")

    def test_explicit_and_environment_take_precedence(self):
        for m in self.modules:
            self.assertEqual(
                m.resolve_identity("explicit-agent", {"A2A_AGENT_NAME": "env-agent"}),
                "explicit-agent",
            )
            self.assertEqual(
                m.resolve_identity(None, {"A2A_AGENT_NAME": "env-agent"}), "env-agent"
            )

    def test_explicit_empty_environment_does_not_leak_process_identity(self):
        with patch.dict(os.environ, {"A2A_AGENT_NAME": "other-agent"}):
            for m in self.modules:
                self.assertEqual(m.resolve_identity(None, {}), "fixture-agent")

    def test_invalid_selected_identity_never_falls_back(self):
        for m in self.modules:
            for value in ("", "bad identity", "../other"):
                with self.subTest(module=m.__name__, value=value):
                    with self.assertRaises(m.SessionStartError):
                        m.resolve_identity(value, {})
                    with self.assertRaises(m.SessionStartError):
                        m.resolve_identity(None, {"A2A_AGENT_NAME": value})

    def test_missing_default_keeps_failure(self):
        self.default.unlink()
        for m in self.modules:
            with self.assertRaisesRegex(m.SessionStartError, "A2A identity missing"):
                m.resolve_identity(None, {})

    def test_invalid_default_keeps_failure(self):
        for value in ("", "invalid identity", "../other"):
            self.default.write_text(value)
            for m in self.modules:
                with self.assertRaises(m.SessionStartError):
                    m.resolve_identity(None, {})

    def test_default_independent_of_process_cwd(self):
        previous = Path.cwd()
        nested = self.root / "nested folder"
        nested.mkdir()
        try:
            os.chdir(nested)
            for m in self.modules:
                self.assertEqual(m.resolve_identity(None, {}), "fixture-agent")
        finally:
            os.chdir(previous)

    def test_hook_main_schema_and_bounds(self):
        for m in self.modules:
            result = m.StartupResult(
                "fixture-agent", "fixture", 0, {}, "bounded fixture preview"
            )
            out, err = io.StringIO(), io.StringIO()
            with (
                patch.dict(os.environ, {}, clear=True),
                patch.object(m, "run_startup", return_value=result) as startup,
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                status = m.main(
                    [
                        "--provider",
                        "codex",
                        "--output",
                        "hook-json",
                        "--max-messages",
                        "8",
                        "--preview-chars",
                        "140",
                        "--max-chars",
                        "1200",
                    ]
                )
            self.assertEqual(status, 0)
            self.assertEqual(err.getvalue(), "")
            self.assertEqual(
                json.loads(out.getvalue()),
                {
                    "hookSpecificOutput": {
                        "hookEventName": "SessionStart",
                        "additionalContext": "bounded fixture preview",
                    }
                },
            )
            self.assertEqual(startup.call_args.kwargs["identity"], "fixture-agent")
            for key, value in [
                ("max_messages", 8),
                ("preview_chars", 140),
                ("max_chars", 1200),
            ]:
                self.assertEqual(startup.call_args.kwargs[key], value)

    def test_startup_errors_still_exit_two(self):
        for m in self.modules:
            out, err = io.StringIO(), io.StringIO()
            with (
                patch.dict(os.environ, {}, clear=True),
                patch.object(
                    m,
                    "run_startup",
                    side_effect=m.SessionStartError("fixture registry failure"),
                ),
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(err),
            ):
                status = m.main([])
            self.assertEqual(status, 2)
            self.assertEqual(out.getvalue(), "")
            self.assertIn("fixture registry failure", err.getvalue())

    @contextlib.contextmanager
    def _blocked_startup(self, m):
        with (
            patch.dict(os.environ, {}, clear=True),
            patch.object(m, "run_startup") as startup,
            contextlib.redirect_stderr(io.StringIO()),
        ):
            yield startup

    def test_bounds_fail_before_startup(self):
        for m in self.modules:
            with self._blocked_startup(m) as startup:
                self.assertEqual(m.main(["--max-messages", "9"]), 2)
                startup.assert_not_called()

    def test_missing_identity_main_never_starts_endpoint(self):
        self.default.unlink()
        for m in self.modules:
            with self._blocked_startup(m) as startup:
                self.assertEqual(m.main([]), 2)
                startup.assert_not_called()
