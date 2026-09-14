"""Bounded argv execution. Own process groups, never signal by program name."""
import os
from pathlib import Path
import signal
import subprocess
import shutil
import threading
import time
from .reaper import OwnedReaper


class ProcessSupervisor:
    def __init__(self):
        self.children = []
        self.results = []
        self.reaper = OwnedReaper()
        self.homes = set()

    def run(self, argv, *, env, cwd, timeout, log):
        if timeout <= 0:
            raise ValueError('Timeout must be positive.')
        if env.get('MIYU_HOME'):
            self.homes.add(env['MIYU_HOME'])
        with Path(log).open('wb') as output:
            child = subprocess.Popen(argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
            # A pipe prevents tools using `tee /dev/stderr` from truncating a file
            # descriptor inherited directly from this logger.
            reader = threading.Thread(target=shutil.copyfileobj, args=(child.stdout,output), daemon=True)
            reader.start()
            self.children.append(child)
            result = {'command': list(map(str, argv)), 'pid': child.pid,
                      'started_monotonic_ns': time.monotonic_ns(), 'timed_out': False,
                      'log': str(log)}
            self.results.append(result)
            # A live unreaped child pins its PID; it cannot be reused during cleanup.
            try:
                deadline = time.monotonic() + timeout
                while os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is None:
                    self.reaper.reap_exited([entry.pid for entry in self.children])
                    if time.monotonic() >= deadline:
                        result['timed_out'] = True
                        break
                    time.sleep(0.02)
            finally:
                # Also stop workers still in the owned group after parent exit.
                try:
                    self._stop(child)
                    self.children.remove(child)
                    result['exit_code'] = child.returncode
                    result['reaped_descendants'] = self.reaper.reap(env.get('MIYU_HOME', ''))
                except (OSError, RuntimeError, subprocess.SubprocessError) as error:
                    result['cleanup_error'] = str(error)
                    raise
                finally:
                    reader.join(timeout=5)
                    if reader.is_alive():
                        error = 'A test descendant retained the output pipe after cleanup.'
                        result.setdefault('cleanup_error', error)
                        raise RuntimeError(error)
                    child.stdout.close()
            result['exit_code'] = child.returncode
            return result

    @staticmethod
    def _stop(child):
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait(timeout=5)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        try:
            for child in self.children:
                self._stop(child)
            self.children.clear()
            for home in self.homes:
                self.reaper.reap(home)
        finally:
            self.reaper.close()
