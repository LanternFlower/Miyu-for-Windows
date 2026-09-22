"""Real Linux descendants exercise cleanup without readable environment data."""
import os
import json
import io
from pathlib import Path
import signal
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.isolation import Sandbox
from lib.process import ProcessSupervisor
from lib.reaper import OwnedReaper
import run_tests


@unittest.skipUnless(sys.platform == 'linux', 'Linux subreaper contract')
class ReaperTests(unittest.TestCase):
    def detached(self, *, dumpable, clear_environment, exit_child=False):
        # The pipe confirms the grandchild changed dumpability before its parent exits.
        code=f'''import os, subprocess, sys
from pathlib import Path
read, write = os.pipe()
child_code = """import ctypes, os, sys, time
assert ctypes.CDLL(None).prctl(4, {dumpable}, 0, 0, 0) == 0
os.write(int(sys.argv[1]), b'R')
os.close(int(sys.argv[1]))
{'os._exit(0)' if exit_child else 'time.sleep(30)'}
"""
child = subprocess.Popen([sys.executable, '-c', child_code, str(write)],
    pass_fds=(write,), start_new_session=True, env={{}} if {clear_environment} else None)
os.close(write)
assert os.read(read, 1) == b'R'
Path('descendant.pid').write_text(str(child.pid))
print(child.pid, flush=True)
'''
        with Sandbox() as box:
            log=box.root/'detached.txt'
            try:
                with ProcessSupervisor() as supervisor:
                    result=supervisor.run([sys.executable,'-c',code],env=box.environment(),
                        cwd=box.root,timeout=5,log=log)
                    pid=int(log.read_text().strip())
                    self.assertEqual(result['exit_code'],0)
                    with self.assertRaises(ProcessLookupError):
                        os.kill(pid,0)
            finally:
                # Red tests must also clean their intentionally escaped descendant.
                marker=box.root/'descendant.pid'
                if marker.exists():
                    pid=int(marker.read_text())
                    try:
                        # An unreaped direct child pins its PID. A completed
                        # successful cleanup must never signal a reused PID.
                        exited=os.waitid(os.P_PID,pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
                    except ChildProcessError:
                        pass
                    else:
                        if exited is None:
                            os.kill(pid,signal.SIGKILL)
                        os.waitpid(pid,0)

    def test_nondumpable_live_descendant_is_killed_and_reaped(self):
        self.detached(dumpable=0,clear_environment=False)

    def test_descendant_without_home_environment_is_killed_and_reaped(self):
        self.detached(dumpable=1,clear_environment=True)

    def test_nondumpable_exited_descendant_is_reaped(self):
        self.detached(dumpable=0,clear_environment=False,exit_child=True)

    def test_preexisting_child_with_same_home_is_not_signalled(self):
        with Sandbox() as box:
            unrelated=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'],
                env=box.environment(),start_new_session=True)
            try:
                with ProcessSupervisor() as supervisor:
                    result=supervisor.run([sys.executable,'-c','print("done")'],
                        env=box.environment(),cwd=box.root,timeout=5,log=box.root/'normal.txt')
                    self.assertEqual(result['exit_code'],0)
                    self.assertIsNone(unrelated.poll())
            finally:
                unrelated.terminate()
                unrelated.wait(timeout=5)

    def test_failed_command_and_cleanup_error_both_survive_report(self):
        with Sandbox() as outer:
            report_dir=outer.root/'report'
            def execute(suite,binary,box,supervisor,output,timeout,frozen):
                return supervisor.run([sys.executable,'-c',
                    'import sys; print("test result: FAILED. 1 failed"); sys.exit(7)'],
                    env=box.environment(),cwd=box.root,timeout=5,log=output/'tests.txt')
            with patch('run_tests.execute',side_effect=execute), \
                    patch('lib.reaper.OwnedReaper.reap',side_effect=PermissionError('denied cleanup')), \
                    patch('sys.stdout',io.StringIO()), \
                    patch('sys.argv',['run_tests.py','--suite','source-unit',
                                      '--report-dir',str(report_dir)]):
                self.assertEqual(run_tests.main(),1)
            report=json.loads((report_dir/'report.json').read_text())
            self.assertEqual(report['status'],'FAIL')
            self.assertEqual(len(report['commands']),1)
            self.assertEqual(report['commands'][0]['exit_code'],7)
            self.assertIn('denied cleanup',report['commands'][0]['cleanup_error'])
            self.assertIn('test result: FAILED.',(report_dir/'tests.txt').read_text())

    def test_signal_permission_failure_is_not_reported_as_cleanup_success(self):
        reaper=OwnedReaper()
        child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
        try:
            with patch('lib.reaper.os.kill',side_effect=PermissionError('denied signal')):
                with self.assertRaisesRegex(PermissionError,'denied signal'):
                    reaper.reap('unused')
            self.assertIsNone(child.poll())
        finally:
            reaper.reap('unused')
            child.wait(timeout=5)
            reaper.close()
