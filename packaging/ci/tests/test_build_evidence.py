import copy
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from lib.common import load_json, sha256_file, write_json
from lib.identity import build_identity
from package import package
from stage import stage
from test_metadata import fixture


class BuildEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='miyu-build-evidence-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = fixture('linux-smoke')
        self.manifest['builds']['gnu-x86_64']['components'] = ['core']
        self.source = self.root/'source'
        self.source.mkdir()
        (self.source/'LICENSE').write_text('fixture license')
        catalog = self.source/'packaging/common/assets.json'
        catalog.parent.mkdir(parents=True)
        write_json(catalog, {'schema_version': 1, 'assets': [{
            'id': 'license', 'component': 'core', 'source_root': 'source',
            'source': 'LICENSE', 'destination': 'share/licenses/miyu/LICENSE',
            'type': 'file', 'mode': '0644'}]})
        self.manifest['locks']['assets'] = sha256_file(catalog)
        self.path = self.root/'manifest.json'
        write_json(self.path, self.manifest)
        self.build = self.root/'build'
        (self.build/'core').mkdir(parents=True)
        binary = self.build/'core/miyu'
        binary.write_bytes(b'fixture binary')
        self.evidence = {'build_id': 'gnu-x86_64', 'component': 'core',
            'build_identity': build_identity(self.manifest, 'gnu-x86_64', 'core'),
            'binary_sha256': sha256_file(binary), 'builder_image': 'sha256:'+'a'*64,
            'source_commit': self.manifest['source_commit'],
            'source_snapshot_sha256': self.manifest['source_snapshot_sha256'],
            'release_input_sha256': sha256_file(self.path), 'offline': True,
            'rustc': 'rustc '+self.manifest['toolchain']['rust']+' (fixture)\n',
            'command': ['cargo', 'build', '--release', '--frozen', '--target',
                'x86_64-unknown-linux-gnu', '--bin', 'miyu',
                '--config', 'source.crates-io.replace-with="vendored-sources"',
                '--config', 'source.vendored-sources.directory="/inputs/vendor"']}
        write_json(self.build/'core/build-record.json', dict(self.evidence,
            container_command=['docker', '/private/host/path']))

    def stage(self):
        with patch('stage.verify_source', return_value=(self.manifest, self.source)), \
                patch('stage.verify_prepared'):
            return stage(self.path, self.root/'inputs', 'gnu-x86_64', self.build, self.root/'stage')

    def package(self):
        args = SimpleNamespace(manifest=self.path, asset_id='gnu-core',
            stage=self.root/'stage', out=self.root/'package', nfpm=None, builder_image=None)
        with patch('package.read_manifest', return_value=self.manifest):
            return package(args)

    def test_real_stage_and_tar_package_preserve_portable_build_evidence(self):
        staged = self.stage()
        self.assertEqual(staged['build_evidence'], {'core': self.evidence})
        self.package()
        record = load_json(self.root/'package/package-record.json')
        self.assertEqual(record['build_evidence'], self.evidence)
        self.assertEqual(record['package_files'], record['files'])
        self.assertNotIn('/private/host/path', (self.root/'package/package-record.json').read_text())

    def test_stage_rejects_build_for_other_frozen_input(self):
        evidence = copy.deepcopy(self.evidence)
        evidence['release_input_sha256'] = '0'*64
        write_json(self.build/'core/build-record.json', evidence)
        with self.assertRaises(ValueError):
            self.stage()

    def test_package_rejects_stage_build_evidence_tampering(self):
        staged = self.stage()
        staged['build_evidence']['core']['binary_sha256'] = '0'*64
        write_json(self.root/'stage/stage-manifest.json', staged)
        with self.assertRaises(ValueError):
            self.package()
