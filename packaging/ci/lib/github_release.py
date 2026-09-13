"""Small GitHub release adapter with hash-checked, non-overwriting uploads."""
import json
from pathlib import Path
import subprocess
import tempfile

from .common import sha256_file


class GitHubRelease:
    def __init__(self,repository):
        self.repository=repository

    def gh(self,*arguments,check=True):
        return subprocess.run(['gh',*map(str,arguments)],check=check,capture_output=True,
                              text=True,timeout=300)

    def tag_commit(self,tag):
        return json.loads(self.gh('api',f'repos/{self.repository}/commits/{tag}').stdout)['sha']

    def release(self,tag):
        # API status is explicit. Authentication/network failures are never "not found".
        result=self.gh('api',f'repos/{self.repository}/releases/tags/{tag}',check=False)
        if result.returncode:
            if 'HTTP 404' in result.stderr:
                return None
            raise RuntimeError('Unable to inspect remote release: '+result.stderr)
        return json.loads(result.stdout)

    def create_draft(self,tag,title,notes,prerelease):
        command=['release','create',tag,'--repo',self.repository,'--verify-tag','--draft',
                 '--title',title,'--notes-file',str(notes)]
        if prerelease:
            command.append('--prerelease')
        self.gh(*command)

    def upload(self,tag,path):
        self.gh('release','upload',tag,str(path),'--repo',self.repository)

    def remote_hash(self,tag,name):
        with tempfile.TemporaryDirectory(prefix='miyu-release-readback-') as temp:
            self.gh('release','download',tag,'--repo',self.repository,'--pattern',name,'--dir',temp)
            return sha256_file(Path(temp)/name)

    def finalize(self,tag,prerelease):
        self.gh('release','edit',tag,'--repo',self.repository,'--draft=false',
                '--prerelease='+str(prerelease).lower(),'--latest='+str(not prerelease).lower())


def verify_remote_allowlist(release,names,complete=False):
    if release is None:
        raise ValueError('Remote release is missing. Cannot verify the asset allowlist.')
    remote=[asset['name'] for asset in release['assets']]
    expected=set(names)
    if (len(remote)!=len(set(remote)) or set(remote)-expected
            or ((complete or not release['draft']) and set(remote)!=expected)):
        raise ValueError('Remote assets differ from the verified publication allowlist.')
    return set(remote)


def publish_verified(manifest,directory,notes,backend):
    tag=manifest['tag']
    if backend.tag_commit(tag)!=manifest['source_commit']:
        raise ValueError('Remote tag does not resolve to the verified source commit.')
    names=sorted(path.name for path in directory.iterdir())
    local={name:sha256_file(directory/name) for name in names}
    release=backend.release(tag)
    if release is None:
        backend.create_draft(tag,f'Miyu {manifest["version"]}',notes,
                             manifest['channels']['github']=='prerelease')
        release=backend.release(tag)
    remote=verify_remote_allowlist(release,names)
    for name in names:
        if name in remote:
            if backend.remote_hash(tag,name)!=local[name]:
                raise ValueError(f'Remote asset has different content. Refusing overwrite: {name}')
        else:
            backend.upload(tag,directory/name)
            if backend.remote_hash(tag,name)!=local[name]:
                raise ValueError(f'Remote upload hash verification failed: {name}')
    release=backend.release(tag)
    verify_remote_allowlist(release,names,complete=True)
    if release['draft']:
        backend.finalize(tag,manifest['channels']['github']=='prerelease')
    final=backend.release(tag)
    verify_remote_allowlist(final,names,complete=True)
    if final['draft']:
        raise ValueError('Release is still a draft after finalization.')
    return final
