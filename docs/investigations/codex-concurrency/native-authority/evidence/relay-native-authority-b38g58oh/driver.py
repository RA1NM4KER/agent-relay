#!/usr/bin/env python3
"""Actual native authority adversaries; no frontend/cooperating fence is used.

Fresh isolated profile, raw Unix WebSocket RPCs and exact owned host processes.
No provider credentials/inference. This does not certify executor containment.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import secrets
import shlex
import signal
import subprocess
import sys
import tempfile
import time

packet = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(packet/'engineering'))
from shared_host_adversary import Peer
from stdio_probe import environment
from controller import Refused
from native_tui_lab import socket_quiet


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--app-server',type=Path,required=True)
    parser.add_argument('--codex',type=Path)
    args=parser.parse_args();exe=args.app_server.resolve(strict=True)
    if 'target' not in exe.parts:raise ValueError('explicit disposable native build required')
    root=Path(tempfile.mkdtemp(prefix='relay-native-authority-',dir='/private/tmp'))
    env=environment(root);authority=root/'codex/relay-authority-v1';authority.mkdir(mode=0o700)
    result=dict(root=str(root),scope='ACTUAL_NATIVE_AUTHORITY_NOT_FULL_PARITY',checks=[],full_parity=False,execution_quiescent=False)
    (root/'driver.py').write_bytes(Path(__file__).read_bytes())
    with exe.open('rb') as stream:result['binary_sha256']=hashlib.file_digest(stream,'sha256').hexdigest()
    keys={name:secrets.token_hex(32) for name in ['A','B','C']}
    recovery={name:secrets.token_hex(32) for name in keys}
    enrollment=secrets.token_hex(32)
    policy=dict(version=1,profile_identity=f"{(root/'codex').stat().st_dev}:{(root/'codex').stat().st_ino}",enrollment_sha256=hashlib.sha256(enrollment.encode()).hexdigest())
    fd=os.open(authority/'policy.json',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    with os.fdopen(fd,'w') as stream:json.dump(policy,stream);stream.flush();os.fsync(stream.fileno())

    process=None;peers=[];log=None;serial=0
    private_operations=[];registrations={}
    def persist_capabilities():
        # Supervisor-only generated authority, never a provider credential and
        # never included in retained/public test evidence.
        temporary=root/'supervisor-capabilities.pending'
        fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
        with os.fdopen(fd,'w') as stream:
            json.dump(dict(enrollment=enrollment,recovery=recovery,leases=keys,registrations=registrations,operations=private_operations),stream)
            stream.flush();os.fsync(stream.fileno())
        temporary.replace(root/'supervisor-capabilities.json')
        directory=os.open(root,os.O_RDONLY)
        try:os.fsync(directory)
        finally:os.close(directory)

    socket=root/'native.sock'
    def interrupted(signum,frame):raise InterruptedError('reserve guard; cleanup owned host')
    signal.signal(signal.SIGTERM,interrupted)
    def check(name,condition):
        if not condition:raise AssertionError(name)
        result['checks'].append(name)
    def until(predicate,seconds=5):
        end=time.monotonic()+seconds
        while not predicate():
            if time.monotonic()>end:raise AssertionError('effect deadline')
            time.sleep(.01)
    def start(label,hold_token=None):
        nonlocal process,log
        log=(root/f'{label}.stderr').open('wb')
        process=subprocess.Popen([str(exe),'--listen','unix://'+str(socket)],env={**env,**({'RELAY_AUTHORITY_TEST_HOLD_TOKEN':hold_token} if hold_token else {})},cwd=root,
            stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=log,start_new_session=True)
        result.setdefault('native_pids',[]).append(process.pid)
        until(lambda:socket.exists() and not socket_quiet(socket),20)
    def peer():
        p=Peer(socket,env['CODEX_HOME']);peers.append(p);return p
    def register(p,who,token):
        registrations[who]=token;persist_capabilities()
        proof=dict(key=keys[who],recoveryKey=recovery[who],epoch=1,token=token)
        if token.startswith('register-'):proof['enrollmentKey']=enrollment
        receipt=p.call('relay/authority',dict(command='register',_relay=proof))
        result.setdefault('controller_receipts',[]).append(dict(controller=who,**receipt))
        return receipt
    def call(p,who,method,params,token=None,extras=None):
        nonlocal serial
        serial+=1
        proof=dict(key=keys[who],epoch=1,token=token or f'op-{serial}')
        proof.update(extras or {})
        private_operations.append(dict(controller=who,token=proof['token'],method=method,fingerprint=hashlib.sha256(json.dumps(params,sort_keys=True).encode()).hexdigest()))
        persist_capabilities()
        return p.call(method,{**params,'_relay':proof})
    def refused(name,action):
        try:action()
        except Refused as error:
            check(name,'RELAY_NATIVE_AUTHORITY' in str(error) or 'native ownership' in str(error))
        else:raise AssertionError(name)
    def stop(kill=False):
        nonlocal process,log
        if kill and process and process.poll() is None:
            process.kill();process.wait(timeout=5)
        for p in peers:
            try:p.close()
            except BaseException as error:result.setdefault('peer_close_errors',[]).append(repr(error))
        peers.clear()
        if process:
            if process.poll() is None:
                (process.kill if kill else process.terminate)()
                try:process.wait(timeout=5)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)
            result.setdefault('host_cleanup',[]).append(dict(pid=process.pid,exit_code=process.returncode))
        if log:log.close();log=None
        process=None
    params=dict(cwd=str(root),model='research-model',modelProvider='research',approvalPolicy='never',sandbox='read-only')
    try:
        start('first');a=peer();b=peer();raw=peer()
        register(a,'A','register-A');register(b,'B','register-B')
        refused('unmediated native creation refused',lambda:raw.call('thread/start',params))
        refused('raw client cannot invent supervisor enrollment',lambda:raw.call('relay/authority',dict(command='register',_relay=dict(key=secrets.token_hex(32),recoveryKey=secrets.token_hex(32),token='invent-owner',epoch=1))))

        if args.codex:
            cli=args.codex.resolve(strict=True)
            if 'target' not in cli.parts:raise ValueError('explicit disposable CLI required')
            probe=subprocess.run([str(cli),'exec','--skip-git-repo-check','-C',str(root),'Must refuse protected native execution.'],
                env=env,cwd=root,stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=30)
            (root/'alternate-exec.stdout').write_text(probe.stdout)
            (root/'alternate-exec.stderr').write_text(probe.stderr)
            check('alternative native exec cannot open a second protected host',probe.returncode!=0 and 'another native host owns protected profile' in probe.stdout+probe.stderr)
        A=call(a,'A','thread/start',params,'create-A')['thread']['id']
        B=call(b,'B','thread/start',params,'create-B')['thread']['id']
        result['bindings']=dict(A=A,B=B);check('two independent native owner bindings',A!=B)
        duplicate=peer()
        refused('second live connection cannot reuse A controller lease',lambda:call(duplicate,'A','thread/resume',dict(threadId=A)))
        old_key=keys['A'];keys['A']=secrets.token_hex(32);replacement=peer()
        register(replacement,'A','recover-A-live')
        refused('retired A lease cannot drive its native root',lambda:a.call('thread/resume',dict(threadId=A,_relay=dict(key=old_key,epoch=1,token='retired-A'))))
        selected=call(replacement,'A','thread/resume',dict(threadId=A))
        check('supervisor recovery retains exact A native root',selected['thread']['id']==A)
        a=replacement
        child=call(a,'A','thread/start',{**params,'ephemeral':True},'helper-A',dict(parentRoot=A))['thread']['id']
        other=call(a,'A','thread/start',params,'other-root-A')['thread']['id']
        result['bindings']['other_A']=other
        result['bindings']['helper_A']=child
        binding=json.loads((authority/'state.json').read_text())['roots'][child]
        check('actual ephemeral native child inherits A exact authority root',binding['root']==A)
        for method in ['thread/resume','thread/fork']:
            refused('native path selection cannot override owned ID: '+method,lambda m=method:call(a,'A',m,dict(threadId=A,path=str(root/'foreign-rollout'))))

        for method in ['thread/resume','thread/fork','turn/start','turn/steer','thread/shellCommand']:
            request=dict(threadId=A)
            if method=='turn/start':request['input']=[dict(type='text',text='must never execute',text_elements=[])]
            if method=='turn/steer':request.update(input=[dict(type='text',text='must never execute',text_elements=[])],expectedTurnId='foreign')
            if method=='thread/shellCommand':request.update(command='true',timeoutMs=1000)
            refused('raw native '+method+' refused',lambda m=method,p=request:raw.call(m,p))
            refused('foreign native '+method+' refused',lambda m=method,p=request:call(b,'B',m,p))
        for method in ['process/spawn','command/exec','fs/writeFile','thread/queue/start','thread/realtime/start','review/start']:
            refused('unclassified alternate execution '+method+' refused',lambda m=method:call(a,'A',m,dict(threadId=A)))
        # Actual command side effects establish B continues after A revocation.
        marker=root/'B-after-A-revoke'
        revoke=call(a,'A','relay/authority',dict(command='revoke',threadId=A),'revoke-A')
        check('durable native revoke never claims quiescence',revoke['state']=='FENCED' and revoke['executionQuiescent'] is False and revoke['handoffPermitted'] is False)
        refused('forged executor receipt cannot advance native handoff',lambda:call(a,'A','relay/authority',dict(command='handoff',threadId=A,executionQuiescent=True)))
        refused('fenced owner cannot resume its native root',lambda:call(a,'A','thread/resume',dict(threadId=A)))
        refused('fenced parent also fences its native ephemeral child',lambda:call(a,'A','thread/resume',dict(threadId=child)))
        refused('owner revoke fences its other native conversation',lambda:call(a,'A','thread/resume',dict(threadId=other)))
        refused('fenced controller cannot escape through native new',lambda:call(a,'A','thread/start',params))

        refused('fenced owner cannot execute native command',lambda:call(a,'A','thread/shellCommand',dict(threadId=A,command='true',timeoutMs=1000)))
        call(b,'B','thread/shellCommand',dict(threadId=B,command=shlex.join([sys.executable,'-c',f'from pathlib import Path;Path({str(marker)!r}).touch()']),timeoutMs=2000))
        until(marker.exists);check('native B executes after A fence',True)
        # An actual committed native reply is lost: send, never read the reply,
        # then crash the exact host after its durable commit becomes observable.
        lost=peer();register(lost,'C','register-C');lost_id=999
        private_operations.append(dict(controller='C',token='lost-reply',method='thread/start'));persist_capabilities()
        lost.ws.send(json.dumps(dict(id=lost_id,method='thread/start',params={**params,'_relay':dict(key=keys['C'],epoch=1,token='lost-reply')})))
        opkey=hashlib.sha256(recovery['C'].encode()).hexdigest()+':lost-reply'
        def committed():
            state=json.loads((authority/'state.json').read_text())
            return state.get('operations',{}).get(opkey,{}).get('result') is not None
        until(committed,20)
        expected=json.loads((authority/'state.json').read_text())['operations'][opkey]['result']['thread']['id']
        roots_before=len(json.loads((authority/'state.json').read_text())['roots'])
        stop(kill=True);check('exact owned native host crash leaves endpoint quiet',socket_quiet(socket))
        saved=json.loads((root/'supervisor-capabilities.json').read_text())
        check('supervisor capabilities retained privately before forwarding',saved['recovery']==recovery and ((root/'supervisor-capabilities.json').stat().st_mode & 0o777)==0o600)
        keys.clear();keys.update(saved['leases']);recovery.clear();recovery.update(saved['recovery'])
        start('restarted','crash-window-B');a2=peer();b2=peer();c2=peer()
        for who,p in [('A',a2),('B',b2),('C',c2)]:
            keys[who]=secrets.token_hex(32);register(p,who,'recover-'+who+'-restart')
        status=c2.call('relay/authority',dict(command='operationStatus',operationToken='lost-reply',_relay=dict(recoveryKey=recovery['C'],epoch=1,token='query-lost')))
        check('native operation status reports committed lost reply',status['state']=='COMMITTED' and status['result']['thread']['id']==expected)

        refused('A remains fenced after actual native host restart',lambda:call(a2,'A','thread/resume',dict(threadId=A)))
        replay=call(c2,'C','thread/start',params,'lost-reply')
        check('lost native reply recovers exact committed identity',replay['thread']['id']==expected)
        check('lost-reply retry creates no additional native root',len(json.loads((authority/'state.json').read_text())['roots'])==roots_before)
        refused('durable token cannot substitute native creation parameters',lambda:call(c2,'C','thread/start',{**params,'model':'different-research-model'},'lost-reply'))
        resumed=call(b2,'B','thread/resume',dict(threadId=B))
        check('B reconnects with same authority after host crash',resumed['thread']['id']==B)
        # Debug-only trusted-host pause is after durable intent and before any
        # forwarding. Kill the exact host while that intent is unresolved.
        pending_key=hashlib.sha256(recovery['B'].encode()).hexdigest()+':crash-window-B'
        before_pending=len(json.loads((authority/'state.json').read_text())['roots'])
        private_operations.append(dict(controller='B',token='crash-window-B',method='thread/start'));persist_capabilities()
        b2.ws.send(json.dumps(dict(id=1001,method='thread/start',params={**params,'_relay':dict(key=keys['B'],epoch=1,token='crash-window-B')})))
        until(lambda:pending_key in json.loads((authority/'state.json').read_text())['operations'])
        stop(kill=True)
        pending=json.loads((authority/'state.json').read_text())['operations'][pending_key]
        check('actual native host killed after intent before execution',pending['result'] is None and pending['bound_thread'] is None)
        start('after-pending-crash');b3=peer();keys['B']=secrets.token_hex(32);register(b3,'B','recover-B-pending')
        unknown=b3.call('relay/authority',dict(command='operationStatus',operationToken='crash-window-B',_relay=dict(recoveryKey=recovery['B'],epoch=1,token='query-pending')))
        check('native crash recovery reports unresolved without guessing',unknown['state']=='UNRESOLVED' and unknown['result'] is None)
        refused('unresolved native creation cannot replay execution',lambda:call(b3,'B','thread/start',params,'crash-window-B'))
        check('unresolved retry creates no native root',len(json.loads((authority/'state.json').read_text())['roots'])==before_pending)
        result['status']='NARROW_NATIVE_ADMISSION_CHECKS_PASSED_NOT_PARITY'
    except BaseException as error:result['exception']=repr(error)
    finally:
        stop();result['socket_quiet_after_cleanup']=socket_quiet(socket)
        if (authority/'state.json').exists():
            # Journal contains capability digests, not capabilities or provider credentials.
            (root/'authority-final.json').write_bytes((authority/'state.json').read_bytes())
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps(result,indent=2))
    if result.get('exception') or result.get('peer_close_errors') or not result['socket_quiet_after_cleanup']:raise SystemExit(1)

if __name__=='__main__':main()
