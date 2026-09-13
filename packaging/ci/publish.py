#!/usr/bin/env python3
"""Publish a verified allowlist. Dry-run by default, never overwrite remote bytes."""
import argparse
from pathlib import Path
import re
import subprocess
import sys

from lib.github_release import GitHubRelease, publish_verified
from lib.manifest import read_manifest
from lib.release_bundle import public_asset_names, verify_bundle


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest',required=True,type=Path)
    parser.add_argument('--dir',required=True,type=Path)
    parser.add_argument('--notes',type=Path)
    parser.add_argument('--repository',default='SHORiN-KiWATA/miyu-agent')
    mode=parser.add_mutually_exclusive_group()
    mode.add_argument('--dry-run',action='store_true')
    mode.add_argument('--execute',action='store_true')
    args=parser.parse_args()
    if not re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+',args.repository):
        parser.error('Invalid repository name')
    try:
        manifest=read_manifest(args.manifest)
        if not args.execute:
            verify_bundle(manifest,args.dir)
            print(f'DRY RUN: {args.repository}, tag={manifest["tag"]}, profile={manifest["profile"]}')
            for name in public_asset_names(manifest):
                print(name)
            return 0
        if manifest['mode']!='release' or manifest['source_dirty']:
            raise ValueError('Publication requires release-mode metadata from a clean tagged source.')
        if args.notes is None or not args.notes.is_file() or not args.notes.read_text().strip():
            raise ValueError('Publication requires a nonempty reviewed --notes file.')
        result=publish_verified(manifest,args.dir,args.notes,GitHubRelease(args.repository))
        print(result['html_url'])
        return 0
    except (ValueError,KeyError,OSError,RuntimeError,subprocess.SubprocessError) as error:
        print(f'ERROR: {error}',file=sys.stderr)
        return 1


if __name__=='__main__':
    sys.exit(main())
