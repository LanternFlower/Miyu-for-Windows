#!/usr/bin/env python3
"""Generate AUR updates from verified release bytes. Never push a channel implicitly."""
import argparse
import difflib
from pathlib import Path
import re
import subprocess
import sys

from lib.common import fresh_directory, load_json, write_json
from lib.github_release import GitHubRelease
from lib.manifest import read_manifest
from lib.release_bundle import verify_bundle

REPO=Path(__file__).resolve().parents[2]
REMOTE='SHORiN-KiWATA/miyu-agent'


def render_pkgbuild(template,version,revision,sha256):
    result=template
    for name,value in (('pkgver',version),('pkgrel',str(revision)),('_release_pkgrel',str(revision))):
        result,count=re.subn(rf'^{name}=.*$',f'{name}={value}',result,count=1,flags=re.M)
        if count!=1:
            raise ValueError(f'Channel template is missing {name}.')
    result,count=re.subn(r"sha256sums=\(\s*'[0-9a-f]{64}'\s*\)",f"sha256sums=(\n  '{sha256}'\n)",result,count=1)
    if count!=1:
        raise ValueError('Channel template checksum is missing or ambiguous.')
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',required=True,type=Path)
    parser.add_argument('--release-output',required=True,type=Path)
    parser.add_argument('--out',required=True,type=Path)
    parser.add_argument('--published-url',help='Require read-back verification of this formal GitHub release.')
    parser.add_argument('--builder-image',default='miyu-distribution-arch:2026-09-14')
    parser.add_argument('--apply',action='store_true',help='Also update the repository channel truth sources. No git operation.')
    args=parser.parse_args()
    try:
        manifest=read_manifest(args.manifest)
        output=verify_bundle(manifest,args.release_output.parent)
        if output!=load_json(args.release_output):
            raise ValueError('Release output is not the verified manifest in its bundle.')
        packages={f['asset_id']:f for f in output['files'] if f['kind']=='package'}
        if not {'arch-core','arch-voice'}.issubset(packages):
            raise ValueError('AUR main and voice assets must share one complete release output.')
        if args.apply and not args.published_url:
            raise ValueError('Applying channel updates requires formal-release read-back verification.')
        if args.published_url:
            expected=f'https://github.com/{REMOTE}/releases/tag/{manifest["tag"]}'
            if args.published_url!=expected:
                raise ValueError('Published URL does not match the frozen tag/repository.')
            remote=GitHubRelease(REMOTE)
            release=remote.release(manifest['tag'])
            if release is None or release['draft'] or remote.tag_commit(manifest['tag'])!=manifest['source_commit']:
                raise ValueError('Channel update requires a formal release of the verified source.')
            for key in ('arch-core','arch-voice'):
                record=packages[key]
                if remote.remote_hash(manifest['tag'],record['filename'])!=record['sha256']:
                    raise ValueError('Remote AUR source asset differs from the verified release.')
        out=fresh_directory(args.out)
        patch=[]
        for package,asset_id in (('miyu','arch-core'),('miyu-voice','arch-voice')):
            relative=Path('packaging/arch')/package/'PKGBUILD'
            original=(REPO/relative).read_text()
            rendered=render_pkgbuild(original,manifest['version'],manifest['package_revision'],packages[asset_id]['sha256'])
            folder=out/package;folder.mkdir()
            (folder/'PKGBUILD').write_text(rendered)
            srcinfo=subprocess.run(['docker','run','--rm','--network','none','--user','65534:65534',
                '--label','io.miyu.distribution.owner=distribution-2026-09-14',
                '--mount',f'type=bind,src={folder.resolve()},dst=/package,readonly',
                '--workdir','/package',args.builder_image,'makepkg','--printsrcinfo'],
                check=True,capture_output=True,text=True,timeout=60).stdout
            (folder/'.SRCINFO').write_text(srcinfo)
            patch.extend(difflib.unified_diff(original.splitlines(True),rendered.splitlines(True),
                fromfile='a/'+str(relative),tofile='b/'+str(relative)))
            if args.apply:
                (REPO/relative).write_text(rendered)
                (REPO/relative.parent/'.SRCINFO').write_text(srcinfo)
        (out/'channels.patch').write_text(''.join(patch))
        write_json(out/'channel-update.json',{'schema_version':1,'version':manifest['version'],
            'revision':manifest['package_revision'],'published_url':args.published_url,
            'applied':args.apply,'remote_push':False,'channels':['aur-miyu','aur-miyu-voice']})
        print(f'Generated reviewed channel files and patch: {out}')
        return 0
    except (ValueError,KeyError,OSError,RuntimeError,subprocess.SubprocessError) as error:
        print(f'ERROR: {error}',file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())
