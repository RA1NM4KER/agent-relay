#!/usr/bin/env python3
"""Private two-phase callback for the source-insertion prototype, not production.

The parent must supply an isolated research directory/home. Only identity
metadata crosses this pipe. This cannot fence native autonomous work or approvals.
"""
import json
import sys
from controller import Refused, Registry


class CallbackRegistry(Registry):
    """Cooperating frontend lab only; no host/peer security boundary implied."""
    def __init__(self, directory, home):
        super().__init__(directory, home)
        with self.connect() as db:
            db.executescript("""
                CREATE TABLE IF NOT EXISTS native_roles(
                    home TEXT, thread TEXT, owner TEXT, ephemeral INTEGER,
                    PRIMARY KEY(home,thread));
                CREATE TABLE IF NOT EXISTS native_views(
                    owner TEXT PRIMARY KEY, primary_thread TEXT, displayed_thread TEXT);
            """)

    def bind_role(self, owner, thread, ephemeral):
        # Only a completed native lifecycle response supplies this field.
        if type(ephemeral) is not bool:
            raise Refused("native lifecycle response omitted ephemeral role")
        with self.transaction() as db:
            row = db.execute("SELECT owner FROM owners WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
            if row != (owner,):
                raise Refused("cannot attach role to foreign/unreserved root")
            row = db.execute("SELECT owner,ephemeral FROM native_roles WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
            expected = (owner, int(ephemeral))
            if row is not None and row != expected:
                raise Refused("native role changed for an existing root")
            db.execute("INSERT OR IGNORE INTO native_roles VALUES(?,?,?,?)",
                       (self.home, thread, owner, int(ephemeral)))

    def select_view(self, owner, primary, displayed):
        # Display observation never creates an owner or role. A foreign read-only
        # snapshot is allowed here but still cannot authorize writes.
        with self.transaction() as db:
            row = db.execute("SELECT sequence FROM selections WHERE owner=?", (owner,)).fetchone()
            sequence = 1 if row is None else row[0] + 1
            db.execute("INSERT OR REPLACE INTO selections VALUES(?,?,?)",
                       (owner, sequence, displayed))
            db.execute("INSERT OR REPLACE INTO native_views VALUES(?,?,?)",
                       (owner, primary, displayed))
            return sequence

    def authorize_mutation(self, owner, thread):
        with self.connect() as db:
            row = db.execute("SELECT owner,epoch,state FROM owners WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
            role = db.execute("SELECT owner,ephemeral FROM native_roles WHERE home=? AND thread=?",
                              (self.home, thread)).fetchone()
            view = db.execute("SELECT primary_thread FROM native_views WHERE owner=?", (owner,)).fetchone()
        if row is None:
            raise Refused("native thread has no research controller")
        self.authorize(owner, thread, row[1])
        if role is None or role[0] != owner:
            raise Refused("native role is unverified")
        if not role[1] and view != (thread,):
            raise Refused("mutation does not match acknowledged primary root")

    def authorize_cleanup(self, owner, thread):
        with self.connect() as db:
            row = db.execute("SELECT owner,state FROM owners WHERE home=? AND thread=?",
                             (self.home, thread)).fetchone()
        # A failed/fenced writer may cancel only its already known roots.
        # No release, reactivation, implicit adoption or guessed orphan identity.
        if row is None or row[0] != owner or row[1] not in {"ACTIVE", "UNKNOWN", "FENCED"}:
            raise Refused("cleanup requires an existing exact owned reservation")


def frame():
    line = sys.stdin.readline(4097)
    if not line or len(line) > 4096:
        raise Refused("missing/oversized callback frame")
    return json.loads(line)


def emit(value):
    print(json.dumps(value), flush=True)


def main():
    message = frame()
    registry = CallbackRegistry(message["registry"], message["home"])
    owner, method, thread = message["client"], message["method"], message.get("thread")
    if method == "selection":
        # Selection is not ownership. A read-only view/child can be displayed.
        sequence = registry.select_view(owner, message.get("primary"), thread)
        emit({"allow": True, "sequence": sequence})
        return
    cleanup = method in {"turn/interrupt", "thread/unsubscribe"}
    transition = method in {"thread/start", "thread/fork", "thread/resume"}
    context = registry.creation_lock() if transition else registry.admission_lock(thread)
    with context:
        token = None
        try:
            if cleanup:
                registry.authorize_cleanup(owner, thread)
            elif method == "thread/resume":
                registry.claim(owner, thread)
            elif method == "thread/fork" or not transition:
                registry.authorize_mutation(owner, thread)
            if transition:
                token = registry.intent(owner, method)
            emit({"allow": True, "phase": "prepared"})
            reply = frame()
            if reply.get("outcome") != "ok":
                raise Refused("native outcome uncertain or refused; reconcile before reusing")
            if transition:
                target = reply.get("thread")
                if not isinstance(target, str) or not target:
                    raise Refused("missing concrete native thread response")
                if method == "thread/resume" and target != thread:
                    raise Refused("native thread mismatch")
                registry.claim(owner, target, token)
                registry.bind_role(owner, target, reply.get("ephemeral"))
                registry.intent_result(token, "BOUND", target)
            emit({"allow": True, "phase": "committed"})
        except BaseException:
            if token:
                registry.intent_result(token, "UNKNOWN")
            registry.quarantine(owner)
            raise


if __name__ == "__main__":
    try:
        main()
    except BaseException as error:
        emit({"allow": False, "reason": type(error).__name__})
        sys.exit(1)
