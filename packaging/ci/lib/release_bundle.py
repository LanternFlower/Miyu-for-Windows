"""Final publication allowlist and integrity checks."""
from pathlib import Path
import re

from .common import canonical_json, load_json, sha256_file
from .source import safe_relative


def output_name(manifest):
    return f'release-manifest-{manifest["version"]}-{manifest["package_revision"]}.json'


def sums_name(manifest):
    return f'SHA256SUMS-{manifest["version"]}-{manifest["package_revision"]}'


def bundle_checksums(directory, filenames):
    return ''.join(f'{sha256_file(Path(directory)/name)}  {name}\n' for name in sorted(filenames))


def verify_bundle(manifest,directory):
    directory=Path(directory)
    output=load_json(directory/output_name(manifest))
    if (output.get('schema_version')!=1 or output['source_commit']!=manifest['source_commit']
            or output['source_snapshot_sha256']!=manifest['source_snapshot_sha256']
            or output['version']!=manifest['version'] or output['package_revision']!=manifest['package_revision']
            or output['profile']!=manifest['profile']):
        raise ValueError('Release output identity differs from frozen input.')
    names=set()
    for record in output['files']:
        name=record['filename']
        safe_relative(name)
        if '/' in name or not re.fullmatch(r'[A-Za-z0-9_.+-]+',name) or name in names:
            raise ValueError('Release allowlist contains an unsafe or duplicate filename.')
        names.add(name)
        path=directory/name
        if path.is_symlink() or sha256_file(path)!=record['sha256']:
            raise ValueError(f'Final publication hash mismatch: {name}')
    inputs=[record for record in output['files'] if record['kind']=='input']
    input_name=f'release-input-{manifest["version"]}-{manifest["package_revision"]}.json'
    if (len(inputs)!=1 or inputs[0]['filename']!=input_name
            or inputs[0]['sha256']!=output.get('release_input_sha256')
            or canonical_json(load_json(directory/input_name))!=canonical_json(manifest)):
        raise ValueError('Publication bundle differs from the complete frozen input.')
    expected_assets={a['filename'] for a in manifest['assets']}
    actual_assets={r['filename'] for r in output['files'] if r['kind']=='package'}
    if expected_assets!=actual_assets:
        raise ValueError('Publication packages differ from the required asset matrix.')
    asset_hashes={r['asset_id']:r['sha256'] for r in output['files'] if r['kind']=='package'}
    expected_checks={(c['asset_id'],c['target'],c['check']) for c in manifest['checks'] if c['required']}
    checks=output['checks']
    actual_checks={(c['asset_id'],c['target'],c['check']) for c in checks}
    if actual_checks!=expected_checks or len(checks)!=len(actual_checks):
        raise ValueError('Publish bundle has incomplete or duplicate acceptance evidence.')
    for check in checks:
        if check['status']!='PASS' or check['artifact_sha256']!=asset_hashes[check['asset_id']]:
            raise ValueError('Publish bundle contains failed/stale acceptance evidence.')
    names.add(output_name(manifest))
    if (directory/sums_name(manifest)).read_text()!=bundle_checksums(directory,names):
        raise ValueError('Final checksum manifest differs from publish files.')
    names.add(sums_name(manifest))
    if {p.name for p in directory.iterdir()}!=names:
        raise ValueError('Publish directory contains missing or undeclared files.')
    return output
