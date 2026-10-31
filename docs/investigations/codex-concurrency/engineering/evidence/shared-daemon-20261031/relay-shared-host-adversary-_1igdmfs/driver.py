#!/usr/bin/env python3
"""Falsify external fencing/quiescence on pinned native Unix WebSocket host.

Two independent native connections, isolated HOME, bounded local effects only.
This intentionally expects unsafe candidate behavior; PASS never means parity.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'feasibility'))
from controller import Refused, Registry
from host_contract import HostContract
from native_tui_lab import observer_transport, socket_quiet
from stdio_probe import environment


class Peer:
    def __init__(self, socket, home):
        self.ws = observer_transport()(str(socket), uri='ws://localhost/rpc',
            compression=None, proxy=None, open_timeout=10, close_timeout=2,
            ping_interval=None, max_size=1024*1024, max_queue=16)
        self.serial = 0
        self.calls, self.events = [], []
        self.closed = False
        self.accepted = {}
        try:
            actual = self.call('initialize', {'clientInfo': {'name': 'relay-host-adversary', 'version': '0'},
                'capabilities': {'experimentalApi': True}})['codexHome']
            if Path(actual).resolve(strict=True) != Path(home).resolve(strict=True):
                raise AssertionError('foreign native home')
            self.ws.send(json.dumps({'method': 'initialized', 'params': {}}))
        except BaseException:
            self.close()
            raise

    def call(self, method, params, timeout=10):
        self.serial += 1
        self.calls.append({'method': method, 'thread': params.get('threadId')})
        self.ws.send(json.dumps({'id': self.serial, 'method': method, 'params': params}))
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            event = json.loads(self.ws.recv(timeout=max(.01, end-time.monotonic())))
            if event.get('id') == self.serial and 'method' not in event:
                if 'error' in event:
                    raise Refused(json.dumps(event['error']))
                if method == 'thread/shellCommand':
                    thread = params['threadId']
                    self.accepted[thread] = self.accepted.get(thread, 0) + 1
                return event['result']
            if 'id' in event and 'method' in event:
                raise Refused('unexpected native server request')
            self.events.append(event)
            if len(self.events) > 4096:
                raise Refused('native event bound')
        raise TimeoutError(method)

    def turns(self, thread):
        result = self.call('thread/turns/list', {'threadId': thread, 'limit': 64})
        if result.get('nextCursor'):
            raise Refused('turn inventory truncated')
        return {t['id']: t['status'] for t in result['data']}

    def drain_turns(self, thread, timeout=5):
        end = time.monotonic() + timeout
        while True:
            states = self.turns(thread)
            if len(states) >= self.accepted.get(thread, 1):
                break
            if time.monotonic() >= end:
                raise Refused('accepted native start unobserved')
            time.sleep(.02)
        for turn, status in states.items():
            if status == 'inProgress':
                self.call('turn/interrupt', {'threadId': thread, 'turnId': turn})
        while True:
            current = self.turns(thread)
            if all(current.get(t) in {'completed', 'interrupted', 'failed'} for t in states):
                return {'kind': 'TURN_DRAIN_ONLY', 'thread': thread,
                        'turns': sorted(states), 'execution_quiescent': False}
            if time.monotonic() >= end:
                raise Refused('exact native turn drain deadline')
            time.sleep(.02)

    def close(self):
        self.ws.close()
        self.closed = True
        if self.ws.socket.fileno() != -1:
            raise AssertionError('peer socket remains open')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--codex', type=Path, required=True)
    args = parser.parse_args()
    executable = args.codex.resolve(strict=True)
    if 'target' not in executable.parts:
        raise ValueError('disposable source build required')
    root = Path(tempfile.mkdtemp(prefix='relay-shared-host-adversary-', dir='/private/tmp'))
    env = environment(root)
    result = dict(root=str(root), scope='REAL_NATIVE_UNIX_HOST_EXTERNAL_FENCE_COUNTEREXAMPLE',
                  full_parity=False, native_host_enforcement=False, checks=[])
    host = native = outsider = process = None
    began = time.monotonic()
    socket = root/'native.sock'
    stderr = (root/'native.stderr').open('wb')
    (root/'driver.py').write_bytes(Path(__file__).read_bytes())
    def terminate(signum, frame):
        raise InterruptedError('reserve guard interrupted owned lab; cleaning up')
    signal.signal(signal.SIGTERM, terminate)
    def check(name, predicate):
        if not predicate:
            raise AssertionError(name)
        result['checks'].append(name)
    def until(predicate):
        end = time.monotonic()+4
        while not predicate():
            if time.monotonic() >= end:
                raise AssertionError('bounded effect deadline')
            time.sleep(.02)
    try:
        result['sha256'] = hashlib.file_digest(executable.open('rb'), 'sha256').hexdigest()
        check('exact pinned research executable', result['sha256'] == 'dfc5fa0a2e9b3522665ad8d8db49dd98898f174f2306f8f6203ab3f2a2ee173e')
        process = subprocess.Popen([str(executable), 'app-server', '--listen', 'unix://'+str(socket)],
            env=env, cwd=root, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=stderr, start_new_session=True)
        result['native_pid'] = process.pid
        until(socket.exists)
        native = Peer(socket, env['CODEX_HOME'])
        outsider = Peer(socket, env['CODEX_HOME'])
        registry = Registry(root/'registry', env['CODEX_HOME'])
        host = HostContract(root/'host', registry, native)
        owners = [host.connect(), host.connect()]
        params = dict(cwd=str(root), model='research-model', modelProvider='research',
                      approvalPolicy='never', sandbox='read-only')
        bindings = [host.submit(owner, 'create', 'thread/start', params) for owner in owners]
        result['bindings'] = bindings
        script = root/'effect.py'
        script.write_text('import os,sys,time\nfrom pathlib import Path\n'
            'if len(sys.argv)>2 and os.fork()==0:\n os.setsid()\n'
            ' for fd in [0,1,2]:\n  n=os.open(os.devnull,os.O_RDWR);os.dup2(n,fd);os.close(n)\n'
            ' path=Path(sys.argv[2])\nelse:path=Path(sys.argv[1])\n'
            'end=time.monotonic()+8\nwith path.open("a",buffering=1) as f:\n'
            ' while time.monotonic()<end:f.write("tick\\n");time.sleep(.02)\n')
        effects = [root/'A', root/'B']; escaped = root/'A-detached'
        for i,binding in enumerate(bindings):
            argv = [sys.executable, str(script), str(effects[i])]
            if i == 0: argv.append(str(escaped))
            host.submit(owners[i], 'work', 'thread/shellCommand', dict(
                threadId=binding['thread'], command=shlex.join(argv), timeoutMs=10000), binding['sequence'])
        until(lambda: all(p.exists() for p in effects) and escaped.exists())
        receipt = host.fence(owners[0], bindings[0]['thread'])
        result['receipt'] = receipt
        a,b,e = [p.stat().st_size for p in [*effects, escaped]]
        time.sleep(.15)
        check('A foreground drains while B progresses', effects[0].stat().st_size == a and effects[1].stat().st_size > b)
        check('detached A writes after native turn completion', escaped.stat().st_size > e)
        try:
            host.submit(owners[0], 'late', 'thread/shellCommand', dict(threadId=bindings[0]['thread'], command='true'), bindings[0]['sequence'])
        except Refused:
            check('cooperating stale owner refused', True)
        else: raise AssertionError('external fence failed')
        marker = root/'raw-peer-bypass'
        outsider.call('thread/resume', dict(threadId=bindings[0]['thread'], excludeTurns=True))
        outsider.call('thread/shellCommand', dict(threadId=bindings[0]['thread'],
            command=shlex.join([sys.executable, '-c', f'from pathlib import Path;Path({str(marker)!r}).touch()']), timeoutMs=2000))
        until(marker.exists)
        check('independent raw native peer bypasses external fenced epoch', True)
        try: host.transfer(owners[0], bindings[0]['thread'], {**receipt, 'execution_quiescent': True})
        except Refused: check('forged quiescence cannot release ownership', True)
        else: raise AssertionError('unsafe transfer')
        # A laboratory service restart must never be mistaken for scoped native
        # recovery. Committed identity survives, but its global generation also
        # rejects B's old admission, exposing another remaining design limit.
        host.close()
        host = HostContract(root/'host', registry, native)
        before_calls = len(native.calls)
        check('committed exact identity recovers without native replay',
              host.status(owners[0], 'create')['response'] == bindings[0]
              and len(native.calls) == before_calls)
        before_b = effects[1].stat().st_size
        try:
            host.submit(owners[1], 'after-restart', 'thread/shellCommand',
                dict(threadId=bindings[1]['thread'], command='true'), bindings[1]['sequence'])
        except Refused:
            check('global service generation rejects B old admission: scoped recovery missing',
                  len(native.calls) == before_calls)
        else: raise AssertionError('old host generation admitted')
        time.sleep(.15)
        check('B existing execution survives service restart', effects[1].stat().st_size > before_b)
        before = escaped.stat().st_size
        process.kill(); process.wait(timeout=5)
        time.sleep(.15)
        check('detached A writes after exact owned host death', escaped.stat().st_size > before)
        result['status'] = 'COUNTEREXAMPLES_CONFIRMED_NOT_PARITY'
        result['registry'] = registry.snapshot()
    except BaseException as error:
        result['exception'] = repr(error)
    finally:
        if host:
            try: host.close()
            except BaseException as error: result.setdefault('cleanup_errors',[]).append(repr(error))
        for name,peer in [('controller',native),('outsider',outsider)]:
            if peer:
                result[name+'_methods'] = peer.calls
                result[name+'_events'] = peer.events
                try: peer.close()
                except BaseException as error: result.setdefault('cleanup_errors',[]).append(repr(error))
        if process:
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)
            result['native_exit'] = process.returncode
        stderr.close()
        # Only bounded detached children; never scan/signal another process.
        remaining = began+12-time.monotonic()
        if remaining>0: time.sleep(remaining)
        result['socket_quiet_after_cleanup'] = socket_quiet(socket)
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps({k:v for k,v in result.items() if not k.endswith('_events')},indent=2))
    if result.get('exception') or result.get('cleanup_errors') or not result['socket_quiet_after_cleanup']:
        raise SystemExit(1)

if __name__ == '__main__': main()
