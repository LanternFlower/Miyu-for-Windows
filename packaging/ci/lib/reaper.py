"""Reap Linux double-forked test daemons without scanning unrelated instances."""
import ctypes
import os
from pathlib import Path
import signal
import sys
import time


class OwnedReaper:
    def __init__(self):
        self.enabled = sys.platform == 'linux'
        self.previous = ctypes.c_int()
        self.preexisting = self._children() if self.enabled else set()
        if self.enabled:
            self.libc = ctypes.CDLL(None, use_errno=True)
            if self.libc.prctl(37, ctypes.byref(self.previous), 0, 0, 0) != 0:
                raise OSError(ctypes.get_errno(), 'Cannot query child subreaper')
            if self.libc.prctl(36, 1, 0, 0, 0) != 0:
                raise OSError(ctypes.get_errno(), 'Cannot enable child subreaper')

    @staticmethod
    def _children():
        return {int(value) for value in Path(f'/proc/self/task/{os.getpid()}/children').read_text().split()}

    def reap_exited(self, protected):
        """Behave like init for adopted zombies while the suite is still running."""
        if not self.enabled:
            return
        for pid in self._children() - self.preexisting - set(protected):
            try:
                result = os.waitid(os.P_PID,pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
                if result is not None:
                    os.waitpid(pid,0)
            except (ProcessLookupError,ChildProcessError):
                pass

    def reap(self, home):
        if not self.enabled:
            return []
        records = []
        children = Path(f'/proc/self/task/{os.getpid()}/children')
        expected = f'MIYU_HOME={home}'.encode()
        deadline = time.monotonic() + 5
        while True:
            owned = []
            for value in children.read_text().split():
                pid = int(value)
                try:
                    # Only direct children adopted by this harness, matched to its exact home.
                    with open(f'/proc/{pid}/environ', 'rb') as stream:
                        environment = stream.read().split(b'\0')
                    if expected not in environment:
                        continue
                    identity = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
                    owned.append((pid, identity))
                except (FileNotFoundError,ProcessLookupError):
                    continue
            if not owned:
                break
            for pid, identity in owned:
                # Unreaped direct children cannot have their PID reused.
                os.kill(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
                records.append({'pid': pid, 'start_ticks': identity})
            if time.monotonic() > deadline:
                raise RuntimeError('Test descendants did not terminate within cleanup deadline.')
        self.reap_exited(())
        return records

    def close(self):
        if self.enabled:
            if self.libc.prctl(36, self.previous.value, 0, 0, 0) != 0:
                raise OSError(ctypes.get_errno(), 'Cannot restore child subreaper')
