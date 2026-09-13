import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.arch_package import recipe, source_install, checkout_commit

REPO = Path(__file__).resolve().parents[3]


class ArchPackageTests(unittest.TestCase):
    def test_generated_recipe_has_exact_voice_dependency_and_rejects_shell_text(self):
        self.assertIn("'miyu=0.6.0-1'", recipe('0.6.0', 1, 'voice'))
        self.assertIn("'onnxruntime'", recipe('0.6.0', 1, 'core'))
        for version in ['0.6.0; touch /tmp/bad', '$(id)', '0.6', 'v0.6.0']:
            with self.assertRaises(ValueError):
                recipe(version, 1, 'core')
        for revision in [True, '1', -1]:
            with self.assertRaises(ValueError):
                recipe('0.6.0', revision, 'core')

    def test_source_recipes_install_from_the_shared_catalog(self):
        with tempfile.TemporaryDirectory(prefix='miyu-source-recipe-') as name:
            root = Path(name)
            source, wiki = root/'source', root/'wiki'
            for checkout in (source, wiki):
                checkout.mkdir()
                subprocess.run(['git', 'init', '-q', str(checkout)], check=True, timeout=30)
                subprocess.run(['git', '-C', str(checkout), '-c', 'user.name=Fixture',
                    '-c', 'user.email=fixture@example.invalid', 'commit', '-q', '--allow-empty',
                    '-m', 'fixture'], check=True, timeout=30)
            (source/'packaging/common').mkdir(parents=True)
            (source/'target/x86_64-unknown-linux-gnu/release').mkdir(parents=True)
            (source/'target/x86_64-unknown-linux-gnu/release/miyu').write_bytes(b'fixture binary')
            (source/'fixture.font').write_bytes(b'catalog-selected bytes')
            (source/'packaging/common/assets.json').write_text(json.dumps({
                'schema_version': 1, 'assets': [{
                    'id': 'font', 'source_root': 'source', 'source': 'fixture.font',
                    'destination': 'share/miyu/fonts/font.ttf', 'mode': '0644',
                    'component': 'core', 'type': 'file'}, {
                    'id': 'wiki-commit', 'source_root': 'runtime',
                    'source': 'default-kb/manifest/shorinwiki.commit',
                    'destination': 'share/miyu/default-kb/manifest/shorinwiki.commit',
                    'mode': '0644', 'component': 'core', 'type': 'file'}]}))
            revision = checkout_commit(wiki)
            destination = root/'package/usr'
            source_install(source, wiki, 'core', destination, revision)
            self.assertEqual((destination/'share/miyu/fonts/font.ttf').read_bytes(), b'catalog-selected bytes')
            self.assertEqual((destination/'share/miyu/default-kb/manifest/shorinwiki.commit').read_text(), revision+'\n')
            self.assertEqual(os.readlink(destination/'bin/miyupm'), 'miyu')
            nested = source/'nested'
            nested.mkdir()
            with self.assertRaises(ValueError):
                checkout_commit(nested)

    def test_binary_wrapper_preserves_alias_and_complete_resource_tree(self):
        with tempfile.TemporaryDirectory(prefix='miyu-repackage-') as name:
            root = Path(name)
            source, destination = root/'src', root/'pkg'
            (source/'usr/bin').mkdir(parents=True)
            (source/'usr/share/miyu/fonts').mkdir(parents=True)
            (source/'usr/bin/miyu').write_bytes(b'fixture binary')
            (source/'usr/bin/miyupm').symlink_to('miyu')
            (source/'usr/share/miyu/fonts/font.ttf').write_bytes(b'font fixture')
            env = dict(os.environ, HOME=str(root), srcdir=str(source), pkgdir=str(destination), CARCH='x86_64')
            subprocess.run(['bash', '-euc', 'source "$1"; package', 'test',
                str(REPO/'packaging/arch/miyu/PKGBUILD')], env=env, check=True, timeout=30)
            self.assertTrue((destination/'usr/bin/miyupm').is_symlink())
            self.assertEqual(os.readlink(destination/'usr/bin/miyupm'), 'miyu')
            self.assertEqual((destination/'usr/share/miyu/fonts/font.ttf').read_bytes(), b'font fixture')


if __name__ == '__main__':
    unittest.main()
