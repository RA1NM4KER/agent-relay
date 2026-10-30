"""Deterministic proof-obligation tests; do not replace real Codex runs."""
import tempfile
from pathlib import Path
import threading
import unittest
from controller import NativeRPC, Refused, Registry


class Obligations(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="relay-obligations-", dir="/private/tmp")
        self.root = Path(self.temp.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.registry = Registry(self.root / "registry", self.home)

    def tearDown(self):
        self.temp.cleanup()

    def test_atomic_foreign_adoption_and_owner_reopen(self):
        epoch = self.registry.claim("A", "thread")
        reopened = Registry(self.root / "registry", self.home)
        with self.assertRaises(Refused):
            reopened.claim("B", "thread")
        self.assertEqual(reopened.claim("A", "thread"), epoch)

    def test_home_alias_does_not_create_another_identity(self):
        alias = self.root / "alias"
        alias.symlink_to(self.home)
        self.assertEqual(self.registry.home, Registry(self.root / "registry", alias).home)

    def test_delayed_selection_cannot_authorize_previous_generation(self):
        old = self.registry.select("A", "old")
        new = self.registry.select("A", "new")
        with self.assertRaises(Refused):
            self.registry.check_selection("A", old, "old")
        self.registry.check_selection("A", new, "new")

    def test_unresolved_creation_blocks_unknown_not_existing_neighbor(self):
        epoch = self.registry.claim("B", "existing")
        self.registry.intent("A", "thread/start")
        with self.assertRaises(Refused):
            self.registry.claim("C", "unknown-root")
        self.registry.authorize("B", "existing", epoch)

    def test_pending_native_operation_blocks_its_existing_controller(self):
        epoch = self.registry.claim("A", "existing")
        self.registry.intent("A", "thread/start")
        with self.assertRaises(Refused):
            self.registry.authorize("A", "existing", epoch)

    def test_foreign_or_fenced_epoch_cannot_write(self):
        epoch = self.registry.claim("A", "thread")
        with self.assertRaises(Refused):
            self.registry.authorize("B", "thread", epoch)
        self.registry.fence("A", "thread", epoch)
        with self.assertRaises(Refused):
            self.registry.authorize("A", "thread", epoch)

    def test_immediate_native_ack_without_start_event_is_not_empty_drain(self):
        rpc = object.__new__(NativeRPC)
        rpc.lock = threading.RLock()
        rpc.changed = threading.Condition(rpc.lock)
        rpc.events = []
        rpc.accepted_starts = {"thread": 1}
        rpc.closed = False
        with self.assertRaisesRegex(Refused, "no observed start"):
            rpc.drain_turns("thread", timeout=.02)

    def test_disconnect_cannot_supply_pending_start_receipt(self):
        rpc = object.__new__(NativeRPC)
        rpc.lock = threading.RLock()
        rpc.changed = threading.Condition(rpc.lock)
        rpc.events = []
        rpc.accepted_starts = {"thread": 1}
        rpc.closed = True
        with self.assertRaises(Refused):
            rpc.drain_turns("thread", timeout=.02)


if __name__ == "__main__":
    unittest.main()
