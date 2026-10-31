from pathlib import Path
import hashlib,json,os,sys
lab=Path(__file__).resolve().parent
for key,row in json.loads((lab/'research-v8-artifacts.json').read_text()).items():
 p=Path(row['path'])
 with p.open('rb') as f:assert hashlib.file_digest(f,'sha256').hexdigest()==row['sha256']
 os.environ[key]=str(p)
src=lab/'tooling/cargo/registry/src/index.crates.io-1949cf8c6b5b557f'
os.execvp('cargo',['cargo',*sys.argv[1:],'--locked','--offline','--config','source.crates-io.replace-with="retained-lab"','--config',f'source.retained-lab.directory="{src}"'])
