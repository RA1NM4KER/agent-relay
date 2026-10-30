import subprocess,os,json,time,shutil,signal
from pathlib import Path
r=Path(__file__).resolve().parent;repo=Path('/Users/kefasmanda/repos/agent-relay');out=[]
for name,argv in [('relay-fmt',['cargo','fmt','--check']),('relay-clippy',['cargo','clippy','--workspace','--all-targets','--offline','--','-D','warnings']),('relay-tests',['cargo','test','--workspace','--offline'])]:
 item={'stage':name,'argv':argv,'started':time.time(),'free_before':shutil.disk_usage(r).free}
 with (r/'logs'/f'{name}.log').open('w') as log:
  child=subprocess.Popen(argv,cwd=repo,env={**os.environ,'CARGO_BUILD_JOBS':'1'},stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
  while child.poll() is None:
   if shutil.disk_usage(r).free<1536*1024*1024:
    item['stopped_for_disk_reserve']=True;os.killpg(child.pid,signal.SIGTERM);break
   time.sleep(1)
  item['exit_code']=child.wait()
 item.update(elapsed_s=time.time()-item['started'],free_after=shutil.disk_usage(r).free)
 (r/'logs'/f'{name}.status.json').write_text(json.dumps(item,indent=2)+'\n');out.append(item);print(json.dumps(item),flush=True)
 if item.get('stopped_for_disk_reserve'):break
(r/'relay-validation.json').write_text(json.dumps(out,indent=2)+'\n')
