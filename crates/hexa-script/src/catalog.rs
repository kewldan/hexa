//! Plugin and template catalog: format, checksum verification and
//! installation (SCR-05). Downloading is done by the application.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::plugin::{
    DEFAULT_PLUGIN_MODULE, PLUGIN_MANIFEST_FILE, Plugin, PluginError, PluginKind, PluginLimits,
    PluginManifest, is_valid_id,
};

/// Current catalog schema version.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

/// A downloadable plugin; the URL points at the `.wasm` module (SCR-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPlugin {
    /// Plugin id (becomes the install directory name).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version string.
    pub version: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Download URL of the module.
    pub url: String,
    /// Lowercase hex SHA-256 of the module.
    pub sha256: String,
    /// Provided kinds.
    pub kinds: Vec<PluginKind>,
}

/// A downloadable structure template (SCR-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTemplate {
    /// Template id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version string.
    pub version: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Download URL of the template file.
    pub url: String,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// File name to install as; defaults to the last URL path segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

/// The catalog document (SCR-05).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// Schema version, currently [`CATALOG_SCHEMA_VERSION`].
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    /// Available plugins.
    #[serde(default)]
    pub plugins: Vec<CatalogPlugin>,
    /// Available templates.
    #[serde(default)]
    pub templates: Vec<CatalogTemplate>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema_version: CATALOG_SCHEMA_VERSION,
            plugins: Vec::new(),
            templates: Vec::new(),
        }
    }
}

fn default_schema() -> u32 {
    CATALOG_SCHEMA_VERSION
}

/// Catalog errors (SCR-05).
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// Malformed JSON.
    #[error("invalid catalog JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Structurally invalid catalog or entry.
    #[error("invalid catalog: {0}")]
    Invalid(String),
    /// Unknown entry id.
    #[error("no catalog entry {0:?}")]
    NotFound(String),
    /// Downloaded bytes do not match the published hash.
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Hash from the catalog.
        expected: String,
        /// Hash of the downloaded bytes.
        actual: String,
    },
    /// The downloaded module is not a valid plugin.
    #[error(transparent)]
    Plugin(#[from] PluginError),
    /// File system error during installation.
    #[error("{path}: {source}")]
    Io {
        /// Offending path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Checks `bytes` against a hex SHA-256 (case-insensitive).
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), CatalogError> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(CatalogError::ChecksumMismatch {
            expected: expected.to_owned(),
            actual,
        })
    }
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> CatalogError + '_ {
    move |source| CatalogError::Io {
        path: path.to_owned(),
        source,
    }
}

impl Catalog {
    /// Parses and validates a catalog.
    pub fn from_json(json: &str) -> Result<Self, CatalogError> {
        let catalog: Self = serde_json::from_str(json)?;
        catalog.validate()?;
        Ok(catalog)
    }

    /// Pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, CatalogError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Checks ids, hashes, kinds and duplicates.
    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.schema_version > CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::Invalid(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.plugins {
            check_entry("plugin", &p.id, &p.sha256)?;
            if p.kinds.is_empty() {
                return Err(CatalogError::Invalid(format!(
                    "plugin {:?} has no kinds",
                    p.id
                )));
            }
            if !seen.insert(("plugin", p.id.as_str())) {
                return Err(CatalogError::Invalid(format!(
                    "duplicate plugin {:?}",
                    p.id
                )));
            }
        }
        for t in &self.templates {
            check_entry("template", &t.id, &t.sha256)?;
            t.target_file_name()?;
            if !seen.insert(("template", t.id.as_str())) {
                return Err(CatalogError::Invalid(format!(
                    "duplicate template {:?}",
                    t.id
                )));
            }
        }
        Ok(())
    }

    /// Plugin entry by id.
    pub fn plugin(&self, id: &str) -> Option<&CatalogPlugin> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// Template entry by id.
    pub fn template(&self, id: &str) -> Option<&CatalogTemplate> {
        self.templates.iter().find(|t| t.id == id)
    }

    /// Verifies and installs the plugin `id` from downloaded `wasm` bytes.
    pub fn install_plugin(
        &self,
        id: &str,
        wasm: &[u8],
        plugins_dir: &Path,
    ) -> Result<PathBuf, CatalogError> {
        self.plugin(id)
            .ok_or_else(|| CatalogError::NotFound(id.to_owned()))?
            .install(wasm, plugins_dir)
    }

    /// Verifies and installs the template `id` from downloaded bytes.
    pub fn install_template(
        &self,
        id: &str,
        bytes: &[u8],
        templates_dir: &Path,
    ) -> Result<PathBuf, CatalogError> {
        self.template(id)
            .ok_or_else(|| CatalogError::NotFound(id.to_owned()))?
            .install(bytes, templates_dir)
    }
}

fn check_entry(what: &str, id: &str, sha256: &str) -> Result<(), CatalogError> {
    if !is_valid_id(id) {
        return Err(CatalogError::Invalid(format!("invalid {what} id {id:?}")));
    }
    if !is_sha256(sha256) {
        return Err(CatalogError::Invalid(format!(
            "{what} {id:?} has an invalid sha256 {sha256:?}"
        )));
    }
    Ok(())
}

impl CatalogPlugin {
    /// The manifest written next to the installed module.
    pub fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            kinds: self.kinds.clone(),
            module: DEFAULT_PLUGIN_MODULE.to_owned(),
        }
    }

    /// Verifies the hash, validates the module, and atomically installs it as
    /// `<plugins_dir>/<id>/{manifest.json, plugin.wasm}` (replacing an older
    /// version). Returns the plugin directory.
    pub fn install(&self, wasm: &[u8], plugins_dir: &Path) -> Result<PathBuf, CatalogError> {
        check_entry("plugin", &self.id, &self.sha256)?;
        verify_sha256(wasm, &self.sha256)?;
        let manifest = self.manifest();
        Plugin::from_bytes(manifest.clone(), wasm, PluginLimits::default())?;

        std::fs::create_dir_all(plugins_dir).map_err(io_err(plugins_dir))?;
        let staging = plugins_dir.join(format!(".{}.installing", self.id));
        let backup = plugins_dir.join(format!(".{}.old", self.id));
        let target = plugins_dir.join(&self.id);
        remove_dir_if_exists(&staging)?;
        remove_dir_if_exists(&backup)?;
        std::fs::create_dir(&staging).map_err(io_err(&staging))?;
        let module_path = staging.join(&manifest.module);
        std::fs::write(&module_path, wasm).map_err(io_err(&module_path))?;
        let manifest_path = staging.join(PLUGIN_MANIFEST_FILE);
        std::fs::write(&manifest_path, manifest.to_json()).map_err(io_err(&manifest_path))?;

        if target.exists() {
            std::fs::rename(&target, &backup).map_err(io_err(&target))?;
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            if backup.exists() {
                let _ = std::fs::rename(&backup, &target);
            }
            return Err(io_err(&target)(e));
        }
        remove_dir_if_exists(&backup)?;
        Ok(target)
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<(), CatalogError> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_err(path)(e)),
        _ => Ok(()),
    }
}

impl CatalogTemplate {
    /// The validated file name the template is installed as.
    pub fn target_file_name(&self) -> Result<String, CatalogError> {
        let name = match &self.file_name {
            Some(name) => name.clone(),
            None => self
                .url
                .split(['?', '#'])
                .next()
                .and_then(|u| u.rsplit('/').next())
                .unwrap_or_default()
                .to_owned(),
        };
        if is_valid_id(&name) {
            Ok(name)
        } else {
            Err(CatalogError::Invalid(format!(
                "template {:?} has no valid file name (got {name:?})",
                self.id
            )))
        }
    }

    /// Verifies the hash and writes `<templates_dir>/<file name>` (via a
    /// temporary file and rename). Returns the installed path.
    pub fn install(&self, bytes: &[u8], templates_dir: &Path) -> Result<PathBuf, CatalogError> {
        check_entry("template", &self.id, &self.sha256)?;
        verify_sha256(bytes, &self.sha256)?;
        let file_name = self.target_file_name()?;
        std::fs::create_dir_all(templates_dir).map_err(io_err(templates_dir))?;
        let staging = templates_dir.join(format!(".{file_name}.installing"));
        let target = templates_dir.join(&file_name);
        std::fs::write(&staging, bytes).map_err(io_err(&staging))?;
        std::fs::rename(&staging, &target).map_err(io_err(&target))?;
        Ok(target)
    }
}
