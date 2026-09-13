"""Frozen profile and asset mapping shared by metadata and release validation."""
from .common import load_json

TARGET_IDS = ('arch-x86_64', 'debian13-x86_64', 'ubuntu2510-x86_64',
              'ubuntu2604-x86_64', 'fedora-current-x86_64', 'macos-arm64')
PROFILES = ('preview-core', 'stable-core', 'stable-full', 'linux-smoke')
CORE_CHECKS = ('artifact-identity', 'package-install', 'assets-complete', 'renderer-live',
               'embedding-live', 'embedding-degrade', 'daemon-lifecycle', 'sandbox-contract',
               'service-lifecycle', 'upgrade-uninstall')
VOICE_CHECKS = ('artifact-identity', 'package-install', 'voice-offline', 'voice-hardware',
                'upgrade-uninstall')


def selected_targets(profile, selection):
    if profile not in PROFILES:
        raise ValueError(f'Unknown profile: {profile}')
    if selection is not None and profile != 'preview-core':
        raise ValueError('Stable profiles cannot reduce the target matrix.')
    defaults = tuple(t for t in TARGET_IDS if t != 'macos-arm64') if profile == 'linux-smoke' else TARGET_IDS
    selected = defaults if selection is None else tuple(selection)
    if not selected or len(set(selected)) != len(selected) or set(selected) - set(TARGET_IDS):
        raise ValueError('Target selection is empty, duplicated or unknown.')
    return [target for target in TARGET_IDS if target in selected]


def filename(asset_id, version, revision, fedora):
    component = 'miyu-voice' if asset_id.endswith('-voice') else 'miyu'
    stem = f'{component}-{version}-{revision}'
    family = asset_id.rsplit('-', 1)[0]
    if family == 'arch':
        return f'{stem}-x86_64.pkg.tar.zst'
    if family == 'deb':
        return f'{component}_{version}-{revision}_amd64.deb'
    if family == 'rpm':
        return f'{stem}.fc{fedora}.x86_64.rpm'
    triple = {'gnu': 'x86_64-unknown-linux-gnu', 'macos': 'aarch64-apple-darwin'}[family]
    return f'{stem}-{triple}.tar.gz'


def release_matrix(catalog_path, profile, selected, version, revision, fedora):
    catalog = load_json(catalog_path)
    if catalog['schema_version'] != 1 or set(catalog['targets']) != set(TARGET_IDS):
        raise ValueError('Target catalog does not match the supported matrix.')
    assets, builds, checks = {}, {}, []
    for target in selected:
        config = catalog['targets'][target]
        build_id = config['build_id']
        components = ['core']
        if profile in ('stable-full', 'linux-smoke') or (profile == 'stable-core' and target != 'macos-arm64'):
            components.append('voice')
        builds.setdefault(build_id, dict(catalog['builds'][build_id], components=[], features={}))
        for component in components:
            if component not in builds[build_id]['components']:
                builds[build_id]['components'].append(component)
                builds[build_id]['features'][component] = ['voice'] if component == 'voice' else []
            asset_id = config[component]
            assets[asset_id] = {'id': asset_id, 'filename': filename(asset_id, version, revision, fedora),
                'build_id': build_id, 'component': component, 'format': config['format'], 'arch': config['arch']}
            required = list(CORE_CHECKS if component == 'core' else VOICE_CHECKS)
            if profile == 'preview-core':
                required.remove('service-lifecycle')
            elif profile == 'linux-smoke':
                required = ['artifact-identity', 'package-install']
                if component == 'core':
                    required += ['assets-complete', 'provider-live']
            if target == 'macos-arm64':
                if component == 'core':
                    required.append('platform-interaction')
                if profile != 'preview-core':
                    required.append('signed-download')
            checks.extend({'asset_id': asset_id, 'target': target, 'check': check, 'required': True}
                          for check in required)
    # GNU tar is a first-class delivery artifact. It receives its own installed probes.
    if 'gnu-x86_64' in builds:
        for component in builds['gnu-x86_64']['components']:
            asset_id = f'gnu-{component}'
            assets[asset_id] = {'id': asset_id, 'filename': filename(asset_id, version, revision, fedora),
                'build_id': 'gnu-x86_64', 'component': component, 'format': 'tar.gz', 'arch': 'x86_64'}
            target = next(t for t in selected if catalog['targets'][t]['build_id'] == 'gnu-x86_64')
            required = list(CORE_CHECKS if component == 'core' else VOICE_CHECKS)
            if profile == 'preview-core':
                required.remove('service-lifecycle')
            elif profile == 'linux-smoke':
                required = ['artifact-identity', 'package-install']
                if component == 'core':
                    required += ['assets-complete', 'provider-live']
            checks.extend({'asset_id': asset_id, 'target': target, 'check': check, 'required': True}
                          for check in required)
    return dict(sorted(builds.items())), [assets[k] for k in sorted(assets)], checks
