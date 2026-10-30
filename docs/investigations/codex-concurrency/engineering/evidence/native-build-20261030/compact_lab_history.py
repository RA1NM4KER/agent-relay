import subprocess,shutil,json,time,hashlib
from pathlib import Path
r=Path(__file__).resolve().parent
base=r/'tooling/cargo/git/checkouts/rusty_v8-1c356b58a53d1d98/350c5a8'
records=[]
def git(p,*args):return subprocess.check_output(['git','-C',str(p),*args],text=True).strip()
for name,url in [('v8','https://github.com/denoland/v8.git'),('third_party/icu','https://chromium.googlesource.com/chromium/deps/icu.git')]:
 p=base/name;assert p.is_relative_to(r) and (p/'.git').is_dir()
 head=git(p,'rev-parse','HEAD');tree=git(p,'rev-parse','HEAD^{tree}')
 subprocess.run(['git','-C',str(p),'diff','--quiet','HEAD'],check=True)
 dest=r/'tooling'/('shallow-'+name.replace('/','-'))
 before=int(subprocess.check_output(['du','-sk',str(p/'.git')],text=True).split()[0])
 with (r/'logs'/('compact-'+name.replace('/','-')+'.log')).open('w') as log:
  subprocess.run(['git','clone','--bare','--depth','1','--single-branch','--no-tags',p.as_uri(),str(dest)],check=True,stdout=log,stderr=subprocess.STDOUT)
 assert git(dest,'rev-parse','HEAD')==head
 assert git(dest,'rev-parse','HEAD^{tree}')==tree
 subprocess.run(['git','-C',str(dest),'config','core.bare','false'],check=True)
 subprocess.run(['git','-C',str(dest),'config','remote.origin.url',url],check=True)
 shutil.copyfile(p/'.git/index',dest/'index')
 old=r/'tooling'/('redundant-history-'+name.replace('/','-'))
 (p/'.git').rename(old);dest.rename(p/'.git')
 try:
  assert git(p,'rev-parse','HEAD')==head
  assert git(p,'rev-parse','HEAD^{tree}')==tree
  subprocess.run(['git','-C',str(p),'diff','--quiet','HEAD'],check=True)
 except BaseException:
  (p/'.git').rename(dest);old.rename(p/'.git');raise
 # Delete only this run's downloaded redundant metadata, after source validation.
 assert old.is_relative_to(r/'tooling') and old.name.startswith('redundant-history-')
 shutil.rmtree(old)
 after=int(subprocess.check_output(['du','-sk',str(p/'.git')],text=True).split()[0])
 records.append(dict(path=str(p),head=head,tree=tree,source_unchanged=True,metadata_before_kib=before,metadata_after_kib=after))
 (r/'history-compaction.json').write_text(json.dumps(records,indent=2)+'\n')
 print(json.dumps(records[-1]),flush=True)
