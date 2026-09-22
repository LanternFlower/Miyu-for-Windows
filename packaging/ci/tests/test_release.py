import copy
import hashlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from lib.common import load_json, sha256_file, write_json
from lib.github_release import publish_verified
from lib.matrix import release_matrix, selected_targets
from lib.identity import build_identity
from lib.release_bundle import bundle_checksums, output_name, sums_name, verify_bundle
import publish
from test_metadata import fixture
from verify_release import aggregate


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.root=Path(self.temp.name)
        manifest=fixture()
        manifest['profile']='linux-smoke'
        manifest['targets']=selected_targets('linux-smoke',None)
        manifest['builds'],manifest['assets'],manifest['checks']=release_matrix(
            Path(__file__).resolve().parents[2]/'common/targets.json','linux-smoke',
            manifest['targets'],'0.6.0',1,44)
        manifest['channels']['macos_direct_signing']='not-distributed'
        self.manifest=manifest
        self.path=self.root/'release-input.json'
        write_json(self.path,manifest)
        self.packages=self.root/'packages';self.reports=self.root/'reports'
        hashes={}
        binaries={}
        for asset in manifest['assets']:
            folder=self.packages/asset['id'];folder.mkdir(parents=True)
            binary=folder/asset['filename'];binary.write_bytes(('test fixture '+asset['id']).encode())
            hashes[asset['id']]=sha256_file(binary)
            license_root='share/licenses/miyu-voice/' if asset['component']=='voice' else 'share/licenses/miyu/'
            build_id,component=asset['build_id'],asset['component']
            binaries[asset['id']]=hashlib.sha256((build_id+component).encode()).hexdigest()
            evidence={'build_id':build_id,'component':component,
                'build_identity':build_identity(manifest,build_id,component),
                'binary_sha256':binaries[asset['id']],
                'builder_image':'sha256:'+hashlib.sha256(build_id.encode()).hexdigest(),
                'source_commit':manifest['source_commit'],
                'source_snapshot_sha256':manifest['source_snapshot_sha256'],
                'release_input_sha256':sha256_file(self.path),
                'rustc':'rustc '+manifest['toolchain']['rust']+' (fixture)\n', 'offline':True,
                'command':['cargo','build','--release','--frozen','--target',
                    manifest['builds'][build_id]['target'],'--bin',
                    'miyu-voice' if component=='voice' else 'miyu',
                    '--config','source.crates-io.replace-with="vendored-sources"',
                    '--config','source.vendored-sources.directory="/inputs/vendor"']}
            if component=='voice':evidence['command']+=['--features','voice']
            write_json(folder/'package-record.json',{'asset':asset,'sha256':hashes[asset['id']],
                'build_evidence':evidence,
                'release_input_sha256':sha256_file(self.path),'source_commit':manifest['source_commit'],
                'source_snapshot_sha256':manifest['source_snapshot_sha256'],
                'files':[{'path':license_root+'LICENSE','type':'file','size':7,
                          'sha256':hashlib.sha256(b'license').hexdigest()},
                    {'path':'bin/'+('miyu-voice' if component=='voice' else 'miyu'),
                     'type':'file','size':42,'sha256':binaries[asset['id']]}]})
        for target in manifest['targets']:
            folder=self.reports/target;folder.mkdir(parents=True)
            checks=[dict(c,status='PASS',artifact_sha256=hashes[c['asset_id']])
                    for c in manifest['checks'] if c['target']==target]
            results={}
            for asset_id in {c['asset_id'] for c in checks}:
                component=next(a['component'] for a in manifest['assets'] if a['id']==asset_id)
                results[asset_id]={'asset_id':asset_id,'binary_sha256':binaries[asset_id],
                    'version':('miyu-voice' if component=='voice' else 'miyu')+' 0.6.0',
                    'files_verified':2,'host_path':'/private/test/home', 'api_key':'fixture-secret'}
                if component=='core':results[asset_id]['provider_live']={
                    'type':'done','text':'Hello from the test fixture.', 'provider_id':'opencodego',
                    'model':'deepseek-v4.1-flash','usage':{'input_tokens':1,'output_tokens':3},
                    'elapsed_ms':42, 'api_key':'fixture-secret'}
            write_json(folder/'cleanup.json',{'remove_exit_code':0,'list_exit_code':0,
                'remaining':'','stderr':'/private/test/home'})
            write_json(folder/'report.json',{'target':target,'status':'PASS',
                'image':manifest['toolchain']['install_images'][target], 'results':results,
                'commands':[{'command':['docker','/private/test/home','fixture-secret']}],
                'source_commit':manifest['source_commit'],
                'source_snapshot_sha256':manifest['source_snapshot_sha256'],
                'release_input_sha256':sha256_file(self.path),'checks':checks})

    def tearDown(self):
        self.temp.cleanup()

    def aggregate(self):
        # Source-byte verification has separate tamper tests in test_prepare.
        with patch('verify_release.verify_source',return_value=(self.manifest,self.root/'source')):
            return aggregate(self.path,self.packages,self.reports,self.root/'publish')

    def test_complete_bundle_then_extra_file_rejected(self):
        result=self.aggregate()
        verify_bundle(self.manifest,self.root/'publish')
        self.assertEqual(len([f for f in result['files'] if f['kind']=='package']),8)
        (self.root/'publish/secret-config.json').write_text('fixture secret')
        with self.assertRaises(ValueError):
            verify_bundle(self.manifest,self.root/'publish')

    def test_only_distribution_packages_are_uploaded(self):
        screenshots=self.root/'source/docs/releases/0.6.0/oobe'
        screenshots.mkdir(parents=True)
        (screenshots/'01-welcome.png').write_bytes(b'\x89PNG\r\n\x1a\nfixture')
        self.aggregate()
        folder=self.root/'publish'
        backend=FakeGitHub(self.manifest['source_commit'])
        publish_verified(self.manifest,folder,self.path,backend)
        expected={asset['filename'] for asset in self.manifest['assets']
                  if asset['format'] in ('archlinux','deb','rpm')}
        self.assertEqual(len(expected),6)
        self.assertEqual(set(backend.bytes),expected)
        internal={path.name for path in folder.iterdir()}-expected
        self.assertIn('01-welcome.png',internal)
        self.assertIn(sums_name(self.manifest),internal)
        self.assertTrue(any(name.endswith('.spdx.json') for name in internal))
        self.assertTrue(any(name.endswith('.tar.gz') for name in internal))
        self.assertGreater(len(internal),6)
        verify_bundle(self.manifest,folder)

    def test_dry_run_lists_only_six_public_packages(self):
        self.aggregate()
        output=io.StringIO()
        with patch('sys.argv',['publish.py','--manifest',str(self.path),
                              '--dir',str(self.root/'publish'),'--dry-run']), \
                patch('sys.stdout',output):
            self.assertEqual(publish.main(),0)
        expected=sorted(asset['filename'] for asset in self.manifest['assets']
                        if asset['format'] in ('archlinux','deb','rpm'))
        self.assertEqual(output.getvalue().splitlines()[1:],expected)

    def test_internal_evidence_tamper_blocks_public_upload(self):
        self.aggregate()
        folder=self.root/'publish'
        (folder/'provenance-0.6.0-1.json').write_text('{}')
        backend=FakeGitHub(self.manifest['source_commit'])
        with self.assertRaisesRegex(ValueError,'hash mismatch'):
            publish_verified(self.manifest,folder,self.path,backend)
        self.assertIsNone(backend.state)
        self.assertEqual(backend.bytes,{})

    def test_build_evidence_and_public_acceptance(self):
        result=self.aggregate()
        provenance=load_json(self.root/'publish/provenance-0.6.0-1.json')
        builds=provenance['predicate']['runDetails']['byproducts']
        self.assertEqual(len(builds),4)
        self.assertTrue(all(b['builder_image'].startswith('sha256:') for b in builds))
        self.assertTrue(all('--frozen' in b['command'] for b in builds))
        acceptance=next(f for f in result['files'] if f['kind']=='acceptance')
        path=self.root/'publish'/acceptance['filename']
        reports=load_json(path)['reports']
        self.assertEqual(len(reports),6)
        self.assertTrue(all(r['cleanup']['remaining']=='' for r in reports))
        self.assertIn('Hello from the test fixture.',path.read_text())
        for private in ('fixture-secret','/private/test/home','api_key','commands'):
            self.assertNotIn(private,path.read_text())

    def test_missing_or_tampered_build_evidence_rejected(self):
        path=self.packages/'deb-core/package-record.json'
        original=load_json(path)
        mutations=[('missing',None),('build_id','arch-x86_64'),('component','voice'),
            ('build_identity','0'*64),('binary_sha256','0'*64),
            ('builder_image','mutable:latest'),('source_commit','0'*40),
            ('source_snapshot_sha256','0'*64),('release_input_sha256','0'*64),
            ('rustc','rustc 0.0.0'),('offline',False),('command',['cargo','build'])]
        for field,value in mutations:
            with self.subTest(field=field):
                record=copy.deepcopy(original)
                if field=='missing':record.pop('build_evidence')
                else:record['build_evidence'][field]=value
                write_json(path,record)
                with self.assertRaises(ValueError):self.aggregate()
        write_json(path,original)

    def test_conflicting_shared_build_rejected(self):
        path=self.packages/'rpm-core/package-record.json'
        record=load_json(path)
        record['build_evidence']['builder_image']='sha256:'+'f'*64
        write_json(path,record)
        with self.assertRaisesRegex(ValueError,'Conflicting'):self.aggregate()

    def test_installed_binary_evidence_mismatch_rejected(self):
        path=self.reports/'debian13-x86_64/report.json'
        report=load_json(path)
        report['results']['deb-core']['binary_sha256']='0'*64
        write_json(path,report)
        with self.assertRaises(ValueError):self.aggregate()

    def test_changed_install_image_or_failed_provider_result_rejected(self):
        path=self.reports/'debian13-x86_64/report.json'
        original=load_json(path)
        for mutation in ('image','missing-result','provider','empty-output'):
            with self.subTest(mutation=mutation):
                report=copy.deepcopy(original)
                if mutation=='image':report['image']='debian:latest'
                elif mutation=='missing-result':report['results'].pop('deb-core')
                elif mutation=='provider':report['results']['deb-core']['provider_live']['type']='error'
                else:report['results']['deb-core']['provider_live']['text']=' '
                write_json(path,report)
                with self.assertRaises(ValueError):self.aggregate()

    def test_failed_cleanup_evidence_rejected(self):
        path=self.reports/'debian13-x86_64/cleanup.json'
        original=load_json(path)
        for field,value in (('remaining','owned-container'),('remove_exit_code',1),('list_exit_code',1)):
            with self.subTest(field=field):
                cleanup=copy.deepcopy(original);cleanup[field]=value;write_json(path,cleanup)
                with self.assertRaises(ValueError):self.aggregate()

    def test_deleted_report_rejected(self):
        (self.reports/'ubuntu2404-x86_64/report.json').unlink()
        with self.assertRaises(OSError):
            self.aggregate()

    def rewrite_output(self, output):
        folder=self.root/'publish'
        write_json(folder/output_name(self.manifest),output)
        names=[r['filename'] for r in output['files']]+[output_name(self.manifest)]
        (folder/sums_name(self.manifest)).write_text(bundle_checksums(folder,names))

    def test_changed_frozen_input_rejected(self):
        self.aggregate()
        changed=copy.deepcopy(self.manifest)
        changed['wiki_commit']=hashlib.sha1(b'other frozen wiki').hexdigest()
        with self.assertRaisesRegex(ValueError,'frozen input'):
            verify_bundle(changed,self.root/'publish')

    def test_output_input_hash_and_unique_input_required(self):
        original=self.aggregate()
        for mutation in ('hash','missing','duplicate'):
            with self.subTest(mutation=mutation):
                output=copy.deepcopy(original)
                record=next(r for r in output['files'] if r['kind']=='input')
                if mutation=='hash':
                    output['release_input_sha256']=hashlib.sha256(b'other input').hexdigest()
                elif mutation=='missing':
                    record['kind']='provenance'
                else:
                    next(r for r in output['files'] if r['kind']=='provenance')['kind']='input'
                self.rewrite_output(output)
                with self.assertRaisesRegex(ValueError,'frozen input'):
                    verify_bundle(self.manifest,self.root/'publish')

    def test_rehashed_embedded_input_rejected(self):
        output=self.aggregate()
        record=next(r for r in output['files'] if r['kind']=='input')
        path=self.root/'publish'/record['filename']
        changed=copy.deepcopy(self.manifest)
        changed['wiki_commit']=hashlib.sha1(b'other embedded wiki').hexdigest()
        write_json(path,changed)
        record.update(sha256=sha256_file(path),size=path.stat().st_size)
        output['release_input_sha256']=record['sha256']
        self.rewrite_output(output)
        with self.assertRaisesRegex(ValueError,'frozen input'):
            verify_bundle(self.manifest,self.root/'publish')

    def test_replaced_binary_rejected(self):
        asset=self.manifest['assets'][0]
        (self.packages/asset['id']/asset['filename']).write_bytes(b'replaced')
        with self.assertRaises(ValueError):
            self.aggregate()

    def test_stale_source_or_failed_check_rejected(self):
        path=self.reports/'debian13-x86_64/report.json'
        original=load_json(path)
        for field,value in (('source_commit','other'),('status','FAIL')):
            report=copy.deepcopy(original);report[field]=value;write_json(path,report)
            with self.assertRaises(ValueError):
                self.aggregate()
        report=copy.deepcopy(original);report['checks'].pop();write_json(path,report)
        with self.assertRaises(ValueError):
            self.aggregate()


class FakeGitHub:
    def __init__(self,sha):
        self.sha=sha;self.state=None;self.bytes={};self.fail_upload=None;self.finalized=0
    def tag_commit(self,tag):return self.sha
    def release(self,tag):
        if self.state is None:return None
        return dict(draft=self.state,assets=[{'name':n} for n in self.bytes],html_url='test://release')
    def create_draft(self,*_):self.state=True
    def upload(self,tag,path):
        if path.name==self.fail_upload:raise RuntimeError('simulated upload interruption')
        self.bytes[path.name]=path.read_bytes()
    def remote_hash(self,tag,name):return hashlib.sha256(self.bytes[name]).hexdigest()
    def finalize(self,*_):self.state=False;self.finalized+=1


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.bundle=BundleTests()
        self.bundle.setUp()
        self.addCleanup(self.bundle.tearDown)
        self.bundle.aggregate()
        self.manifest=self.bundle.manifest
        self.folder=self.bundle.root/'publish'
        self.notes=self.bundle.path
        self.names=sorted(asset['filename'] for asset in self.manifest['assets']
                          if asset['format'] in ('archlinux','deb','rpm'))

    def publish(self,backend):
        return publish_verified(self.manifest,self.folder,self.notes,backend)

    def test_remote_extra_asset_rejected_before_upload_or_finalize(self):
        for draft in (True,False):
            for extra in ('unverified-macos.tar.gz','provenance-0.6.0-1.json'):
                with self.subTest(draft=draft,extra=extra):
                    backend=FakeGitHub(self.manifest['source_commit']);backend.state=draft
                    backend.bytes={name:(self.folder/name).read_bytes() for name in self.names}
                    backend.bytes[extra]=b'extra'
                    before=dict(backend.bytes)
                    with self.assertRaisesRegex(ValueError,'allowlist'):
                        self.publish(backend)
                    self.assertEqual(backend.bytes,before)
                    self.assertEqual(backend.finalized,0)
                    self.assertEqual(backend.state,draft)

    def test_asset_added_during_upload_keeps_draft(self):
        backend=FakeGitHub(self.manifest['source_commit'])
        original=backend.upload
        def upload(tag,path):
            original(tag,path)
            backend.bytes['unverified-macos.tar.gz']=b'extra'
        backend.upload=upload
        with self.assertRaisesRegex(ValueError,'allowlist'):
            self.publish(backend)
        self.assertTrue(backend.state)
        self.assertEqual(backend.finalized,0)

    def test_asset_added_during_finalize_cannot_report_success(self):
        backend=FakeGitHub(self.manifest['source_commit'])
        original=backend.finalize
        def finalize(*args):
            original(*args)
            backend.bytes['unverified-macos.tar.gz']=b'extra'
        backend.finalize=finalize
        with self.assertRaisesRegex(ValueError,'allowlist'):
            self.publish(backend)

    def test_interrupted_upload_stays_draft_retry_and_conflict(self):
        backend=FakeGitHub(self.manifest['source_commit'])
        backend.fail_upload=self.names[1]
        with self.assertRaises(RuntimeError):self.publish(backend)
        self.assertTrue(backend.state);self.assertEqual(backend.finalized,0)
        backend.fail_upload=None
        self.publish(backend)
        self.assertFalse(backend.state);self.assertEqual(backend.finalized,1)
        self.publish(backend)
        self.assertEqual(backend.finalized,1)
        backend.bytes[self.names[0]]=b'changed remote bytes'
        with self.assertRaisesRegex(ValueError,'Refusing overwrite'):
            self.publish(backend)
        self.assertEqual(backend.bytes[self.names[0]],b'changed remote bytes')
