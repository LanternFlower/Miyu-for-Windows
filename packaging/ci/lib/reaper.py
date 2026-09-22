"""Reap Linux double-forked test daemons without scanning unrelated instances."""
import ctypes
import errno
import os
from pathlib import Path
import signal
import sys
import time


# ── 「已退出但还没回收」怎么问 ──────────────────────────────────────────────
#
# 监督进程靠 `waitid(..., WNOWAIT)` 轮询子进程：WNOWAIT 保证**不回收**，于是那个
# PID 一直被僵尸态钉着，清理期间不可能被别的进程复用。换成 `poll()` / `waitpid()`
# 就废了这条不变量——它们会当场回收，PID 立刻可被复用。
#
# 但 CPython **在 macOS 上不导出 `os.waitid`**（系统本身有 `waitid(2)`，只是没编
# 进 os 模块），连 `os.P_PID` `os.WNOWAIT` 这些常量一起没有。2026-09-22 加上
# macOS CI 的第二次运行就撞在这儿：
#
#     AttributeError: module 'os' has no attribute 'waitid'
#
# 所以那条路直接调 libc 的同一个系统调用，语义逐位一致，不换语义只换入口。
if hasattr(os, 'waitid'):
    def exited_without_reaping(pid):
        return os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None
else:
    # 取值按 **macOS 的 sys/wait.h**（这一支只在没有 os.waitid 时才走，即 macOS）。
    # 别照搬到 Linux：那边 WNOWAIT 是 0x01000000，不是 0x20。
    _P_PID = 1
    _WNOHANG, _WEXITED, _WNOWAIT = 0x01, 0x04, 0x20

    class _SigInfo(ctypes.Structure):
        # 只读前六个字段；尾巴按 macOS 的 siginfo_t 留足（si_addr / si_value /
        # si_band / __pad[7]），免得内核写越界。
        _fields_ = [('si_signo', ctypes.c_int), ('si_errno', ctypes.c_int),
                    ('si_code', ctypes.c_int), ('si_pid', ctypes.c_int),
                    ('si_uid', ctypes.c_uint), ('si_status', ctypes.c_int),
                    ('_tail', ctypes.c_byte * 128)]

    _libc = ctypes.CDLL(None, use_errno=True)

    def exited_without_reaping(pid):
        info = _SigInfo()
        ctypes.set_errno(0)
        code = _libc.waitid(_P_PID, pid, ctypes.byref(info),
                            _WEXITED | _WNOHANG | _WNOWAIT)
        if code != 0:
            number = ctypes.get_errno()
            if number == errno.ECHILD:
                # 连这个子进程都没有了，当已退出处理；调用方随后自己 wait。
                return True
            raise OSError(number, os.strerror(number))
        # WNOHANG 下没有子进程改变状态时返回 0 且把 si_pid 留成 0。
        return info.si_pid != 0


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
                if exited_without_reaping(pid):
                    os.waitpid(pid,0)
            except (ProcessLookupError,ChildProcessError):
                pass

    def reap(self, home):
        """Stop adopted descendants. Environment values do not prove ownership."""
        if not self.enabled:
            return []
        records = []
        deadline = time.monotonic() + 5
        while True:
            owned = []
            # The supervisor launches serially. Its subreaper boundary owns new
            # direct children, including detached/execed/cleared-environment ones.
            # Preexisting children remain outside that boundary, even if their
            # environment happens to contain the same MIYU_HOME.
            for pid in self._children() - self.preexisting:
                try:
                    identity = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
                    owned.append((pid, identity))
                except (FileNotFoundError,ProcessLookupError):
                    continue
            if not owned:
                break
            for pid, identity in owned:
                # Unreaped direct children cannot have their PID reused.
                os.kill(pid, signal.SIGKILL)
                while os.waitpid(pid, os.WNOHANG) == (0, 0):
                    if time.monotonic() > deadline:
                        raise RuntimeError('Test descendants did not terminate within cleanup deadline.')
                    time.sleep(.01)
                records.append({'pid': pid, 'start_ticks': identity})
            if time.monotonic() > deadline:
                raise RuntimeError('Test descendants did not terminate within cleanup deadline.')
        self.reap_exited(())
        return records

    def close(self):
        if self.enabled:
            if self.libc.prctl(36, self.previous.value, 0, 0, 0) != 0:
                raise OSError(ctypes.get_errno(), 'Cannot restore child subreaper')
