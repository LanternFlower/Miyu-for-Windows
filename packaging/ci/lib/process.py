"""Bounded argv execution. Own process groups, never signal by program name."""
import os
from pathlib import Path
import signal
import subprocess
import shutil
import threading
import time
from .reaper import OwnedReaper, exited_without_reaping


def _alive(pid):
    """只做诊断：这个 PID 还在不在（僵尸也算在）。"""
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        # 存在，但不是我们能发信号的——PID 被复用成别人的进程时就是这个样子。
        return 'exists-but-not-ours'


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
                while not exited_without_reaping(child.pid):
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
        # 出错时要说清是哪一步：2026-09-22 macOS CI 只报了「[Errno 1] Operation
        # not permitted」，光这一句分不出是 killpg 还是 wait，也看不出当时这个
        # 子进程是死是活。诊断信息比省几行代码值钱。
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError as error:
            raise OSError(
                error.errno,
                f'killpg(pid={child.pid}, SIGKILL) failed: {error.strerror}; '
                f'returncode={child.returncode}, alive={_alive(child.pid)}',
            ) from error
        try:
            child.wait(timeout=5)
        except OSError as error:
            raise OSError(
                error.errno,
                f'wait(pid={child.pid}) failed: {error.strerror}',
            ) from error

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
