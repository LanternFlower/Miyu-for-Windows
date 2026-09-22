"""Canonical current-source export. Never stage or commit the user's tree."""
import hashlib
import os
from pathlib import Path, PurePosixPath
import stat
import subprocess

from .common import canonical_json, fresh_directory, sha256_file, write_json


def git(repo, *argv):
    return subprocess.run(['git', '-C', str(repo), *argv], check=True,
        capture_output=True, timeout=60).stdout.decode('utf-8').strip()


def safe_relative(value):
    path = PurePosixPath(value)
    if (not value or path.is_absolute() or str(path) != value
            or any(part in ('..', '.git') for part in path.parts)
            or '\\' in value or '\n' in value or '\r' in value):
        raise ValueError(f'Unsafe relative path: {value!r}')
    return path


def source_files(repo, include_new=()):
    repo = Path(repo).resolve()
    tracked = set(git(repo, 'ls-files', '-z').split('\0')) - {''}
    for value in include_new:
        safe_relative(value)
        if value not in tracked:
            ignored = subprocess.run(['git', '-C', str(repo), 'check-ignore', '-q', '--', value],
                                     timeout=30).returncode
            if ignored == 0:
                raise ValueError(f'Cannot include ignored source: {value}')
            if ignored != 1:
                raise ValueError(f'Cannot inspect source ignore state: {value}')
        tracked.add(value)
    result = []
    for value in sorted(tracked):
        safe_relative(value)
        path = repo/value
        if not path.exists() and not path.is_symlink():
            if value in include_new:
                raise ValueError(f'Explicit source is missing: {value}')
            continue  # A tracked deletion is part of the current preview snapshot.
        if not path.parent.resolve().is_relative_to(repo):
            raise ValueError(f'Source parent escapes repository: {value}')
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            target = os.readlink(path)
            if Path(target).is_absolute() or not path.resolve().is_relative_to(repo):
                raise ValueError(f'Source link escapes repository: {value}')
            result.append({'path': value, 'mode': '120000', 'type': 'symlink', 'target': target,
                           'sha256': hashlib.sha256(target.encode()).hexdigest()})
        elif stat.S_ISREG(info.st_mode):
            result.append({'path': value, 'mode': '100755' if info.st_mode & 0o111 else '100644',
                           'type': 'file', 'size': info.st_size, 'sha256': sha256_file(path)})
        else:
            raise ValueError(f'Unsupported source entry: {value}')
    return result


def snapshot_digest(records):
    return hashlib.sha256(canonical_json(records)).hexdigest()


def export_source(repo, destination, records):
    destination = fresh_directory(destination)
    for record in records:
        source = Path(repo)/record['path']
        target = destination/record['path']
        target.parent.mkdir(parents=True, exist_ok=True)
        if record['type'] == 'symlink':
            target.symlink_to(record['target'])
        else:
            # Copy the exact bytes checked here. A concurrent source edit must fail.
            content = source.read_bytes()
            if hashlib.sha256(content).hexdigest() != record['sha256']:
                raise ValueError(f'Source changed during export: {record["path"]}')
            target.write_bytes(content)
            target.chmod(int(record['mode'][-3:], 8))
    write_json(destination.parent/'source-files.json', records)
    return snapshot_digest(records)
