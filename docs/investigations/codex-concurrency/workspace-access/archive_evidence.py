#!/usr/bin/env python3
"""Whitelist only experiment evidence, never enrollments or private capabilities."""
from pathlib import Path
import hashlib
import json
import shutil

REPO=Path(__file__).resolve().parents[4]
LAB=REPO/'.lab/workspace-access'
OUT=Path(__file__).parent/'evidence'
for source in sorted((LAB/'runs').iterdir()):
 if not (source/'result.json').exists():continue
 for item in source.rglob('*'):
  if item.is_file() and item.name in {'result.json','driver.py','spec.json','script','console.log','state.json','footprint.json','footprint.stdout','footprint.stderr'}:
   target=OUT/'runs'/source.name/item.relative_to(source);target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(item,target)
for source in sorted((LAB/'projects').iterdir()):
 if not source.name.startswith(('publisher-','native-sandbox-')) or not (source/'result.json').exists():continue
 for item in source.rglob('*'):
  if item.is_file() and item.name in {'result.json','attempts.json','profile.sbpl','stdout','stderr'}:
   target=OUT/'host-probes'/source.name/item.relative_to(source);target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(item,target)
index=[]
for item in sorted(OUT.rglob('*')):
 if item.is_file() and item.name!='index.json':
  index.append(dict(path=str(item.relative_to(OUT)),bytes=item.stat().st_size,sha256=hashlib.sha256(item.read_bytes()).hexdigest()))
(OUT/'index.json').write_text(json.dumps(dict(files=index,privateCapabilitiesArchived=False,fullParity=False),indent=2)+'\n')
print(json.dumps(dict(files=len(index),bytes=sum(x['bytes'] for x in index))))
