//! Shared installed-resource candidates. Inputs keep tests independent of process state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    Fonts,
    Models,
    Memes,
    Scripts,
    DefaultKb,
}

impl ResourceKind {
    pub(crate) fn directory(self) -> &'static str {
        match self {
            Self::Fonts => "fonts",
            Self::Models => "models",
            Self::Memes => "memes",
            Self::Scripts => "scripts",
            Self::DefaultKb => "default-kb",
        }
    }
    fn env(self) -> &'static str {
        match self {
            Self::Fonts => "MIYU_RENDERER_FONTS_DIR",
            Self::Models => "MIYU_EMBEDDING_MODELS_DIR",
            Self::Memes => "MIYU_MEMES_DIR",
            Self::Scripts => "MIYU_SYSTEM_SCRIPTS_DIR",
            Self::DefaultKb => "MIYU_DEFAULT_KB_DIR",
        }
    }
    fn authoritative_override(self) -> bool {
        matches!(self, Self::Memes | Self::Scripts | Self::DefaultKb)
    }
    fn source_relative(self) -> PathBuf {
        Path::new(if matches!(self, Self::Memes | Self::Scripts) {
            "src"
        } else {
            "assets"
        })
        .join(self.directory())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourcePlatform {
    Linux,
    Macos,
    Other,
}

impl ResourcePlatform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResourceInputs {
    pub(crate) executable: Option<PathBuf>,
    /// Miyu home (~/.miyu or MIYU_HOME), not the shell HOME.
    pub(crate) home: Option<PathBuf>,
    pub(crate) overrides: BTreeMap<ResourceKind, PathBuf>,
    pub(crate) platform: ResourcePlatform,
    pub(crate) debug: bool,
    pub(crate) source_root: PathBuf,
}

impl ResourceInputs {
    pub(crate) fn capture() -> Self {
        let overrides = [
            ResourceKind::Fonts,
            ResourceKind::Models,
            ResourceKind::Memes,
            ResourceKind::Scripts,
            ResourceKind::DefaultKb,
        ]
        .into_iter()
        .filter_map(|kind| std::env::var_os(kind.env()).map(|value| (kind, PathBuf::from(value))))
        .collect();
        Self {
            executable: super::miyu_executable().ok(),
            home: super::miyu_home_dir(),
            overrides,
            platform: ResourcePlatform::current(),
            debug: cfg!(debug_assertions),
            source_root: PathBuf::from(env!("MIYU_WORKSPACE_ROOT")),
        }
    }

    pub(crate) fn candidates(&self, kind: ResourceKind) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(path) = self.overrides.get(&kind) {
            paths.push(path.clone());
            if kind.authoritative_override() {
                return paths;
            }
        }
        if kind == ResourceKind::Models {
            if let Some(home) = &self.home {
                paths.push(home.join("models"));
            }
        }
        if let Some(prefix) = installation_prefix(self.executable.as_deref()) {
            paths.push(prefix.join("share/miyu").join(kind.directory()));
        }
        if self.platform == ResourcePlatform::Linux {
            paths.push(Path::new("/usr/share/miyu").join(kind.directory()));
            if kind == ResourceKind::Fonts {
                paths.push(PathBuf::from("/usr/share/fonts/noto-cjk"));
            }
        }
        if self.debug {
            paths.push(self.source_root.join(kind.source_relative()));
        }
        // A /usr/bin installation already contributed the Linux fallback.
        let mut unique = Vec::new();
        paths.retain(|path| {
            if unique.contains(path) {
                false
            } else {
                unique.push(path.clone());
                true
            }
        });
        paths
    }

    pub(crate) fn directory(&self, kind: ResourceKind, child: &Path) -> PathBuf {
        let candidates: Vec<_> = self
            .candidates(kind)
            .into_iter()
            .map(|path| path.join(child))
            .collect();
        candidates
            .iter()
            .find(|path| path.is_dir())
            .or_else(|| candidates.first())
            .cloned()
            .unwrap_or_else(|| {
                Path::new("/nonexistent/miyu-resources")
                    .join(kind.directory())
                    .join(child)
            })
    }
}

pub(crate) fn installation_prefix(executable: Option<&Path>) -> Option<PathBuf> {
    executable
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

pub fn candidates(kind: ResourceKind) -> Vec<PathBuf> {
    ResourceInputs::capture().candidates(kind)
}
pub fn directory(kind: ResourceKind) -> PathBuf {
    ResourceInputs::capture().directory(kind, Path::new(""))
}
pub fn child_directory(kind: ResourceKind, child: &Path) -> PathBuf {
    ResourceInputs::capture().directory(kind, child)
}

#[cfg(test)]
mod distribution_resources {
    use super::*;
    fn inputs(root: &Path) -> ResourceInputs {
        ResourceInputs {
            executable: Some(root.join("安装 prefix/bin/miyu")),
            home: Some(root.join("home")),
            overrides: BTreeMap::new(),
            platform: ResourcePlatform::Linux,
            debug: false,
            source_root: root.join("source"),
        }
    }
    #[test]
    fn prefix_precedes_system_and_debug_source() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = inputs(temp.path());
        input.debug = true;
        let paths = input.candidates(ResourceKind::Fonts);
        assert_eq!(paths[0], temp.path().join("安装 prefix/share/miyu/fonts"));
        assert_eq!(
            paths.last().unwrap(),
            &input.source_root.join("assets/fonts")
        );
    }
    #[test]
    fn models_keep_user_priority_without_expanding_other_user_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = inputs(temp.path());
        input
            .overrides
            .insert(ResourceKind::Models, temp.path().join("override"));
        let paths = input.candidates(ResourceKind::Models);
        assert_eq!(paths[0], temp.path().join("override"));
        assert_eq!(paths[1], input.home.as_ref().unwrap().join("models"));
        assert_eq!(paths[2], temp.path().join("安装 prefix/share/miyu/models"));
        for kind in [
            ResourceKind::Fonts,
            ResourceKind::Memes,
            ResourceKind::Scripts,
            ResourceKind::DefaultKb,
        ] {
            assert!(!input
                .candidates(kind)
                .iter()
                .any(|path| path.starts_with(input.home.as_ref().unwrap())));
        }
    }
    #[test]
    fn release_rejects_executable_ancestor_and_cwd_source_forgery() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = inputs(temp.path());
        input.executable = Some(temp.path().join("untrusted/target/release/miyu"));
        input.source_root = temp.path().join("untrusted");
        let fake = input.source_root.join("target/src/memes/default");
        std::fs::create_dir_all(&fake).unwrap();
        assert_ne!(
            input.directory(ResourceKind::Memes, Path::new("default")),
            fake
        );
        assert!(input
            .candidates(ResourceKind::Models)
            .iter()
            .all(|path| !path.ends_with("assets/models")));
    }
    #[test]
    fn cellar_and_opt_bin_entries_use_their_own_share() {
        for prefix in ["/opt/homebrew/Cellar/miyu/0.6.0", "/opt/homebrew/opt/miyu"] {
            let mut input = inputs(Path::new("/test"));
            input.platform = ResourcePlatform::Macos;
            input.executable = Some(Path::new(prefix).join("bin/miyu"));
            assert_eq!(
                input.candidates(ResourceKind::Scripts),
                vec![Path::new(prefix).join("share/miyu/scripts")]
            );
        }
    }
    #[test]
    fn authoritative_overrides_do_not_fall_back_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = inputs(temp.path());
        for kind in [
            ResourceKind::Memes,
            ResourceKind::Scripts,
            ResourceKind::DefaultKb,
        ] {
            let absent = temp.path().join(format!("absent-{}", kind.directory()));
            input.overrides.insert(kind, absent.clone());
            assert_eq!(input.candidates(kind), vec![absent.clone()]);
            assert_eq!(input.directory(kind, Path::new("")), absent);
        }
    }
    #[test]
    fn missing_resources_remain_missing_and_test_executable_cannot_spawn() {
        let mut input = inputs(Path::new("/nonexistent/distribution-resource-test"));
        input.platform = ResourcePlatform::Macos;
        assert!(!input
            .directory(ResourceKind::DefaultKb, Path::new(""))
            .exists());
        assert_eq!(
            super::super::miyu_executable().unwrap(),
            Path::new("/nonexistent/miyu-test-harness")
        );
    }
}
