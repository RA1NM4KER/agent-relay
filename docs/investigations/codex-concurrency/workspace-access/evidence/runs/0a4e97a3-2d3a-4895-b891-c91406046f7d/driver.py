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
from publisher import Publisher, Refused, collect_regular

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
    parser.add_argument('--first-only', action='store_true')
    parser.add_argument('--publication', action='store_true')
    parser.add_argument('--footprint', action='store_true')
    parser.add_argument('--hold-seconds', type=float, default=.5)
    parser.add_argument('--working-set-mib', type=int, choices=[0, 64], default=0)
    args = parser.parse_args()
    if args.publication and (args.mode != 'readonly-draft' or args.action != 'stop' or not args.first_only or args.agents < 2):
        raise ValueError('publication requires readonly-draft / stop first only / multiple sessions')
    if int(subprocess.check_output(['sysctl','-n','kern.memorystatus_vm_pressure_level'],text=True)) >= 4:
        raise RuntimeError('host memory pressure is critical; no VM experiment launched')
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
               'workingSetMiBPerVm': args.working_set_mib, 'firstOnly': args.first_only,
               'workspaceQuiescence': 'UNPROVEN', 'handoffPermitted': False, 'samples': [], 'vms': []}
    processes, logs, objects = [], [], []
    broker = None
    capabilities = []
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
            if int(subprocess.check_output(['sysctl','-n','kern.memorystatus_vm_pressure_level'],text=True)) >= 4:
                raise RuntimeError('host memory pressure became critical; stop exact research VMs')
            if shutil.disk_usage(LAB).free < 1536*1024**2:
                raise RuntimeError('unchanged runtime reserve')
            if predicate(): return
            time.sleep(.05)
        raise AssertionError(label)

    def sample(phase):
        rows = subprocess.check_output(['ps','-axo','pid=,ppid=,rss=,time=,comm='], text=True).splitlines()
        # Observational telemetry only; never used to identify a stop target.
        selected = [row.strip() for row in rows if '/workspace-vm' in row or 'com.apple.Virtualization.VirtualMachine' in row]
        if args.footprint and phase == 'active':
            targets=[line.split()[0] for line in selected]
            if targets:
                command=['footprint','-j',str(run/'footprint.json'),'--noCategories','--swapped']+targets
                observation=subprocess.run(command,capture_output=True,text=True,timeout=10)
                (run/'footprint.stdout').write_text(observation.stdout)
                (run/'footprint.stderr').write_text(observation.stderr)
                results['footprintExitCode']=observation.returncode
        results['samples'].append({'phase':phase,'elapsedSeconds':time.monotonic()-started,
                                  'processTelemetry': selected,
                                  'vmStat':subprocess.check_output(['vm_stat'],text=True),
                                  'swap':subprocess.check_output(['sysctl','vm.swapusage'],text=True).strip(),
                                  'memoryPressure':subprocess.check_output(['sysctl','-n','kern.memorystatus_vm_pressure_level'],text=True).strip(),
                                  'freeBytes':shutil.disk_usage(LAB).free})
    try:
        if args.publication:
            publication_root = projects/'publication-host-only'
            publication_root.mkdir()
            (publication_root/'.relay-disposable-project').write_text('DISPOSABLE_RESEARCH_COPY\n')
            shutil.copytree(reference,publication_root/'canonical')
            broker = Publisher(publication_root)
            capabilities = [broker.register() for _ in range(args.agents)]
            results['publication'] = {'canonicalExported':False,'checks':[],'handoffPermitted':False}
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
                      f"/bin/busybox dd if=/dev/zero of=/tmp/working-set bs=1048576 count={args.working_set_mib} 2>/dev/null; "
                      f"echo bad >>/project/readonly-attempt 2>/dev/null; "
                      f"/bin/busybox ln -s {fixture}/private-canary {destination}/escape; "
                      f"/bin/busybox cat {destination}/escape 2>/dev/null; "
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
        time.sleep(args.hold_seconds)
        sample('active-later')
        for fixture,control,before in objects:
            results['vms'].append({'fixture':str(fixture),'beforeAction':effects(fixture),'stateBefore':state(control),'sourceHashBefore':before})
        if broker:
            checks = results['publication']['checks']
            for index,(fixture,_,_) in enumerate(objects):
                payload = collect_regular(fixture/'draft','held.log')
                assert payload.startswith(b'HELD_WRITE')
                broker.put(capabilities[index],1,'guest-initial',f'guest-{index}.txt',payload,None)
            checks.append('real draft bytes from every running guest published to non-exported project copy')
            receipt = broker.revoke(capabilities[0],1)
            assert receipt['drained'] and not receipt['handoffPermitted']
            results['publication']['AReceiptBeforeKernelStop'] = receipt
            checks.append('A publication fenced while its guest still running')
            try: broker.put(capabilities[0],1,'bypass','late.txt',b'bad',None)
            except Refused: checks.append('A fresh publication rejected at host boundary')
            else: raise AssertionError('fenced A publication bypass')
            canonical_before = tree_hash(publication_root/'canonical')
            wait(lambda:all(effects(f)['heldBytes'] > results['vms'][i]['beforeAction']['heldBytes'] for i,(f,_,_) in enumerate(objects)), 'live detached writes continue before kernel stop')
            assert tree_hash(publication_root/'canonical') == canonical_before
            checks.append('continuing detached guest effects cannot directly modify canonical copy')
        action_started=time.monotonic()
        targets = objects[:1] if args.first_only else objects
        for _,control,_ in targets:
            (control/args.action).touch()
        if args.action=='detach':
            wait(lambda:all(state(c).get('sharesDetached') for _,c,_ in targets),'runtime share replacement acknowledged')
            for value,(fixture,control,_) in zip(results['vms'],objects):value['atDetachAcknowledgement']=effects(fixture)
            time.sleep(1)
            for value,(fixture,control,_) in zip(results['vms'],objects):value['oneSecondAfterDetach']=effects(fixture)
        elif args.action=='stop':
            wait(lambda:all(state(c).get('vmStopped') for _,c,_ in targets),'trusted exact-kernel stops')
        else:
            wait(lambda:all(p.poll() is not None for p in processes[:len(targets)]),'exact manager self-SIGKILL')
        if broker:
            old_effects = [effects(f)['heldBytes'] for f,_,_ in objects]
            wait(lambda:all(state(c).get('vmRunning') and effects(f)['heldBytes'] > old_effects[i] for i,(f,c,_) in enumerate(objects) if i), 'B through H advance after A exact-kernel stop')
            for index,(fixture,_,_) in enumerate(objects[1:],1):
                broker.put(capabilities[index],1,'guest-after-A-stop',f'after-{index}.txt',collect_regular(fixture/'draft','held.log'),None)
            checks.append('B through H continue and publish after trusted A kernel stop')
            assert not (publication_root/'canonical/late.txt').exists()
            assert broker.revoke(capabilities[0],1)['drained']
            checks.append('A remains publication-fenced after exact kernel stop; Relay handoff still disabled')
        results['actionObservationSeconds']=time.monotonic()-action_started
        for value,(fixture,control,_) in zip(results['vms'],objects):
            value.update(afterAction=effects(fixture),stateAfter=state(control),sourceHashAfter=tree_hash(fixture/'seed'))
        results['completed']=True
    except BaseException as error:
        results.update(completed=False,error=repr(error))
    finally:
        if broker: broker.close()
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
