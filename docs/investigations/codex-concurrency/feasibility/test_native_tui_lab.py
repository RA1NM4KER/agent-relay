"""Adversarial evidence-oracle cases, not native execution proof."""
import unittest
from pathlib import Path
import socket
import tempfile
from native_tui_lab import changed_bindings, completed_foreground, loaded_bindings, SharedLoadedObserver, socket_quiet


def event(kind, thread="root", turn="turn", sequence=1, client="A"):
    return dict(kind=kind, thread=thread, turn=turn, sequence=sequence, client=client)


class EvidenceOracle(unittest.TestCase):
    def test_daemon_cleanup_does_not_confuse_a_stale_socket_with_a_listener(self):
        with tempfile.TemporaryDirectory(dir="/private/tmp") as root:
            path = Path(root) / "control.sock"
            self.assertTrue(socket_quiet(path))
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
                server.bind(str(path))
                server.listen(1)
                self.assertFalse(socket_quiet(path))
            self.assertTrue(path.exists())
            self.assertTrue(socket_quiet(path))

    def test_shared_proof_requires_exact_in_memory_roots_and_refuses_mutation(self):
        self.assertFalse(loaded_bindings(["persisted-elsewhere"], {"A": "root"}))
        self.assertFalse(loaded_bindings([], {"A": "root"}))
        self.assertFalse(loaded_bindings(["root"], {}))
        self.assertTrue(loaded_bindings(["root", "helper"], {"A": "root"}))
        with self.assertRaises(AssertionError):
            loaded_bindings(["root", "root"], {"A": "root"})
        with self.assertRaises(AssertionError):
            SharedLoadedObserver.__new__(SharedLoadedObserver).call("thread/resume", {"threadId": "root"})

    def test_missing_selection_or_duplicate_ids_cannot_pass_navigation(self):
        self.assertFalse(changed_bindings({}, {}))
        self.assertFalse(changed_bindings({}, {"A": "old"}))
        self.assertFalse(changed_bindings({"A": None}, {"A": "old"}))
        self.assertFalse(changed_bindings({"A": "same", "B": "same"}, {"A": "old", "B": "other"}))
        self.assertTrue(changed_bindings({"A": "new"}, {"A": "old"}))

    def test_empty_or_accept_without_native_start_cannot_supply_drain(self):
        self.assertFalse(completed_foreground([], {}, {}))
        self.assertFalse(completed_foreground([], {"A": "root"}, {}))
        self.assertFalse(completed_foreground([event("turn/accepted"), event("turn/completed")], {"A": "root"}, {}))

    def test_other_root_or_old_phase_completion_cannot_supply_drain(self):
        rows = [event("turn/accepted"), event("turn/started"), event("turn/completed", thread="foreign")]
        self.assertFalse(completed_foreground(rows, {"A": "root"}, {}))
        rows[-1] = event("turn/completed")
        self.assertTrue(completed_foreground(rows, {"A": "root"}, {}))
        self.assertFalse(completed_foreground(rows, {"A": "root"}, {"A": 1}))

    def test_unobserved_accepted_turn_and_coverage_loss_fail_closed(self):
        rows = [event("turn/accepted"), event("turn/started"), event("turn/completed"),
                event("turn/accepted", turn="later")]
        self.assertFalse(completed_foreground(rows, {"A": "root"}, {}))
        with self.assertRaises(AssertionError):
            completed_foreground([event("coverage-lost")], {"A": "root"}, {})


if __name__ == "__main__":
    unittest.main()
