#!/usr/bin/env python3
"""Prepare verified, offline-consumable inputs from a frozen source snapshot."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tomllib

from lib.common import (BlockedError, fresh_directory, load_json, sha256_file,
                        write_json)
from lib.downloads import download_name, obtain, safe_extract
from lib.inputs import input_inventory, verify_prepared
from lib.manifest import digest, validate_manifest
from lib.source import safe_relative, snapshot_digest

LOCK_PATHS = {'cargo': 'Cargo.lock', 'toolchain': 'packaging/common/toolchain.lock.json',
              'third_party': 'packaging/common/third-party.lock.json',
              'assets': 'packaging/common/assets.json',
              'targets': 'packaging/common/targets.json'}
BUILD_IDS = {'gnu-x86_64', 'arch-x86_64', 'macos-arm64'}


def verify_source(manifest_path):
    """The manifest-adjacent export is the sole source, including lock validation."""
    manifest_path = Path(manifest_path).resolve()
    source = manifest_path.parent/'source'
    if source.is_symlink() or not source.is_dir():
        raise ValueError('Manifest-adjacent source snapshot is missing or is a link.')
    manifest = load_json(manifest_path)
    records = load_json(manifest_path.parent/'source-files.json')
    if not isinstance(records, list) or snapshot_digest(records) != manifest.get('source_snapshot_sha256'):
        raise ValueError('Source snapshot inventory digest differs from manifest.')
    paths = set()
    for record in records:
        relative = str(safe_relative(record['path']))
        if relative in paths:
            raise ValueError(f'Duplicate source snapshot path: {relative}')
        paths.add(relative)
        path = source/relative
        if not path.parent.resolve().is_relative_to(source):
            raise ValueError(f'Source snapshot parent escapes root: {relative}')
        info = path.lstat()
        if record['type'] == 'symlink':
            if (not stat.S_ISLNK(info.st_mode) or os.readlink(path) != record['target']
                    or not path.resolve().is_relative_to(source)
                    or hashlib.sha256(record['target'].encode()).hexdigest() != record['sha256']):
                raise ValueError(f'Source snapshot link changed: {relative}')
        elif record['type'] == 'file':
            if (not stat.S_ISREG(info.st_mode) or sha256_file(path) != record['sha256']
                    or info.st_size != record['size']
                    or ('100755' if info.st_mode & 0o111 else '100644') != record['mode']):
                raise ValueError(f'Source snapshot file changed: {relative}')
        else:
            raise ValueError(f'Unknown source snapshot type: {relative}')
    actual = {p.relative_to(source).as_posix() for p in source.rglob('*')
              if p.is_file() or p.is_symlink()}
    if actual != paths:
        raise ValueError('Source snapshot contains missing or undeclared files.')
    for name, relative in LOCK_PATHS.items():
        if sha256_file(source/relative) != manifest['locks'][name]:
            raise ValueError(f'Frozen source lock hash changed: {name}')
    validate_manifest(manifest, source/LOCK_PATHS['targets'])
    if load_json(source/LOCK_PATHS['toolchain']) != manifest['toolchain']:
        raise ValueError('Frozen toolchain contents differ from manifest.')
    return manifest, source


def validate_third_party(lock, manifest, source):
    if type(lock.get('schema_version')) is not int or lock['schema_version'] != 1:
        raise ValueError('Unsupported third-party lock schema.')
    if lock['wiki']['commit'] != manifest['wiki_commit']:
        raise ValueError('Wiki commit differs from frozen manifest.')
    digest(lock['wiki']['commit'], 40)
    ids = set()
    for record in lock['archives']:
        if (not re.fullmatch(r'[a-z0-9][a-z0-9-]*', record['id']) or record['id'] in ids
                or not record['url'].startswith('https://') or not record['license']
                or not record['version'] or record['format'] not in ('tar.gz', 'tar.bz2', 'file')
                or not record['targets'] or set(record['targets']) - BUILD_IDS):
            raise ValueError(f'Invalid or duplicate locked archive: {record.get("id")}')
        digest(record['sha256'])
        download_name(record)
        ids.add(record['id'])
    required = {'wiki'}
    if 'gnu-x86_64' in manifest['builds']:
        required.add('ort-gnu')
    if any('voice' in b['components'] for b in manifest['builds'].values()):
        required.update(('voice-kws', 'voice-sense', 'voice-vad', 'sherpa-onnx-license'))
        if set(manifest['builds']) & {'gnu-x86_64', 'arch-x86_64'}:
            required.add('sherpa-linux')
        if 'macos-arm64' in manifest['builds']:
            required.add('sherpa-macos')
    if required - ids:
        raise ValueError(f'Required locked archives are missing: {sorted(required - ids)}')
    cargo = tomllib.loads((source/'Cargo.lock').read_text())
    sherpa = next(p['version'] for p in cargo['package'] if p['name'] == 'sherpa-onnx-sys')
    for record in lock['archives']:
        if record['id'] in ('sherpa-linux', 'sherpa-macos') and record['version'] != sherpa:
            raise ValueError('Sherpa archive version differs from Cargo.lock.')
    for fixture in lock.get('fixtures', []):
        safe_relative(fixture['member'])
        digest(fixture['sha256'])
        if fixture['archive_id'] not in ids or type(fixture['size']) is not int or fixture['size'] <= 0:
            raise ValueError('Fixture references an unknown archive or invalid size.')


def unpack_root(archive, destination, scratch):
    safe_extract(archive, scratch)
    children = list(scratch.iterdir())
    if len(children) != 1 or not children[0].is_dir() or children[0].is_symlink():
        raise ValueError(f'Archive must contain one top-level directory: {archive}')
    children[0].rename(destination)
    scratch.rmdir()


def check_embedding(source):
    model = source/'assets/models/bge-small-zh-v1.5-int8'
    manifest = load_json(model/'manifest.json')
    for name, expected in manifest['sha256'].items():
        safe_relative(name)
        if sha256_file(model/name) != expected:
            raise ValueError(f'Embedding model manifest hash mismatch: {name}')


def prepare_resources(manifest, source, output, *, cache, offline):
    lock = load_json(source/LOCK_PATHS['third_party'])
    validate_third_party(lock, manifest, source)
    check_embedding(source)
    archives = output/'archives'
    archives.mkdir()
    if set(manifest['builds']) & {'gnu-x86_64', 'arch-x86_64'}:
        nfpm = dict(manifest['toolchain']['nfpm'], id='nfpm')
        tool_archive = obtain(nfpm, archives/download_name(nfpm), cache=cache, offline=offline)
        safe_extract(tool_archive, output/'tools/nfpm')
        if not (output/'tools/nfpm/nfpm').is_file():
            raise ValueError('Locked nFPM archive does not contain the packaging executable.')
    runtime = output/'runtime'
    runtime.mkdir()
    receipts = {}
    if set(manifest['builds']) & {'gnu-x86_64', 'arch-x86_64'}:
        receipts['nfpm'] = {'path': tool_archive.relative_to(output).as_posix(),
                            'sha256': nfpm['sha256'], 'url': nfpm['url']}
    for record in lock['archives']:
        if not set(record['targets']) & set(manifest['builds']):
            continue
        # Preserve sherpa's original basename for SHERPA_ONNX_ARCHIVE_DIR.
        name = download_name(record) if record['id'].startswith('sherpa-') and record['format'] != 'file' else record['id']+'-'+download_name(record)
        path = obtain(record, archives/name, cache=cache, offline=offline)
        receipts[record['id']] = {'path': path.relative_to(output).as_posix(),
                                  'sha256': record['sha256'], 'url': record['url']}
    def downloaded(id):
        return output/receipts[id]['path']

    unpack_root(downloaded('wiki'), output/'wiki', output/'.extract-wiki')
    for path in list((output/'wiki').rglob('.git')):
        if path.is_dir() and not path.is_symlink():
            shutil.rmtree(path)
        else:
            path.unlink()
    if not (output/'wiki/wiki').is_dir() or not (output/'wiki/LICENSE').is_file():
        raise ValueError('Wiki snapshot is missing wiki content or LICENSE.')
    wiki_lock = next(r for r in lock['archives'] if r['id'] == 'wiki')
    if wiki_lock['version'] != manifest['wiki_commit'] or manifest['wiki_commit'] not in wiki_lock['url']:
        raise ValueError('Wiki archive is not tied to the frozen commit.')
    kb = runtime/'default-kb/manifest'
    kb.mkdir(parents=True)
    write_json(kb/'manifest.json', {'name': 'miyu-default-kb', 'generated_by': 'miyu distribution prepare'})
    (kb/'shorinwiki.commit').write_text(manifest['wiki_commit']+'\n')
    if 'ort-gnu' in receipts:
        unpack_root(downloaded('ort-gnu'), runtime/'onnxruntime', output/'.extract-ort')
    licenses = runtime/'licenses'
    licenses.mkdir()
    for id, filename in [('sherpa-onnx-license', 'sherpa-onnx-LICENSE'),
                         ('silero-vad-license', 'silero-vad-LICENSE')]:
        if id in receipts:
            shutil.copyfile(downloaded(id), licenses/filename)
    fixtures = output/'voice-fixtures'
    models = fixtures/'models'
    models.mkdir(parents=True)
    members = {}
    for id in ('voice-kws', 'voice-sense'):
        if id not in receipts:
            continue
        temporary = output/('.extract-'+id)
        safe_extract(downloaded(id), temporary)
        children = list(temporary.iterdir())
        if len(children) != 1 or not children[0].is_dir():
            raise ValueError(f'Voice model archive has unexpected layout: {id}')
        target = models/children[0].name
        children[0].rename(target)
        temporary.rmdir()
        members[id] = target.parent
    if 'voice-vad' in receipts:
        shutil.copyfile(downloaded('voice-vad'), models/'silero_vad.onnx')
    fixture_receipts = []
    for fixture in lock.get('fixtures', []):
        if fixture['archive_id'] not in members:
            continue
        path = members[fixture['archive_id']]/fixture['member']
        if path.stat().st_size != fixture['size'] or sha256_file(path) != fixture['sha256']:
            raise ValueError(f'Voice fixture hash mismatch: {fixture["id"]}')
        if 'audio' in fixture:
            wavs = fixtures/'wavs'
            wavs.mkdir(exist_ok=True)
            wav = wavs/(fixture['archive_id']+'-'+Path(fixture['member']).name)
            shutil.copyfile(path, wav)
            fixture_receipts.append({'id': fixture['id'], 'path': wav.relative_to(output).as_posix(),
                                     'sha256': fixture['sha256']})
    return receipts, fixture_receipts


def reuse_vendor(source, output, cache):
    cache = Path(cache).resolve()
    prepared = load_json(cache/'prepared.json')
    if (type(prepared.get('schema_version')) is not int or prepared['schema_version'] != 1
            or prepared.get('vendor_complete') is not True or (cache/'vendor').is_symlink()
            or prepared.get('cargo_lock_sha256') != sha256_file(source/'Cargo.lock')):
        raise ValueError('Vendor cache is incomplete or its Cargo.lock differs.')
    records = [r for r in prepared['files'] if r['path'].startswith('vendor/')]
    expected = {r['path'] for r in records}
    actual = {p.relative_to(cache).as_posix() for p in (cache/'vendor').rglob('*')
              if p.is_file() or p.is_symlink()}
    if not records or actual != expected:
        raise ValueError('Vendor cache file inventory differs.')
    for record in records:
        safe_relative(record['path'])
        path = cache/record['path']
        if (path.is_symlink() or record['type'] != 'file' or sha256_file(path) != record['sha256']
                or path.stat().st_size != record['size']):
            raise ValueError(f'Vendor cache SHA256/type mismatch: {record["path"]}')
    config_record = next((r for r in prepared['files'] if r['path'] == 'cargo-config.toml'), None)
    if (config_record is None or (cache/'cargo-config.toml').is_symlink()
            or sha256_file(cache/'cargo-config.toml') != config_record['sha256']):
        raise ValueError('Vendor cache configuration SHA256 differs.')
    shutil.copytree(cache/'vendor', output/'vendor')
    for record in records:
        if sha256_file(output/record['path']) != record['sha256']:
            raise ValueError('Vendor cache changed during copy.')
    config = tomllib.loads((cache/'cargo-config.toml').read_text())
    config['source']['vendored-sources']['directory'] = str(output/'vendor')
    lines = []
    for name, values in sorted(config['source'].items()):
        lines.append('[source.'+json.dumps(name)+']')
        for key, value in sorted(values.items()):
            if not isinstance(value, str):
                raise ValueError('Unexpected vendor source configuration type.')
            lines.append(json.dumps(key)+' = '+json.dumps(value))
        lines.append('')
    (output/'cargo-config.toml').write_text('\n'.join(lines))
    (output/'cargo-vendor.log').write_text(f'Reused verified vendor cache: {cache}\n')


def prepare_vendor(source, output, *, offline, vendor_cache=None):
    if vendor_cache is not None:
        reuse_vendor(source, output, vendor_cache)
        check_vendor(source, output)
        return
    command = ['cargo', 'vendor', '--locked', '--versioned-dirs',
               '--manifest-path', str(source/'Cargo.toml')]
    if offline:
        command.append('--offline')
    command.append(str(output/'vendor'))
    try:
        completed = subprocess.run(command, check=True, cwd=source, capture_output=True,
                                   text=True, timeout=1200)
    except FileNotFoundError as error:
        raise BlockedError('Cargo is required for complete offline input preparation.') from error
    except subprocess.TimeoutExpired as error:
        raise BlockedError('Cargo vendor exceeded its 1200-second timeout.') from error
    except subprocess.CalledProcessError as error:
        (output/'cargo-vendor.log').write_text(error.stderr or '')
        if offline:
            raise BlockedError('Cargo offline dependency cache is incomplete. See cargo-vendor.log.') from error
        raise ValueError('Cargo vendor failed. See cargo-vendor.log.') from error
    (output/'cargo-vendor.log').write_text(completed.stderr)
    config = tomllib.loads(completed.stdout)
    if config.get('source', {}).get('crates-io', {}).get('replace-with') != 'vendored-sources':
        raise ValueError('Cargo vendor did not produce an offline source configuration.')
    (output/'cargo-config.toml').write_text(completed.stdout)
    check_vendor(source, output)


def check_vendor(source, output):
    # Require Cargo itself to resolve the exported manifest against only the vendor.
    subprocess.run(['cargo', 'metadata', '--locked', '--offline', '--format-version', '1',
                    '--manifest-path', str(source/'Cargo.toml'), '--config', str(output/'cargo-config.toml')],
                   cwd=source, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                   timeout=120)


def prepare(manifest_path, output, *, cache=None, offline=False, skip_vendor=False, vendor_cache=None):
    manifest_path = Path(manifest_path).resolve()
    manifest_sha256 = sha256_file(manifest_path)
    manifest, source = verify_source(manifest_path)
    output = Path(output).absolute()
    if output.is_symlink():
        raise ValueError('Prepared output cannot be a symbolic link.')
    output = output.resolve()
    if output.is_relative_to(source) or source.is_relative_to(output):
        raise ValueError('Prepared inputs and the frozen source must be separate directories.')
    output = fresh_directory(output)
    (output/'.miyu-owned-prepared-inputs').write_text(manifest_sha256+'\n')
    archives, fixtures = prepare_resources(manifest, source, output, cache=cache, offline=offline)
    if not skip_vendor:
        prepare_vendor(source, output, offline=offline, vendor_cache=vendor_cache)
    # Detect source edits racing preparation, including changes during Cargo vendor.
    verify_source(manifest_path)
    if sha256_file(manifest_path) != manifest_sha256:
        raise ValueError('Release manifest changed during preparation.')
    result = {'schema_version': 1, 'manifest_sha256': manifest_sha256,
              'source_commit': manifest['source_commit'],
              'source_snapshot_sha256': manifest['source_snapshot_sha256'],
              'wiki_commit': manifest['wiki_commit'], 'vendor_complete': not skip_vendor,
              'cargo_lock_sha256': manifest['locks']['cargo'],
              'vendor_directory': 'vendor' if not skip_vendor else None,
              'cargo_config': 'cargo-config.toml' if not skip_vendor else None,
              'archives': archives, 'fixtures': fixtures, 'files': input_inventory(output)}
    write_json(output/'prepared.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--cache', type=Path, help='Existing download cache, always rehashed.')
    parser.add_argument('--offline', action='store_true', help='Reject network and require complete caches.')
    parser.add_argument('--vendor-cache', type=Path, help='Previous complete inputs with the same Cargo.lock.')
    parser.add_argument('--skip-vendor', action='store_true', help='Resources only. Marks vendor_complete false.')
    args = parser.parse_args()
    try:
        result = prepare(args.manifest, args.out, cache=args.cache,
                         offline=args.offline, skip_vendor=args.skip_vendor, vendor_cache=args.vendor_cache)
        print(f'Prepared {len(result["files"])} verified inputs. Wiki {result["wiki_commit"]}. '
              f'Cargo vendor complete: {result["vendor_complete"]}.')
        return 0
    except BlockedError as error:
        print(f'BLOCKED: {error}', file=sys.stderr)
        return 3
    except (ValueError, KeyError, TypeError, OSError, tarfile.TarError, subprocess.SubprocessError) as error:
        print(f'ERROR: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
