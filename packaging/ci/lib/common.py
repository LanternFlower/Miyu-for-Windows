"""Strict JSON, deterministic hashing and output-directory contracts."""
import hashlib
import json
from pathlib import Path


class BlockedError(RuntimeError):
    """A required external environment is unavailable (CLI exit 3)."""


def canonical_json(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2,
                       allow_nan=False) + '\n').encode('utf-8')


def write_json(path, value):
    Path(path).write_bytes(canonical_json(value))


def load_json(path):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError(f'Duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(Path(path).read_text(encoding='utf-8'), object_pairs_hook=pairs)


def sha256_file(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def fresh_directory(path):
    path = Path(path).absolute()
    if path.is_symlink() or (path.exists() and (not path.is_dir() or any(path.iterdir()))):
        raise ValueError(f'Output directory must be empty: {path}')
    path.mkdir(parents=True, exist_ok=True)
    return path
