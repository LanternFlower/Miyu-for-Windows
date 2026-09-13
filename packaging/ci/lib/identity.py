"""Content-derived build identity without manifest/output hash cycles."""
import hashlib
from .common import canonical_json


def build_identity(manifest, build_id, component):
    build = manifest['builds'][build_id]
    if component not in build['components']:
        raise ValueError('Component is not declared for this build.')
    inputs = {key: manifest[key] for key in ('source_commit', 'source_snapshot_sha256',
        'wiki_commit', 'locks', 'version', 'package_revision', 'toolchain')}
    inputs.update(build_id=build_id, component=component, target=build['target'],
                  features=build['features'][component])
    return hashlib.sha256(canonical_json(inputs)).hexdigest()
