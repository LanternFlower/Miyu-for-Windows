"""Inventory and verify prepared bytes against a frozen release input."""
import hashlib
import os
from pathlib import Path
import stat

from .common import canonical_json, load_json, sha256_file
from .source import safe_relative


def input_inventory(output):
    result = []
    output = Path(output)
    for path in sorted(output.rglob('*')):
        relative = path.relative_to(output).as_posix()
        if relative == 'prepared.json':
            continue
        if path.is_symlink():
            if not path.resolve().is_relative_to(output.resolve()):
                raise ValueError(f'Prepared link escapes input root: {relative}')
            result.append({'path': relative, 'type': 'symlink', 'target': os.readlink(path)})
        elif path.is_file():
            result.append({'path': relative, 'type': 'file', 'mode': f'{stat.S_IMODE(path.stat().st_mode):04o}',
                           'size': path.stat().st_size, 'sha256': sha256_file(path)})
        elif not path.is_dir():
            raise ValueError(f'Unsupported prepared input type: {relative}')
    return result


def verify_prepared(inputs, manifest, manifest_path=None, *, require_vendor=False):
    inputs = Path(inputs).absolute()
    if inputs.is_symlink() or not inputs.is_dir() or (inputs/'prepared.json').is_symlink():
        raise ValueError('Prepared inputs must be a regular directory with a regular manifest.')
    prepared = load_json(inputs/'prepared.json')
    if type(prepared.get('schema_version')) is not int or prepared['schema_version'] != 1:
        raise ValueError('Unsupported prepared-input schema version.')
    expected_manifest_sha = (sha256_file(manifest_path) if manifest_path is not None
                             else hashlib.sha256(canonical_json(manifest)).hexdigest())
    if prepared.get('manifest_sha256') != expected_manifest_sha:
        raise ValueError('Prepared inputs belong to a different release manifest.')
    for field in ('source_commit', 'source_snapshot_sha256', 'wiki_commit'):
        if prepared.get(field) != manifest[field]:
            raise ValueError(f'Prepared inputs differ from release source: {field}')
    if prepared.get('cargo_lock_sha256') != manifest['locks']['cargo']:
        raise ValueError('Prepared Cargo.lock differs from release input.')
    if type(prepared.get('vendor_complete')) is not bool:
        raise ValueError('Prepared vendor completeness must be explicit.')
    if require_vendor and prepared['vendor_complete'] is not True:
        raise ValueError('Complete Cargo vendor inputs are required for this operation.')
    paths = set()
    for record in prepared['files']:
        safe_relative(record['path'])
        if record['path'] in paths or record['path'] == 'prepared.json':
            raise ValueError('Duplicate or self-referencing prepared inventory entry.')
        paths.add(record['path'])
    if prepared['files'] != input_inventory(inputs):
        raise ValueError('Prepared inputs have missing, extra or changed files, links or modes.')
    if prepared['vendor_complete'] and (
            prepared.get('vendor_directory') != 'vendor' or prepared.get('cargo_config') != 'cargo-config.toml'
            or not (inputs/'vendor').is_dir() or not (inputs/'cargo-config.toml').is_file()):
        raise ValueError('Complete vendor inputs are missing their directory or configuration.')
    return prepared
