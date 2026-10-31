"""Frontend-contract tests; simulated responses are not native host authority."""
import contextlib
import io
import json
import sqlite3
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from controller import Refused
from native_callback import CallbackRegistry, main


class CallbackObligations(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="relay-callback-test-", dir="/private/tmp")
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.registry = CallbackRegistry(self.root / "registry", self.home)

    def tearDown(self):
        self.temp.cleanup()

    def bind(self, owner, thread, ephemeral=False):
        self.registry.claim(owner, thread)
        self.registry.bind_role(owner, thread, ephemeral)

    def callback(self, method, thread=None, reply=None, **extra):
        message = {"registry": str(self.root / "registry"), "home": str(self.home),
                   "client": "A", "method": method, "thread": thread, **extra}
        frames = [message] + ([] if reply is None else [reply])
        output = io.StringIO()
        with patch("sys.stdin", io.StringIO("".join(json.dumps(x) + "\n" for x in frames))):
            with contextlib.redirect_stdout(output):
                main()
        return [json.loads(x) for x in output.getvalue().splitlines()]

    def test_selection_and_native_view_commit_atomically(self):
        self.registry.select_view("A", "old", "old")
        with self.registry.connect() as db:
            db.execute("CREATE TRIGGER fail_view BEFORE INSERT ON native_views "
                       "BEGIN SELECT RAISE(ABORT, 'injected view failure'); END")
        with self.assertRaises(sqlite3.IntegrityError):
            self.registry.select_view("A", "new", "new")
        self.registry.check_selection("A", 1, "old")
        with self.registry.connect() as db:
            self.assertEqual(db.execute("SELECT primary_thread,displayed_thread FROM native_views").fetchone(),
                             ("old", "old"))

    def test_background_native_role_does_not_replace_foreground(self):
        self.bind("A", "foreground")
        self.bind("A", "temporary", True)
        self.registry.select_view("A", "foreground", "foreground")
        self.registry.authorize_mutation("A", "temporary")
        self.registry.authorize_mutation("A", "foreground")
        with self.registry.connect() as db:
            self.assertEqual(db.execute("SELECT primary_thread,displayed_thread FROM native_views").fetchone(),
                             ("foreground", "foreground"))

    def test_displayed_foreign_snapshot_confers_no_write_or_cleanup(self):
        self.bind("A", "primary")
        self.bind("B", "foreign")
        self.registry.select_view("A", "primary", "foreign")
        self.registry.authorize_mutation("A", "primary")
        for operation in [self.registry.authorize_mutation, self.registry.authorize_cleanup]:
            with self.assertRaises(Refused):
                operation("A", "foreign")

    def test_old_primary_stays_reserved_but_cannot_be_foreground_write_target(self):
        self.bind("A", "old")
        self.bind("A", "new")
        self.registry.select_view("A", "old", "old")
        self.registry.select_view("A", "new", "new")
        with self.assertRaises(Refused):
            self.registry.authorize_mutation("A", "old")
        self.registry.authorize_mutation("A", "new")
        with self.assertRaises(Refused):
            self.registry.claim("B", "old")

    def test_missing_or_changed_role_never_grants_background_admission(self):
        self.registry.claim("A", "unverified")
        self.registry.select_view("A", "unverified", "unverified")
        with self.assertRaises(Refused):
            self.registry.authorize_mutation("A", "unverified")
        with self.assertRaises(Refused):
            self.registry.bind_role("A", "unverified", None)
        self.registry.bind_role("A", "unverified", False)
        with self.assertRaises(Refused):
            self.registry.bind_role("A", "unverified", True)
        with self.assertRaises(Refused):
            self.registry.bind_role("B", "unverified", True)

    def test_cleanup_after_quarantine_does_not_reopen_writes_or_release(self):
        self.bind("A", "primary")
        self.registry.select_view("A", "primary", "primary")
        self.registry.quarantine("A")
        for method in ["turn/interrupt", "thread/unsubscribe"]:
            self.assertEqual(self.callback(method, "primary", {"outcome": "ok"})[-1]["phase"], "committed")
        self.assertEqual(self.registry.snapshot()["owners"][0][3], "UNKNOWN")
        with self.assertRaises(Refused):
            self.registry.authorize_mutation("A", "primary")
        with self.assertRaises(Refused):
            self.registry.claim("B", "primary")

    def test_cleanup_after_fence_does_not_reactivate(self):
        self.bind("A", "root")
        epoch = self.registry.snapshot()["owners"][0][2]
        self.registry.fence("A", "root", epoch)
        self.registry.authorize_cleanup("A", "root")
        self.assertEqual(self.registry.snapshot()["owners"][0][3], "FENCED")
        for thread in ["unregistered", "guessed-orphan"]:
            with self.assertRaises(Refused):
                self.registry.authorize_cleanup("A", thread)

    def test_lifecycle_response_persists_role_without_selecting_background(self):
        self.bind("A", "primary")
        self.registry.select_view("A", "primary", "primary")
        self.callback("thread/start", reply={"outcome": "ok", "thread": "background", "ephemeral": True})
        self.registry.authorize_mutation("A", "background")
        self.registry.authorize_mutation("A", "primary")

    def test_missing_native_role_quarantines_uncertain_created_root(self):
        with self.assertRaises(Refused):
            self.callback("thread/start", reply={"outcome": "ok", "thread": "created"})
        self.assertEqual(self.registry.snapshot()["owners"][0][3], "UNKNOWN")
        self.assertEqual(self.registry.snapshot()["intents"][0][3], "UNKNOWN")

    def test_caller_flag_cannot_turn_old_primary_into_background(self):
        self.bind("A", "old")
        self.bind("A", "new")
        self.registry.select_view("A", "new", "new")
        with self.assertRaisesRegex(Refused, "acknowledged primary"):
            self.callback("turn/start", "old", {"outcome": "ok"}, ephemeral=True)


if __name__ == "__main__":
    unittest.main()
