from pathlib import Path
import hashlib,json,os,re,subprocess,tarfile,tomllib,urllib.request,io
lab=Path(__file__).resolve().parent
src=lab/'tooling/cargo/registry/src/index.crates.io-1949cf8c6b5b557f'
packages=tomllib.loads((lab/'codex/codex-rs/Cargo.lock').read_text())['package']
args=['cargo','metadata','--format-version','1','--locked','--offline','--config','source.crates-io.replace-with="retained-lab"','--config',f'source.retained-lab.directory="{src}"']
rows=json.loads((lab/'native-authority-source-resolution.json').read_text()) if (lab/'native-authority-source-resolution.json').exists() else []
try:
 for attempt in range(100):
  with (lab/'native-authority-metadata.json').open('wb') as out:
   p=subprocess.run(args,cwd=lab/'codex/codex-rs',stdout=out,stderr=subprocess.PIPE,text=True)
  if p.returncode==0:print('Metadata resolved; downloaded',len(rows),'missing packages');break
  (lab/'native-authority-resolution-last-error.txt').write_text(p.stderr)
  match=re.search(r"no matching package named `([^`]+)` found",p.stderr)
  if not match:match=re.search(r"searched package name: `([^`]+)`",p.stderr)
  if not match:match=re.search(r"requirement `([A-Za-z0-9_-]+) =",p.stderr)
  if not match:raise RuntimeError(p.stderr)
  name=match.group(1)
  choices=[p for p in packages if p['name']==name and p.get('source','').startswith('registry+')]
  assert choices,name
  for package in choices:
   version=package['version'];expected=package['checksum'];directory=src/f'{name}-{version}'
   if directory.exists():continue
   cache=Path('/Users/kefasmanda/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f')/f'{name}-{version}.crate'
   if cache.exists():blob=cache.read_bytes();origin='existing global archive (verified)'
   else:
    url=f'https://static.crates.io/crates/{name}/{name}-{version}.crate'
    with urllib.request.urlopen(url,timeout=30) as response:blob=response.read(32*1024*1024)
    origin=url
   assert hashlib.sha256(blob).hexdigest()==expected
   with tarfile.open(fileobj=io.BytesIO(blob),mode='r:gz') as archive:
    members=archive.getmembers()
    assert all(m.name.split('/')[0]==directory.name and not m.issym() and not m.islnk() for m in members)
    archive.extractall(src,filter='data')
   files={str(p.relative_to(directory)):hashlib.sha256(p.read_bytes()).hexdigest() for p in directory.rglob('*') if p.is_file() and p.name not in ['.cargo-checksum.json','.cargo-ok']}
   (directory/'.cargo-checksum.json').write_text(json.dumps({'package':expected,'files':files},separators=(',',':')))
   rows.append(dict(name=name,version=version,sha256=expected,bytes=len(blob),origin=origin))
 else:raise RuntimeError('bounded resolver iteration limit')
finally:(lab/'native-authority-source-resolution.json').write_text(json.dumps(rows,indent=2)+'\n')
