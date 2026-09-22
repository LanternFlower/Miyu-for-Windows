//! Linux Landlock backend. All Linux-only constants and syscalls live here.

use super::SandboxPolicy;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// 内核能不能用:`Some(abi)` 能(ABI 号,>= 3 算完整),`None` 不能。启动时记日志用。
pub(super) fn probe() -> Option<i64> {
    let abi = unsafe {
        libc::syscall(
            NR_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (abi > 0).then_some(abi as i64)
}

// ── Landlock UAPI,本地定义(内核 ABI 稳定;与 dsh landlock-run 逐字一致) ──

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const NR_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const NR_LANDLOCK_ADD_RULE: libc::c_long = 445;
const NR_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REFER: u64 = 1 << 13; // ABI 2
const FS_TRUNCATE: u64 = 1 << 14; // ABI 3
const FS_IOCTL_DEV: u64 = 1 << 15; // ABI 5
const ABI1_MASK: u64 = FS_REFER - 1;
const MAX_ABI: i64 = 5;

fn fs_mask_for_abi(abi: i64) -> u64 {
    let mut mask = ABI1_MASK;
    if abi >= 2 {
        mask |= FS_REFER;
    }
    if abi >= 3 {
        mask |= FS_TRUNCATE;
    }
    if abi >= 5 {
        mask |= FS_IOCTL_DEV;
    }
    mask
}

/// fork 前就把路径转成 C 字符串:`pre_exec` 里不该再分配。
pub(super) struct Rules {
    read_only: Vec<CString>,
    read_write: Vec<CString>,
}

impl Rules {
    pub(super) fn prepare(policy: &SandboxPolicy) -> Self {
        let to_c = |paths: &[PathBuf]| {
            paths
                .iter()
                .filter_map(|path| CString::new(path.as_os_str().as_bytes()).ok())
                .collect::<Vec<_>>()
        };
        Self {
            read_only: to_c(&policy.read_only),
            read_write: to_c(&policy.read_write),
        }
    }

    /// 子进程里跑:建规则集 → 逐条加路径 → no_new_privs → 套到自己身上。
    /// 出错就返回 errno 风格的 io::Error(不分配),spawn 随之失败。
    pub(super) fn apply(&self) -> std::io::Result<()> {
        let abi = unsafe {
            libc::syscall(
                NR_LANDLOCK_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        };
        if abi <= 0 {
            // ENOSYS:内核没编 Landlock;EOPNOTSUPP:编了但没启用。失败关闭。
            return Err(std::io::Error::from_raw_os_error(libc::ENOSYS));
        }
        let handled = fs_mask_for_abi((abi as i64).min(MAX_ABI));
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        let ruleset_fd = unsafe {
            libc::syscall(
                NR_LANDLOCK_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        } as libc::c_int;
        if ruleset_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let read_side = (FS_EXECUTE | FS_READ_FILE | FS_READ_DIR) & handled;
        for path in &self.read_only {
            add_rule(ruleset_fd, path, read_side)?;
        }
        for path in &self.read_write {
            add_rule(ruleset_fd, path, handled)?;
        }
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::syscall(NR_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        unsafe { libc::close(ruleset_fd) };
        Ok(())
    }
}

fn add_rule(ruleset_fd: libc::c_int, path: &CString, mut access: u64) -> std::io::Result<()> {
    let path_fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if path_fd < 0 {
        // 授权根打不开:失败关闭,不静默缩小授权集。
        return Err(std::io::Error::last_os_error());
    }
    // 非目录只能带文件类的权限位(内核对目录专属位报 EINVAL)——`/dev/null` 这类
    // 授权靠它。
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(path_fd, &mut st) } == 0 && (st.st_mode & libc::S_IFMT) != libc::S_IFDIR
    {
        access &= FS_EXECUTE | FS_WRITE_FILE | FS_READ_FILE | FS_TRUNCATE | FS_IOCTL_DEV;
    }
    let attr = PathBeneathAttr {
        allowed_access: access,
        parent_fd: path_fd,
    };
    let rc = unsafe {
        libc::syscall(
            NR_LANDLOCK_ADD_RULE,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &attr as *const PathBeneathAttr,
            0u32,
        )
    };
    let error = (rc != 0).then(std::io::Error::last_os_error);
    unsafe { libc::close(path_fd) };
    match error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
