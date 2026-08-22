//! 旧目录布局的迁移。
//!
//! 和 [`super::resource_migration`] 是两代方案，形状类似但校验更细：这一代要
//! 处理映射之间的嵌套与冲突（`ensure_nested_mapping_compatible`、
//! `ensure_no_conflicts`），因为旧布局里几个目录可能互为父子。
//!
//! 符号链接单独校验（`ensure_absolute_symlink_targets_stable`）：搬完之后指向
//! 绝对路径的链接会失效，得先确认它们指的地方不受影响。

use crate::paths::*;

pub(crate) const LAYOUT_MARKER: &str = ".layout-v1";

#[derive(Clone, Debug)]
pub(crate) struct LegacyLayout {
    pub(crate) config_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) documents_dir: PathBuf,
    pub(crate) pictures_dirs: Vec<PathBuf>,
}

impl LegacyLayout {
    pub(crate) fn exists(&self) -> Result<bool> {
        for path in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.state_dir,
            &self.documents_dir,
        ]
        .into_iter()
        .chain(self.pictures_dirs.iter())
        {
            if entry_exists(path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub(crate) root_dir: PathBuf,
    pub(crate) config_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) state_dir: PathBuf,
}

impl Layout {
    pub(crate) fn marker(&self) -> PathBuf {
        self.root_dir.join(LAYOUT_MARKER)
    }

    pub(crate) fn resource_marker(&self) -> PathBuf {
        self.root_dir.join(RESOURCE_LAYOUT_MARKER)
    }

    pub(crate) fn resource_journal(&self) -> PathBuf {
        self.root_dir.join(RESOURCE_MIGRATION_JOURNAL)
    }
}

pub(crate) fn current_process_is_daemon() -> bool {
    std::env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == "__daemon")
}

pub(crate) fn daemon_is_running_at(runtime_dir: &Path, current_process_is_daemon: bool) -> bool {
    sys::ipc_probe(&runtime_dir.join("core.sock"))
        || runtime_lock_is_held(&runtime_dir.join("core.lock"))
        || (!current_process_is_daemon && runtime_lock_is_held(&runtime_dir.join("starter.lock")))
}

pub(crate) fn marker_exists_at(path: &Path, label: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "Miyu {label} must not be a symbolic link: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            bail!("Miyu {label} is not a regular file: {}", path.display())
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn try_acquire_runtime_lock(path: &Path) -> Result<Option<File>> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    sys::apply_mode(&mut options, 0o600);
    let file = options.open(path)?;
    Ok(if sys::try_lock_exclusive(&file)? {
        Some(file)
    } else {
        None
    })
}

#[derive(Clone, Debug)]
pub(crate) struct MigrationMapping {
    pub(crate) source: PathBuf,
    pub(crate) destination: PathBuf,
}

impl MigrationMapping {
    pub(crate) fn new(source: impl Into<PathBuf>, destination: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
        }
    }
}

pub(crate) struct MigrationLease(File);

impl Drop for MigrationLease {
    fn drop(&mut self) {
        sys::unlock(&self.0);
    }
}

pub(crate) fn legacy_daemon_is_running(legacy: &LegacyLayout) -> bool {
    legacy_daemon_is_running_at(
        legacy,
        std::env::var_os("XDG_RUNTIME_DIR")
            .as_deref()
            .map(Path::new),
    )
}

pub(crate) fn legacy_daemon_is_running_at(legacy: &LegacyLayout, xdg_runtime_dir: Option<&Path>) -> bool {
    let mut runtime_dirs = vec![legacy.state_dir.clone()];
    if let Some(runtime_dir) = xdg_runtime_dir {
        runtime_dirs.push(runtime_dir.to_path_buf());
    }
    runtime_dirs
        .into_iter()
        .map(|runtime_dir| runtime_dir.join("miyu"))
        .any(|runtime_dir| {
            sys::ipc_probe(&runtime_dir.join("core.sock"))
                || runtime_lock_is_held(&runtime_dir.join("core.lock"))
                || runtime_lock_is_held(&runtime_dir.join("starter.lock"))
        })
}

pub(crate) fn runtime_lock_is_held(path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        // Failure to inspect an existing lock must not authorize migration.
        Err(_) => return true,
    };
    match sys::try_lock_exclusive(&file) {
        Ok(true) => {
            sys::unlock(&file);
            false
        }
        _ => true,
    }
}

pub(crate) fn migrate_legacy_layout(legacy: &LegacyLayout, next: &Layout) -> Result<()> {
    if layout_marker_exists(next)? {
        return Ok(());
    }
    let mappings = legacy_migration_mappings(legacy, next);
    preflight_with_disposable_cache(&mappings, &legacy.cache_dir)?;

    ensure_private_dir(&next.root_dir)?;
    for directory in [
        &next.config_dir,
        &next.data_dir,
        &next.cache_dir,
        &next.state_dir,
    ] {
        ensure_existing_directory(directory)?;
    }
    let _lease = acquire_migration_lock(&next.root_dir)?;
    if layout_marker_exists(next)? {
        return Ok(());
    }
    // Repeat the full preflight while holding the layout lock. The first pass
    // guarantees a known conflict does not even create ~/.miyu; this pass
    // closes the race with another new Miyu process before any data moves.
    let active = preflight_with_disposable_cache(&mappings, &legacy.cache_dir)?;
    let next_bash_hook = next.config_dir.join("shell/bash-hook.sh");
    let next_zsh_hook = next.config_dir.join("shell/zsh-hook.zsh");
    let had_bash_hook = entry_exists(&legacy.config_dir.join("shell/bash-hook.sh"))?
        || entry_exists(&next_bash_hook)?;
    let had_zsh_hook = entry_exists(&legacy.config_dir.join("shell/zsh-hook.zsh"))?
        || entry_exists(&next_zsh_hook)?;
    for mapping in &active {
        migrate_entry_unchecked(&mapping.source, &mapping.destination).with_context(|| {
            format!(
                "migrating Miyu user files from {} to {}",
                mapping.source.display(),
                mapping.destination.display()
            )
        })?;
    }

    if had_bash_hook || had_zsh_hook {
        let home = next
            .root_dir
            .parent()
            .context("the Miyu home directory has no parent")?;
        let bash_hook = had_bash_hook.then_some(next_bash_hook);
        let zsh_hook = had_zsh_hook.then_some(next_zsh_hook);
        crate::shell::refresh_migrated_hook_sources(
            home,
            bash_hook.as_deref(),
            zsh_hook.as_deref(),
        )
        .context("refreshing shell hook paths after Miyu directory migration")?;
    }

    write_marker(&next.marker())?;
    Ok(())
}

pub(crate) fn legacy_migration_mappings(legacy: &LegacyLayout, next: &Layout) -> Vec<MigrationMapping> {
    let mut mappings = vec![
        MigrationMapping::new(&legacy.config_dir, &next.config_dir),
        MigrationMapping::new(&legacy.data_dir, &next.data_dir),
        MigrationMapping::new(&legacy.cache_dir, &next.cache_dir),
        MigrationMapping::new(&legacy.documents_dir, next.data_dir.join("documents")),
    ];
    // Older installations may have no XDG state directory and therefore use
    // the data directory for both values. Mapping the same source twice would
    // make the migration appear self-conflicting; keep its contents under the
    // unified data directory and leave the new state directory empty.
    if !same_platform_path(&legacy.state_dir, &legacy.data_dir) {
        mappings.push(MigrationMapping::new(&legacy.state_dir, &next.state_dir));
    }
    for source in &legacy.pictures_dirs {
        // `Pictures/miyu` and `Pictures/Miyu` are separate legacy candidates
        // on Unix but the same directory on case-insensitive Windows volumes.
        if mappings
            .iter()
            .any(|mapping| same_platform_path(&mapping.source, source))
        {
            continue;
        }
        mappings.push(MigrationMapping::new(
            source,
            next.data_dir.join("pictures"),
        ));
    }
    mappings
}

fn same_platform_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

pub(crate) fn existing_mappings(mappings: &[MigrationMapping]) -> Result<Vec<MigrationMapping>> {
    let mut active = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        if mapping.source == mapping.destination || !entry_exists(&mapping.source)? {
            continue;
        }
        if mapping.destination.starts_with(&mapping.source)
            || mapping.source.starts_with(&mapping.destination)
        {
            bail!(
                "Miyu directory migration cannot move overlapping paths: {} and {}",
                mapping.source.display(),
                mapping.destination.display()
            );
        }
        active.push(mapping.clone());
    }
    for (index, left) in active.iter().enumerate() {
        for right in active.iter().skip(index + 1) {
            if left.source == right.source
                || left.source.starts_with(&right.source)
                || right.source.starts_with(&left.source)
            {
                bail!(
                    "Miyu directory migration has overlapping legacy sources: {} and {}",
                    left.source.display(),
                    right.source.display()
                );
            }
        }
    }
    Ok(active)
}

pub(crate) fn entry_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn acquire_migration_lock(root: &Path) -> Result<MigrationLease> {
    // Lock the directory itself so a failed preflight never leaves a lock file
    // behind in an otherwise untouched destination layout.
    let file = sys::lock_directory_exclusive(root)
        .with_context(|| format!("opening migration lock directory {}", root.display()))?;
    Ok(MigrationLease(file))
}

pub(crate) fn ensure_private_dir(path: &Path) -> Result<()> {
    ensure_existing_directory(path)?;
    if !entry_exists(path)? {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        builder
            .create(path)
            .with_context(|| format!("creating {}", path.display()))?;
    }
    ensure_existing_directory(path)?;
    sys::set_mode(path, 0o700).with_context(|| format!("securing {}", path.display()))?;
    Ok(())
}

pub(crate) fn ensure_existing_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!(
            "Miyu refuses to use a symbolic-link directory: {}",
            path.display()
        ),
        Ok(metadata) if !metadata.is_dir() => bail!(
            "Miyu expected a directory but found another file: {}",
            path.display()
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn layout_marker_exists(layout: &Layout) -> Result<bool> {
    let marker = layout.marker();
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!(
            "Miyu layout marker must not be a symbolic link: {}",
            marker.display()
        ),
        Ok(metadata) if !metadata.is_file() => bail!(
            "Miyu layout marker is not a regular file: {}",
            marker.display()
        ),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn write_marker(path: &Path) -> Result<()> {
    let temporary = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    sys::apply_mode(&mut options, 0o600);
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(b"1\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("installing migration marker {}", path.display()))?;
    sync_parent(path)?;
    Ok(())
}

pub(crate) fn migrate_entry(source: &Path, destination: &Path) -> Result<()> {
    let mappings = existing_mappings(&[MigrationMapping::new(source, destination)])?;
    preflight_mappings(&mappings)?;
    let Some(mapping) = mappings.first() else {
        return Ok(());
    };
    migrate_entry_unchecked(&mapping.source, &mapping.destination)
}

/// Runs `existing_mappings` + `preflight_mappings`, except that the cache is
/// treated as disposable: caches routinely contain relative symlinks (for
/// example HuggingFace-style blob layouts), and refusing to move one would
/// otherwise brick startup forever over data Miyu can rebuild. When the cache
/// tree alone fails preflight it is discarded and dropped from the migration
/// instead of failing it.
pub(crate) fn preflight_with_disposable_cache(
    mappings: &[MigrationMapping],
    legacy_cache: &Path,
) -> Result<Vec<MigrationMapping>> {
    let mut active = existing_mappings(mappings)?;
    if let Some(index) = active
        .iter()
        .position(|mapping| mapping.source == legacy_cache)
    {
        let projections = active
            .iter()
            .map(|mapping| (mapping.source.clone(), mapping.destination.clone()))
            .collect::<Vec<_>>();
        let cache = &active[index];
        let migratable = ensure_supported_entry_tree(&cache.source)
            .and_then(|_| ensure_absolute_symlink_targets_stable(&cache.source, &projections))
            .and_then(|_| ensure_no_conflicts(&cache.source, &cache.destination));
        if let Err(reason) = migratable {
            eprintln!(
                "{}: {reason:#}",
                t(
                    "discarding the legacy Miyu cache instead of migrating it",
                    "旧版 Miyu 缓存无法迁移，已直接丢弃"
                )
            );
            discard_legacy_cache(&cache.source);
            active.remove(index);
        }
    }
    preflight_mappings(&active)?;
    Ok(active)
}

/// Best effort: a leftover legacy cache is only cold-start overhead, never a
/// reason to fail the migration. `symlink_metadata` keeps a symlinked cache
/// directory from being deleted through the link.
pub(crate) fn discard_legacy_cache(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let _ = if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

pub(crate) fn preflight_mappings(mappings: &[MigrationMapping]) -> Result<()> {
    let projections = mappings
        .iter()
        .map(|mapping| (mapping.source.clone(), mapping.destination.clone()))
        .collect::<Vec<_>>();
    for mapping in mappings {
        ensure_supported_entry_tree(&mapping.source)?;
        ensure_absolute_symlink_targets_stable(&mapping.source, &projections)?;
        ensure_no_conflicts(&mapping.source, &mapping.destination)?;
    }
    for (index, left) in mappings.iter().enumerate() {
        for right in mappings.iter().skip(index + 1) {
            ensure_mapping_pair_compatible(left, right)?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_supported_entry_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        if target.is_relative() {
            bail!(
                "Miyu directory migration refuses relative symbolic link {}; its target would change after moving",
                path.display()
            );
        }
        return Ok(());
    }
    if metadata.is_file() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            ensure_supported_entry_tree(&entry?.path())?;
        }
        return Ok(());
    }
    bail!("unsupported file type while migrating {}", path.display())
}

pub(crate) fn ensure_absolute_symlink_targets_stable(
    path: &Path,
    projections: &[(PathBuf, PathBuf)],
) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        if target.is_absolute() {
            for (source, destination) in projections {
                if let Ok(relative) = target.strip_prefix(source) {
                    bail!(
                        "Miyu directory migration refuses symbolic link {} because its absolute target moves from {} to {}",
                        path.display(),
                        target.display(),
                        destination.join(relative).display()
                    );
                }
            }
        }
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            ensure_absolute_symlink_targets_stable(&entry?.path(), projections)?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_mapping_pair_compatible(left: &MigrationMapping, right: &MigrationMapping) -> Result<()> {
    if left.destination == right.destination {
        return ensure_projected_entries_compatible(&left.source, &right.source, &left.destination);
    }
    if let Ok(relative) = right.destination.strip_prefix(&left.destination) {
        return ensure_nested_mapping_compatible(left, relative, right);
    }
    if let Ok(relative) = left.destination.strip_prefix(&right.destination) {
        return ensure_nested_mapping_compatible(right, relative, left);
    }
    Ok(())
}

pub(crate) fn ensure_nested_mapping_compatible(
    outer: &MigrationMapping,
    relative_destination: &Path,
    inner: &MigrationMapping,
) -> Result<()> {
    let Some(projected_source) = projected_source_entry(&outer.source, relative_destination)?
    else {
        return Ok(());
    };
    ensure_projected_entries_compatible(&projected_source, &inner.source, &inner.destination)
}

/// Locates the source entry which an outer mapping would project onto a nested
/// destination. A non-directory ancestor is already a conflict because the
/// inner mapping needs that destination path to remain traversable.
pub(crate) fn projected_source_entry(source_root: &Path, relative: &Path) -> Result<Option<PathBuf>> {
    let mut current = source_root.to_path_buf();
    for component in relative.components() {
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() {
            bail!(
                "Miyu directory migration found a projected path conflict at {}",
                current.display()
            );
        }
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(current))
}

pub(crate) fn ensure_projected_entries_compatible(
    left: &Path,
    right: &Path,
    projected_destination: &Path,
) -> Result<()> {
    let left_meta = fs::symlink_metadata(left)?;
    let right_meta = fs::symlink_metadata(right)?;
    if left_meta.is_dir() && right_meta.is_dir() {
        for entry in fs::read_dir(left)? {
            let entry = entry?;
            let right_child = right.join(entry.file_name());
            match fs::symlink_metadata(&right_child) {
                Ok(_) => ensure_projected_entries_compatible(
                    &entry.path(),
                    &right_child,
                    &projected_destination.join(entry.file_name()),
                )?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        return Ok(());
    }
    if entries_identical(left, &left_meta, right, &right_meta)? {
        return Ok(());
    }
    bail!(
        "Miyu directory migration found conflicting legacy entries {} and {} projected to {}",
        left.display(),
        right.display(),
        projected_destination.display()
    )
}

pub(crate) fn ensure_no_conflicts(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    match fs::symlink_metadata(destination) {
        Ok(destination_meta) => {
            if source_meta.is_dir() && destination_meta.is_dir() {
                for entry in fs::read_dir(source)? {
                    let entry = entry?;
                    ensure_no_conflicts(&entry.path(), &destination.join(entry.file_name()))?;
                }
                return Ok(());
            }
            if entries_identical(source, &source_meta, destination, &destination_meta)? {
                return Ok(());
            }
            bail!(
                "Miyu directory migration found conflicting entries: {} and {}; move or rename one of them and retry",
                source.display(),
                destination.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(crate) fn migrate_entry_unchecked(source: &Path, destination: &Path) -> Result<()> {
    let source_meta = fs::symlink_metadata(source)?;
    match fs::symlink_metadata(destination) {
        Ok(destination_meta) => {
            if source_meta.is_dir() && destination_meta.is_dir() {
                for entry in fs::read_dir(source)? {
                    let entry = entry?;
                    migrate_entry_unchecked(&entry.path(), &destination.join(entry.file_name()))?;
                }
                remove_empty_dir(source)?;
                return Ok(());
            }
            if entries_identical(source, &source_meta, destination, &destination_meta)? {
                remove_entry(source, &source_meta)?;
                return Ok(());
            }
            bail!(
                "Miyu directory migration found a conflict that appeared after preflight: {} and {}",
                source.display(),
                destination.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => {
            sync_parent(destination)?;
            if source.parent() != destination.parent() {
                sync_parent(source)?;
            }
            Ok(())
        }
        Err(error) if is_cross_device_error(&error) => {
            copy_entry(source, &source_meta, destination)?;
            remove_entry(source, &source_meta)
        }
        Err(error) => Err(error.into()),
    }
}

fn is_cross_device_error(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(17)
    }
}

pub(crate) fn copy_entry(source: &Path, metadata: &fs::Metadata, destination: &Path) -> Result<()> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        sys::symlink_or_copy(&target, destination)?;
        sync_parent(destination)?;
        return Ok(());
    }
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        fs::set_permissions(destination, metadata.permissions())?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let child_meta = fs::symlink_metadata(entry.path())?;
            copy_entry(
                &entry.path(),
                &child_meta,
                &destination.join(entry.file_name()),
            )?;
        }
        sys::sync_parent(destination)?;
        sync_parent(destination)?;
        return Ok(());
    }
    if !metadata.is_file() {
        bail!("unsupported file type while migrating {}", source.display());
    }
    let temporary = destination.with_extension(format!(
        "miyu-migrate-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut input = File::open(source)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    sys::apply_mode(&mut options, sys::mode_of(&metadata.permissions()));
    let mut output = options.open(&temporary)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    fs::rename(&temporary, destination)?;
    sync_parent(destination)?;
    Ok(())
}

pub(crate) fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        sys::sync_parent(parent)?;
    }
    Ok(())
}

pub(crate) fn entries_identical(
    left: &Path,
    left_meta: &fs::Metadata,
    right: &Path,
    right_meta: &fs::Metadata,
) -> Result<bool> {
    if left_meta.file_type().is_symlink() || right_meta.file_type().is_symlink() {
        return Ok(left_meta.file_type().is_symlink()
            && right_meta.file_type().is_symlink()
            && fs::read_link(left)? == fs::read_link(right)?);
    }
    if !left_meta.is_file()
        || !right_meta.is_file()
        || left_meta.len() != right_meta.len()
        || sys::mode_of(&left_meta.permissions()) & 0o7777
            != sys::mode_of(&right_meta.permissions()) & 0o7777
    {
        return Ok(false);
    }
    let mut left_file = File::open(left)?;
    let mut right_file = File::open(right)?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_count = left_file.read(&mut left_buffer)?;
        let right_count = right_file.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}

pub(crate) fn remove_entry(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    sync_parent(path)
}

pub(crate) fn remove_empty_dir(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
