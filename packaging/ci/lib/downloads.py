"""Locked downloads and archive extraction without filesystem escapes."""
import os
from pathlib import Path, PurePosixPath
import shutil
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

from .common import BlockedError, load_json, sha256_file


def download_name(record):
    name = urllib.parse.unquote(PurePosixPath(urllib.parse.urlsplit(record['url']).path).name)
    if not name or name in ('.', '..') or '/' in name or '\\' in name:
        raise ValueError('Download URL has an unsafe filename.')
    return name


def cache_candidate(record, cache):
    """Accept direct files or this task's download receipts, always confined to cache."""
    if cache is None:
        return None
    cache = Path(cache).resolve()
    candidates = [cache/download_name(record), cache/record['id']]
    receipt = cache/(record['id']+'.json')
    if receipt.is_file() and not receipt.is_symlink():
        value = load_json(receipt)
        if value.get('url') == record['url'] and 'path' in value:
            # Inventory receipts originally record worktree-relative paths.
            candidates.insert(0, cache/Path(value['path']).name)
    for candidate in candidates:
        if candidate.is_symlink():
            raise ValueError(f'Cache input must not be a symbolic link: {candidate}')
        if candidate.is_file():
            if not candidate.resolve().is_relative_to(cache):
                raise ValueError('Cache input escapes cache root.')
            return candidate
    return None


def obtain(record, destination, *, cache=None, offline=False):
    """Atomically materialize verified bytes. Corrupt cache never silently redownloads."""
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise ValueError(f'Download destination already exists: {destination}')
    source = cache_candidate(record, cache)
    if source is not None and sha256_file(source) != record['sha256']:
        raise ValueError(f'SHA256 mismatch for cached archive: {record["id"]}')
    if source is None and offline:
        raise BlockedError(f'Offline cache is missing {record["id"]}: {record["url"]}')
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.download-', dir=destination.parent)
    try:
        with os.fdopen(fd, 'wb') as output:
            if source is not None:
                with source.open('rb') as stream:
                    shutil.copyfileobj(stream, output)
            else:
                if urllib.parse.urlsplit(record['url']).scheme != 'https':
                    raise ValueError('Locked download URL must use HTTPS.')
                try:
                    with urllib.request.urlopen(record['url'], timeout=60) as stream:
                        if urllib.parse.urlsplit(stream.url).scheme != 'https':
                            raise ValueError('Download redirected away from HTTPS.')
                        deadline = time.monotonic() + 600
                        while chunk := stream.read(1024 * 1024):
                            if time.monotonic() > deadline:
                                raise TimeoutError('Download exceeded 600 seconds.')
                            output.write(chunk)
                except (urllib.error.URLError, TimeoutError) as error:
                    raise BlockedError(f'Download failed for {record["url"]}: {error}') from error
            output.flush()
            os.fsync(output.fileno())
        if sha256_file(temporary) != record['sha256']:
            raise ValueError(f'SHA256 mismatch for downloaded archive: {record["id"]}')
        os.chmod(temporary, 0o644)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)
    return destination


def _member_path(value):
    path = PurePosixPath(value)
    if (not value or path.is_absolute() or '..' in path.parts or '\\' in value
            or '\x00' in value):
        raise ValueError(f'Unsafe archive path: {value!r}')
    return path


def _link_target(member, path):
    value = member.linkname
    target = PurePosixPath(value)
    if not value or target.is_absolute() or '\\' in value:
        raise ValueError(f'Unsafe archive link: {member.name}')
    parts = list(path.parent.parts if member.issym() else ())
    for part in target.parts:
        if part == '..':
            if not parts:
                raise ValueError(f'Archive link escapes root: {member.name}')
            parts.pop()
        elif part != '.':
            parts.append(part)
    return PurePosixPath(*parts)


def safe_extract(archive_path, destination):
    """Preflight every member before creating anything. Allow only internal links."""
    destination = Path(destination)
    if destination.is_symlink() or (destination.exists() and any(destination.iterdir())):
        raise ValueError(f'Archive destination must be empty: {destination}')
    with tarfile.open(archive_path, 'r:*') as archive:
        entries = {}
        for member in archive.getmembers():
            path = _member_path(member.name)
            if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                raise ValueError(f'Unsupported archive member type: {member.name}')
            if path in entries:
                raise ValueError(f'Duplicate archive member: {member.name}')
            entries[path] = member
        def regular_target(path, visited=()):
            if path in visited or path not in entries:
                raise ValueError(f'Archive link is cyclic or dangling: {path}')
            member = entries[path]
            if member.isfile():
                return path
            if member.issym() or member.islnk():
                return regular_target(_link_target(member, path), (*visited, path))
            raise ValueError(f'Archive link does not name a regular member: {path}')

        for path, member in entries.items():
            for parent in path.parents:
                if parent in entries and not entries[parent].isdir():
                    raise ValueError(f'Archive parent is not a directory: {member.name}')
            if member.issym() or member.islnk():
                regular_target(path)
        destination.mkdir(parents=True, exist_ok=True)
        for path, member in entries.items():
            target = destination/str(path)
            target.parent.mkdir(parents=True, exist_ok=True)
            if member.isdir():
                target.mkdir(exist_ok=True)
                target.chmod(0o755)
            elif member.isfile():
                with archive.extractfile(member) as source, target.open('xb') as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
        for path, member in entries.items():
            target = destination/str(path)
            if member.issym():
                target.symlink_to(member.linkname)
            elif member.islnk():
                os.link(destination/str(regular_target(path)), target)
