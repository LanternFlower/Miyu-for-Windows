//! `miyu import`: restore an exported installation onto this machine.

use super::export::miyu_home;
use super::manifest::{Manifest, MANIFEST_NAME};
use super::registry::unit_for;
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use miyu_base::i18n::text as t;
use miyu_base::paths::MiyuPaths;
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    pub force: bool,
}

#[derive(Debug)]
pub struct ImportReport {
    pub restored: usize,
    pub unknown_units: BTreeSet<String>,
    pub backup: Option<PathBuf>,
    pub secrets_included: bool,
    pub index_included: bool,
    /// Session workspaces that pointed at directories this machine does not
    /// have; cleared so a turn does not try to run in them.
    pub cleared_workspaces: usize,
}

pub fn import(paths: &MiyuPaths, archive: &Path, options: &ImportOptions) -> Result<ImportReport> {
    let root = miyu_home(paths)?;
    let manifest = read_manifest(archive)?;
    check_versions(&manifest)?;

    if !options.force {
        if let Some(reason) = occupied(paths) {
            bail!(
                "{}\n{}",
                reason,
                t(
                    "pass --force to overwrite (the current data is exported to a backup archive first)",
                    "如需覆盖请传 --force（覆盖前会先把现有数据导出成备份包）"
                )
            );
        }
    }

    let backup = if options.force && occupied(paths).is_some() {
        Some(backup_current(paths, archive)?)
    } else {
        None
    };

    // Unpack beside the target first: a half-extracted archive must never be
    // able to leave MIYU_HOME in a mixed state.
    let staging = tempfile::tempdir_in(root.parent().unwrap_or(&root))
        .context("creating a staging directory")?;
    let staged = staging.path().join("home");
    extract(archive, staging.path())?;

    let mut unknown_units = BTreeSet::new();
    for entry in &manifest.entries {
        if unit_for(&entry.path).is_none() {
            // Written by a newer build that knows about data this one does
            // not. Restoring it verbatim is strictly better than dropping it.
            unknown_units.insert(entry.unit.clone());
        }
    }

    let restored = install(&staged, &root)?;
    stamp_layout_markers(&root)?;
    let cleared_workspaces = super::fixups::apply(paths)?;

    Ok(ImportReport {
        restored,
        unknown_units,
        backup,
        secrets_included: manifest.secrets_included,
        index_included: manifest.scope.index,
        cleared_workspaces,
    })
}

/// Why importing here would destroy something, or `None` when the target is
/// effectively empty.
fn occupied(paths: &MiyuPaths) -> Option<String> {
    if paths.config_file.exists() {
        return Some(format!(
            "{}: {}",
            t("a configuration already exists", "目标已有配置"),
            paths.config_file.display()
        ));
    }
    let conversations = paths.state_dir.join("conversation.db");
    if conversations.exists() {
        return Some(format!(
            "{}: {}",
            t("conversation history already exists", "目标已有会话历史"),
            conversations.display()
        ));
    }
    None
}

fn read_manifest(archive: &Path) -> Result<Manifest> {
    let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    for entry in tar.entries().context("reading the archive")? {
        let entry = entry?;
        if entry.path()?.to_string_lossy() == MANIFEST_NAME {
            return serde_json::from_reader(entry).context("parsing the archive manifest");
        }
    }
    bail!(
        "{}",
        t(
            "this file has no Miyu manifest; it is not a miyu export archive",
            "包里没有 Miyu 清单，这不是 miyu export 生成的归档"
        )
    )
}

/// Refuses archives this build cannot safely open, mirroring the checks
/// `AppConfig::migrate` and the schema migrations already make.
fn check_versions(manifest: &Manifest) -> Result<()> {
    if manifest.config_version > miyu_base::config::CURRENT_CONFIG_VERSION {
        bail!(
            "{} ({} > {})",
            t(
                "the archive's configuration is newer than this build supports; upgrade Miyu first",
                "包里的配置版本高于当前 Miyu 支持的版本；请先升级 Miyu"
            ),
            manifest.config_version,
            miyu_base::config::CURRENT_CONFIG_VERSION
        );
    }
    let newer = manifest.schemas_newer_than(miyu_core::state::latest_schema_version());
    if !newer.is_empty() {
        let detail = newer
            .iter()
            .map(|(unit, version)| format!("{unit}={version}"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "{} ({detail} > {})",
            t(
                "the archive's database schema is newer than this build supports; upgrade Miyu first",
                "包里的数据库 schema 高于当前 Miyu 支持的版本；请先升级 Miyu"
            ),
            miyu_core::state::latest_schema_version()
        );
    }
    Ok(())
}

fn extract(archive: &Path, into: &Path) -> Result<()> {
    let file = File::open(archive).with_context(|| format!("opening {}", archive.display()))?;
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    for entry in tar.entries().context("reading the archive")? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        // Refuse anything that would escape the staging directory.
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
            || path.is_absolute()
        {
            bail!("archive entry escapes its root: {}", path.display());
        }
        let destination = into.join(&path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&destination)
            .with_context(|| format!("extracting {}", path.display()))?;
    }
    Ok(())
}

/// Moves the staged tree into place, replacing whatever it covers.
fn install(staged: &Path, root: &Path) -> Result<usize> {
    let mut installed = 0usize;
    let mut stack = vec![staged.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for child in std::fs::read_dir(&dir)?.flatten() {
            let path = child.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(staged)
                .context("staged path outside the staging root")?;
            let destination = root.join(rel);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Rename first (same filesystem, atomic); fall back to a copy when
            // the staging dir landed elsewhere.
            if std::fs::rename(&path, &destination).is_err() {
                std::fs::copy(&path, &destination)
                    .with_context(|| format!("installing {}", rel.display()))?;
            }
            installed += 1;
        }
    }
    Ok(installed)
}

/// Marks the restored tree as already using the current layout, so
/// `MiyuPaths::new` does not try to migrate it from a legacy one.
fn stamp_layout_markers(root: &Path) -> Result<()> {
    for marker in [".layout-v1", ".resource-layout-v1"] {
        let path = root.join(marker);
        if !path.exists() {
            std::fs::write(&path, "1").with_context(|| format!("writing {}", path.display()))?;
        }
    }
    Ok(())
}

fn backup_current(paths: &MiyuPaths, archive: &Path) -> Result<PathBuf> {
    let directory = archive.parent().unwrap_or(Path::new("."));
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let destination = directory.join(format!("miyu-backup-{stamp}.tar.gz"));
    let options = super::export::ExportOptions {
        all: true,
        ..Default::default()
    };
    super::export::export(paths, &destination, &options)
        .context("backing up the current installation before overwriting it")?;
    Ok(destination)
}
