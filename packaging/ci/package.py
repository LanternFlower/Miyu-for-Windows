#!/usr/bin/env python3
"""Package a verified staging tree. Final package hashes are separate from release input."""
import argparse
import gzip
import json
from pathlib import Path
import subprocess
import sys
import tarfile

from lib.common import BlockedError, fresh_directory, load_json, sha256_file, write_json
from lib.manifest import read_manifest
from lib.staging import tree_manifest
from stage import payload_binary_hash, validate_build_evidence


def tar_package(stage, destination, epoch):
    with destination.open('wb') as raw, gzip.GzipFile(filename='',mode='wb',fileobj=raw,mtime=epoch) as zipped:
        with tarfile.open(fileobj=zipped,mode='w',format=tarfile.PAX_FORMAT) as archive:
            for path in sorted(stage.rglob('*')):
                info = archive.gettarinfo(str(path),str(path.relative_to(stage)))
                info.uid=info.gid=0
                info.uname=info.gname='root'
                info.mtime=epoch
                if info.isfile():
                    with path.open('rb') as content:
                        archive.addfile(info,content)
                else:
                    archive.addfile(info)


def package_inventory(asset, inventory):
    # Fedora's filesystem package owns these directories with mode 0555.
    # Packages own their private subdirectories and files, not shared roots.
    shared={'bin','lib','share','share/licenses'}
    return [entry for entry in inventory if not (asset['format']=='rpm'
        and entry['type']=='directory' and entry['path'] in shared)]


def nfpm_config(manifest, asset, stage, inventory):
    family = 'deb' if asset['format']=='deb' else 'fedora'
    config=load_json(Path(__file__).resolve().parents[1]/'linux'/f'nfpm-{family}.yaml')
    # glibc 下限 = 构建基座的 glibc（`toolchain.lock.json` 的
    # `builders.gnu-x86_64.glibc`）。在更新的发行版上构建出的二进制会引用更高
    # 版本的符号，装到更老的系统上直接起不来，所以这两件事必须是同一个数——
    # 2026-09-20 之前它在 nfpm 的两份 yaml 和这里各写了一遍。
    glibc=manifest['builders']['gnu-x86_64']['glibc']
    libc=f'libc6 (>= {glibc})' if family=='deb' else f'glibc >= {glibc}'
    voice=asset['component']=='voice'
    name='miyu-voice' if voice else 'miyu'
    release=str(manifest['package_revision'])
    if family=='fedora':
        release+=f'.fc{manifest["fedora_version"]}'
    config.update(name=name,version=manifest['version'],release=release,
        maintainer='SHORiN <shorin@users.noreply.github.com>',
        homepage='https://github.com/SHORiN-KiWATA/miyu-agent',license='MIT AND OFL-1.1',
        description='Miyu voice front end' if voice else 'Miyu terminal AI assistant',contents=[])
    if voice:
        exact=f'{manifest["version"]}-{release}'
        config['depends']=[f'miyu (= {exact})' if family=='deb' else f'miyu = {exact}',
            'libasound2t64' if family=='deb' else 'alsa-lib',
            'libstdc++6' if family=='deb' else 'libstdc++',
            'libgcc-s1' if family=='deb' else 'libgcc', libc]
    if not voice:
        config['depends']=[libc,*config.get('depends',[])]
    for entry in package_inventory(asset,inventory):
        dest='/usr/'+entry['path']
        record={'dst':dest,'file_info':{'mode':int(entry['mode'],8)}}
        if entry['type']=='directory':
            record['type']='dir'
        elif entry['type']=='symlink':
            record.update(type='symlink',src=entry['target'])
        else:
            record['src']=str(stage/entry['path'])
        config['contents'].append(record)
    return config


def package(args):
    manifest=read_manifest(args.manifest)
    asset=next((a for a in manifest['assets'] if a['id']==args.asset_id),None)
    if asset is None:
        raise ValueError('Unknown release asset ID.')
    staging=load_json(args.stage/'stage-manifest.json')
    if staging['release_input_sha256']!=sha256_file(args.manifest) or staging['build_id']!=asset['build_id']:
        raise ValueError('Staging provenance does not match release input/asset.')
    stage=(args.stage/asset['component']).resolve()
    inventory=tree_manifest(stage)
    if inventory!=staging['components'][asset['component']]:
        raise ValueError('Staged resources have changed since validation.')
    evidence=validate_build_evidence(staging.get('build_evidence',{}).get(asset['component']),
        manifest,sha256_file(args.manifest),asset['build_id'],asset['component'],
        payload_binary_hash(inventory,asset['component']))
    out=fresh_directory(args.out)
    destination=out/asset['filename']
    if asset['format']=='tar.gz':
        tar_package(stage,destination,manifest['source_date_epoch'])
    elif asset['format'] in ('deb','rpm'):
        if args.nfpm is None or not args.nfpm.is_absolute():
            raise BlockedError('Provide the checksum-verified nFPM executable via absolute --nfpm.')
        version=subprocess.run([str(args.nfpm),'--version'],check=True,capture_output=True,text=True,timeout=30)
        if 'GitVersion:    '+manifest['toolchain']['nfpm']['version'] not in version.stdout:
            raise ValueError('nFPM version does not match the frozen toolchain.')
        config=nfpm_config(manifest,asset,stage,inventory)
        write_json(out/'nfpm-config.yaml',config)
        with (out/'package.log').open('wb') as log:
            subprocess.run([str(args.nfpm),'package','--config',str(out/'nfpm-config.yaml'),
                '--packager',asset['format'],'--target',str(destination)],
                check=True,stdout=log,stderr=subprocess.STDOUT,timeout=300)
    elif asset['format']=='archlinux':
        if not args.builder_image:
            raise BlockedError('Arch packaging requires the prepared native --builder-image.')
        from lib.arch_package import package_arch
        package_arch(manifest,asset,stage,inventory,destination,args.builder_image)
    else:
        raise ValueError('Unsupported package format.')
    record={'schema_version':1,'asset':asset,'sha256':sha256_file(destination),'files':inventory,
            'package_files':package_inventory(asset,inventory),'build_evidence':evidence,
            'size':destination.stat().st_size,'release_input_sha256':sha256_file(args.manifest),
            'source_commit':manifest['source_commit'],
            'source_snapshot_sha256':manifest['source_snapshot_sha256']}
    write_json(out/'package-record.json',record)
    return destination


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',required=True,type=Path)
    parser.add_argument('--stage',required=True,type=Path)
    parser.add_argument('--asset-id',required=True)
    parser.add_argument('--out',required=True,type=Path)
    parser.add_argument('--nfpm',type=Path)
    parser.add_argument('--builder-image',help='Prepared native Arch image for makepkg.')
    args=parser.parse_args()
    try:
        print(package(args))
        return 0
    except BlockedError as error:
        print(f'BLOCKED: {error}',file=sys.stderr)
        return 3
    except (ValueError,KeyError,OSError,subprocess.SubprocessError) as error:
        print(f'ERROR: {error}',file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())
