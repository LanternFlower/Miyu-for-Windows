//! Cross-platform base directories with a Windows environment fallback.
//!
//! `directories::BaseDirs` uses `SHGetKnownFolderPath` on Windows. That API can
//! fail in restricted processes even when the standard profile environment
//! variables are present, which used to make Miyu fail before reading config.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct PlatformDirs {
    home_dir: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    state_dir: Option<PathBuf>,
}

impl PlatformDirs {
    pub(crate) fn new() -> Option<Self> {
        if let Some(base) = directories::BaseDirs::new() {
            return Some(Self {
                home_dir: base.home_dir().to_path_buf(),
                config_dir: base.config_dir().to_path_buf(),
                data_dir: base.data_dir().to_path_buf(),
                cache_dir: base.cache_dir().to_path_buf(),
                state_dir: base.state_dir().map(Path::to_path_buf),
            });
        }

        #[cfg(windows)]
        {
            let absolute_env = |name: &str| {
                std::env::var_os(name)
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
            };
            let home_dir = absolute_env("USERPROFILE").or_else(|| {
                let drive = std::env::var_os("HOMEDRIVE")?;
                let tail = std::env::var_os("HOMEPATH")?;
                let path = PathBuf::from(drive).join(tail);
                path.is_absolute().then_some(path)
            })?;
            let config_dir = absolute_env("APPDATA")
                .unwrap_or_else(|| home_dir.join("AppData/Roaming"));
            let cache_dir = absolute_env("LOCALAPPDATA")
                .unwrap_or_else(|| home_dir.join("AppData/Local"));
            return Some(Self {
                home_dir,
                data_dir: config_dir.clone(),
                config_dir,
                cache_dir,
                state_dir: None,
            });
        }

        #[cfg(not(windows))]
        None
    }

    pub(crate) fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    pub(crate) fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub(crate) fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub(crate) fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub(crate) fn state_dir(&self) -> Option<&Path> {
        self.state_dir.as_deref()
    }
}
