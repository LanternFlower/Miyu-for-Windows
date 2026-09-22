"""Internal container entry point. Prepared dependencies only, no network."""
import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--component', choices=('core', 'voice'), required=True)
    parser.add_argument('--target', required=True, choices=('x86_64-unknown-linux-gnu',))
    parser.add_argument('--build-identity', required=True)
    parser.add_argument('--epoch', required=True)
    args = parser.parse_args()
    if platform.machine() != 'x86_64':
        raise RuntimeError('Build runner must be native x86_64.')
    env = dict(os.environ, CARGO_HOME='/build/cargo-home', CARGO_TARGET_DIR='/target',
               MIYU_BUILD_ID=args.build_identity, SOURCE_DATE_EPOCH=args.epoch,
               CARGO_BUILD_JOBS='2', CARGO_NET_OFFLINE='true', SHERPA_ONNX_ARCHIVE_DIR='/inputs/archives')
    Path('/build/cargo-home').mkdir(exist_ok=True)
    binary = 'miyu-voice' if args.component == 'voice' else 'miyu'
    command = ['cargo', 'build', '--release', '--frozen', '--target', args.target, '--bin', binary,
               '--config', 'source.crates-io.replace-with="vendored-sources"',
               '--config', 'source.vendored-sources.directory="/inputs/vendor"']
    if args.component == 'voice':
        command += ['--features', 'voice']
    subprocess.run(command, cwd='/source', env=env, check=True, timeout=7200)
    destination = Path('/build')/binary
    shutil.copy2(Path('/target')/args.target/'release'/binary, destination)
    with tempfile.TemporaryDirectory(prefix='miyu-version-') as home:
        probe_env = dict(env, MIYU_HOME=home, HOME=home, XDG_RUNTIME_DIR=home)
        version = subprocess.run([str(destination), '--version'], env=probe_env,
            check=True, capture_output=True, text=True, timeout=30).stdout.strip()
    record = {'command': command, 'version_output': version, 'architecture': platform.machine(),
              'rustc': subprocess.run(['rustc','-Vv'],check=True,capture_output=True,text=True,timeout=30).stdout,
              'offline': True}
    Path('/build/compile.json').write_text(json.dumps(record, indent=2)+'\n')


if __name__ == '__main__':
    main()
