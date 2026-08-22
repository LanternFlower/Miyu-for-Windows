//! 发布安装：目录交换与私有目录创建。
//!
//! 交换而不是逐文件覆盖，中途崩掉时技能要么是旧版要么是新版，不会是一半。
//! `StagedDirectory` 的 `Drop` 清理没交换成的暂存目录。

use crate::skills::draft::*;

pub(crate) fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let mut stats = PackageStats {
        directories: 1,
        ..PackageStats::default()
    };
    fs::create_dir(destination)?;
    let result = (|| {
        secure_directory(destination)?;
        copy_tree_inner(source, destination, 0, &mut stats)?;
        if let Some(parent) = destination.parent() {
            crate::sys::sync_parent(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

pub(crate) fn copy_tree_inner(
    source: &Path,
    destination: &Path,
    depth: usize,
    stats: &mut PackageStats,
) -> Result<()> {
    if depth > MAX_SKILL_PACKAGE_DEPTH {
        bail!("skill package exceeds the directory depth limit");
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        let target = destination.join(entry.file_name());
        if metadata.file_type().is_symlink() {
            bail!("skill packages may not contain symbolic links");
        }
        if metadata.is_dir() {
            stats.directories += 1;
            if stats.directories > MAX_SKILL_PACKAGE_DIRS {
                bail!("skill package exceeds the directory-count limit");
            }
            fs::create_dir(&target)?;
            secure_directory(&target)?;
            copy_tree_inner(&entry.path(), &target, depth + 1, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(metadata.len())
                .context("skill package size overflow")?;
            if stats.files > MAX_SKILL_PACKAGE_FILES || stats.bytes > MAX_SKILL_PACKAGE_BYTES {
                bail!("skill package exceeds file-count or total-size limits");
            }
            fs::copy(entry.path(), &target)?;
            File::options().read(true).write(true).open(&target)?.sync_all()?;
        } else {
            bail!("skill package contains an unsupported file type");
        }
    }
    crate::sys::sync_parent(destination)?;
    Ok(())
}

pub(crate) fn ensure_directory_chain(base: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(base)
        .with_context(|| format!("path escapes skill root: {}", directory.display()))?;
    let mut current = base.to_path_buf();
    let metadata = fs::symlink_metadata(&current)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "skill path contains an unsafe directory: {}",
            current.display()
        );
    }
    for component in relative.components() {
        current.push(component);
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "skill path contains an unsafe directory: {}",
                current.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn create_private_directory_chain(base: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(base)
        .with_context(|| format!("path escapes skill root: {}", directory.display()))?;
    secure_directory(base)?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => secure_directory(&current)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                secure_directory(&current)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) struct StagedDirectory {
    pub(crate) path: PathBuf,
    pub(crate) armed: bool,
}

impl StagedDirectory {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(crate) fn exchange_skill_directories(staged: &Path, target: &Path) -> Result<()> {
    exchange_directories(staged, target).with_context(|| {
        format!(
            "atomically exchanging skill directories {} and {}",
            staged.display(),
            target.display()
        )
    })?;
    if let Some(parent) = target.parent() {
        if let Err(error) = crate::sys::sync_parent(parent) {
            tracing::warn!(path = %parent.display(), error = %error, "failed to sync updated skill directory");
        }
    }
    Ok(())
}

pub(crate) fn install_updated_skill(
    staged: &Path,
    target: &Path,
    expected_revision: &str,
    staged_guard: &mut StagedDirectory,
) -> Result<()> {
    exchange_skill_directories(staged, target)?;
    let replaced_revision = skill_revision(staged);
    let replaced_matches = matches!(
        replaced_revision.as_deref(),
        Ok(revision) if revision == expected_revision
    );
    if !replaced_matches {
        if let Err(rollback_error) = exchange_skill_directories(staged, target) {
            staged_guard.disarm();
            bail!(
                "live skill changed during publication and rollback failed; the old version is preserved at {}: {rollback_error:#}",
                staged.display()
            );
        }
        bail!("skill changed during publication; the live version was restored");
    }
    if let Err(error) = fs::remove_dir_all(staged) {
        tracing::warn!(path = %staged.display(), error = %error, "failed to remove replaced skill directory");
    }
    staged_guard.disarm();
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn install_new_skill(staged: &Path, target: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, staged, target) = exchange_operands(staged, target)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            staged.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn install_new_skill(staged: &Path, target: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, staged, target) = exchange_operands(staged, target)?;
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            staged.as_ptr(),
            parent.as_raw_fd(),
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn install_new_skill(staged: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        bail!("skill already exists; create never overwrites");
    }
    fs::rename(staged, target)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn exchange_directories(left: &Path, right: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, left, right) = exchange_operands(left, right)?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            left.as_ptr(),
            parent.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn exchange_directories(left: &Path, right: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;

    let (parent, left, right) = exchange_operands(left, right)?;
    let result = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            left.as_ptr(),
            parent.as_raw_fd(),
            right.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn exchange_directories(left: &Path, right: &Path) -> Result<()> {
    let parent = left.parent().context("staged skill has no parent")?;
    if right.parent() != Some(parent) {
        bail!("atomic skill exchange requires a shared parent directory");
    }
    let temporary = parent.join(format!(
        ".miyu-skill-swap-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    fs::rename(right, &temporary)?;
    if let Err(error) = fs::rename(left, right) {
        let _ = fs::rename(&temporary, right);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temporary, left) {
        let _ = fs::rename(right, &temporary);
        let _ = fs::rename(left, right);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn exchange_operands(
    left: &Path,
    right: &Path,
) -> Result<(File, std::ffi::CString, std::ffi::CString)> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = left.parent().context("staged skill has no parent")?;
    if right.parent() != Some(parent) {
        bail!("atomic skill exchange requires a shared parent directory");
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let parent = options.open(parent)?;
    let left = std::ffi::CString::new(
        left.file_name()
            .context("staged skill has no file name")?
            .as_bytes(),
    )?;
    let right = std::ffi::CString::new(
        right
            .file_name()
            .context("target skill has no file name")?
            .as_bytes(),
    )?;
    Ok((parent, left, right))
}

pub(crate) struct PublishLease {
    pub(crate) _file: File,
}

pub(crate) fn acquire_publish_lock(paths: &MiyuPaths) -> Result<PublishLease> {
    let root = paths.skill_drafts_dir();
    create_private_dir(&root)?;
    let lock_path = root.join(PUBLISH_LOCK_FILE);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("skill publish lock path is unsafe: {}", lock_path.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    crate::sys::apply_mode(&mut options, 0o600);
    let file = options.open(&lock_path)?;
    crate::sys::lock_exclusive(&file)?;
    Ok(PublishLease { _file: file })
}

pub(crate) fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    secure_directory(path)
}

pub(crate) fn secure_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("expected a regular directory: {}", path.display());
    }
    crate::sys::set_mode(path, 0o700)?;
    Ok(())
}

pub(crate) fn write_private_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    crate::sys::apply_mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}
