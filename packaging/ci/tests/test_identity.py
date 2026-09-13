import copy
import unittest
from test_metadata import fixture
from lib.identity import build_identity


class IdentityTests(unittest.TestCase):
    def test_identical_inputs_identical_identity(self):
        manifest = fixture()
        self.assertEqual(build_identity(manifest, 'gnu-x86_64', 'core'),
                         build_identity(copy.deepcopy(manifest), 'gnu-x86_64', 'core'))

    def test_revision_resource_feature_changes_rotate_identity(self):
        baseline = fixture()
        original = build_identity(baseline, 'gnu-x86_64', 'core')
        for field in ('source_snapshot_sha256', 'wiki_commit', 'package_revision'):
            manifest = copy.deepcopy(baseline)
            manifest[field] = 'changed'
            self.assertNotEqual(original, build_identity(manifest, 'gnu-x86_64', 'core'))
        manifest = copy.deepcopy(baseline)
        manifest['locks']['assets'] = 'changed'
        self.assertNotEqual(original, build_identity(manifest, 'gnu-x86_64', 'core'))
        self.assertNotEqual(original, build_identity(baseline, 'gnu-x86_64', 'voice'))
        self.assertNotEqual(original, build_identity(baseline, 'arch-x86_64', 'core'))
