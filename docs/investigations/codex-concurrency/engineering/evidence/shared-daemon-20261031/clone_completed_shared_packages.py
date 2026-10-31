from pathlib import Path
import fcntl, hashlib, json, os, shutil, stat, subprocess
lab=Path('/private/tmp/relay-native-build-20261030-9fw2zv7w')
source=lab/'target/research-package'; manifest=json.loads((lab/'research-package-manifest.json').read_text()); reports=[]
def digest(p):
 with p.open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
for name in ['relay-native-tui-lab-j2t4xjw9','relay-native-tui-lab-zqnciiy7']:
 root=Path('/private/tmp')/name; result=json.loads((root/'result.json').read_text())
 assert result['daemon_pid_absent_after_cleanup'] and result['daemon_socket_quiet_after_cleanup']
 home=root/'codex'; release=(home/'packages/app-server-daemon/current').resolve(strict=True)
 assert release.is_relative_to(home.resolve())
 before=shutil.disk_usage('/private/tmp').free
 with (home/'app-server-daemon/daemon.lock').open('a+') as daemon_lock, (home/'packages/app-server-daemon/install.lock').open('a+') as install_lock:
  fcntl.flock(daemon_lock,fcntl.LOCK_EX|fcntl.LOCK_NB);fcntl.flock(install_lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
  assert not (home/'app-server-daemon/daemon.pid').exists()
  opened=subprocess.run(['/usr/sbin/lsof','-nP','+D',str(release)],capture_output=True,text=True)
  assert opened.returncode==1 and not opened.stdout,opened.stdout
  report={'root':str(root),'free_before':before,'files':[]}
  for relative,expected in manifest.items():
   src=source/relative; target=release/relative
   assert digest(src)==expected==digest(target)
   if relative=='codex-package.json':continue
   original=target.stat(); temporary=target.with_name(target.name+'.clone-pending');backup=target.with_name(target.name+'.original-backup')
   assert not temporary.exists() and not backup.exists()
   try:
    subprocess.run(['/bin/cp','-c','-p',str(src),str(temporary)],check=True)
    os.chmod(temporary,stat.S_IMODE(original.st_mode));os.utime(temporary,ns=(original.st_atime_ns,original.st_mtime_ns))
    assert temporary.stat().st_uid==original.st_uid and temporary.stat().st_gid==original.st_gid and digest(temporary)==expected
    os.link(target,backup);os.replace(temporary,target)
    assert digest(target)==expected and target.stat().st_mtime_ns==original.st_mtime_ns
    backup.unlink()
   except BaseException:
    if backup.exists():os.replace(backup,target)
    raise
   finally:
    if temporary.exists():temporary.unlink()
   report['files'].append(dict(path=relative,sha256=expected,logical_bytes_preserved=original.st_size,mtime_preserved=True))
  report['free_after']=shutil.disk_usage('/private/tmp').free;reports.append(report)
(lab/'completed-shared-package-clones.json').write_text(json.dumps(reports,indent=2)+'\n')
print(json.dumps({'free_before':reports[0]['free_before'],'free_after':reports[-1]['free_after'],'all_files_preserved':True}))
