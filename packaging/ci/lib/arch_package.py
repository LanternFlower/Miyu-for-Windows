"""Native makepkg packaging and shared resource installation for Arch source recipes."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import uuid

if __package__ in (None, ''):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from lib.common import load_json, sha256_file, write_json
    from lib.manifest import validate_manifest
    from lib.staging import install_file, selected_files, tree_manifest, validate_assets
else:
    from .common import load_json, sha256_file, write_json
    from .manifest import validate_manifest
    from .staging import install_file, selected_files, tree_manifest, validate_assets

OWNER = 'io.miyu.distribution.owner=distribution-2026-09-14'
CORE_DEPENDS = ('alsa-lib', 'chafa', 'gcc-libs', 'glibc', 'onnxruntime', 'python', 'ripgrep')


def recipe(version, revision, component):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version) or type(revision) is not int or revision < 1:
        raise ValueError('Invalid Arch package version/revision.')
    if component not in ('core', 'voice'):
        raise ValueError('Invalid Arch package component.')
    voice = component == 'voice'
    name = 'miyu-voice' if voice else 'miyu'
    depends = (f'miyu={version}-{revision}', 'alsa-lib', 'bzip2', 'gcc-libs', 'glibc') if voice else CORE_DEPENDS
    dependencies = ' '.join("'" + value + "'" for value in depends)
    description = 'Voice wake word and speech recognition front end' if voice else 'Terminal AI assistant'
    licenses = "'MIT' 'Apache-2.0'" if voice else "'MIT' 'OFL-1.1'"
    return f'''# Generated from validated release input. No build-time downloads.
pkgname={name}
pkgver={version}
pkgrel={revision}
pkgdesc='{description}'
arch=('x86_64')
url='https://github.com/SHORiN-KiWATA/miyu-agent'
license=({licenses})
depends=({dependencies})
options=('!strip' '!debug' '!lto' '!purge' 'staticlibs' 'libtool' '!zipman' 'emptydirs')
source=()
sha256sums=()
package() {{
    install -d "${{pkgdir}}/usr"
    cp -a /stage/. "${{pkgdir}}/usr/"
}}
'''


def package_arch(manifest, asset, stage: Path, inventory: list, destination: Path, builder_image: str):
    validate_manifest(manifest)
    if (asset not in manifest['assets'] or asset['build_id'] != 'arch-x86_64'
            or asset['format'] != 'archlinux' or asset['arch'] != 'x86_64'):
        raise ValueError('Asset is not a declared native Arch package.')
    stage = Path(stage).resolve(strict=True)
    destination = Path(destination).absolute()
    if destination.name != asset['filename'] or destination.exists() or destination.is_symlink():
        raise ValueError('Destination must be a new declared asset filename.')
    if any(',' in str(path) or '\n' in str(path) for path in (stage, destination.parent)):
        raise ValueError('Docker mount paths cannot contain commas or newlines.')
    if tree_manifest(stage) != inventory:
        raise ValueError('Staging inventory changed before Arch packaging.')
    if any('libonnxruntime' in Path(entry['path']).name for entry in inventory):
        raise ValueError('Arch packages must use the system onnxruntime provider.')
    inspect = subprocess.run(['docker', 'image', 'inspect', builder_image], check=True,
        capture_output=True, text=True, timeout=30)
    image = json.loads(inspect.stdout)[0]
    if (image['Architecture'] != 'amd64'
            or image.get('Config', {}).get('Labels', {}).get('io.miyu.distribution.owner') != 'distribution-2026-09-14'):
        raise ValueError('Expected the owned native x86_64 Arch builder image.')
    image_id = image['Id']
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise ValueError('Builder image did not resolve to an immutable ID.')
    destination.parent.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex
    name = 'miyu-arch-package-' + run_id
    log_path = destination.parent / 'arch-package.log'
    with tempfile.TemporaryDirectory(prefix='miyu-arch-', dir=destination.parent) as temporary:
        work = Path(temporary)
        write_json(work/'.distribution-owner.json', {'run_id': run_id, 'pid': os.getpid()})
        (work/'PKGBUILD').write_text(recipe(manifest['version'], manifest['package_revision'], asset['component']))
        uid, gid = (os.getuid(), os.getgid()) if os.getuid() else (1000, 1000)
        if os.getuid() == 0:
            os.chown(work, uid, gid)
        command = ['docker', 'run', '--rm', '--name', name, '--label', OWNER,
            '--network', 'none', '--user', f'{uid}:{gid}', '--workdir', '/build',
            '--env', 'HOME=/build', '--env', 'LC_ALL=C.UTF-8',
            '--env', 'PACKAGER=Miyu Release <shorin@users.noreply.github.com>',
            '--env', f'SOURCE_DATE_EPOCH={manifest["source_date_epoch"]}',
            '--mount', f'type=bind,src={stage},dst=/stage,readonly',
            '--mount', f'type=bind,src={work},dst=/build', image_id,
            'bash', '-euc',
            'cp /etc/makepkg.conf /build/makepkg.conf\n'
            'printf "\\nCOMPRESSZST=(zstd -c -T2 -10 -)\\n" >> /build/makepkg.conf\n'
            'makepkg --nodeps --holdver --noconfirm --config /build/makepkg.conf\n'
            'namcap /build/*.pkg.tar.zst > /build/namcap.log 2>&1 || echo "$?" > /build/namcap.exit\n'
            'bsdtar -xOf /build/*.pkg.tar.zst .PKGINFO > /build/package-info.txt\n']
        try:
            with log_path.open('wb') as log:
                subprocess.run(command, check=True, stdout=log, stderr=subprocess.STDOUT, timeout=900)
            package = work / asset['filename']
            if not package.is_file() or package.is_symlink():
                raise ValueError('makepkg did not create the declared package.')
            info = (work/'package-info.txt').read_text()
            wanted = f'pkgver = {manifest["version"]}-{manifest["package_revision"]}'
            component = asset['component']
            expected_name = 'miyu-voice' if component == 'voice' else 'miyu'
            dependencies = ([f'miyu={manifest["version"]}-{manifest["package_revision"]}', 'alsa-lib', 'bzip2', 'gcc-libs', 'glibc']
                if component == 'voice' else list(CORE_DEPENDS))
            actual_dependencies = [line.removeprefix('depend = ') for line in info.splitlines() if line.startswith('depend = ')]
            if (wanted not in info.splitlines() or 'arch = x86_64' not in info.splitlines()
                    or f'pkgname = {expected_name}' not in info.splitlines() or actual_dependencies != dependencies):
                raise ValueError('makepkg metadata differs from release input.')
            shutil.copyfile(work/'namcap.log', destination.parent/'namcap.log')
            shutil.copyfile(work/'package-info.txt', destination.parent/'package-info.txt')
            shutil.copyfile(package, destination)
            namcap_text = (work/'namcap.log').read_text(errors='replace')
            write_json(destination.parent/'arch-package-record.json', {
                'schema_version': 1, 'builder_image': image_id, 'command': command,
                'non_root_uid': uid, 'sha256': sha256_file(destination),
                'source_commit': manifest['source_commit'], 'source_snapshot_sha256': manifest['source_snapshot_sha256'],
                'namcap_exit_code': int((work/'namcap.exit').read_text()) if (work/'namcap.exit').exists() else 0,
                'namcap_errors': namcap_text.count(' E: '), 'namcap_warnings': namcap_text.count(' W: ')})
            if ' E: ' in namcap_text:
                raise ValueError('namcap found package errors. See namcap.log.')
        finally:
            subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, timeout=30)
    return destination


def checkout_commit(root):
    top = subprocess.run(['git', '-C', str(root), 'rev-parse', '--show-toplevel'],
        check=True, capture_output=True, text=True, timeout=30).stdout.strip()
    if Path(top).resolve() != root.resolve():
        raise ValueError('Expected a standalone source checkout, not its parent repository.')
    return subprocess.run(['git', '-C', str(root), 'rev-parse', 'HEAD'], check=True,
        capture_output=True, text=True, timeout=30).stdout.strip()


def source_install(source, wiki, component, destination, wiki_commit, sherpa_license=None):
    """Use the same assets.json for VCS and tag recipes. Never download during package()."""
    source, wiki, destination = Path(source).resolve(), Path(wiki).resolve(), Path(destination)
    actual_wiki = checkout_commit(wiki)
    if not re.fullmatch(r'[0-9a-f]{40}', wiki_commit) or actual_wiki != wiki_commit:
        raise ValueError('Wiki checkout does not match the declared revision.')
    source_commit = checkout_commit(source)
    catalog = validate_assets(load_json(source/'packaging/common/assets.json'))
    if destination.exists() and any(destination.iterdir()):
        raise ValueError('Resource installation destination must be empty.')
    with tempfile.TemporaryDirectory(prefix='miyu-source-assets-') as temporary:
        runtime = Path(temporary)
        meta = runtime/'default-kb/manifest'
        meta.mkdir(parents=True)
        write_json(meta/'manifest.json', {'name': 'miyu-default-kb', 'source_commit': source_commit,
            'wiki_commit': wiki_commit, 'generated_by': 'packaging/common/assets.json'})
        (meta/'shorinwiki.commit').write_text(wiki_commit+'\n')
        if sherpa_license:
            install_file(Path(sherpa_license), runtime/'licenses/sherpa-onnx-LICENSE', 0o644)
        roots = {'source': source, 'wiki': wiki, 'runtime': runtime}
        binary = 'miyu-voice' if component == 'voice' else 'miyu'
        install_file(source/'target/x86_64-unknown-linux-gnu/release'/binary, destination/'bin'/binary, 0o755)
        if component == 'core':
            (destination/'bin/miyupm').symlink_to('miyu')
        for rule in catalog['assets']:
            if rule['component'] != component or 'arch-x86_64' not in rule.get('build_ids', ['arch-x86_64']):
                continue
            for source_file, relative in selected_files(rule, roots):
                install_file(source_file, destination/relative, int(rule['mode'], 8))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('source-install',))
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--wiki', required=True, type=Path)
    parser.add_argument('--wiki-commit', required=True)
    parser.add_argument('--component', required=True, choices=('core', 'voice'))
    parser.add_argument('--destination', required=True, type=Path)
    parser.add_argument('--sherpa-license', type=Path)
    args = parser.parse_args()
    source_install(args.source, args.wiki, args.component, args.destination, args.wiki_commit, args.sherpa_license)


if __name__ == '__main__':
    main()
