import os,sys,json,time,subprocess,shutil,signal
from pathlib import Path
r=Path(__file__).resolve().parent
stage=sys.argv[1];argv=sys.argv[2:]
env={k:os.environ[k] for k in ['PATH','TMPDIR','LANG','SDKROOT','DEVELOPER_DIR'] if k in os.environ}
env.update(HOME=str(r/'home'),CARGO_HOME=str(r/'tooling/cargo'),RUSTUP_HOME=str(r/'tooling/rustup'),CARGO_TARGET_DIR=str(r/'target'),CARGO_PROFILE_DEV_DEBUG='0',CARGO_INCREMENTAL='0',CARGO_BUILD_JOBS='1',CARGO_NET_GIT_FETCH_WITH_CLI='true')
env['PATH']=str(r/'tooling/cargo/bin')+':'+env.get('PATH','/usr/bin:/bin')
start=time.time();result={'stage':stage,'argv':argv,'started':start,'build_settings':{k:env[k] for k in ['CARGO_PROFILE_DEV_DEBUG','CARGO_INCREMENTAL','CARGO_BUILD_JOBS']},'free_before':shutil.disk_usage(r).free}
(r/'logs'/f'{stage}.status.json').write_text(json.dumps(result,indent=2)+'\n')
with (r/'logs'/f'{stage}.log').open('w') as log:
 child=subprocess.Popen(argv,cwd=r/'codex/codex-rs',env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
 result['pid']=child.pid
 (r/'logs'/f'{stage}.status.json').write_text(json.dumps(result,indent=2)+'\n')
 while child.poll() is None:
  if shutil.disk_usage(r).free < 1536*1024*1024:
   result['stopped_for_disk_reserve']=True
   os.killpg(child.pid,signal.SIGTERM)
   break
  time.sleep(2)
 code=child.wait()
result.update(exit_code=code,elapsed_s=time.time()-start,free_after=shutil.disk_usage(r).free)
(r/'logs'/f'{stage}.status.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2));sys.exit(code)
