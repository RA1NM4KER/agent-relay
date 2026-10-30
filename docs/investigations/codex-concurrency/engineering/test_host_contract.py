"""Contract interleavings, not proof of native-host or OS enforcement."""
import concurrent.futures
from pathlib import Path
import tempfile
import threading
import time
import unittest
from host_contract import HostContract
from controller import Refused, Registry


class Native:
    closed = False

    def __init__(self):
        self.calls = 0
        self.entered = threading.Event()
        self.release = threading.Event()

    def call(self, method, params):
        self.calls += 1
        self.entered.set()
        if not self.release.wait(2):
            raise TimeoutError("test release missing")
        return {"thread": {"id": "exact-native-thread"}}


class ContractTests(unittest.TestCase):
    def test_close_waits_for_admitted_operation_before_releasing_service_lock(self):
        with tempfile.TemporaryDirectory(prefix="relay-host-test-", dir="/private/tmp") as directory:
            root = Path(directory)
            (root / "home").mkdir()
            registry = Registry(root / "registry", root / "home")
            native = Native()
            host = HostContract(root / "host", registry, native)
            peer = host.connect()
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                call = pool.submit(host.submit, peer, "create", "thread/start", {})
                self.assertTrue(native.entered.wait(1))
                close = pool.submit(host.close)
                time.sleep(.03)
                self.assertFalse(close.done())
                with self.assertRaises(Refused):
                    HostContract(root / "host", registry, native)
                native.release.set()
                call.result(timeout=2)
                close.result(timeout=2)
            replacement = HostContract(root / "host", registry, native)
            self.assertEqual(replacement.status(peer, "create")["state"], "COMMITTED")
            with self.assertRaisesRegex(Refused, "old host generation"):
                replacement.submit(peer, "new", "thread/start", {})
            replacement.close()

    def test_same_token_concurrent_retry_forwards_once(self):
        with tempfile.TemporaryDirectory(prefix="relay-host-test-", dir="/private/tmp") as directory:
            root = Path(directory)
            (root / "home").mkdir()
            registry = Registry(root / "registry", root / "home")
            native = Native()
            native.release.set()
            host = HostContract(root / "host", registry, native)
            try:
                peer = host.connect()
                with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                    replies = list(pool.map(lambda _: host.submit(peer, "token", "thread/start", {}), range(8)))
                self.assertEqual(native.calls, 1)
                self.assertTrue(all(reply == replies[0] for reply in replies))
                with self.assertRaisesRegex(Refused, "different request"):
                    host.submit(peer, "token", "thread/start", {"changed": True})
            finally:
                host.close()


if __name__ == "__main__":
    unittest.main()
