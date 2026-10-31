import json,os,sys,subprocess,sqlite3
from pathlib import Path
sys.path.insert(0,'/private/tmp/agent-relay-native-validation/docs/investigations/codex-concurrency/feasibility')
from native_tui_lab import SharedLoadedObserver,socket_quiet,file_sha256
lab=Path(__file__).resolve().parent
root=Path('/private/tmp/relay-native-tui-lab-o4fbcor9')
with sqlite3.connect(root/'admission/registry.sqlite') as db:
 assert db.execute('SELECT count(*) FROM owners').fetchone()[0]==0
exe=lab/'target/research-package/bin/codex'
env={'PATH':os.environ['PATH'],'HOME':str(root/'home'),'CODEX_HOME':str(root/'codex'),'TMPDIR':str(root),'LANG':'en_US.UTF-8','TERM':'xterm-256color','RUST_LOG':'error'}
result={'scope':'NATIVE_WS_READ_OBSERVER_SMOKE','full_parity':False};observer=None;daemon=None
try:
 started=subprocess.run([str(exe),'app-server','daemon','start'],env=env,cwd=root,capture_output=True,text=True,timeout=180)
 result['start']={'exit_code':started.returncode,'stdout':started.stdout,'stderr':started.stderr};assert started.returncode==0
 daemon=json.loads(started.stdout)
 assert file_sha256(Path(daemon['managedCodexPath']))==file_sha256(exe)
 observer=SharedLoadedObserver(exe,env,daemon['socketPath'])
 assert observer.loaded()==[]
 try:observer.call('thread/resume',{'threadId':'must-never-forward'})
 except AssertionError:pass
 else:raise AssertionError('read observer forwarded mutation')
 assert observer.calls==['initialize','thread/loaded/list']
 result['methods']=observer.calls;result['status']='PASSED'
except BaseException as error:result['exception']=repr(error)
finally:
 if observer:
  observer.close();result['observer_socket_closed']=observer.ws.socket.fileno()==-1
 stopped=subprocess.run([str(exe),'app-server','daemon','stop'],env=env,cwd=root,capture_output=True,text=True,timeout=180)
 result['stop']={'exit_code':stopped.returncode,'stdout':stopped.stdout,'stderr':stopped.stderr}
 if daemon:
  try:os.kill(daemon['pid'],0)
  except ProcessLookupError:result['daemon_pid_absent']=True
  else:result['daemon_pid_absent']=False
  result['daemon_socket_quiet']=socket_quiet(daemon['socketPath'])
  assert result['daemon_pid_absent'] and result['daemon_socket_quiet']
 assert stopped.returncode==0
 (root/'observer-smoke.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
if result.get('exception'):raise SystemExit(1)
