"""Executable contract laboratory; NOT native host enforcement or production code.

One exclusive laboratory service wraps the real backend. Its clients do not get
the backend object. Unmodified Codex still accepts requests outside this service;
run_integrated.py deliberately demonstrates that limitation. No transport or
same-uid security boundary is claimed by an in-process Python API.
"""
import contextlib
import fcntl
import hashlib
import json
from pathlib import Path
import secrets
import sqlite3
import sys
import threading
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "feasibility"))
from controller import Client, Refused


class HostContract:
    def __init__(self, directory, registry, native):
        self.directory = Path(directory)
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.guard = (self.directory / "host.lock").open("a+")
        try:
            fcntl.flock(self.guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BaseException:
            self.guard.close()
            raise Refused("another laboratory host owns this service domain")
        self.registry, self.native = registry, native
        self.generation = uuid.uuid4().hex
        self.db = self.directory / "operations.sqlite"
        self.clients = {}
        self.locks = {}
        self.lock = threading.Lock()
        self.closed = False
        with self.connection() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS peers(
                    digest TEXT PRIMARY KEY, owner TEXT, generation TEXT);
                CREATE TABLE IF NOT EXISTS operations(
                    peer TEXT, token TEXT, fingerprint TEXT, state TEXT,
                    response TEXT, PRIMARY KEY(peer, token));
            ''')

    @contextlib.contextmanager
    def connection(self):
        with contextlib.closing(sqlite3.connect(self.db, timeout=10)) as db:
            db.execute("PRAGMA synchronous=FULL")
            with db:
                yield db

    def connect(self):
        with self.lock:
            if self.closed:
                raise Refused("host closed")
            secret = secrets.token_hex(32)
            digest = hashlib.sha256(secret.encode()).hexdigest()
            owner = uuid.uuid4().hex
            with self.connection() as db:
                db.execute("INSERT INTO peers VALUES(?,?,?)", (digest, owner, self.generation))
            self.clients[owner] = Client(self.registry, self.native, owner)
            self.locks[owner] = threading.RLock()
            return secret

    def authenticate(self, secret, mutation=False):
        if self.closed or not isinstance(secret, str):
            raise Refused("host closed or missing peer capability")
        digest = hashlib.sha256(secret.encode()).hexdigest()
        with self.connection() as db:
            row = db.execute("SELECT owner,generation FROM peers WHERE digest=?", (digest,)).fetchone()
        if row is None:
            raise Refused("unknown peer capability")
        if mutation and row[1] != self.generation:
            raise Refused("old host generation; reconnect/recovery required, no implicit adoption")
        return digest, row[0]

    def status(self, secret, token):
        digest, _ = self.authenticate(secret)
        with self.connection() as db:
            row = db.execute("SELECT state,response FROM operations WHERE peer=? AND token=?",
                             (digest, token)).fetchone()
        if row is None:
            return {"state": "NOT_FOUND"}
        return {"state": row[0], "response": json.loads(row[1]) if row[1] else None}

    def submit(self, secret, token, method, params, sequence=None, *, after_apply=None):
        """Persist BEFORE forwarding; commit BEFORE returning; never replay UNKNOWN.

        after_apply is fault injection only, not a client RPC. Token is a bounded
        client-generated operation ID. Stored fingerprints bind the entire request;
        request bodies and capability secrets are not stored in the operation log.
        """
        if not isinstance(token, str) or not 1 <= len(token) <= 128:
            raise Refused("invalid operation token")
        digest, owner = self.authenticate(secret, mutation=True)
        fingerprint = hashlib.sha256(json.dumps([method, params, sequence],
            sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        with self.locks[owner]:
            self.authenticate(secret, mutation=True)
            with self.connection() as db:
                row = db.execute("SELECT fingerprint,state,response FROM operations WHERE peer=? AND token=?",
                                 (digest, token)).fetchone()
                if row:
                    if row[0] != fingerprint:
                        raise Refused("token reused for different request")
                    if row[1] != "COMMITTED":
                        raise Refused("operation unresolved; never retry native execution blindly")
                    return json.loads(row[2])
                # Unknown methods are refused before durable intent/native execution.
                if method not in {"thread/start", "thread/fork", "thread/resume",
                                  "turn/start", "turn/steer", "thread/shellCommand"}:
                    raise Refused("unclassified mutation")
                db.execute("INSERT INTO operations VALUES(?,?,?,'PENDING',NULL)",
                           (digest, token, fingerprint))
            client = self.clients[owner]
            try:
                if method in {"thread/start", "thread/fork", "thread/resume"}:
                    response = client.transition(method, params)
                else:
                    response = client.write(method, params, sequence)
                if after_apply:
                    after_apply(response)
                with self.connection() as db:
                    db.execute("UPDATE operations SET state='COMMITTED',response=? WHERE peer=? AND token=?",
                               (json.dumps(response), digest, token))
                return response
            except BaseException:
                # Native application and this journal are NOT one transaction.
                # Even a known local refusal is conservatively quarantined here.
                client.quarantined = True
                self.registry.quarantine(owner)
                with self.connection() as db:
                    db.execute("UPDATE operations SET state='UNKNOWN' WHERE peer=? AND token=?", (digest, token))
                raise

    def fence(self, secret, thread):
        _, owner = self.authenticate(secret, mutation=True)
        with self.locks[owner]:
            self.authenticate(secret, mutation=True)
            receipt = self.clients[owner].fence_and_drain(thread)
            return {**receipt, "host_generation": self.generation,
                    "epoch": self.clients[owner].epochs[thread]}

    def transfer(self, secret, thread, receipt):
        self.authenticate(secret, mutation=True)
        # No platform/native executor receipt exists. Caller booleans, PID exit,
        # quiet files, or a correctly shaped JSON object cannot supply one.
        raise Refused("EXECUTOR_AUTHORITY_UNPROVEN: reservation retained")

    def close(self):
        with self.lock:
            if self.closed:
                return
            self.closed = True
            locks = list(self.locks.values())
        # Keep the exclusive service inode held until admitted calls finish.
        # A caller authenticated before close must recheck after its peer lock.
        with contextlib.ExitStack() as stack:
            for lock in locks:
                stack.enter_context(lock)
            fcntl.flock(self.guard, fcntl.LOCK_UN)
            self.guard.close()
