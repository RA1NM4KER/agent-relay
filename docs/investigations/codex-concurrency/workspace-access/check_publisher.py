#!/usr/bin/env python3
"""Real filesystem publication, epoch fencing and process-crash experiments."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import uuid
from unittest.mock import patch
import publisher

from publisher import Publisher, Refused, collect_regular, digest

REPO = Path(__file__).resolve().parents[4]
LAB = REPO/'.lab/workspace-access'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--crash-worker', type=Path)
    parser.add_argument('--vault', type=Path)
    parser.add_argument('--crash-point', choices=['after-intent','after-replace'])
    args = parser.parse_args()
    if args.crash_worker:
        caps = json.loads(args.vault.read_text())
        broker = Publisher(args.crash_worker)
        broker.put(caps['A'],1,'crash-operation','README.md',b'crash-data',caps['base'],crash=args.crash_point)
        raise AssertionError('crash hook did not fire')
    root = LAB/'projects'/('publisher-'+str(uuid.uuid4()))
    root.mkdir(parents=True)
    (root/'.relay-disposable-project').write_text('DISPOSABLE_RESEARCH_COPY\n')
    (root/'canonical').mkdir()
    (root/'canonical/README.md').write_text('initial')
    broker = Publisher(root)
    a,b = broker.register(),broker.register()
    result = {'root':str(root),'checks':[],'nativeCodexIntegrated':False,'handoffPermitted':False,
              'scope':'HOST_ONLY_BOUNDED_REGULAR_FILE_PUBLICATION'}
    def check(name,value):
        if not value: raise AssertionError(name)
        result['checks'].append(name)
    def refused(name,call):
        try:call()
        except (Refused,OSError):check(name,True)
        else:raise AssertionError(name)
    try:
        # Input really comes from the VZ private draft of a prior workspace run.
        candidates=[]
        for p in (LAB/'runs').glob('*/result.json'):
            v=json.loads(p.read_text())
            if v.get('mode')=='readonly-draft' and v.get('completed'):
                candidates.append(Path(v['vms'][0]['fixture'])/'draft')
        draft=candidates[0]
        payload=collect_regular(draft,'held.log')
        check('captured real guest draft bytes',payload.startswith(b'HELD_WRITE'))
        refused('guest-created absolute symlink cannot cause host read outside draft',lambda:collect_regular(draft,'escape'))
        scratch=root/'input';scratch.mkdir();(scratch/'one').write_text('payload');os.link(scratch/'one',scratch/'two')
        refused('hard-linked input is unclassified and rejected',lambda:collect_regular(scratch,'one'))
        refused('relative traversal is rejected',lambda:broker.put(a,1,'escape','../outside',b'bad',None))
        refused('absolute pathname is rejected',lambda:broker.put(a,1,'absolute',str(root/'outside'),b'bad',None))
        entered,release=threading.Event(),threading.Event()
        with ThreadPoolExecutor(max_workers=1) as pool:
            future=pool.submit(broker.put,a,1,'pending','README.md',payload,digest(b'initial'),entered,release)
            assert entered.wait(2)
            receipt=broker.revoke(a,1,timeout=.05)
            check('revocation closes admission without claiming queued I/O drained',receipt['publicationAdmissionClosed'] and not receipt['drained'])
            broker.put(b,1,'B-independent','other.rs',b'B survives',None)
            check('B publishes while A remains paused and fenced',(root/'canonical/other.rs').read_bytes()==b'B survives')
            release.set()
            refused('queued old-epoch publication rejected before issue',future.result)
        check('A queued bytes never reached canonical file',(root/'canonical/README.md').read_bytes()==b'initial')
        receipt=broker.revoke(a,1)
        check('publication drain requires zero pending and zero unresolved',receipt['drained'] and receipt['pending']==0)
        check('scoped publisher receipt never authorizes Relay handoff',receipt['handoffPermitted'] is False)
        refused('fenced old owner cannot publish fresh token',lambda:broker.put(a,1,'fresh','README.md',b'bad',digest(b'initial')))
        refused('foreign capability cannot invent publication authority',lambda:broker.put('foreign',1,'new','README.md',b'bad',None))
        refused('wrong epoch cannot publish',lambda:broker.put(b,0,'wrong','README.md',b'bad',None))
        committed=broker.put(b,1,'committed','README.md',payload,digest(b'initial'))
        check('committed bytes materialize in real project copy',(root/'canonical/README.md').read_bytes()==payload)
        broker.close();broker=Publisher(root)
        replay=broker.put(b,1,'committed','README.md',payload,digest(b'initial'))
        check('lost committed reply recovers exact receipt without replay',replay==committed)
        refused('operation token cannot substitute content',lambda:broker.put(b,1,'committed','README.md',b'other',digest(b'initial')))
        # Pause at the real file fsync call after temporary bytes have been
        # written. This is a userspace interception, not a blocked kernel-I/O test.
        c = broker.register()
        issued, unblock = threading.Event(), threading.Event()
        real_fsync = os.fsync
        worker_id = []
        def intercepted_fsync(fd):
            if worker_id and threading.get_ident() == worker_id[0] and not issued.is_set():
                issued.set()
                if not unblock.wait(3): raise Refused('issued I/O pause expired')
            return real_fsync(fd)
        def issuing_write():
            worker_id.append(threading.get_ident())
            return broker.put(c,1,'issued','issued.rs',b'issued before revoke',None)
        with patch.object(publisher.os,'fsync',intercepted_fsync):
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(issuing_write)
                try:
                    check('actual temporary-file write reaches fsync boundary',issued.wait(2))
                    pending = broker.revoke(c,1,timeout=.05)
                    check('issued I/O blocks publication drain',not pending['drained'] and pending['pending']==1)
                    broker.put(b,1,'B-issued-independent','parallel.rs',b'B parallel',None)
                    check('B publishes while A waits at file fsync',(root/'canonical/parallel.rs').read_bytes()==b'B parallel')
                    refused('same pending path reservation blocks another writer',lambda:broker.put(b,1,'same-issued','issued.rs',b'bad',None))
                finally: unblock.set()
                future.result(timeout=2)
        check('admitted in-flight mutation resolves before drain',broker.revoke(c,1)['drained'] and (root/'canonical/issued.rs').read_bytes()==b'issued before revoke')
        refused('revoked authority cannot start another write after drain',lambda:broker.put(c,1,'after-drain','late.rs',b'bad',None))
        result['crashes']=[]
        for point,exit_code in [('after-intent',86),('after-replace',87)]:
            fixture=LAB/'projects'/('publisher-crash-'+str(uuid.uuid4()));fixture.mkdir();(fixture/'.relay-disposable-project').write_text('DISPOSABLE_RESEARCH_COPY\n');(fixture/'canonical').mkdir();(fixture/'canonical/README.md').write_bytes(b'initial')
            old=Publisher(fixture);ca,cb=old.register(),old.register();old.close()
            vault=fixture/'private-caps.json';fd=os.open(vault,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
            with os.fdopen(fd,'w') as stream:json.dump({'A':ca,'B':cb,'base':digest(b'initial')},stream)
            child=subprocess.run([sys.executable,__file__,'--crash-worker',str(fixture),'--vault',str(vault),'--crash-point',point],capture_output=True,text=True,timeout=5)
            check(point+' actual publisher process death',child.returncode==exit_code)
            recovered=Publisher(fixture)
            rec=recovered.revoke(ca,1)
            check(point+' uncertain outcome retains reservation',not rec['drained'] and rec['unresolvedPaths']==['README.md'])
            refused(point+' never repeats uncertain native filesystem mutation',lambda:recovered.put(ca,1,'crash-operation','README.md',b'crash-data',digest(b'initial')))
            refused(point+' case-alias path cannot bypass uncertain reservation',lambda:recovered.put(cb,1,'alias','readme.md',b'B',None))
            recovered.put(cb,1,'other','B.rs',b'B recovery',None)
            check(point+' independent B recovery proceeds',(fixture/'canonical/B.rs').read_bytes()==b'B recovery')
            result['crashes'].append({'point':point,'exitCode':child.returncode,'receipt':rec,'canonicalDataAfterCrash':(fixture/'canonical/README.md').read_text(),'stderr':child.stderr})
            recovered.close()
        result['passed']=True
    except BaseException as error:
        result.update(passed=False,error=repr(error))
    finally:
        broker.close()
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps({'root':str(root),'passed':result.get('passed'),'checks':len(result['checks']),'error':result.get('error')}))
    return 0 if result.get('passed') else 1

if __name__=='__main__':raise SystemExit(main())
