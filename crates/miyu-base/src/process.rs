//! 进程级的小工具(09-16 从 runtime 下沉:配置目录缓存这种底层模块也要用,不能反向认识 runtime)。

/// 把 glibc arena 里已释放的整段内存还给操作系统。大块临时解析（models.dev
/// 目录、整回合请求体）释放后 glibc 默认把页留在 arena 里，RSS 不降。
/// 非 glibc 分配器下是无害空转。
#[cfg(target_os = "linux")]
pub fn trim_process_memory() {
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn trim_process_memory() {}
