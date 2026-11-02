#!/usr/bin/env python3
"""Actual VZ workspace primitive probes. Only generated copies may be shared."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import time
import uuid

REPO = Path(__file__).resolve().parents[4]
LAB = REPO/'.lab/workspace-access'
SOURCE = Path(__file__).parent


def tree_hash(root):
    h = hashlib.sha256()
    for path in sorted(root.rglob('*')):
        if path.is_file() and not path.is_symlink():
            h.update(str(path.relative_to(root)).encode()+b'\0'+path.read_bytes())
    return h.hexdigest()


def source_tree(target):
    archive = subprocess.check_output(['git', 'archive', 'HEAD', '--', 'Cargo.toml', 'Cargo.lock', 'crates', 'README.md'], cwd=REPO)
    with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
        if any(not (member.isfile() or member.isdir()) for member in stream.getmembers()):
            raise ValueError('source fixture must contain only ordinary tracked files')
        stream.extractall(target, filter='data')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=['direct-rw', 'readonly-draft'], required=True)
    parser.add_argument('--action', choices=['detach', 'stop', 'crash'], default='detach')
    parser.add_argument('--agents', type=int, choices=[1, 2, 8], default=1)
    parser.add_argument('--memory-mib', type=int, choices=[128, 192], default=192)
    args = parser.parse_args()
    if shutil.disk_usage(LAB).free < 4*1024**3:
        raise RuntimeError('unchanged 4 GiB entry guard')
    run = LAB/'runs'/str(uuid.uuid4())
    run.mkdir(parents=True)
    shutil.copyfile(__file__, run/'driver.py')
    projects = LAB/'projects'/run.name
    projects.mkdir(parents=True)
    reference = projects/'reference'
    reference.mkdir()
    source_tree(reference)
    results = {'run': str(run), 'mode': args.mode, 'action': args.action, 'agents': args.agents,
               'memoryMiBPerVm': args.memory_mib, 'nativeCodexIntegrated': False,
               'workspaceQuiescence': 'UNPROVEN', 'handoffPermitted': False, 'samples': [], 'vms': []}
    processes, logs, objects = [], [], []
    started = time.monotonic()
    free_before = shutil.disk_usage(LAB).free

    def state(control):
        try: return json.loads((control/'state.json').read_text())
        except (OSError, ValueError): return {}

    def effects(fixture):
        root = fixture/('seed' if args.mode == 'direct-rw' else 'draft')
        mmap = root/'mmap.bin'
        return {'heldBytes': (root/'held.log').stat().st_size if (root/'held.log').exists() else 0,
                'mmapCounter': int.from_bytes(mmap.read_bytes()[:8], 'little') if mmap.exists() else 0,
                'burstBytes': (root/'burst.bin').stat().st_size if (root/'burst.bin').exists() else 0}

    def wait(predicate, label, seconds=15):
        deadline = time.monotonic()+seconds
        while time.monotonic()<deadline:
            if shutil.disk_usage(LAB).free < 1536*1024**2:
                raise RuntimeError('unchanged runtime reserve')
            if predicate(): return
            time.sleep(.05)
        raise AssertionError(label)

    def sample(phase):
        rows = subprocess.check_output(['ps','-axo','pid=,ppid=,rss=,time=,comm='], text=True).splitlines()
        # Observational telemetry only; never used to identify a stop target.
        selected = [row.strip() for row in rows if '/workspace-vm' in row or 'com.apple.Virtualization.VirtualMachine' in row]
        results['samples'].append({'phase':phase,'elapsedSeconds':time.monotonic()-started,
                                  'processTelemetry': selected,
                                  'vmStat':subprocess.check_output(['vm_stat'],text=True),
                                  'swap':subprocess.check_output(['sysctl','vm.swapusage'],text=True).strip(),
                                  'freeBytes':shutil.disk_usage(LAB).free})
    try:
        sample('before')
        for index in range(args.agents):
            fixture = projects/str(index)
            fixture.mkdir()
            (fixture/'.relay-disposable-project').write_text('DISPOSABLE_RESEARCH_COPY\n')
            shutil.copytree(reference,fixture/'seed')
            (fixture/'draft').mkdir()
            (fixture/'private-canary').write_text('host-only disposable canary')
            before = tree_hash(fixture/'seed')
            control = run/str(index)
            control.mkdir()
            destination = '/project' if args.mode=='direct-rw' else '/draft'
            script = (f"echo WORKSPACE_START; /bin/busybox head -c 40 /project/Cargo.toml; "
                      f"echo bad >>/project/readonly-attempt 2>/dev/null; "
                      f"/workspace-mmap-probe {destination}/mmap.bin & "
                      f"/bin/busybox setsid /bin/sh -c 'exec 3>>{destination}/held.log; "
                      "i=0; while [ $i -lt 1000 ]; do echo HELD_WRITE >&3; i=$((i+1)); /bin/busybox sleep .01; done' & "
                      f"/bin/busybox dd if=/dev/zero of={destination}/burst.bin bs=65536 count=512 2>/dev/null & "
                      "/bin/busybox sleep 12")
            (control/'script').write_text(script)
            spec = {'kernel':str(LAB/'vm-images/kernel'),'initramfs':str(LAB/'vm-images/workspace-initramfs'),
                    'fixture':str(fixture),'control':str(control),'script':str(control/'script'),
                    'mode':args.mode,'identity':str(uuid.uuid4()),'label':f'worker-{index}','memoryMiB':args.memory_mib}
            (control/'spec.json').write_text(json.dumps(spec))
            log=(control/'console.log').open('wb');logs.append(log)
            process=subprocess.Popen([str(LAB/'workspace-vm'),str(control/'spec.json')],stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
            processes.append(process);objects.append((fixture,control,before))
        wait(lambda:all(state(c).get('vmRunning') and effects(f)['heldBytes']>0 and effects(f)['mmapCounter']>0 for f,c,_ in objects),
             'all workspace VMs produced real held-FD and mmap writes')
        results['startupAndFirstEffectsSeconds']=time.monotonic()-started
        sample('active')
        time.sleep(.5)
        for fixture,control,before in objects:
            results['vms'].append({'fixture':str(fixture),'beforeAction':effects(fixture),'stateBefore':state(control),'sourceHashBefore':before})
        action_started=time.monotonic()
        for _,control,_ in objects:
            (control/args.action).touch()
        if args.action=='detach':
            wait(lambda:all(state(c).get('sharesDetached') for _,c,_ in objects),'runtime share replacement acknowledged')
            for value,(fixture,control,_) in zip(results['vms'],objects):value['atDetachAcknowledgement']=effects(fixture)
            time.sleep(1)
            for value,(fixture,control,_) in zip(results['vms'],objects):value['oneSecondAfterDetach']=effects(fixture)
        elif args.action=='stop':
            wait(lambda:all(state(c).get('vmStopped') for _,c,_ in objects),'trusted exact-kernel stops')
        else:
            wait(lambda:all(p.poll() is not None for p in processes),'exact manager self-SIGKILL')
        results['actionObservationSeconds']=time.monotonic()-action_started
        for value,(fixture,control,_) in zip(results['vms'],objects):
            value.update(afterAction=effects(fixture),stateAfter=state(control),sourceHashAfter=tree_hash(fixture/'seed'))
        results['completed']=True
    except BaseException as error:
        results.update(completed=False,error=repr(error))
    finally:
        for _,control,_ in objects:(control/'stop').touch()
        for process in processes:
            try:process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:process.wait(timeout=8)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=3)
        for log in logs:log.close()
        results['cleanup']=[{'pid':p.pid,'exitCode':p.returncode,'state':state(c)} for p,(_,c,_) in zip(processes,objects)]
        sample('after')
        results['elapsedSeconds']=time.monotonic()-started
        results['freeByteDelta']=shutil.disk_usage(LAB).free-free_before
        (run/'result.json').write_text(json.dumps(results,indent=2)+'\n')
        print(json.dumps({'run':str(run),'completed':results.get('completed'),'error':results.get('error'),
                          'startupSeconds':results.get('startupAndFirstEffectsSeconds'),'actionSeconds':results.get('actionObservationSeconds')}))
    return 0 if results.get('completed') else 1

if __name__=='__main__':raise SystemExit(main())
