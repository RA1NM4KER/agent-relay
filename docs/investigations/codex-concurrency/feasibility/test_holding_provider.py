"""Real loopback fixture obligations; these are not native TUI proof."""
import http.client
import tempfile
import time
from pathlib import Path
import unittest
from urllib.parse import urlsplit
from holding_provider import HoldingProvider


class HoldingObligations(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="relay-holding-test-", dir="/private/tmp")
        self.path = Path(self.temp.name) / "events.jsonl"
        self.provider = HoldingProvider(self.path)
        self.clients = []

    def tearDown(self):
        for client, response in self.clients:
            response.close()
            client.close()
        self.provider.close()
        self.temp.cleanup()

    def open_stream(self, thread):
        url = urlsplit(self.provider.url)
        client = http.client.HTTPConnection(url.hostname, url.port, timeout=3)
        client.request("POST", "/v1/responses", body="request-payload-must-not-be-recorded",
                       headers={"thread-id": thread})
        response = client.getresponse()
        self.assertEqual(response.status, 200)
        self.assertEqual(response.read(1), b"e")
        self.clients.append((client, response))
        return client, response

    def await_condition(self, condition):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if condition():
                return
            time.sleep(.02)
        self.fail("fixture lifecycle evidence missing")

    def test_eight_open_streams_and_one_disconnect_leave_seven_active(self):
        threads = {f"00000000-0000-0000-0000-{i:012d}" for i in range(8)}
        streams = {thread: self.open_stream(thread) for thread in threads}
        self.assertEqual(set(self.provider.active(threads)), threads)
        first = sorted(threads)[0]
        client, response = streams[first]
        response.close()
        client.close()
        self.await_condition(lambda: self.provider.disconnected({first}))
        self.assertEqual(set(self.provider.active(threads)), threads - {first})
        self.assertNotIn("request-payload", self.path.read_text())
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)

    def test_expiry_cannot_reuse_an_old_disconnect_as_cancellation(self):
        self.provider.close()
        self.path = self.path.with_name("short.jsonl")
        self.provider = HoldingProvider(self.path, lifetime=.4)
        thread = "00000000-0000-0000-0000-000000000001"
        client, response = self.open_stream(thread)
        response.close()
        client.close()
        self.await_condition(lambda: self.provider.disconnected({thread}))
        self.open_stream(thread)
        self.await_condition(lambda: any(r.get("reason") == "expired" for r in self.provider.snapshot()))
        self.assertFalse(self.provider.disconnected({thread}))

    def test_missing_identity_does_not_create_an_inferred_binding(self):
        url = urlsplit(self.provider.url)
        client = http.client.HTTPConnection(url.hostname, url.port, timeout=3)
        try:
            client.request("POST", "/v1/responses", body='{"thread":"guessed-from-body"}')
            response = client.getresponse()
            self.assertEqual(response.status, 400)
            response.read()
            self.assertEqual(self.provider.snapshot(), [])
            self.assertIn("http_refused", self.path.read_text())
            self.assertNotIn("guessed-from-body", self.path.read_text())
        finally:
            client.close()


if __name__ == "__main__":
    unittest.main()
