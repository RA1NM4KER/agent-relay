#!/usr/bin/env python3
"""Prepare a tiny no-network/no-share guest from checksum-verified Alpine media.

Does not mount an image, install software or start a VM. Published images remain
in the isolated lab and are reused by exact SHA256. No host files enter the guest.
"""
from pathlib import Path
import gzip
import hashlib
import json
import struct
import urllib.request

LAB=Path('/private/tmp/relay-native-build-20261030-9fw2zv7w')
ROOT=LAB/'vm-images'
ROOT.mkdir(exist_ok=True)
VERSION='3.22.7'
NAME=f'alpine-virt-{VERSION}-aarch64.iso'
BASE='https://dl-cdn.alpinelinux.org/alpine/v3.22/releases/aarch64/'


def digest(path):
    with path.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()


def media():
    checksum=ROOT/(NAME+'.sha256')
    if not checksum.exists():
        with urllib.request.urlopen(BASE+checksum.name,timeout=30) as response:data=response.read(4096)
        checksum.write_bytes(data)
    expected=checksum.read_text().split()[0]
    if len(expected)!=64 or not all(c in '0123456789abcdef' for c in expected):raise ValueError('bad published checksum')
    target=ROOT/NAME
    reused=target.exists()
    if not reused:
        temporary=ROOT/(NAME+'.pending')
        with urllib.request.urlopen(BASE+NAME,timeout=30) as response,temporary.open('xb') as out:
            total=0
            while block:=response.read(1024*1024):
                total+=len(block)
                if total>120*1024*1024:raise ValueError('media size limit')
                out.write(block)
        if digest(temporary)!=expected:raise ValueError('published media checksum mismatch')
        temporary.replace(target)
    if digest(target)!=expected:raise ValueError('cached image checksum mismatch')
    return target,dict(url=BASE+NAME,checksum_url=BASE+checksum.name,sha256=expected,bytes=target.stat().st_size,reused=reused,verification='publisher SHA256 over HTTPS; detached signature not verified')


def record(data,offset):
    length=data[offset]
    if length==0:return None
    item=data[offset:offset+length]
    extent=struct.unpack_from('<I',item,2)[0]
    size=struct.unpack_from('<I',item,10)[0]
    count=item[32];raw=item[33:33+count]
    name=raw.decode('ascii',errors='replace').split(';')[0].rstrip('.').lower()
    extra=33+count
    if extra%2:extra+=1
    # Rock Ridge NM preserves the original useful Linux filename.
    while extra+4<=len(item):
        kind=item[extra:extra+2];n=item[extra+2]
        if n<4:break
        if kind==b'NM' and n>=5:name=item[extra+5:extra+n].decode('utf-8')
        extra+=n
    return name,extent,size,bool(item[25]&2),length


def extract(iso):
    with iso.open('rb') as stream:
        for sector in range(16,64):
            stream.seek(sector*2048);descriptor=stream.read(2048)
            if descriptor[:7]==b'\x01CD001\x01':break
        else:raise ValueError('no ISO9660 primary descriptor')
        root=record(descriptor,156)
        def children(item):
            _,extent,size,_,_=item
            stream.seek(extent*2048);data=stream.read(size);offset=0;result=[]
            while offset<len(data):
                entry=record(data,offset)
                if entry is None:offset=(offset//2048+1)*2048;continue
                offset+=entry[4]
                if entry[0] not in ('\x00','\x01'):result.append(entry)
            return result
        boot=next(item for item in children(root) if item[0]=='boot')
        listing=children(boot);outputs=[]
        for needle,target in [('vmlinuz-virt','kernel'),('initramfs-virt','base-initramfs')]:
            item=next(item for item in listing if item[0]==needle)
            stream.seek(item[1]*2048);data=stream.read(item[2])
            if target=='kernel' and data[4:8]==b'zimg':
                # Linux EFI zboot-header.S: payload offset/length at 8/12.
                # https://github.com/torvalds/linux/blob/v6.12/drivers/firmware/efi/libstub/zboot-header.S
                offset,length=struct.unpack_from('<II',data,8)
                if offset+length>len(data) or data[offset:offset+2]!=b'\x1f\x8b':raise ValueError('unsupported EFI compressed payload')
                data=gzip.decompress(data[offset:offset+length])
            elif target=='kernel' and data.startswith(b'\x1f\x8b'):data=gzip.decompress(data)
            destination=ROOT/target;destination.write_bytes(data)
            outputs.append(dict(name=target,bytes=len(data),sha256=hashlib.sha256(data).hexdigest()))
        if (ROOT/'kernel').read_bytes()[56:60]!=b'ARMd':raise ValueError('expected raw ARM64 kernel image')
        return outputs


def cpio_entry(name,payload,mode,ino,rdevmajor=0,rdevminor=0):
    name=name.encode()+b'\0'
    fields=[ino,mode,0,0,1,0,len(payload),0,0,rdevmajor,rdevminor,len(name),0]
    header=b'070701'+''.join(f'{value:08x}' for value in fields).encode()
    entry=header+name;entry+=b'\0'*((-len(entry))%4)
    return entry+payload+b'\0'*((-len(payload))%4)


def append_init():
    script=(Path(__file__).parent/'guest_init.sh').read_bytes()
    entries=[cpio_entry('init',script,0o100755,0x700001),
             cpio_entry('dev/console',b'',0o020600,0x700002,5,1),
             cpio_entry('dev/null',b'',0o020666,0x700003,1,3),
             cpio_entry('TRAILER!!!',b'',0,0x700004)]
    base=(ROOT/'base-initramfs').read_bytes()
    if not base.startswith(b'\x1f\x8b'):raise ValueError('unsupported base initramfs encoding')
    final=base+gzip.compress(b''.join(entries),mtime=0)
    target=ROOT/'realm-initramfs';target.write_bytes(final)
    return dict(name=target.name,bytes=len(final),sha256=hashlib.sha256(final).hexdigest(),guest_init_sha256=hashlib.sha256(script).hexdigest())


if __name__=='__main__':
    iso,provenance=media();outputs=extract(iso);outputs.append(append_init())
    result=dict(scope='DISPOSABLE_GUEST_PREPARATION_NOT_CONTAINMENT_PROOF',media=provenance,outputs=outputs,host_files_shared=False,network_enabled=False,vm_started=False)
    (ROOT/'manifest.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result,indent=2))
