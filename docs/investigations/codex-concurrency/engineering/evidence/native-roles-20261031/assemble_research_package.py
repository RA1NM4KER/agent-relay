import os,sys,json,hashlib,shutil,subprocess
import stat
from pathlib import Path
lab=Path(__file__).resolve().parent
source=lab/'codex'
os.environ['CODEX_REPO_ROOT']=str(source)
sys.path.insert(0,str(source/'scripts'))
from codex_package import layout
from codex_package.layout import build_package_dir,validate_package_dir
from codex_package.targets import PackageInputs,PACKAGE_VARIANTS,TARGET_SPECS
# Preserve the official layout/validation while avoiding duplicate physical
# storage on APFS. These are independent copy-on-write files, not hardlinks.
def clone_executable(src,dest,*,is_windows):
 assert not is_windows
 dest.parent.mkdir(parents=True,exist_ok=True)
 subprocess.run(['/bin/cp','-c',str(src),str(dest)],check=True)
 assert dest.stat().st_ino != src.stat().st_ino
 assert hashlib.sha256(dest.read_bytes()).digest()==hashlib.sha256(src.read_bytes()).digest()
 dest.chmod(dest.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
layout.copy_executable=clone_executable
rg=Path(shutil.which('rg')).resolve(strict=True)
package=lab/'target/research-package'
package.mkdir(exist_ok=False)
inputs=PackageInputs(entrypoint_bin=lab/'target/debug/codex',
 code_mode_host_bin=lab/'target/debug/codex-code-mode-host',
 exec_server_bin=lab/'target/debug/codex-exec-server',rg_bin=rg,
 bwrap_bin=None,codex_command_runner_bin=None,codex_windows_sandbox_setup_bin=None)
# A distinct research package version prevents the native initial-start logic
# from treating this disposable package as a production auto-update channel.
build_package_dir(package,'0.166.0-relay-research',PACKAGE_VARIANTS['codex'],TARGET_SPECS['aarch64-apple-darwin'],inputs)
validate_package_dir(package,PACKAGE_VARIANTS['codex'],TARGET_SPECS['aarch64-apple-darwin'])
manifest={str(p.relative_to(package)):hashlib.sha256(p.read_bytes()).hexdigest()
 for p in package.rglob('*') if p.is_file()}
(lab/'research-package-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(json.dumps({'package':str(package),'sha256':manifest},indent=2))
