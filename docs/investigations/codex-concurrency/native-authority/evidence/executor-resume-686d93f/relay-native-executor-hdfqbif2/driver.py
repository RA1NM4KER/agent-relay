#!/usr/bin/env python3
"""Real native /shell -> enrolled RAM-only VM, with independent revocation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import subprocess
import tempfile
import time

from check_native import Peer, Refused, environment, socket_quiet


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--app-server", type=Path, required=True)
    parser.add_argument("--manager", type=Path, required=True)
    parser.add_argument("--images", type=Path, required=True)
    parser.add_argument("--interrupt", action="store_true", help="exercise actual native cancellation before revocation")
    parser.add_argument("--stop-delay-ms", type=int, default=0, choices=range(0, 1001))
    parser.add_argument("--require-stopped", action="store_true", help="fail if native cancellation does not obtain trusted kernel-stop evidence")
    args = parser.parse_args()
    if shutil.disk_usage(args.images).free < 4 * 1024**3:
        raise RuntimeError("unchanged 4 GiB integration entry guard")
    root = Path(tempfile.mkdtemp(prefix="relay-native-executor-", dir="/private/tmp"))
    shutil.copyfile(__file__, root / "driver.py")
    env = environment(root)
    authority = root / "codex/relay-authority-v1"
    authority.mkdir(mode=0o700)
    enrollment = secrets.token_hex(32)
    owners = {name: {"key": secrets.token_hex(32), "recoveryKey": secrets.token_hex(32)}
              for name in ("A", "B")}
    vault = root / "supervisor-capabilities.json"
    fd = os.open(vault, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump({"enrollment": enrollment, "owners": owners}, stream)
        stream.flush()
        os.fsync(stream.fileno())
    policy = {"version": 1, "profile_identity": f"{authority.parent.stat().st_dev}:{authority.parent.stat().st_ino}",
              "enrollment_sha256": hashlib.sha256(enrollment.encode()).hexdigest(), "shell_realm": {}}
    for name, path in (("manager", args.manager), ("kernel", args.images / "kernel"),
                       ("initramfs", args.images / "executor-initramfs")):
        path = path.resolve(strict=True)
        policy["shell_realm"][name] = str(path)
        policy["shell_realm"][name + "_sha256"] = digest(path)
    (authority / "policy.json").write_text(json.dumps(policy))
    result = {"root": str(root), "checks": [], "scope": "NATIVE_USER_SHELL_RAM_ONLY_LINUX_GUEST",
              "fullParity": False, "handoffPermitted": False,
              "nativeServerSHA256": digest(args.app_server), "policy": policy,
              "configuredMemoryBytesPerRealm": 192 * 1024**2}
    result["interruptExperiment"] = args.interrupt
    peers, process, log = [], None, None
    serial = 0
    started = time.monotonic()

    def check(name, value):
        if not value:
            raise AssertionError(name)
        result["checks"].append(name)

    def until(predicate, description, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.05)
        raise AssertionError(description)

    def call(peer, owner, method, params):
        nonlocal serial
        serial += 1
        return peer.call(method, {**params, "_relay": {"key": owners[owner]["key"],
                         "epoch": 1, "token": f"executor-{serial}"}})

    def runtimes():
        found = {}
        for directory in (authority / "realm-executions").glob("*"):
            if not directory.is_dir():
                continue
            intent = {"thread": "unknown:" + directory.name, "realmID": directory.name}
            state = {"status": "UNKNOWN", "vmStopped": False}
            try:
                intent = json.loads((directory / "intent.json").read_text())
                state = json.loads((directory / "state.json").read_text())
            except (OSError, ValueError) as error:
                state["observationError"] = str(error)
            found[intent["thread"]] = (directory, intent, state)
        return found

    def pump(peer, thread):
        peer.turns(thread)
        # Input/started-item notifications contain the script itself. They
        # cannot prove that the guest ever executed or emitted a heartbeat.
        return "".join(event.get("params", {}).get("delta", "")
                       for event in peer.events
                       if event.get("method") == "item/commandExecution/outputDelta"
                       and event.get("params", {}).get("threadId") == thread)

    def interrupted(signum, frame):
        raise InterruptedError("reserve guard; close exact owned execution realms")

    signal.signal(signal.SIGTERM, interrupted)
    sock = root / "native.sock"
    try:
        log = (root / "host.stderr").open("wb")
        process = subprocess.Popen([str(args.app_server), "--listen", f"unix://{sock}"],
                                   env=env, cwd=root, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=log, start_new_session=True)
        until(lambda: sock.exists() and not socket_quiet(sock), "native endpoint")
        bindings = {}
        for owner in owners:
            peer = Peer(sock, env["CODEX_HOME"])
            peers.append(peer)
            peer.call("relay/authority", {"command": "register", "_relay": {
                **owners[owner], "enrollmentKey": enrollment, "epoch": 1, "token": f"register-{owner}"}})
            bindings[owner] = call(peer, owner, "thread/start", {
                "cwd": str(root), "model": "research-model", "modelProvider": "research",
                "approvalPolicy": "never", "sandbox": "read-only"})["thread"]["id"]
        result["bindings"] = bindings
        canary = root / "host-only-canary"
        canary.write_text("host-only-research-sentinel")
        for owner, peer in zip(owners, peers):
            script = (f"/bin/busybox cat {canary} 2>/dev/null || echo HOST_CANARY_UNAVAILABLE; "
                      f"/bin/busybox mkdir -p /work; /bin/busybox setsid /bin/sh -c '"
                      f"while :; do echo {owner} >>/work/effects; echo REALM_{owner}_EFFECT; "
                      "/bin/busybox sleep .1; done' </dev/null & /bin/busybox sleep 15")
            call(peer, owner, "thread/shellCommand", {"threadId": bindings[owner],
                 "command": script, "timeoutMs": 25000})
        until(lambda: len(runtimes()) == 2 and all(v[2].get("vmRunning") for v in runtimes().values()),
              "two actual native shell execution realms running")
        check("two real native shell requests launch distinct execution realms", len(runtimes()) == 2)
        for owner, peer in zip(owners, peers):
            until(lambda owner=owner, peer=peer: f"REALM_{owner}_EFFECT" in pump(peer, bindings[owner]),
                  f"native {owner} delivers detached guest output")
        result["bootAndNativeDispatchSeconds"] = time.monotonic() - started
        running = runtimes()
        result["runningRealms"] = {owner: running[thread][2] for owner, thread in bindings.items()}
        for owner, thread in bindings.items():
            _, intent, state = running[thread]
            check(f"{owner} native identity matches trusted realm identity",
                  intent["root"] == thread and intent["realmID"] == state["realmID"]
                  and intent["epoch"] == 1 and bool(intent["hostGeneration"]))
            check(f"{owner} no host filesystem or external execution devices",
                  all(state[key] == 0 for key in ("networkDevices", "storageDevices", "sharedDirectories", "socketDevices")))
        pids = [str(value[2]["managerPID"]) for value in running.values()]
        result["managerRSSSample"] = subprocess.check_output(["ps", "-o", "pid=,rss=", "-p", ",".join(pids)], text=True)
        expected_unknown = False
        if args.interrupt:
            if args.stop_delay_ms:
                (running[bindings["A"]][0] / "test-stop-delay-ms").write_text(str(args.stop_delay_ms))
            turns = peers[0].turns(bindings["A"])
            active = [turn for turn, state in turns.items() if state == "inProgress"]
            check("native A has one exact active shell turn before interrupt", len(active) == 1)
            interrupt_started = time.monotonic()
            call(peers[0], "A", "turn/interrupt", {"threadId": bindings["A"], "turnId": active[0]})
            until(lambda: all(state != "inProgress" for state in peers[0].turns(bindings["A"]).values()),
                  "native A cancellation completes")
            result["nativeCancellationRealmState"] = runtimes()[bindings["A"]][2]
            expected_unknown = not result["nativeCancellationRealmState"].get("vmStopped")
            result["nativeCancellationQuiescence"] = "UNKNOWN" if expected_unknown else "KERNEL_STOPPED"
            result["nativeInterruptSeconds"] = time.monotonic() - interrupt_started
            result["injectedStopDelayMs"] = args.stop_delay_ms
            if args.require_stopped:
                check("native cancellation waits for exact trusted kernel-stop acknowledgement", not expected_unknown)
                check("native stop waits beyond injected delay instead of 100 ms task teardown",
                      result["nativeInterruptSeconds"] >= args.stop_delay_ms / 1000)
            try:
                call(peers[0], "A", "relay/authority", {"command": "handoff", "threadId": bindings["A"], "executionQuiescent": True})
            except Refused as error:
                check("native handoff refuses after cancellation regardless of caller quiescence assertion",
                      "RELAY_NATIVE_AUTHORITY" in str(error))
            else:
                raise AssertionError("native cancellation incorrectly permitted handoff")
        revoke = call(peers[0], "A", "relay/authority", {"command": "revoke", "threadId": bindings["A"]})
        check("native revoke does not manufacture full quiescence or handoff", not revoke["executionQuiescent"] and not revoke["handoffPermitted"])
        if expected_unknown:
            check("native cancellation does not substitute turn completion for trusted kernel stop", True)
            result["expectedUnknownThreads"] = [bindings["A"]]
        else:
            until(lambda: runtimes()[bindings["A"]][2].get("vmStopped"), "trusted A kernel stop")
            state = runtimes()[bindings["A"]][2]
            check("owner-specific native fence stops exact A kernel", state["status"] == "STOPPED" and state["admissionClosed"])
        count = pump(peers[1], bindings["B"]).count("REALM_B_EFFECT")
        until(lambda: runtimes()[bindings["B"]][2].get("vmRunning") and
              pump(peers[1], bindings["B"]).count("REALM_B_EFFECT") >= count + 3,
              "B continues after A revoke", timeout=4)
        check("B detached execution continues after A cancellation" if args.interrupt else
              "B detached execution continues after A kernel stop", True)
        check("guest scripts cannot alter host-only research canary", canary.read_text() == "host-only-research-sentinel")
        try:
            call(peers[0], "A", "thread/shellCommand", {"threadId": bindings["A"], "command": "true", "timeoutMs": 1000})
        except Refused as error:
            check("fenced native A cannot launch another executor", "RELAY_NATIVE_AUTHORITY" in str(error))
        else:
            raise AssertionError("fenced native launch accepted")
        call(peers[1], "B", "relay/authority", {"command": "revoke", "threadId": bindings["B"]})
        until(lambda: runtimes()[bindings["B"]][2].get("vmStopped"), "trusted B kernel stop")
        check("B stops independently through its own native fence", True)
        result["passed"] = True
    except BaseException as error:
        result.update(passed=False, error=repr(error))
    finally:
        # Close every enrolled realm before touching native host cleanup; no PID
        # guessing, no process-group inference, no guest-text safety receipt.
        fences = authority / "realm-fences"
        fences.mkdir(exist_ok=True)
        for data in owners.values():
            (fences / hashlib.sha256(data["recoveryKey"].encode()).hexdigest()).touch()
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            live = [v for v in runtimes().values() if not v[2].get("vmStopped")]
            if not live:
                break
            time.sleep(.1)
        result["finalRealms"] = {thread: {"intent": intent, "state": state}
                                  for thread, (_, intent, state) in runtimes().items()}
        result["realmCleanupProven"] = all(v["state"].get("vmStopped") for v in result["finalRealms"].values())
        result["unprovenRealmThreads"] = [thread for thread, value in result["finalRealms"].items()
                                         if not value["state"].get("vmStopped")]
        for peer in peers:
            try:
                peer.close()
            except Exception:
                pass
        if process:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=6)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
            result["hostCleanup"] = {"pid": process.pid, "exitCode": process.returncode,
                                      "socketQuiet": socket_quiet(sock)}
        if log:
            log.close()
        manager_ids = {str(value["state"].get("managerPID")) for value in result["finalRealms"].values()}
        processes = subprocess.check_output(["ps", "-axo", "pid=,comm="], text=True).splitlines()
        result["managerProcessesRemaining"] = [line.strip() for line in processes
                                                if line.split() and line.split()[0] in manager_ids
                                                and str(args.manager) in line]
        # Evidence I/O may fail; all runtime cleanup above must still happen.
        for index, peer in enumerate(peers):
            try:
                (root / f"native-events-{index}.json").write_text(json.dumps(peer.events, indent=2) + "\n")
            except OSError as error:
                result.setdefault("evidenceErrors", []).append(str(error))
        result["elapsedSeconds"] = time.monotonic() - started
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
    cleanup_matches = result.get("realmCleanupProven") or (
        args.interrupt and not result.get("managerProcessesRemaining")
        and set(result.get("unprovenRealmThreads", [])) <= set(result.get("expectedUnknownThreads", [])))
    return 0 if result.get("passed") and cleanup_matches else 1


if __name__ == "__main__":
    raise SystemExit(main())
