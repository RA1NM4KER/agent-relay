#!/usr/bin/env python3
"""Private two-phase callback for the source-insertion prototype, not production.

The parent must supply an isolated research directory/home. Only identity
metadata crosses this pipe. This cannot fence native autonomous work or approvals.
"""
import json
import sys
from controller import Refused, Registry


def frame():
    line = sys.stdin.readline(4097)
    if not line or len(line) > 4096:
        raise Refused("missing/oversized callback frame")
    return json.loads(line)


def emit(value):
    print(json.dumps(value), flush=True)


def main():
    message = frame()
    registry = Registry(message["registry"], message["home"])
    owner, method, thread = message["client"], message["method"], message.get("thread")
    if method == "selection":
        # Selection is not ownership. A read-only view/child can be displayed.
        sequence = registry.select(owner, thread)
        emit({"allow": True, "sequence": sequence})
        return
    transition = method in {"thread/start", "thread/fork", "thread/resume"}
    context = registry.creation_lock() if transition else registry.admission_lock(thread)
    with context:
        token = None
        try:
            if method == "thread/resume":
                registry.claim(owner, thread)
            elif method == "thread/fork" or not transition:
                row = next((r for r in registry.snapshot()["owners"] if r[0] == thread), None)
                if row is None:
                    raise Refused("native thread has no research controller")
                registry.authorize(owner, thread, row[2])
                if not transition:
                    with registry.connect() as db:
                        selected = db.execute("SELECT thread FROM selections WHERE owner=?", (owner,)).fetchone()
                    if selected != (thread,):
                        raise Refused("mutation does not match acknowledged selection")
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
