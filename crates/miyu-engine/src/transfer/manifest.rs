//! What an archive says about itself.

use serde::{Deserialize, Serialize};

/// Version of the manifest format itself, so a future build can tell an
/// archive it fully understands from one it must be careful with.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

pub const MANIFEST_NAME: &str = "manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    /// Miyu version that wrote the archive, for the human reading an error.
    pub miyu_version: String,
    pub exported_at: String,
    /// `config.jsonc` schema version; import refuses anything newer than this
    /// build supports.
    pub config_version: u32,
    /// `PRAGMA user_version` per exported database, keyed by unit id.
    #[serde(default)]
    pub schema_versions: std::collections::BTreeMap<String, i64>,
    pub scope: Scope,
    /// False when `--no-secrets` stripped credentials; import says so in its
    /// summary rather than leaving the user to wonder why nothing authenticates.
    pub secrets_included: bool,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Scope {
    pub index: bool,
    pub platforms: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// `DataUnit::id` that claimed this path. Unknown ids are restored
    /// verbatim so an archive from a newer build does not lose data here.
    pub unit: String,
    /// Path relative to `MIYU_HOME`, always `/`-separated.
    pub path: String,
    pub size: u64,
    pub blake3: String,
}

impl Manifest {
    /// Databases whose schema is newer than this build can open. Import stops
    /// rather than let a migration run backwards.
    pub fn schemas_newer_than(&self, latest: i64) -> Vec<(&str, i64)> {
        self.schema_versions
            .iter()
            .filter(|(_, version)| **version > latest)
            .map(|(unit, version)| (unit.as_str(), *version))
            .collect()
    }
}
