#!/usr/bin/env python3
"""Stage declared binaries and complete package resources without executing Miyu."""
import argparse
from pathlib import Path
import re
import sys

from lib.common import fresh_directory, load_json, sha256_file, write_json
from lib.identity import build_identity
from lib.inputs import verify_prepared
from prepare import verify_source
from lib.staging import install_file, selected_files, tree_manifest, validate_assets


BUILD_EVIDENCE_FIELDS = ('build_id', 'component', 'build_identity', 'binary_sha256',
    'builder_image', 'source_commit', 'source_snapshot_sha256', 'release_input_sha256',
    'rustc', 'offline', 'command')


def validate_build_evidence(record, manifest, input_hash, build_id, component, binary_hash):
    """Validate and select portable evidence. Host Docker mounts never enter release assets."""
    if not isinstance(record, dict) or any(key not in record for key in BUILD_EVIDENCE_FIELDS):
        raise ValueError('Required build evidence is missing.')
    evidence = {key: record[key] for key in BUILD_EVIDENCE_FIELDS}
    expected = {'build_id': build_id, 'component': component,
        'build_identity': build_identity(manifest, build_id, component),
        'binary_sha256': binary_hash, 'source_commit': manifest['source_commit'],
        'source_snapshot_sha256': manifest['source_snapshot_sha256'],
        'release_input_sha256': input_hash}
    if any(evidence[key] != value for key, value in expected.items()):
        raise ValueError(f'Build evidence differs from frozen input or binary: {build_id}/{component}')
    if (not isinstance(evidence['builder_image'], str)
            or not re.fullmatch(r'sha256:[0-9a-f]{64}', evidence['builder_image'])
            or not isinstance(binary_hash, str) or not re.fullmatch(r'[0-9a-f]{64}', binary_hash)):
        raise ValueError('Build evidence requires immutable image and binary SHA256 digests.')
    command = ['cargo', 'build', '--release', '--frozen', '--target',
        manifest['builds'][build_id]['target'], '--bin', 'miyu-voice' if component == 'voice' else 'miyu',
        '--config', 'source.crates-io.replace-with="vendored-sources"',
        '--config', 'source.vendored-sources.directory="/inputs/vendor"']
    features = manifest['builds'][build_id]['features'][component]
    if features:
        command += ['--features', ','.join(features)]
    if (evidence['offline'] is not True or evidence['command'] != command
            or not isinstance(evidence['rustc'], str)
            or not evidence['rustc'].startswith('rustc '+manifest['toolchain']['rust']+' ')):
        raise ValueError('Build evidence differs from the frozen compiler or offline command.')
    return evidence


def payload_binary_hash(inventory, component):
    binary = 'bin/'+('miyu-voice' if component == 'voice' else 'miyu')
    entries = [entry for entry in inventory if entry['path'] == binary]
    if len(entries) != 1 or entries[0]['type'] != 'file' or entries[0].get('size', 0) <= 0:
        raise ValueError('Package binary payload is missing or duplicated.')
    return entries[0].get('sha256')


def stage(manifest_path, inputs, build_id, build_root, destination):
    manifest, source = verify_source(manifest_path)
    verify_prepared(inputs,manifest,manifest_path)
    if build_id not in manifest['builds']:
        raise ValueError('Build ID is not declared in the release input.')
    catalog_path = source/'packaging/common/assets.json'
    if sha256_file(catalog_path) != manifest['locks']['assets']:
        raise ValueError('Resource catalog differs from the frozen input.')
    catalog = validate_assets(load_json(catalog_path))
    destination = fresh_directory(destination)
    roots = {'source': source, 'wiki': inputs/'wiki', 'runtime': inputs/'runtime'}
    components = manifest['builds'][build_id]['components']
    evidence = {}
    for component in components:
        record = load_json(build_root/component/'build-record.json')
        binary = build_root/component/('miyu-voice' if component == 'voice' else 'miyu')
        evidence[component] = validate_build_evidence(record, manifest, sha256_file(manifest_path),
            build_id, component, sha256_file(binary))
        install_file(binary, destination/component/'bin'/binary.name, 0o755)
        if component == 'core':
            (destination/component/'bin/miyupm').symlink_to('miyu')
    for rule in catalog['assets']:
        if rule['component'] not in components or build_id not in rule.get('build_ids', [build_id]):
            continue
        for source_file, relative in selected_files(rule, roots):
            install_file(source_file, destination/rule['component']/relative, int(rule['mode'], 8))
    result = {'schema_version': 1, 'build_id': build_id,
        'release_input_sha256': sha256_file(manifest_path),
        'source_snapshot_sha256': manifest['source_snapshot_sha256'],
        'build_evidence': evidence,
        'components': {component: tree_manifest(destination/component) for component in components}}
    write_json(destination/'stage-manifest.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--inputs', required=True, type=Path)
    parser.add_argument('--build-id', required=True)
    parser.add_argument('--build-root', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    try:
        result = stage(args.manifest, args.inputs, args.build_id, args.build_root, args.out)
        print(f'Staged {args.build_id}: '+', '.join(result['components']))
        return 0
    except (ValueError, KeyError, OSError) as error:
        print(f'ERROR: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
