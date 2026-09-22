"""Input corruption must fail before preparation can be accepted."""
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.common import BlockedError, sha256_file, write_json
from lib.downloads import obtain, safe_extract
from lib.inputs import input_inventory, verify_prepared
from lib.source import snapshot_digest
from prepare import LOCK_PATHS, reuse_vendor, verify_source
from test_metadata import fixture


class DownloadTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.cache = self.root/'cache'
        self.cache.mkdir()
        self.record = {'id': 'fixture', 'url': 'https://example.invalid/input.tgz',
                       'sha256': hashlib.sha256(b'locked bytes').hexdigest()}

    def tearDown(self):
        self.temp.cleanup()

    def test_cached_tampering_is_rejected_without_network(self):
        # 缓存自己那份按 record id 命名；被人改过就是缓存坏了，要当场停住。
        (self.cache/'fixture').write_bytes(b'tampered')
        with patch('urllib.request.urlopen', side_effect=AssertionError('network used')):
            with self.assertRaisesRegex(ValueError, 'SHA256'):
                obtain(self.record, self.root/'result', cache=self.cache, offline=True)
        self.assertFalse((self.root/'result').exists())

    def test_offline_missing_input_is_blocked(self):
        with patch('urllib.request.urlopen', side_effect=AssertionError('network used')):
            with self.assertRaises(BlockedError):
                obtain(self.record, self.root/'result', cache=self.cache, offline=True)

    def test_verified_cache_is_reusable_without_network(self):
        (self.cache/'input.tgz').write_bytes(b'locked bytes')
        with patch('urllib.request.urlopen', side_effect=AssertionError('network used')):
            obtain(self.record, self.root/'result', cache=self.cache, offline=True)
        self.assertEqual((self.root/'result').read_bytes(), b'locked bytes')

    def _serve(self, payload):
        """一次性的假 HTTPS 响应,数它被读了几次。"""
        def opener(url, timeout=None):
            self.served += 1
            stream = io.BytesIO(payload)
            stream.url = url
            stream.__enter__ = lambda: stream
            stream.__exit__ = lambda *args: None
            return stream
        return opener

    def test_a_download_seeds_the_cache_so_the_next_run_stays_offline(self):
        """09-21:发版链每跑一次就重下同一批锁定归档(语音三件约 190MB)。

        第一次走网络并落进缓存,第二次即便断网也要成功——退回改动前,第二次
        会因为缓存空而去 urlopen,被这里的断言当场抓住。
        """
        self.served = 0
        with patch('urllib.request.urlopen', self._serve(b'locked bytes')):
            obtain(self.record, self.root/'first', cache=self.cache, offline=False)
        self.assertEqual(self.served, 1)
        self.assertEqual((self.cache/'fixture').read_bytes(), b'locked bytes')
        with patch('urllib.request.urlopen', side_effect=AssertionError('network used')):
            obtain(self.record, self.root/'second', cache=self.cache, offline=True)
        self.assertEqual((self.root/'second').read_bytes(), b'locked bytes')

    def test_two_records_sharing_a_url_filename_do_not_poison_each_other(self):
        """09-22 发 0.6.2 时实测：锁里 `sherpa-onnx-license` 与 `silero-vad-license`
        的 URL 末段都是 `LICENSE`。缓存原来按末段命名，先写的那份占住位置、后一条
        存不进去，下一次发版取到别人的字节，哈希对不上，整条链停在这里。

        退回改动前这条会红：第二件取到第一件的字节，报 SHA256 mismatch。
        """
        other = {'id': 'other-fixture', 'url': 'https://example.invalid/input.tgz',
                 'sha256': hashlib.sha256(b'other bytes').hexdigest()}
        self.served = 0
        with patch('urllib.request.urlopen', self._serve(b'locked bytes')):
            obtain(self.record, self.root/'first', cache=self.cache, offline=False)
        with patch('urllib.request.urlopen', self._serve(b'other bytes')):
            obtain(other, self.root/'second', cache=self.cache, offline=False)
        self.assertEqual((self.root/'second').read_bytes(), b'other bytes')
        # 断网重取，两件都要各自命中自己那份。
        with patch('urllib.request.urlopen', side_effect=AssertionError('network used')):
            obtain(self.record, self.root/'again-first', cache=self.cache, offline=True)
            obtain(other, self.root/'again-second', cache=self.cache, offline=True)
        self.assertEqual((self.root/'again-first').read_bytes(), b'locked bytes')
        self.assertEqual((self.root/'again-second').read_bytes(), b'other bytes')

    def test_a_foreign_file_with_the_same_name_is_a_miss_not_a_corruption(self):
        """按文件名认的那一路是外来输入（CI 递进来一篮子文件）。名字撞上、内容对不上
        很正常，那是「不是这一件」，照常去锁定 URL 取；不能当成缓存被改过而停住。
        """
        (self.cache/'input.tgz').write_bytes(b'somebody elses bytes')
        self.served = 0
        with patch('urllib.request.urlopen', self._serve(b'locked bytes')):
            obtain(self.record, self.root/'result', cache=self.cache, offline=False)
        self.assertEqual(self.served, 1)
        self.assertEqual((self.root/'result').read_bytes(), b'locked bytes')

    def test_no_cache_directory_means_no_write_and_no_crash(self):
        self.served = 0
        with patch('urllib.request.urlopen', self._serve(b'locked bytes')):
            obtain(self.record, self.root/'only', cache=None, offline=False)
        self.assertEqual((self.root/'only').read_bytes(), b'locked bytes')

    def test_a_poisoned_cache_entry_is_never_trusted_after_seeding(self):
        """回写不削弱校验:缓存被人改过,下一次照样逐字节重算并拒绝。"""
        self.served = 0
        with patch('urllib.request.urlopen', self._serve(b'locked bytes')):
            obtain(self.record, self.root/'first', cache=self.cache, offline=False)
        (self.cache/'fixture').write_bytes(b'tampered')
        with patch('urllib.request.urlopen', side_effect=AssertionError('network used')):
            with self.assertRaisesRegex(ValueError, 'SHA256'):
                obtain(self.record, self.root/'third', cache=self.cache, offline=True)


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def archive(self, entries):
        path = self.root/'input.tar'
        with tarfile.open(path, 'w') as archive:
            for name, kind, target in entries:
                member = tarfile.TarInfo(name)
                if kind == 'file':
                    member.size = len(target)
                    archive.addfile(member, io.BytesIO(target))
                else:
                    member.type = tarfile.SYMTYPE if kind == 'symlink' else tarfile.LNKTYPE
                    member.linkname = target
                    archive.addfile(member)
        return path

    def test_traversal_and_absolute_paths_are_rejected(self):
        for name in ('../escaped', '/absolute', 'root/../escaped'):
            with self.subTest(name=name):
                source = self.archive([(name, 'file', b'bad')])
                with self.assertRaises(ValueError):
                    safe_extract(source, self.root/'extract')
                self.assertFalse((self.root/'escaped').exists())

    def test_escaping_links_and_link_parent_pivot_are_rejected(self):
        for entries in (
            [('root/link', 'symlink', '../../escape')],
            [('root/link', 'symlink', '/tmp/escape')],
            [('root/link', 'hardlink', '../escape')],
            [('root/link', 'symlink', 'other'), ('root/link/file', 'file', b'bad')],
        ):
            with self.subTest(entries=entries):
                source = self.archive(entries)
                with self.assertRaises(ValueError):
                    safe_extract(source, self.root/'extract')

    def test_internal_library_link_is_preserved(self):
        source = self.archive([('root/lib.so.1.23.2', 'file', b'library'),
                               ('root/lib.so.1', 'symlink', 'lib.so.1.23.2'),
                               ('root/lib.so', 'symlink', 'lib.so.1')])
        safe_extract(source, self.root/'extract')
        self.assertTrue((self.root/'extract/root/lib.so').is_symlink())
        self.assertEqual((self.root/'extract/root/lib.so').read_bytes(), b'library')


class FrozenSourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root/'source'
        repo = Path(__file__).resolve().parents[3]
        records = []
        for relative in [*LOCK_PATHS.values(), 'Cargo.toml']:
            path = self.source/relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes((repo/relative).read_bytes())
            path.chmod(0o644)
            records.append({'path': relative, 'type': 'file', 'mode': '100644',
                            'size': path.stat().st_size, 'sha256': sha256_file(path)})
        records.sort(key=lambda r: r['path'])
        manifest = fixture()
        manifest['source_snapshot_sha256'] = snapshot_digest(records)
        manifest['locks'] = {name: sha256_file(self.source/path) for name, path in LOCK_PATHS.items()}
        manifest['wiki_commit'] = json.loads((self.source/LOCK_PATHS['third_party']).read_text())['wiki']['commit']
        self.manifest = self.root/'release-input.json'
        write_json(self.manifest, manifest)
        write_json(self.root/'source-files.json', records)

    def tearDown(self):
        self.temp.cleanup()

    def test_frozen_source_accepts_exact_inventory(self):
        _, source = verify_source(self.manifest)
        self.assertEqual(source, self.source)

    def test_modified_source_or_added_file_is_rejected(self):
        path = self.source/'Cargo.toml'
        original = path.read_bytes()
        path.write_bytes(original+b'\n# tampered\n')
        with self.assertRaisesRegex(ValueError, 'file changed'):
            verify_source(self.manifest)
        path.write_bytes(original)
        (self.source/'undeclared').write_text('unexpected')
        with self.assertRaisesRegex(ValueError, 'undeclared'):
            verify_source(self.manifest)

    def test_inventory_or_frozen_lock_mismatch_is_rejected(self):
        manifest = json.loads(self.manifest.read_text())
        manifest['locks']['cargo'] = hashlib.sha256(b'wrong locked file').hexdigest()
        write_json(self.manifest, manifest)
        with self.assertRaisesRegex(ValueError, 'lock hash'):
            verify_source(self.manifest)
        records = json.loads((self.root/'source-files.json').read_text())
        records.pop()
        write_json(self.root/'source-files.json', records)
        with self.assertRaisesRegex(ValueError, 'inventory digest'):
            verify_source(self.manifest)


class PreparedInputsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.inputs = self.root/'inputs'
        self.inputs.mkdir()
        (self.inputs/'resource').write_text('locked data')
        self.manifest = fixture()
        self.manifest_path = self.root/'release-input.json'
        write_json(self.manifest_path, self.manifest)
        prepared = {'schema_version': 1, 'manifest_sha256': sha256_file(self.manifest_path),
                    'cargo_lock_sha256': self.manifest['locks']['cargo'], 'vendor_complete': False,
                    'files': input_inventory(self.inputs)}
        for field in ('source_commit', 'source_snapshot_sha256', 'wiki_commit'):
            prepared[field] = self.manifest[field]
        write_json(self.inputs/'prepared.json', prepared)

    def tearDown(self):
        self.temp.cleanup()

    def test_prepared_inventory_detects_tampering_additions_and_wrong_source(self):
        verify_prepared(self.inputs, self.manifest, self.manifest_path)
        (self.inputs/'resource').write_text('tampered')
        with self.assertRaisesRegex(ValueError, 'changed files'):
            verify_prepared(self.inputs, self.manifest, self.manifest_path)
        (self.inputs/'resource').write_text('locked data')
        (self.inputs/'extra').write_text('new')
        with self.assertRaisesRegex(ValueError, 'extra'):
            verify_prepared(self.inputs, self.manifest, self.manifest_path)
        (self.inputs/'extra').unlink()
        wrong = dict(self.manifest, wiki_commit=hashlib.sha1(b'another wiki').hexdigest())
        with self.assertRaisesRegex(ValueError, 'wiki_commit'):
            verify_prepared(self.inputs, wrong, self.manifest_path)

    def test_partial_vendor_and_changed_manifest_are_rejected(self):
        with self.assertRaisesRegex(ValueError, 'Complete Cargo vendor'):
            verify_prepared(self.inputs, self.manifest, self.manifest_path, require_vendor=True)
        self.manifest_path.write_text(self.manifest_path.read_text()+'\n')
        with self.assertRaisesRegex(ValueError, 'different release manifest'):
            verify_prepared(self.inputs, self.manifest, self.manifest_path)

    def test_vendor_reuse_is_copied_and_corruption_is_rejected(self):
        source = self.root/'source'
        source.mkdir()
        (source/'Cargo.lock').write_text('locked dependencies')
        vendor = self.inputs/'vendor'
        vendor.mkdir()
        (vendor/'crate.rs').write_text('pub fn example() {}')
        (self.inputs/'cargo-config.toml').write_text(
            '[source.crates-io]\nreplace-with = "vendored-sources"\n'
            '[source.vendored-sources]\ndirectory = "old/vendor"\n')
        write_json(self.inputs/'prepared.json', {'schema_version': 1, 'vendor_complete': True,
            'cargo_lock_sha256': sha256_file(source/'Cargo.lock'), 'files': input_inventory(self.inputs)})
        output = self.root/'copied'
        output.mkdir()
        reuse_vendor(source, output, self.inputs)
        self.assertEqual((output/'vendor/crate.rs').read_bytes(), (vendor/'crate.rs').read_bytes())
        self.assertFalse((output/'vendor/crate.rs').samefile(vendor/'crate.rs'))
        (vendor/'crate.rs').write_text('tampered dependency')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            reuse_vendor(source, self.root/'second', self.inputs)


if __name__ == '__main__':
    unittest.main()
