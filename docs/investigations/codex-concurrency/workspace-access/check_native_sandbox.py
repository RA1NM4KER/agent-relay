#!/usr/bin/env python3
"""Compare a native static filesystem sandbox; never claim process quiescence."""
from pathlib import Path
import argparse
import json
import os
import resource
import shutil
import subprocess
import time
import uuid

REPO=Path(__file__).resolve().parents[4]
LAB=REPO/'.lab/workspace-access'

def main():
 p=argparse.ArgumentParser();p.add_argument('--agents',type=int,choices=[1,8],default=1);p.add_argument('--working-set-mib',type=int,choices=[0,64],default=0);p.add_argument('--inherit-canonical-fd',action='store_true');args=p.parse_args()
 if shutil.disk_usage(LAB).free < 4*1024**3: raise RuntimeError('unchanged 4 GiB entry guard')
 root=LAB/'projects'/('native-sandbox-'+str(uuid.uuid4()));root.mkdir(parents=True)
 (root/'.relay-disposable-project').write_text('DISPOSABLE_RESEARCH_COPY\n')
 result={'root':str(root),'agents':args.agents,'canonicalFDInherited':args.inherit_canonical_fd,'workingSetMiB':args.working_set_mib,'handoffPermitted':False,'executionQuiescence':'UNKNOWN','mechanism':'DEPRECATED_NATIVE_STATIC_FILESYSTEM_SANDBOX'}
 children=[];fds=[];started=time.monotonic()
 try:
  for i in range(args.agents):
   if shutil.disk_usage(LAB).free < 1536*1024**2: raise RuntimeError('unchanged runtime reserve')
   actor=root/str(i);actor.mkdir();draft=actor/'draft';draft.mkdir();canonical=actor/'canonical';canonical.mkdir()
   target=canonical/'inherited.bin';target.write_bytes(b'CANARY');fd=os.open(target,os.O_WRONLY);fds.append(fd)
   binary=LAB/'native-probe'
   q=json.dumps
   profile='\n'.join(['(version 1)','(deny default)','(import "dyld-support.sb")','(allow process-fork)', '(allow process-exec (literal '+q(str(binary))+'))','(allow sysctl-read)','(allow signal (target self))',
     '(allow file-read* (subpath "/System/Library") (subpath "/System/Volumes/Preboot/Cryptexes/OS/System/Library") (subpath "/usr/lib") (literal "/dev/null") (literal "/dev/urandom") (literal '+q(str(binary))+') (subpath '+q(str(draft))+') (subpath '+q(str(canonical))+'))',
     '(allow file-map-executable (subpath "/System/Library") (subpath "/System/Volumes/Preboot/Cryptexes/OS/System/Library") (subpath "/usr/lib") (literal '+q(str(binary))+'))',
     '(allow file-write* (subpath '+q(str(draft))+'))'])
   (actor/'profile.sbpl').write_text(profile)
   def limits():resource.setrlimit(resource.RLIMIT_FSIZE,(128*1024**2,128*1024**2))
   child=subprocess.Popen(['/usr/bin/sandbox-exec','-f',str(actor/'profile.sbpl'),str(binary),str(draft),str(canonical),str(fd if args.inherit_canonical_fd else -1),str(args.working_set_mib)],
      cwd=draft,env={'PATH':'/usr/bin:/bin','HOME':str(draft)},pass_fds=(fd,) if args.inherit_canonical_fd else (),stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE,preexec_fn=limits)
   children.append((child,actor))
  deadline=time.monotonic()+5
  while time.monotonic()<deadline:
   if all((a/'draft/attempts.json').exists() and (a/'draft/held.log').exists() for _,a in children):break
   if any(c.poll() is not None for c,_ in children):raise RuntimeError('native sandbox wrapper exited before workload')
   time.sleep(.05)
  else:raise RuntimeError('native sandbox workload deadline')
  result['startupAndFirstEffectsSeconds']=time.monotonic()-started
  result['telemetry']=subprocess.check_output(['ps','-axo','pid=,ppid=,rss=,time=,comm='],text=True)
  result['telemetry']=[line.strip() for line in result['telemetry'].splitlines() if str(binary) in line]
  before=[(a/'draft/held.log').stat().st_size for _,a in children]
  for c,_ in children:c.terminate()
  for c,_ in children:c.wait(timeout=3)
  time.sleep(.5)
  result['privateEffectsAfterParentDeath']=[(a/'draft/held.log').stat().st_size>n for n,(_,a) in zip(before,children)]
  result['attempts']=[json.loads((a/'draft/attempts.json').read_text()) for _,a in children]
  result['canonicalUnchanged']=all((a/'canonical/inherited.bin').read_bytes()==b'CANARY' and not (a/'canonical/forbidden').exists() for _,a in children)
  result['completed']=True
 except BaseException as error:result.update(completed=False,error=repr(error))
 finally:
  for c,a in children:
   if c.poll() is None:c.terminate()
   stdout,stderr=c.communicate(timeout=6);(a/'stdout').write_bytes(stdout);(a/'stderr').write_bytes(stderr);result.setdefault('exitCodes',[]).append(c.returncode)
  for fd in fds:os.close(fd)
  # Detached fixture children have a fixed 200-iteration lifetime. Waiting is
  # cleanup housekeeping, NOT an authoritative execution-quiescence receipt.
  time.sleep(max(0,6-(time.monotonic()-started)))
  result['elapsedSeconds']=time.monotonic()-started
  (root/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps({k:result.get(k) for k in ['root','completed','error','canonicalUnchanged','privateEffectsAfterParentDeath']}))
 return 0 if result.get('completed') else 1

if __name__=='__main__':raise SystemExit(main())
