import os,sys,json,time,sqlite3,subprocess,signal
from pathlib import Path
lab=Path(__file__).resolve().parent
harness=Path('/private/tmp/agent-relay-native-validation/docs/investigations/codex-concurrency/feasibility/native_tui_lab.py')
exe=lab/'target/debug/codex'
subprocess.run([str(exe),'--version'],check=True,capture_output=True,timeout=45)
before=set(Path('/private/tmp').glob('relay-native-tui-lab-*'))
fault_source="import errno,runpy,sys;sys.path.insert(0,str(__import__('pathlib').Path(sys.argv[1]).resolve().parent))\nfrom pathlib import Path\noriginal=Path.write_bytes\ndef write_bytes(path,data):\n if path.name.startswith('pane-') and path.suffix=='.terminal':\n  raise OSError(errno.ENOSPC,'injected terminal evidence write failure')\n return original(path,data)\nPath.write_bytes=write_bytes\nsys.argv=sys.argv[1:]\nrunpy.run_path(sys.argv[0],run_name='__main__')\n"
with (lab/'logs/signal-write-failure-retry-harness.log').open('w') as output:
 child=subprocess.Popen([sys.executable,'-c',fault_source,str(harness),'--codex',str(exe),'--mode','embedded'],stdout=output,stderr=subprocess.STDOUT)
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
  assert result['harness_pid']==child.pid,result
  assert len(result.get('cleanup_errors',[]))==8,result
  assert all('terminal evidence' in e['stage'] and 'injected terminal evidence' in e['error'] for e in result['cleanup_errors']),result
  assert all(e['exit_code'] is not None for e in result['frontend_cleanup']),result
  prefix=str(exe)+' --no-alt-screen -C '+str(root)+'/'
  alive=[row for row in subprocess.check_output(['ps','-axo','args='],text=True).splitlines() if row.strip().startswith(prefix)]
  assert not alive,alive
  record={'root':str(root),'status':'PASSED','signal':'SIGTERM after eight native bindings plus injected ENOSPC on every terminal evidence write','harness_exit':code,'exception':result['exception'],'remaining_exact_frontends':alive,'full_parity':False}
  (lab/'signal-write-failure-retry-result.json').write_text(json.dumps(record,indent=2)+'\n');print(json.dumps(record,indent=2))
 finally:
  if child.poll() is None:
   child.send_signal(signal.SIGTERM)
   child.wait(timeout=60)
