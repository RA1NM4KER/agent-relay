"""Experimental host-only regular-file publisher, NOT a full workspace/handoff adapter.

Canonical files are never exported to the guest. Input is bounded bytes, not a
client pathname to open on the host. Unknown outcomes reserve their target paths.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import secrets
import sqlite3
import stat
import threading
import time
import unicodedata
import uuid


class Refused(RuntimeError):
    pass


def digest(data):
    return hashlib.sha256(data).hexdigest()


def components(relative):
    path = PurePosixPath(relative)
    if not relative or path.is_absolute() or str(path) != relative or len(relative) > 512:
        raise Refused('noncanonical relative file name')
    parts = path.parts
    if any(p in ('', '.', '..') or '\0' in p or p.startswith('.relay-') for p in parts):
        raise Refused('unsafe file name')
    return parts


def collect_regular(root, relative):
    """Never follow guest-created links into a host secret or another root."""
    parts = components(relative)
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 1024*1024:
                raise Refused('unclassified file type, alias, or size')
            data = os.read(fd, 1024*1024+1)
            if len(data) != info.st_size:
                raise Refused('changing input; no consistent byte request')
            return data
        finally:
            os.close(fd)
    finally:
        os.close(directory)


class Publisher:
    def __init__(self, fixture):
        self.fixture = Path(fixture).resolve(strict=True)
        if not (self.fixture/'.relay-disposable-project').is_file():
            raise Refused('only a marked disposable fixture is permitted')
        self.canonical = self.fixture/'canonical'
        self.canonical.mkdir(exist_ok=True)
        self.rootfd = os.open(self.canonical, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.guard = open(self.fixture/'publisher.lock', 'a+b')
        fcntl.flock(self.guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.db = sqlite3.connect(self.fixture/'publisher.sqlite', check_same_thread=False)
        self.db.execute('PRAGMA journal_mode=WAL')
        self.db.execute('PRAGMA synchronous=FULL')
        self.db.execute('PRAGMA fullfsync=ON')
        self.db.execute('PRAGMA checkpoint_fullfsync=ON')
        self.db.executescript('''
          CREATE TABLE IF NOT EXISTS owners (id TEXT PRIMARY KEY, cap TEXT UNIQUE, epoch INTEGER, active INTEGER);
          CREATE TABLE IF NOT EXISTS ops (owner TEXT, token TEXT, fp TEXT, path TEXT, alias TEXT, state TEXT, result TEXT, PRIMARY KEY(owner,token));
        ''')
        # A process death is not proof that an admitted I/O outcome is known.
        self.db.execute("UPDATE owners SET active=0 WHERE id IN (SELECT owner FROM ops WHERE state IN ('INTENT','ISSUING'))")
        self.db.commit()
        self.lock = threading.RLock()
        self.changed = threading.Condition(self.lock)
        self.pending = {}

    def close(self):
        with self.lock:
            if any(self.pending.values()): raise Refused('live I/O cannot be detached')
            self.db.close()
            os.close(self.rootfd)
            self.guard.close()

    def register(self):
        cap = secrets.token_hex(32)
        with self.lock:
            self.db.execute('INSERT INTO owners VALUES (?,?,1,1)', (str(uuid.uuid4()), digest(cap.encode())))
            self.db.commit()
        return cap

    def _owner(self, cap, epoch):
        row = self.db.execute('SELECT id,epoch,active FROM owners WHERE cap=?', (digest(cap.encode()),)).fetchone()
        if row is None or row[1] != epoch: raise Refused('foreign or stale epoch')
        return row

    def _parent(self, parts):
        fd = os.dup(self.rootfd)
        try:
            for part in parts[:-1]:
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = child
            return fd
        except BaseException:
            os.close(fd)
            raise

    def put(self, cap, epoch, token, relative, data, base, entered=None, release=None, crash=None):
        parts = components(relative)
        if not isinstance(data, bytes) or len(data) > 1024*1024: raise Refused('bounded bytes required')
        alias = unicodedata.normalize('NFD', relative).casefold()
        fp = digest(json.dumps([epoch,relative,digest(data),base],sort_keys=True).encode())
        with self.lock:
            owner, _, active = self._owner(cap,epoch)
            old = self.db.execute('SELECT fp,state,result FROM ops WHERE owner=? AND token=?',(owner,token)).fetchone()
            if old:
                if old[0] != fp: raise Refused('operation-token substitution')
                if old[1] == 'COMMITTED': return json.loads(old[2])
                raise Refused('unresolved or refused operation; never replay I/O')
            if not active: raise Refused('owner fenced')
            if self.db.execute("SELECT 1 FROM ops WHERE alias=? AND state IN ('INTENT','ISSUING')",(alias,)).fetchone():
                raise Refused('uncertain or pending path reservation')
            self.db.execute("INSERT INTO ops VALUES (?,?,?,?,?,'INTENT',NULL)",(owner,token,fp,relative,alias))
            self.db.commit()
            self.pending[owner] = self.pending.get(owner,0)+1
        try:
            if entered: entered.set()
            if release and not release.wait(5): raise Refused('bounded test pause expired')
            if crash == 'after-intent': os._exit(86)
            with self.lock:
                if not self._owner(cap,epoch)[2]:
                    self.db.execute("UPDATE ops SET state='REJECTED' WHERE owner=? AND token=?",(owner,token));self.db.commit()
                    raise Refused('queued write fenced before issue')
                self.db.execute("UPDATE ops SET state='ISSUING' WHERE owner=? AND token=?",(owner,token));self.db.commit()
            parent = self._parent(parts)
            temporary = '.relay-publish-'+str(uuid.uuid4())
            try:
                try:
                    fd = os.open(parts[-1],os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=parent)
                except FileNotFoundError: existing = None
                else:
                    try:
                        info = os.fstat(fd)
                        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 1024*1024:
                            raise Refused('unclassified canonical file')
                        existing_bytes = os.read(fd,1024*1024+1)
                        if len(existing_bytes) != info.st_size: raise Refused('canonical read incomplete')
                        existing = digest(existing_bytes)
                    finally: os.close(fd)
                if existing != base: raise Refused('base conflict')
                fd = os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600,dir_fd=parent)
                try:
                    remaining = memoryview(data)
                    while remaining:
                        written = os.write(fd,remaining)
                        if written <= 0: raise OSError('incomplete write')
                        remaining = remaining[written:]
                    os.fsync(fd)
                finally: os.close(fd)
                os.rename(temporary,parts[-1],src_dir_fd=parent,dst_dir_fd=parent)
                os.fsync(parent)
                if crash == 'after-replace': os._exit(87)
            finally: os.close(parent)
            result = {'path':relative,'sha256':digest(data),'owner':owner,'epoch':epoch,'token':token}
            with self.lock:
                self.db.execute("UPDATE ops SET state='COMMITTED',result=? WHERE owner=? AND token=?",(json.dumps(result),owner,token));self.db.commit()
            return result
        finally:
            with self.changed:
                self.pending[owner] -= 1
                self.changed.notify_all()

    def revoke(self, cap, epoch, timeout=1):
        with self.changed:
            owner,_,_ = self._owner(cap,epoch)
            self.db.execute('UPDATE owners SET active=0 WHERE id=?',(owner,));self.db.commit()
            deadline = time.monotonic()+timeout
            while self.pending.get(owner,0):
                remaining = deadline-time.monotonic()
                if remaining <= 0: break
                self.changed.wait(remaining)
            unresolved = self.db.execute("SELECT path FROM ops WHERE owner=? AND state IN ('INTENT','ISSUING')",(owner,)).fetchall()
            drained = self.pending.get(owner,0)==0 and not unresolved
            if drained: os.fsync(self.rootfd)
            return {'owner':owner,'epoch':epoch,'publicationAdmissionClosed':True,'drained':drained,
                    'pending':self.pending.get(owner,0),'unresolvedPaths':[row[0] for row in unresolved],
                    'scope':'BOUNDED_REGULAR_FILE_PUBLICATION_ONLY','handoffPermitted':False}
