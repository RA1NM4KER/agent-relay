#!/usr/bin/env python3
"""Reject unsafe sharing specifications before a guest can boot."""
import json
from pathlib import Path
import subprocess
import uuid

REPO=Path(__file__).resolve().parents[4]
LAB=REPO/'.lab/workspace-access'
root=LAB/'projects'/('config-'+str(uuid.uuid4()));root.mkdir()
(root/'.relay-disposable-project').write_text('DISPOSABLE_RESEARCH_COPY\n')
(root/'seed').mkdir();(root/'draft').mkdir();outside=root/'fake-outside';outside.mkdir()
control=LAB/'runs'/root.name;control.mkdir();(control/'script').write_text('true\n')
base=dict(kernel=str(LAB/'vm-images/kernel'),initramfs=str(LAB/'vm-images/workspace-initramfs'),fixture=str(root),control=str(control),script=str(control/'script'),identity=str(uuid.uuid4()),mode='readonly-draft',label='config-test',memoryMiB=128)
results=[]
def refused(name,spec):
 p=control/(name+'.json');p.write_text(json.dumps(spec))
 child=subprocess.run([str(LAB/'workspace-vm'),str(p)],capture_output=True,text=True,timeout=3)
 assert child.returncode==2 and not (control/'state.json').exists(),name
 results.append(dict(name=name,exitCode=child.returncode,stderr=child.stderr))
for name in ['seed','draft']:
 (root/name).rmdir();(root/name).symlink_to(outside,target_is_directory=True)
 refused(name+'-symlink',base)
 (root/name).unlink();(root/name).mkdir()
refused('control-under-export',dict(base,control=str(root/'draft')))
refused('script-outside-control',dict(base,script=str(root/'.relay-disposable-project')))
refused('kernel-commandline-injection',dict(base,label='bad relay.script=other'))
(control/'result.json').write_text(json.dumps(dict(checks=results,passed=True,handoffPermitted=False),indent=2)+'\n')
print(json.dumps(dict(run=str(control),checks=len(results),passed=True)))
