"""Protection tests use disposable files and real subprocesses only."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.isolation import Sandbox, OwnershipError
from lib.process import ProcessSupervisor


class IsolationTests(unittest.TestCase):
    def test_fresh_roots_and_environment(self):
        original = dict(os.environ)
        with Sandbox() as first, Sandbox() as second:
            self.assertNotEqual(first.root, second.root)
            env = first.environment(dict(original, MIYU_HOME='/production',
                MIYU_ONNXRUNTIME_LIB='/secret/lib', OPENAI_API_KEY='secret',
                ANTHROPIC_BASE_URL='https://production', XDG_CONFIG_HOME='/production'))
            self.assertEqual(env['MIYU_HOME'], str(first.root / 'miyu'))
            self.assertNotIn('MIYU_ONNXRUNTIME_LIB', env)
            self.assertNotIn('OPENAI_API_KEY', env)
            self.assertNotIn('ANTHROPIC_BASE_URL', env)
            self.assertTrue(env['XDG_CONFIG_HOME'].startswith(str(first.root)))
            self.assertEqual(env['CARGO_HOME'], original.get('CARGO_HOME', str(Path.home()/'.cargo')))
            self.assertEqual(env['RUSTUP_HOME'], original.get('RUSTUP_HOME', str(Path.home()/'.rustup')))
        self.assertFalse(first.root.exists())
        self.assertEqual(original, dict(os.environ))

    def test_wrong_marker_refuses_cleanup(self):
        box = Sandbox()
        good = box.marker.read_bytes()
        box.marker.write_text('{}')
        with self.assertRaises(OwnershipError):
            box.cleanup()
        self.assertTrue(box.root.exists())
        box.marker.write_bytes(good)
        box.cleanup()

    def test_existing_directory_cannot_be_adopted(self):
        with tempfile.TemporaryDirectory() as existing:
            with self.assertRaises(TypeError):
                Sandbox(root=Path(existing))
            self.assertTrue(Path(existing).exists())

    def test_symlink_inside_root_does_not_delete_target(self):
        with tempfile.TemporaryDirectory() as other:
            sentinel = Path(other) / 'keep'
            sentinel.write_text('untouched')
            with Sandbox() as box:
                (box.root / 'outside').symlink_to(other, target_is_directory=True)
            self.assertEqual(sentinel.read_text(), 'untouched')

    def test_replaced_root_refuses_cleanup(self):
        box = Sandbox()
        saved = box.root.with_name(box.root.name + '-saved')
        box.root.rename(saved)
        box.root.symlink_to(saved, target_is_directory=True)
        try:
            with self.assertRaises(OwnershipError):
                box.cleanup()
        finally:
            box.root.unlink()
            saved.rename(box.root)
            box.cleanup()

    def test_timeout_reaps_only_owned_process(self):
        unrelated = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            with Sandbox() as box, ProcessSupervisor() as supervisor:
                result = supervisor.run([sys.executable, '-c', 'import time; time.sleep(30)'],
                    env=box.environment(), cwd=box.root, timeout=0.15,
                    log=box.root/'timeout.txt')
                self.assertTrue(result['timed_out'])
                with self.assertRaises(ProcessLookupError):
                    os.kill(result['pid'], 0)
                self.assertIsNone(unrelated.poll())
        finally:
            unrelated.terminate()
            unrelated.wait(timeout=5)

    def test_unknown_and_unimplemented_suites_fail_closed(self):
        runner = Path(__file__).resolve().parents[1] / 'run_tests.py'
        with tempfile.TemporaryDirectory() as tmp:
            report_dir = Path(tmp)/'blocked'
            run = subprocess.run([sys.executable, str(runner), '--suite', 'voice-offline',
                '--report-dir', str(report_dir)], capture_output=True, timeout=10)
            self.assertEqual(run.returncode, 3, run.stderr)
            report = json.loads((report_dir/'report.json').read_text())
            self.assertEqual(report['status'], 'BLOCKED')
            self.assertEqual(report['commands'], [])
            unknown = subprocess.run([sys.executable, str(runner), '--suite', '__daemon',
                '--report-dir', str(Path(tmp)/'unknown')], capture_output=True, timeout=10)
            self.assertEqual(unknown.returncode, 2)
            relative = subprocess.run([sys.executable, str(runner), '--suite', 'installed-core',
                '--binary', 'miyu', '--report-dir', str(Path(tmp)/'relative')],
                capture_output=True, timeout=10)
            self.assertEqual(relative.returncode, 2)

    def test_tee_stderr_cannot_truncate_prior_log(self):
        with Sandbox() as box, ProcessSupervisor() as supervisor:
            log=box.root/'tee.txt'
            result=supervisor.run(['sh','-c',"printf 'before\\n'; printf 'during\\n' | tee /dev/stderr; printf 'after\\n'"],
                env=box.environment(),cwd=box.root,timeout=5,log=log)
            self.assertEqual(result['exit_code'],0)
            self.assertIn('before',log.read_text())
            self.assertIn('after',log.read_text())

    @unittest.skipUnless(sys.platform == 'linux', 'Linux subreaper contract')
    def test_adopted_zombie_reaped_while_suite_runs(self):
        code='''import os, time, sys
read, write = os.pipe()
parent = os.fork()
if parent == 0:
    child = os.fork()
    if child == 0:
        os._exit(0)
    os.write(write, str(child).encode())
    os._exit(0)
os.close(write)
pid = int(os.read(read, 100))
os.waitpid(parent, 0)
deadline=time.monotonic()+3
while time.monotonic()<deadline:
    try: os.kill(pid,0)
    except ProcessLookupError: sys.exit(0)
    time.sleep(.02)
sys.exit(1)
'''
        with Sandbox() as box, ProcessSupervisor() as supervisor:
            result=supervisor.run([sys.executable,'-c',code],env=box.environment(),
                cwd=box.root,timeout=5,log=box.root/'zombie.txt')
            self.assertEqual(result['exit_code'],0)

    @unittest.skipUnless(sys.platform == 'linux', 'Linux subreaper contract')
    def test_detached_daemon_is_reaped(self):
        code = '''import os, subprocess, sys
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], start_new_session=True)
print(child.pid, flush=True)
'''
        with Sandbox() as box, ProcessSupervisor() as supervisor:
            log = box.root/'detached.txt'
            result = supervisor.run([sys.executable, '-c', code], env=box.environment(),
                cwd=box.root, timeout=5, log=log)
            pid = int(log.read_text().strip())
            self.assertEqual(result['exit_code'], 0)
            self.assertIn(pid, [record['pid'] for record in result['reaped_descendants']])
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)


if __name__ == '__main__':
    unittest.main()
