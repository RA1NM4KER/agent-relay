import os,sys,json,hashlib
from pathlib import Path
lab=Path(__file__).resolve().parent
os.environ['CODEX_REPO_ROOT']=str(lab/'codex')
sys.path.insert(0,str(lab/'codex/scripts'))
from codex_package.v8 import resolve_codex_v8_cargo_env
from codex_package.targets import TARGET_SPECS
overrides=resolve_codex_v8_cargo_env(TARGET_SPECS['aarch64-apple-darwin'],cache_root=lab/'v8-cache')
record={k:{'path':v,'sha256':hashlib.sha256(Path(v).read_bytes()).hexdigest()} for k,v in overrides.items()}
(lab/'research-v8-artifacts.json').write_text(json.dumps(record,indent=2)+'\n')
os.environ.update(overrides)
os.execvp('cargo',['cargo','build','--locked','-p','codex-code-mode-host','--bin','codex-code-mode-host','-p','codex-exec-server-cli','--bin','codex-exec-server'])
