"""Research controller, never imported by production Relay.

Real Codex stdio backend; durable admission for cooperating clients. This is NOT
a host-wide fence: bypass clients, native scheduling and arbitrary descendants
are outside its enforcement. Full handoff deliberately refuses without proof.
"""
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import queue
import sqlite3
import subprocess
import sys
import threading
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "experiments"))
from stdio_probe import Server


class Refused(RuntimeError):
    pass


class Registry:
    def __init__(self, directory, home):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        os.chmod(self.directory, 0o700)
        home = Path(home).resolve(strict=True)
        stat = home.stat()
        self.home = f"{stat.st_dev}:{stat.st_ino}"
        self.db = self.directory / "registry.sqlite"
        with self.creation_lock():
            with self.connect() as db:
                db.executescript('''
                CREATE TABLE IF NOT EXISTS owners(
                    home TEXT, thread TEXT, owner TEXT, epoch TEXT, state TEXT,
                    PRIMARY KEY(home,thread));
                CREATE TABLE IF NOT EXISTS intents(
                    token TEXT PRIMARY KEY, owner TEXT, method TEXT, state TEXT,
                    thread TEXT);
                CREATE TABLE IF NOT EXISTS selections(
                    owner TEXT PRIMARY KEY, sequence INTEGER, thread TEXT);
                ''')

    def connect(self):
        db = sqlite3.connect(self.db, timeout=10, isolation_level=None)
        db.execute("PRAGMA synchronous=FULL")
        return db

    @contextlib.contextmanager
    def transaction(self):
        with contextlib.closing(self.connect()) as db:
            db.execute("BEGIN IMMEDIATE")
            try:
                yield db
                db.execute("COMMIT")
            except BaseException:
                db.execute("ROLLBACK")
                raise

    @contextlib.contextmanager
    def creation_lock(self):
        # Stable inode, no lock-file deletion. Serializes only native transitions.
        with (self.directory / "creation.lock").open("a+") as file:
            fcntl.flock(file, fcntl.LOCK_EX)
            yield

    @contextlib.contextmanager
    def admission_lock(self, thread):
        key = hashlib.sha256((self.home + "\0" + thread).encode()).hexdigest()
        with (self.directory / (key + ".admission.lock")).open("a+") as file:
            fcntl.flock(file, fcntl.LOCK_EX)
            yield

    def claim(self, owner, thread, intent_token=""):
        with self.transaction() as db:
            row = db.execute("SELECT owner,epoch,state FROM owners WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
            if row:
                if row[0] != owner or row[2] != "ACTIVE":
                    raise Refused("native conversation already reserved or fenced")
                return row[1]
            if db.execute("SELECT 1 FROM intents WHERE state IN ('PENDING','UNKNOWN') AND method IN ('thread/start','thread/fork') AND token != ?", (intent_token,)).fetchone():
                raise Refused("unresolved creation may own this unregistered native thread")
            epoch = uuid.uuid4().hex
            db.execute("INSERT INTO owners VALUES(?,?,?,?,?)", (self.home, thread, owner, epoch, "ACTIVE"))
            return epoch

    def authorize(self, owner, thread, epoch):
        with self.transaction() as db:
            if db.execute("SELECT 1 FROM intents WHERE owner=? AND state IN ('PENDING','UNKNOWN')", (owner,)).fetchone():
                raise Refused("controller has an unresolved native operation")
            row = db.execute("SELECT owner,epoch,state FROM owners WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
            if row != (owner, epoch, "ACTIVE"):
                raise Refused("stale, foreign, or fenced controller epoch")

    def select(self, owner, thread):
        with self.transaction() as db:
            row = db.execute("SELECT sequence FROM selections WHERE owner=?", (owner,)).fetchone()
            sequence = 1 if row is None else row[0] + 1
            db.execute("INSERT OR REPLACE INTO selections VALUES(?,?,?)", (owner, sequence, thread))
            return sequence

    def check_selection(self, owner, sequence, thread):
        with self.connect() as db:
            row = db.execute("SELECT sequence,thread FROM selections WHERE owner=?", (owner,)).fetchone()
        if row != (sequence, thread):
            raise Refused("stale frontend selection")

    def intent(self, owner, method):
        token = uuid.uuid4().hex
        with self.transaction() as db:
            if db.execute("SELECT 1 FROM intents WHERE state IN ('PENDING','UNKNOWN') AND method IN ('thread/start','thread/fork')").fetchone():
                raise Refused("unresolved native creation requires recovery")
            db.execute("INSERT INTO intents VALUES(?,?,?,?,NULL)", (token, owner, method, "PENDING"))
        return token

    def intent_result(self, token, state, thread=None):
        with self.transaction() as db:
            db.execute("UPDATE intents SET state=?,thread=? WHERE token=?", (state, thread, token))

    def fence(self, owner, thread, epoch):
        with self.transaction() as db:
            row = db.execute("SELECT owner,epoch,state FROM owners WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
            if row != (owner, epoch, "ACTIVE"):
                raise Refused("cannot fence stale/foreign owner")
            db.execute("UPDATE owners SET state='FENCED' WHERE home=? AND thread=?", (self.home, thread))

    def quarantine(self, owner):
        with self.transaction() as db:
            db.execute("UPDATE owners SET state='UNKNOWN' WHERE home=? AND owner=?", (self.home, owner))

    def snapshot(self):
        with self.connect() as db:
            return {"owners": db.execute("SELECT thread,owner,epoch,state FROM owners WHERE home=?", (self.home,)).fetchall(),
                    "intents": db.execute("SELECT token,owner,method,state,thread FROM intents").fetchall()}


class NativeRPC:
    """One actual controlling connection. Never creates an observer writer."""
    def __init__(self, executable, env):
        self.version = subprocess.check_output([executable, "--version"], env=env,
            cwd=env["HOME"], text=True, stderr=subprocess.DEVNULL, timeout=5).strip()
        if self.version != "codex-cli 0.166.0":
            raise Refused("research native adapter is pinned to codex-cli 0.166.0")
        class CheckedServer(Server):
            def call(self, method, params, timeout=25):
                response = super().call(method, params, timeout)
                if method == "initialize":
                    self.initialization = response
                return response
        self.base = CheckedServer(executable, env, "feasibility-native")
        actual_home = self.base.initialization.get("result", {}).get("codexHome")
        try:
            matches = bool(actual_home) and Path(actual_home).resolve(strict=True) == Path(env["CODEX_HOME"]).resolve(strict=True)
        except (OSError, TypeError):
            matches = False
        if not matches:
            self.base.close()
            raise Refused("native initialize resolved an unexpected CODEX_HOME")
        self.verified_home = actual_home
        self.lock = threading.RLock()
        self.changed = threading.Condition(self.lock)
        self.pending = {}
        self.events = []
        self.serial = self.base.serial
        self.closed = False
        self.calls = []
        self.accepted_starts = {}
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        while True:
            event = self.base.responses.get()
            with self.changed:
                if event.get("research_server_eof"):
                    self.closed = True
                    for channel in self.pending.values():
                        channel.put(Refused("native connection lost; recovery required"))
                    self.changed.notify_all()
                    return
                if "id" in event and event["id"] in self.pending:
                    self.pending[event["id"]].put(event)
                else:
                    if len(self.events) >= 10000:
                        self.closed = True
                        self.changed.notify_all()
                        return
                    self.events.append(event)
                    self.changed.notify_all()

    def call(self, method, params, timeout=10):
        channel = queue.Queue()
        with self.lock:
            if self.closed:
                raise Refused("native event coverage unavailable")
            self.serial += 1
            serial = self.serial
            self.pending[serial] = channel
            self.calls.append({"method": method, "thread": params.get("threadId"), "id": serial})
            self.base.send({"id": serial, "method": method, "params": params})
        try:
            response = channel.get(timeout=timeout)
            if isinstance(response, BaseException):
                raise response
            if "error" in response:
                raise Refused(json.dumps(response["error"]))
            if method in {"turn/start", "thread/shellCommand"}:
                with self.changed:
                    thread = params["threadId"]
                    self.accepted_starts[thread] = self.accepted_starts.get(thread, 0) + 1
                    self.changed.notify_all()
            return response["result"]
        finally:
            with self.lock:
                self.pending.pop(serial, None)

    def events_for(self, thread):
        with self.lock:
            return [e for e in self.events if e.get("params", {}).get("threadId") == thread
                    or e.get("params", {}).get("thread", {}).get("id") == thread]

    def drain_turns(self, thread, timeout=5):
        deadline = time.monotonic() + timeout
        with self.changed:
            # An immediate shell-command RPC acknowledgement can precede its
            # turn/started event. Empty observations are not a drain proof.
            expected = self.accepted_starts.get(thread, 0)
            while True:
                started = {e["params"]["turn"]["id"] for e in self.events_for(thread)
                           if e.get("method") == "turn/started"}
                if len(started) >= expected:
                    break
                remaining = deadline - time.monotonic()
                if self.closed or remaining <= 0:
                    raise Refused("accepted native command has no observed start; drain unknown")
                self.changed.wait(remaining)
            events = self.events_for(thread)
            started = {e["params"]["turn"]["id"] for e in events if e.get("method") == "turn/started"}
            completed = {e["params"]["turn"]["id"] for e in events if e.get("method") == "turn/completed"}
        for turn in started - completed:
            self.call("turn/interrupt", {"threadId": thread, "turnId": turn})
        with self.changed:
            while True:
                if self.closed:
                    raise Refused("cannot establish turn drain after disconnect")
                completed = {e["params"]["turn"]["id"] for e in self.events_for(thread)
                             if e.get("method") == "turn/completed"}
                if started <= completed:
                    return {"kind": "TURN_DRAIN_ONLY", "thread": thread,
                            "turns": sorted(started), "execution_quiescent": False}
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Refused("turn drain timeout")
                self.changed.wait(remaining)

    def close(self, kill=False):
        self.base.close(kill=kill)
        self.reader.join(timeout=3)


class Client:
    def __init__(self, registry, native, owner=None):
        self.registry, self.native = registry, native
        self.owner = owner or uuid.uuid4().hex
        self.lock = threading.RLock()
        self.selected = None
        self.epochs = {}
        self.sequence = 0
        self.quarantined = False
        if any(row[1] == self.owner and row[3] in {"PENDING", "UNKNOWN"}
               for row in self.registry.snapshot()["intents"]):
            self.quarantined = True
            self.registry.quarantine(self.owner)

    def transition(self, method, params):
        if method not in {"thread/start", "thread/fork", "thread/resume"}:
            raise Refused("unrecognized lifecycle method")
        with self.lock, self.registry.creation_lock():
            if self.quarantined:
                raise Refused("client has unresolved native operation")
            reserved = None
            if method == "thread/resume":
                reserved = params["threadId"]
                self.epochs[reserved] = self.registry.claim(self.owner, reserved)
            elif method == "thread/fork":
                self._authorize(params["threadId"], None)
            token = self.registry.intent(self.owner, method)
            try:
                result = self.native.call(method, params)
                thread = result["thread"]["id"]
                if reserved is not None and thread != reserved:
                    raise Refused("native resume returned a different thread")
                epoch = self.registry.claim(self.owner, thread, token)
                self.epochs[thread] = epoch
                sequence = self.registry.select(self.owner, thread)
                self.registry.intent_result(token, "BOUND", thread)
                self.selected, self.sequence = thread, sequence
                return {"thread": thread, "epoch": epoch, "sequence": sequence, "intent": token}
            except BaseException:
                # Conservative: failed/uncertain operation never releases potentially live work.
                self.registry.intent_result(token, "UNKNOWN")
                self.registry.quarantine(self.owner)
                self.quarantined = True
                raise

    def _authorize(self, thread, sequence):
        if self.quarantined or self.native.closed:
            raise Refused("client or native coverage unknown")
        if sequence is not None:
            self.registry.check_selection(self.owner, sequence, thread)
        self.registry.authorize(self.owner, thread, self.epochs.get(thread))

    def write(self, method, params, sequence):
        if method not in {"turn/start", "turn/steer", "thread/shellCommand"}:
            raise Refused("unsupported mutation must not pass through this prototype")
        with self.lock, self.registry.admission_lock(params["threadId"]):
            self._authorize(params["threadId"], sequence)
            try:
                return self.native.call(method, params)
            except BaseException:
                self.registry.quarantine(self.owner)
                self.quarantined = True
                raise

    def fence_and_drain(self, thread):
        with self.lock, self.registry.admission_lock(thread):
            self._authorize(thread, None)
            self.registry.fence(self.owner, thread, self.epochs[thread])
            return self.native.drain_turns(thread)

    def transfer(self, thread, successor, receipt):
        # No implemented native tree-quiescence capability: never upgrade turn
        # completion, stable file observations, process death, or a boolean to proof.
        raise Refused("FULL_HANDOFF_UNPROVEN: native execution-tree receipt unavailable")
