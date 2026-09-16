//! 草稿的创建、编辑与发布。
//!
//! 发布是**目录交换**（`exchange_skill_directories`）而不是逐文件覆盖：中途崩
//! 掉时技能要么是旧版要么是新版，不会是一半。`StagedDirectory` 的 `Drop` 负责
//! 清理没交换成功的暂存目录。
//!
//! 打包有四重上限（文件数、总字节、目录数、深度）：草稿内容由模型生成，一个
//! 递归复制就能把磁盘填满。

mod install;
pub(crate) use install::*;

use crate::skills::*;

pub(crate) const DRAFT_MANIFEST: &str = "draft.json";

pub(crate) const DRAFT_PACKAGE_DIR: &str = "package";

pub(crate) const DRAFT_VERSION: u32 = 1;

pub(crate) const DRAFT_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub(crate) const PUBLISHED_DRAFT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) const MAX_SKILL_PACKAGE_FILES: usize = 512;

pub(crate) const MAX_SKILL_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) const MAX_SKILL_PACKAGE_DIRS: usize = 128;

pub(crate) const MAX_SKILL_PACKAGE_DEPTH: usize = 16;

pub(crate) const MAX_DRAFT_MANIFEST_BYTES: u64 = 64 * 1024;

pub(crate) const PUBLISH_LOCK_FILE: &str = ".publish.lock";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DraftKind {
    Create,
    Update,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DraftManifest {
    pub(crate) version: u32,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) scope: SkillScope,
    pub(crate) persona_scope: Option<String>,
    pub(crate) kind: DraftKind,
    pub(crate) base_revision: Option<String>,
    pub(crate) created_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillDraft {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub persona_scope: Option<String>,
    pub kind: String,
    pub skill_dir: String,
    pub skill_file: String,
    pub base_revision: Option<String>,
    pub created_at: u64,
    pub last_modified_at: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct PublishedSkill {
    pub name: String,
    pub scope: String,
    pub persona_scope: Option<String>,
    pub path: String,
    pub revision: String,
    pub operation: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeletedSkill {
    pub name: String,
    pub scope: String,
    pub persona_scope: Option<String>,
    pub path: String,
}

pub fn create_draft(
    config: &AppConfig,
    paths: &MiyuPaths,
    name: &str,
    description: &str,
    scope: SkillScope,
) -> Result<SkillDraft> {
    prune_expired_drafts(paths)?;
    validate_skill_name(name)?;
    validate_description(description)?;
    let persona_scope = persona_scope(config, scope);
    let target = target_path(paths, name, scope, persona_scope.as_deref())?;
    if target.exists() {
        bail!("skill already exists in {} scope: {name}", scope.as_str());
    }
    let manifest = new_manifest(name, scope, persona_scope, DraftKind::Create, None);
    let package = create_empty_draft(paths, &manifest)?;
    let result = (|| {
        let skill_dir = package.join(name);
        fs::create_dir(&skill_dir)?;
        write_private_file(
            &skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {}\n---\n\n# {name}\n\n## Workflow\n\nDescribe the reusable workflow here.\n",
                serde_json::to_string(description.trim())?
            )
            .as_bytes(),
        )?;
        write_draft_manifest(paths, &manifest)?;
        draft_public(paths, &manifest)
    })();
    cleanup_failed_draft(paths, &manifest, result)
}

pub fn update_draft(
    config: &AppConfig,
    paths: &MiyuPaths,
    name: &str,
    scope: SkillScope,
) -> Result<SkillDraft> {
    prune_expired_drafts(paths)?;
    validate_skill_name(name)?;
    let persona_scope = persona_scope(config, scope);
    let source = target_path(paths, name, scope, persona_scope.as_deref())?;
    if !source.join("SKILL.md").is_file() {
        bail!("skill not found in {} scope: {name}", scope.as_str());
    }
    let _lease = acquire_publish_lock(paths)?;
    ensure_directory_chain(&paths.skills_dir, &source)?;
    validate_skill_package(&source, name)?;
    let revision_before = skill_revision(&source)?;
    let manifest = new_manifest(
        name,
        scope,
        persona_scope,
        DraftKind::Update,
        Some(revision_before.clone()),
    );
    let package = create_empty_draft(paths, &manifest)?;
    let result = (|| {
        copy_tree(&source, &package.join(name))?;
        let revision_after = skill_revision(&source)?;
        if revision_after != revision_before {
            bail!("skill changed while its update draft was being created; retry");
        }
        validate_skill_package(&package.join(name), name)?;
        write_draft_manifest(paths, &manifest)?;
        draft_public(paths, &manifest)
    })();
    cleanup_failed_draft(paths, &manifest, result)
}

pub fn publish_draft(paths: &MiyuPaths, draft_id: &str) -> Result<PublishedSkill> {
    validate_draft_id(draft_id)?;
    let _lease = acquire_publish_lock(paths)?;
    prune_expired_drafts_unlocked(paths)?;
    let manifest = read_manifest(paths, draft_id)?;
    let draft_root = paths.skill_drafts_dir().join(draft_id);
    let source = draft_root.join(DRAFT_PACKAGE_DIR).join(&manifest.name);
    ensure_directory_chain(&draft_root, &source)?;
    let target = target_path(
        paths,
        &manifest.name,
        manifest.scope,
        manifest.persona_scope.as_deref(),
    )?;
    let parent = target.parent().context("skill target has no parent")?;
    create_private_dir(&paths.skills_dir)?;
    create_private_directory_chain(&paths.skills_dir, parent)?;
    let staged = parent.join(format!(
        ".miyu-skill-stage-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let source_revision_before = skill_revision(&source)?;
    copy_tree(&source, &staged)?;
    let source_revision_after = skill_revision(&source)?;
    if source_revision_after != source_revision_before {
        bail!("skill draft changed while it was being published; retry");
    }
    let mut staged_guard = StagedDirectory::new(staged.clone());
    validate_skill_package(&staged, &manifest.name)?;
    let revision = skill_revision(&staged)?;
    if skill_revision(&source)? != source_revision_after {
        bail!("skill draft changed before installation; retry");
    }

    match manifest.kind {
        DraftKind::Create => {
            if target.exists() {
                bail!(
                    "skill already exists; create never overwrites: {}",
                    manifest.name
                );
            }
            install_new_skill(&staged, &target).with_context(|| {
                format!(
                    "publishing skill from {} to {}",
                    staged.display(),
                    target.display()
                )
            })?;
            if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
                tracing::warn!(path = %parent.display(), error = %error, "failed to sync published skill directory");
            }
            staged_guard.disarm();
        }
        DraftKind::Update => {
            if !target.is_dir() {
                bail!("skill disappeared before update: {}", manifest.name);
            }
            ensure_directory_chain(&paths.skills_dir, &target)?;
            validate_skill_package(&target, &manifest.name)?;
            let expected = manifest
                .base_revision
                .as_deref()
                .context("update draft is missing its base revision")?;
            let current = skill_revision(&target)?;
            if current != expected {
                bail!(
                    "skill changed after the update draft was created; create a new update draft"
                );
            }
            install_updated_skill(&staged, &target, &current, &mut staged_guard)?;
        }
    }
    let archived_draft = paths.skill_drafts_dir().join(format!(
        ".published-{}-{:016x}",
        manifest.id,
        rand::random::<u64>()
    ));
    if let Err(error) = fs::rename(&draft_root, &archived_draft) {
        tracing::warn!(path = %draft_root.display(), error = %error, "failed to archive published skill draft");
    } else if let Err(error) = File::open(paths.skill_drafts_dir()).and_then(|dir| dir.sync_all()) {
        tracing::warn!(path = %archived_draft.display(), error = %error, "failed to sync published skill draft archive");
    }
    Ok(PublishedSkill {
        name: manifest.name,
        scope: manifest.scope.as_str().to_string(),
        persona_scope: manifest.persona_scope,
        path: target.display().to_string(),
        revision,
        operation: match manifest.kind {
            DraftKind::Create => "create",
            DraftKind::Update => "update",
        }
        .to_string(),
    })
}

pub fn delete_skill(
    config: &AppConfig,
    paths: &MiyuPaths,
    name: &str,
    scope: SkillScope,
) -> Result<DeletedSkill> {
    validate_skill_name(name)?;
    let persona_scope = persona_scope(config, scope);
    let target = target_path(paths, name, scope, persona_scope.as_deref())?;
    let _lease = acquire_publish_lock(paths)?;
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!("skill path is unsafe: {}", target.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("skill not found in {} scope: {name}", scope.as_str())
        }
        Err(error) => return Err(error.into()),
    }
    ensure_directory_chain(&paths.skills_dir, &target)?;
    validate_skill_package(&target, name)?;
    fs::remove_dir_all(&target)?;
    if let Some(parent) = target.parent() {
        if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
            tracing::warn!(path = %parent.display(), error = %error, "failed to sync deleted skill directory");
        }
    }
    Ok(DeletedSkill {
        name: name.to_string(),
        scope: scope.as_str().to_string(),
        persona_scope,
        path: target.display().to_string(),
    })
}

pub fn list_drafts(paths: &MiyuPaths) -> Result<Vec<SkillDraft>> {
    prune_expired_drafts(paths)?;
    let root = paths.skill_drafts_dir();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut drafts = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') {
            continue;
        }
        match read_manifest(paths, &id).and_then(|manifest| draft_public(paths, &manifest)) {
            Ok(draft) => drafts.push(draft),
            Err(error) => {
                tracing::warn!(draft_id = id, error = %error, "skipping invalid skill draft")
            }
        }
    }
    drafts.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    Ok(drafts)
}

pub fn prune_expired_drafts(paths: &MiyuPaths) -> Result<usize> {
    let _lease = acquire_publish_lock(paths)?;
    prune_expired_drafts_unlocked(paths)
}

pub(crate) fn prune_expired_drafts_unlocked(paths: &MiyuPaths) -> Result<usize> {
    let root = paths.skill_drafts_dir();
    if !root.is_dir() {
        return Ok(0);
    }
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let published_archive = entry
            .file_name()
            .to_string_lossy()
            .starts_with(".published-");
        let inspection = match inspect_latest_modified(&entry.path()) {
            Ok(inspection) => inspection,
            Err(error) => {
                tracing::warn!(path = %entry.path().display(), error = %error, "failed to inspect skill draft age");
                continue;
            }
        };
        let modified = match inspection {
            DraftInspection::Valid(modified) => modified,
            DraftInspection::Invalid => {
                let error = anyhow::anyhow!("draft exceeds inspection limits");
                tracing::warn!(path = %entry.path().display(), error = %error, "removing invalid skill draft");
                match fs::remove_dir_all(entry.path()) {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                continue;
            }
        };
        let age = now.duration_since(modified).unwrap_or_default();
        let retention = if published_archive {
            PUBLISHED_DRAFT_RETENTION
        } else {
            DRAFT_RETENTION
        };
        if age >= retention {
            tracing::info!(path = %entry.path().display(), "removing expired skill draft");
            match fs::remove_dir_all(entry.path()) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(removed)
}

pub(crate) fn new_manifest(
    name: &str,
    scope: SkillScope,
    persona_scope: Option<String>,
    kind: DraftKind,
    base_revision: Option<String>,
) -> DraftManifest {
    DraftManifest {
        version: DRAFT_VERSION,
        id: format!("draft-{:032x}", rand::random::<u128>()),
        name: name.to_string(),
        scope,
        persona_scope,
        kind,
        base_revision,
        created_at: unix_time(SystemTime::now()),
    }
}

pub(crate) fn create_empty_draft(paths: &MiyuPaths, manifest: &DraftManifest) -> Result<PathBuf> {
    let root = paths.skill_drafts_dir();
    create_private_dir(&root)?;
    let draft = root.join(&manifest.id);
    fs::create_dir(&draft)?;
    let result = (|| {
        secure_directory(&draft)?;
        let package = draft.join(DRAFT_PACKAGE_DIR);
        fs::create_dir(&package)?;
        secure_directory(&package)?;
        Ok(package)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&draft);
    }
    result
}

pub(crate) fn write_draft_manifest(paths: &MiyuPaths, manifest: &DraftManifest) -> Result<()> {
    let draft = paths.skill_drafts_dir().join(&manifest.id);
    write_private_file(
        &draft.join(DRAFT_MANIFEST),
        format!("{}\n", serde_json::to_string_pretty(manifest)?).as_bytes(),
    )
}

pub(crate) fn cleanup_failed_draft<T>(
    paths: &MiyuPaths,
    manifest: &DraftManifest,
    result: Result<T>,
) -> Result<T> {
    if result.is_err() {
        let _ = fs::remove_dir_all(paths.skill_drafts_dir().join(&manifest.id));
    }
    result
}

pub(crate) fn draft_public(paths: &MiyuPaths, manifest: &DraftManifest) -> Result<SkillDraft> {
    let skill_dir = paths
        .skill_drafts_dir()
        .join(&manifest.id)
        .join(DRAFT_PACKAGE_DIR)
        .join(&manifest.name);
    Ok(SkillDraft {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        scope: manifest.scope.as_str().to_string(),
        persona_scope: manifest.persona_scope.clone(),
        kind: match manifest.kind {
            DraftKind::Create => "create",
            DraftKind::Update => "update",
        }
        .to_string(),
        skill_file: skill_dir.join("SKILL.md").display().to_string(),
        skill_dir: skill_dir.display().to_string(),
        base_revision: manifest.base_revision.clone(),
        created_at: manifest.created_at,
        last_modified_at: unix_time(latest_modified(&skill_dir).unwrap_or(UNIX_EPOCH)),
    })
}

pub(crate) fn read_manifest(paths: &MiyuPaths, draft_id: &str) -> Result<DraftManifest> {
    validate_draft_id(draft_id)?;
    let draft = paths.skill_drafts_dir().join(draft_id);
    let metadata = fs::symlink_metadata(&draft)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("skill draft root must be a regular directory");
    }
    let path = draft.join(DRAFT_MANIFEST);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("skill draft manifest must be a regular file");
    }
    if metadata.len() > MAX_DRAFT_MANIFEST_BYTES {
        bail!("skill draft manifest exceeds its size limit");
    }
    let manifest: DraftManifest = serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("parsing {}", path.display()))?;
    if manifest.version != DRAFT_VERSION || manifest.id != draft_id {
        bail!("unsupported or mismatched skill draft manifest");
    }
    validate_skill_name(&manifest.name)?;
    target_path(
        paths,
        &manifest.name,
        manifest.scope,
        manifest.persona_scope.as_deref(),
    )?;
    match manifest.kind {
        DraftKind::Create if manifest.base_revision.is_some() => {
            bail!("create draft must not contain a base revision")
        }
        DraftKind::Update => {
            let revision = manifest
                .base_revision
                .as_deref()
                .context("update draft is missing its base revision")?;
            if revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid update draft base revision");
            }
        }
        DraftKind::Create => {}
    }
    let now = unix_time(SystemTime::now());
    if manifest.created_at > now.saturating_add(300) {
        bail!("skill draft creation timestamp is in the future");
    }
    Ok(manifest)
}

pub(crate) fn validate_draft_id(id: &str) -> Result<()> {
    let mut components = Path::new(id).components();
    if !id.starts_with("draft-")
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        bail!("invalid skill draft id");
    }
    Ok(())
}

pub(crate) fn validate_skill_package(root: &Path, expected_name: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("skill package root must be a regular directory");
    }
    let raw = read_skill_file(&root.join("SKILL.md"))?;
    parse_skill_metadata(&raw, Some(expected_name))?;
    let mut stats = PackageStats {
        directories: 1,
        ..PackageStats::default()
    };
    validate_package_tree(root, 0, &mut stats)?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct PackageStats {
    pub(crate) files: usize,
    pub(crate) directories: usize,
    pub(crate) bytes: u64,
}

pub(crate) fn validate_package_tree(
    path: &Path,
    depth: usize,
    stats: &mut PackageStats,
) -> Result<()> {
    if depth > MAX_SKILL_PACKAGE_DEPTH {
        bail!("skill package exceeds the directory depth limit");
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!(
                "skill packages may not contain symbolic links: {}",
                entry.path().display()
            );
        }
        if metadata.is_dir() {
            stats.directories += 1;
            if stats.directories > MAX_SKILL_PACKAGE_DIRS {
                bail!("skill package exceeds the directory-count limit");
            }
            validate_package_tree(&entry.path(), depth + 1, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(metadata.len())
                .context("skill package size overflow")?;
            if stats.files > MAX_SKILL_PACKAGE_FILES || stats.bytes > MAX_SKILL_PACKAGE_BYTES {
                bail!("skill package exceeds file-count or total-size limits");
            }
        } else {
            bail!(
                "skill package contains an unsupported file type: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

pub(crate) fn skill_revision(root: &Path) -> Result<String> {
    let mut entries = Vec::new();
    let mut stats = PackageStats {
        directories: 1,
        ..PackageStats::default()
    };
    collect_revision_entries(root, root, 0, &mut stats, &mut entries)?;
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    for entry in entries {
        hasher.update(&[entry.kind]);
        hash_length_prefixed(&mut hasher, entry.relative.as_os_str().as_encoded_bytes());
        hasher.update(&entry.mode.to_le_bytes());
        hasher.update(&entry.length.to_le_bytes());
        if entry.kind == b'f' {
            let mut file = File::open(entry.path)?;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(crate) struct RevisionEntry {
    pub(crate) relative: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) kind: u8,
    pub(crate) mode: u32,
    pub(crate) length: u64,
}

pub(crate) fn collect_revision_entries(
    root: &Path,
    path: &Path,
    depth: usize,
    stats: &mut PackageStats,
    entries: &mut Vec<RevisionEntry>,
) -> Result<()> {
    if depth > MAX_SKILL_PACKAGE_DEPTH {
        bail!("skill package exceeds the directory depth limit");
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            bail!("skill packages may not contain symbolic links");
        }
        let relative = entry.path().strip_prefix(root)?.to_path_buf();
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::MetadataExt;
            metadata.mode()
        };
        #[cfg(not(unix))]
        let mode = 0;
        if metadata.is_dir() {
            stats.directories += 1;
            if stats.directories > MAX_SKILL_PACKAGE_DIRS {
                bail!("skill package exceeds the directory-count limit");
            }
            entries.push(RevisionEntry {
                relative,
                path: entry.path(),
                kind: b'd',
                mode,
                length: 0,
            });
            collect_revision_entries(root, &entry.path(), depth + 1, stats, entries)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(metadata.len())
                .context("skill package size overflow")?;
            if stats.files > MAX_SKILL_PACKAGE_FILES || stats.bytes > MAX_SKILL_PACKAGE_BYTES {
                bail!("skill package exceeds file-count or total-size limits");
            }
            entries.push(RevisionEntry {
                relative,
                path: entry.path(),
                kind: b'f',
                mode,
                length: metadata.len(),
            });
        } else {
            bail!("skill package contains an unsupported file type");
        }
    }
    Ok(())
}

pub(crate) fn hash_length_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

pub(crate) enum DraftInspection {
    Valid(SystemTime),
    Invalid,
}

pub(crate) fn latest_modified(path: &Path) -> Result<SystemTime> {
    match inspect_latest_modified(path)? {
        DraftInspection::Valid(modified) => Ok(modified),
        DraftInspection::Invalid => bail!("skill draft exceeds inspection limits"),
    }
}

pub(crate) fn inspect_latest_modified(path: &Path) -> Result<DraftInspection> {
    let mut latest = UNIX_EPOCH;
    let mut visited = 0usize;
    let mut pending = vec![(path.to_path_buf(), 0usize)];
    while let Some((path, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_SKILL_PACKAGE_FILES + MAX_SKILL_PACKAGE_DIRS + 16 {
            return Ok(DraftInspection::Invalid);
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if modified > latest {
            latest = modified;
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            if depth > MAX_SKILL_PACKAGE_DEPTH + 4 {
                return Ok(DraftInspection::Invalid);
            }
            for entry in fs::read_dir(path)? {
                match entry {
                    Ok(entry) => pending.push((entry.path(), depth + 1)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(DraftInspection::Valid(latest))
}

pub(crate) fn unix_time(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
