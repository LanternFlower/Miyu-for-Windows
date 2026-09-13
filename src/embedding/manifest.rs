//! 本地模型资产：目录布局与查找链。
//!
//! 一个模型就是一个目录（`manifest.json` + ONNX + tokenizer），目录名即模型 id。
//! 查找顺序：环境变量 → `~/.miyu/models` → 安装前缀 → Linux 系统目录 →
//! 仅 debug 的源码目录。候选链由 paths::resources 统一生成。

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub(crate) const MODELS_DIR_ENV: &str = "MIYU_EMBEDDING_MODELS_DIR";
pub(crate) const DEFAULT_LOCAL_MODEL: &str = "bge-small-zh-v1.5-int8";
const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Pooling {
    Cls,
    Mean,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ModelManifest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) display_name: String,
    #[serde(default = "default_model_file")]
    pub(crate) model_file: String,
    #[serde(default = "default_tokenizer_file")]
    pub(crate) tokenizer_file: String,
    pub(crate) dims: usize,
    #[serde(default = "default_pooling")]
    pub(crate) pooling: Pooling,
    #[serde(default = "default_true")]
    pub(crate) normalize: bool,
    #[serde(default = "default_max_length")]
    pub(crate) max_length: usize,
    #[serde(default)]
    pub(crate) query_prefix: String,
    #[serde(default = "default_min_score")]
    pub(crate) min_score: f32,
}

fn default_model_file() -> String {
    "model.onnx".to_string()
}
fn default_tokenizer_file() -> String {
    "tokenizer.json".to_string()
}
fn default_pooling() -> Pooling {
    Pooling::Cls
}
fn default_true() -> bool {
    true
}
fn default_max_length() -> usize {
    512
}
fn default_min_score() -> f32 {
    0.35
}

#[derive(Debug, Clone)]
pub(crate) struct LocalModel {
    pub(crate) dir: PathBuf,
    pub(crate) manifest: ModelManifest,
}

impl LocalModel {
    pub(crate) fn model_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.model_file)
    }

    pub(crate) fn tokenizer_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.tokenizer_file)
    }

    /// Stable identity for stored vectors: switching models invalidates them.
    pub(crate) fn model_id(&self) -> String {
        format!("local:{}", self.manifest.id)
    }
}

/// `name` is either a model id looked up along the search chain, or a path to
/// a model directory (anything containing a separator, or an existing dir).
pub(crate) fn resolve_local_model(name: &str) -> Result<LocalModel> {
    resolve_local_model_from(name, &candidate_model_dirs())
}

fn resolve_local_model_from(name: &str, candidates: &[PathBuf]) -> Result<LocalModel> {
    let name = name.trim();
    if name.is_empty() {
        bail!("embedding.local_model is empty");
    }
    let as_path = Path::new(name);
    if as_path.is_absolute() || name.contains('/') || name.contains('\\') {
        return load_model_dir(as_path);
    }
    for base in candidates {
        let dir = base.join(name);
        if dir.join(MANIFEST_FILE).is_file() {
            return load_model_dir(&dir);
        }
    }
    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "embedding model `{name}` was not found; install it under /usr/share/miyu/models or ~/.miyu/models, or set {MODELS_DIR_ENV} (searched: {searched})"
    )
}

pub(crate) fn candidate_model_dirs() -> Vec<PathBuf> {
    crate::paths::resources::candidates(crate::paths::resources::ResourceKind::Models)
}

pub(crate) fn load_model_dir(dir: &Path) -> Result<LocalModel> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: ModelManifest = serde_json::from_str(&text)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    if manifest.dims == 0 {
        bail!("{}: dims must be positive", manifest_path.display());
    }
    if manifest.id.trim().is_empty() {
        bail!("{}: id is empty", manifest_path.display());
    }
    let model = LocalModel {
        dir: dir.to_path_buf(),
        manifest,
    };
    for path in [model.model_path(), model.tokenizer_path()] {
        if !path.is_file() {
            bail!("embedding model file is missing: {}", path.display());
        }
    }
    Ok(model)
}

/// Model ids available along the search chain, for pickers and `embed status`.
pub(crate) fn installed_local_models() -> Vec<LocalModel> {
    let mut seen = std::collections::BTreeSet::new();
    let mut models = Vec::new();
    for base in candidate_model_dirs() {
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.join(MANIFEST_FILE).is_file() {
                continue;
            }
            if let Ok(model) = load_model_dir(&dir) {
                if seen.insert(model.manifest.id.clone()) {
                    models.push(model);
                }
            }
        }
    }
    models
}

#[cfg(test)]
mod distribution_resources {
    use super::*;

    fn model(base: &Path) -> PathBuf {
        let dir = base.join("fixture-model");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"id":"fixture-model","dims":512}"#,
        )
        .unwrap();
        std::fs::write(dir.join("model.onnx"), b"model fixture").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{}").unwrap();
        dir
    }

    #[test]
    fn model_override_missing_manifest_falls_back_but_invalid_manifest_errors() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = temp.path().join("override");
        let user = temp.path().join("user");
        let installed = temp.path().join("installed");
        let candidates = vec![explicit.clone(), user.clone(), installed.clone()];
        assert!(resolve_local_model_from("fixture-model", &candidates).is_err());
        let installed_dir = model(&installed);
        assert_eq!(
            resolve_local_model_from("fixture-model", &candidates)
                .unwrap()
                .dir,
            installed_dir
        );
        let user_dir = model(&user);
        assert_eq!(
            resolve_local_model_from("fixture-model", &candidates)
                .unwrap()
                .dir,
            user_dir
        );
        let explicit_dir = model(&explicit);
        assert_eq!(
            resolve_local_model_from("fixture-model", &candidates)
                .unwrap()
                .dir,
            explicit_dir
        );
        std::fs::write(explicit_dir.join("manifest.json"), "not JSON").unwrap();
        assert!(resolve_local_model_from("fixture-model", &candidates).is_err());
    }
}
