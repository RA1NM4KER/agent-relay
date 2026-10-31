import os,sys,json,time,sqlite3,subprocess,signal
from pathlib import Path
lab=Path(__file__).resolve().parent
harness=Path('/private/tmp/agent-relay-native-validation/docs/investigations/codex-concurrency/feasibility/native_tui_lab.py')
exe=lab/'target/debug/codex'
subprocess.run([str(exe),'--version'],check=True,capture_output=True,timeout=45)
before=set(Path('/private/tmp').glob('relay-native-tui-lab-*'))
with (lab/'logs/signal-cleanup-harness.log').open('w') as output:
 child=subprocess.Popen([sys.executable,str(harness),'--codex',str(exe),'--mode','embedded'],stdout=output,stderr=subprocess.STDOUT)
 try:
  end=time.monotonic()+20
  while time.monotonic()<end:
   roots=set(Path('/private/tmp').glob('relay-native-tui-lab-*'))-before
   if len(roots)==1:
    root=next(iter(roots));dbpath=root/'admission/registry.sqlite'
    if dbpath.exists():
     try:
      with sqlite3.connect(dbpath,timeout=1) as db:count=db.execute('SELECT count(*) FROM native_views').fetchone()[0]
      if count==8:break
     except sqlite3.OperationalError:pass
   if child.poll() is not None:raise AssertionError('native harness exited before signal check')
   time.sleep(.05)
  else:raise AssertionError('eight native bindings not observed before signal')
  child.send_signal(signal.SIGTERM)
  code=child.wait(timeout=60)
  result=json.loads((root/'result.json').read_text())
  assert code==1 and 'lab received SIGTERM' in result.get('exception',''),result
  prefix=str(exe)+' --no-alt-screen -C '+str(root)+'/'
  alive=[row for row in subprocess.check_output(['ps','-axo','args='],text=True).splitlines() if row.strip().startswith(prefix)]
  assert not alive,alive
  record={'root':str(root),'status':'PASSED','signal':'SIGTERM after eight native bindings','harness_exit':code,'exception':result['exception'],'remaining_exact_frontends':alive,'full_parity':False}
  (lab/'signal-cleanup-result.json').write_text(json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
 finally:
  if child.poll() is None:
   child.send_signal(signal.SIGTERM)
   child.wait(timeout=60)
