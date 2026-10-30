import subprocess,shutil,json
from pathlib import Path
r=Path(__file__).resolve().parent;base=r/'tooling/cargo/git/checkouts/rusty_v8-1c356b58a53d1d98/350c5a8';dbroot=r/'tooling/cargo/git/db';records=[]
def git(p,*args):return subprocess.check_output(['git','-C',str(p),*args],text=True,stderr=subprocess.DEVNULL).strip()
def size(p):return int(subprocess.check_output(['du','-sk',str(p)],text=True).split()[0])
for gd in sorted(base.rglob('.git')):
 p=gd.parent
 if p==base or p in [base/'v8',base/'third_party/icu'] or not gd.is_dir() or size(gd)<10000:continue
 head=git(p,'rev-parse','HEAD');tree=git(p,'rev-parse','HEAD^{tree}')
 subprocess.run(['git','-C',str(p),'diff','--quiet','HEAD'],check=True)
 matches=[]
 for db in dbroot.iterdir():
  if db.is_dir() and subprocess.run(['git','-C',str(db),'cat-file','-e',head+'^{commit}'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0:matches.append(db)
 if len(matches)!=1:records.append({'path':str(p),'skipped':'bare cache not uniquely established'});continue
 db=matches[0];assert git(db,'rev-parse',head+'^{tree}')==tree
 dest=r/'tooling/shallow-next';cachedest=r/'tooling/shallow-cache-next';old=r/'tooling/redundant-next';cacheold=r/'tooling/redundant-cache-next';before=size(gd)+size(db);free=shutil.disk_usage(r).free
 with (r/'logs/compact-remaining.log').open('a') as log:subprocess.run(['git','clone','--bare','--depth','1','--single-branch','--no-tags',p.as_uri(),str(dest)],check=True,stdout=log,stderr=subprocess.STDOUT)
 assert git(dest,'rev-parse','HEAD')==head and git(dest,'rev-parse','HEAD^{tree}')==tree
 shutil.copytree(dest,cachedest)
 subprocess.run(['git','-C',str(cachedest),'update-ref','refs/commit/'+head,head],check=True)
 subprocess.run(['git','-C',str(dest),'config','core.bare','false'],check=True);shutil.copyfile(gd/'index',dest/'index')
 gd.rename(old);dest.rename(gd);db.rename(cacheold);cachedest.rename(db)
 try:
  assert git(p,'rev-parse','HEAD')==head and git(p,'rev-parse','HEAD^{tree}')==tree
  assert git(db,'rev-parse',head+'^{tree}')==tree
  subprocess.run(['git','-C',str(p),'diff','--quiet','HEAD'],check=True)
 except BaseException:
  gd.rename(dest);old.rename(gd);db.rename(cachedest);cacheold.rename(db);raise
 # Every removed directory was created by this laboratory run; source stays intact.
 for q in [old,cacheold]:assert q.parent==r/'tooling';shutil.rmtree(q)
 records.append({'path':str(p),'head':head,'tree':tree,'source_unchanged':True,'metadata_before_kib':before,'metadata_after_kib':size(gd)+size(db),'free_before':free,'free_after':shutil.disk_usage(r).free})
 (r/'remaining-history-compaction.json').write_text(json.dumps(records,indent=2)+'\n');print(json.dumps(records[-1]),flush=True)
