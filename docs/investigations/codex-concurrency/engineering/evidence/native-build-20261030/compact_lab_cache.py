import subprocess,shutil,json
from pathlib import Path
r=Path(__file__).resolve().parent
base=r/'tooling/cargo/git/checkouts/rusty_v8-1c356b58a53d1d98/350c5a8'
records=[]
def git(p,*args):return subprocess.check_output(['git','-C',str(p),*args],text=True).strip()
for name,cache,url in [('v8','v8-fe0fb0544a81ddba','https://github.com/denoland/v8.git'),('third_party/icu','icu-65e5d034597e79c3','https://chromium.googlesource.com/chromium/deps/icu.git')]:
 source=base/name;p=r/'tooling/cargo/git/db'/cache;head=git(source,'rev-parse','HEAD');tree=git(source,'rev-parse','HEAD^{tree}');assert git(p,'rev-parse',head+'^{tree}')==tree
 dest=r/'tooling'/('shallow-cache-'+cache)
 before=int(subprocess.check_output(['du','-sk',str(p)],text=True).split()[0]);free=shutil.disk_usage(r).free
 with (r/'logs'/('compact-cache-'+cache+'.log')).open('w') as log:
  subprocess.run(['git','clone','--bare','--depth','1','--single-branch','--no-tags',source.as_uri(),str(dest)],check=True,stdout=log,stderr=subprocess.STDOUT)
 assert git(dest,'rev-parse','HEAD')==head and git(dest,'rev-parse','HEAD^{tree}')==tree
 subprocess.run(['git','-C',str(dest),'config','remote.origin.url',url],check=True)
 subprocess.run(['git','-C',str(dest),'update-ref','refs/commit/'+head,head],check=True)
 old=r/'tooling'/('redundant-cache-'+cache);p.rename(old);dest.rename(p)
 assert git(p,'rev-parse',head+'^{tree}')==tree
 assert old.is_relative_to(r/'tooling') and old.name.startswith('redundant-cache-')
 shutil.rmtree(old)
 after=int(subprocess.check_output(['du','-sk',str(p)],text=True).split()[0]);records.append(dict(path=str(p),head=head,tree=tree,metadata_before_kib=before,metadata_after_kib=after,free_before=free,free_after=shutil.disk_usage(r).free))
 (r/'cache-compaction.json').write_text(json.dumps(records,indent=2)+'\n');print(json.dumps(records[-1]),flush=True)
