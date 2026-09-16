//! Missing process sandbox backend. A requested policy must refuse execution.

pub(super) fn probe() -> Option<i64> {
    None
}

pub(super) fn apply() -> std::io::Result<()> {
    // Raw errno construction is safe after fork. Do not allocate or take locks.
    Err(std::io::Error::from_raw_os_error(libc::ENOTSUP))
}
