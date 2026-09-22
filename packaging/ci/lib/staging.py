"""Resource selection and installation manifests. No network or product execution."""
import fnmatch
import os
from pathlib import Path

from .common import load_json, sha256_file
from .source import safe_relative


def validate_assets(catalog):
    if catalog.get('schema_version') != 1 or not catalog.get('assets'):
        raise ValueError('Unsupported or empty resource catalog.')
    seen = set()
    for rule in catalog['assets']:
        if rule['id'] in seen:
            raise ValueError(f'Duplicate resource ID: {rule["id"]}')
        seen.add(rule['id'])
        for key in ('source', 'destination'):
            safe_relative(rule[key])
        if (rule['source_root'] not in ('source', 'wiki', 'runtime')
                or rule['component'] not in ('core', 'voice')
                or rule['type'] not in ('file', 'tree')
                or rule['mode'] not in ('0644', '0755')):
            raise ValueError(f'Invalid resource rule: {rule["id"]}')
        for path in rule.get('required_files', []) + rule.get('indexes', []):
            safe_relative(path)
    return catalog


def selected_files(rule, roots):
    base = Path(roots[rule['source_root']])/rule['source']
    if not base.exists():
        raise ValueError(f'Missing resource {rule["id"]}: {base}')
    if rule['type'] == 'file':
        if not base.is_file() or base.stat().st_size == 0:
            raise ValueError(f'Resource must be a nonempty file: {base}')
        return [(base, Path(rule['destination']))]
    if not base.is_dir() or base.is_symlink():
        raise ValueError(f'Resource must be a directory: {base}')
    entries = []
    for path in sorted(base.rglob('*')):
        relative = path.relative_to(base)
        if set(relative.parts) & set(rule.get('exclude', [])):
            continue
        if path.is_dir() and not path.is_symlink():
            continue
        if not any(fnmatch.fnmatchcase(path.name, pattern) for pattern in rule.get('include', ['*'])):
            continue
        if not path.resolve().is_relative_to(base.resolve()):
            raise ValueError(f'Resource symlink escapes tree: {path}')
        if not path.is_file():
            raise ValueError(f'Resource is not a regular file: {path}')
        entries.append((path, Path(rule['destination'])/relative))
    if not entries:
        raise ValueError(f'Resource selection is empty: {rule["id"]}')
    included = {str(path.relative_to(base)) for path, _ in entries}
    for required in rule.get('required_files', []):
        if required not in included or (base/required).stat().st_size == 0:
            raise ValueError(f'Missing required resource: {rule["id"]}/{required}')
    for index_path in rule.get('indexes', []):
        index = load_json(base/index_path)
        for meme in index['memes']:
            safe_relative(meme['file'])
            relative = str(Path(index_path).parent/meme['file'])
            if relative not in included:
                raise ValueError(f'Meme index references missing image: {relative}')
    model_manifest = base/'manifest.json'
    if rule['id'] == 'embedding-model':
        model = load_json(model_manifest)
        for name, expected in model['sha256'].items():
            safe_relative(name)
            if sha256_file(base/name) != expected:
                raise ValueError(f'Model resource hash mismatch: {name}')
    return entries


def install_file(source, destination, mode):
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() or destination.is_symlink():
        raise ValueError(f'Duplicate staging destination: {destination}')
    if source.is_symlink():
        link = os.readlink(source)
        if Path(link).is_absolute() or '..' in Path(link).parts:
            raise ValueError(f'Unsafe staging link: {source}')
        destination.symlink_to(link)
    else:
        destination.write_bytes(source.read_bytes())
        destination.chmod(mode)


def tree_manifest(root):
    root = Path(root)
    result = []
    for path in sorted(root.rglob('*')):
        relative = str(path.relative_to(root))
        if path.is_symlink():
            if not path.resolve().is_relative_to(root.resolve()) or not path.exists():
                raise ValueError(f'Staged link is broken or escaping: {relative}')
            result.append({'path': relative, 'type': 'symlink', 'target': os.readlink(path),
                           'mode': '0777', 'size': path.lstat().st_size})
        elif path.is_dir():
            result.append({'path': relative, 'type': 'directory', 'mode': '0755', 'size': 0})
        elif path.is_file():
            result.append({'path': relative, 'type': 'file', 'mode': f'{path.stat().st_mode & 0o777:04o}',
                           'size': path.stat().st_size, 'sha256': sha256_file(path)})
        else:
            raise ValueError(f'Invalid staged file type: {relative}')
    return result
