//! Online template repository index (STRC-19). Fetching is done by the
//! app; this module defines the index format and verifies downloads.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::diag::{Error, Result};

/// One downloadable template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub url: String,
    /// Lower-case hex SHA-256 of the file at `url`.
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `hexpat`, `bt`, `ksy` or `h`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
}

/// Repository index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateIndex {
    pub templates: Vec<IndexEntry>,
}

impl TemplateIndex {
    /// Parses and validates an index (unique ids, http(s) URLs, SHA-256 hex).
    pub fn from_json(json: &str) -> Result<Self> {
        let index: TemplateIndex = serde_json::from_str(json)
            .map_err(|e| Error::invalid(format!("invalid template index: {e}")))?;
        index.validate()?;
        Ok(index)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for e in &self.templates {
            if e.id.is_empty()
                || !e
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            {
                return Err(Error::invalid(format!("invalid template id `{}`", e.id)));
            }
            if !ids.insert(e.id.as_str()) {
                return Err(Error::invalid(format!("duplicate template id `{}`", e.id)));
            }
            if !(e.url.starts_with("https://") || e.url.starts_with("http://")) {
                return Err(Error::invalid(format!("`{}`: URL must be http(s)", e.id)));
            }
            if e.sha256.len() != 64 || !e.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(Error::invalid(format!(
                    "`{}`: sha256 must be 64 hex digits",
                    e.id
                )));
            }
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&IndexEntry> {
        self.templates.iter().find(|e| e.id == id)
    }
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Checks downloaded bytes against the entry's hash.
pub fn verify(entry: &IndexEntry, bytes: &[u8]) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(&entry.sha256) {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "checksum mismatch for `{}`: expected {}, got {actual}",
            entry.id, entry.sha256
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn index(sha: &str) -> String {
        format!(
            r#"{{"templates": [{{"id": "png", "name": "PNG", "version": "1.0", "url": "https://example.com/png.hexpat", "sha256": "{sha}", "extensions": ["png"]}}]}}"#
        )
    }

    #[test]
    fn parse_and_verify() {
        assert_eq!(sha256_hex(b"hello"), HELLO);
        let idx = TemplateIndex::from_json(&index(HELLO)).unwrap();
        let e = idx.get("png").unwrap();
        assert!(verify(e, b"hello").is_ok());
        assert!(
            verify(e, b"hellO")
                .unwrap_err()
                .to_string()
                .contains("mismatch")
        );
        let round = TemplateIndex::from_json(&idx.to_json()).unwrap();
        assert_eq!(round, idx);
        assert!(idx.to_json().contains("\"sha256\""));
    }

    #[test]
    fn validation() {
        assert!(TemplateIndex::from_json(&index("abc")).is_err());
        assert!(TemplateIndex::from_json("{}").is_err());
        let dup = format!(
            r#"{{"templates": [{e}, {e}]}}"#,
            e = format!(
                r#"{{"id": "a", "name": "A", "version": "1", "url": "https://x/a", "sha256": "{HELLO}"}}"#
            )
        );
        assert!(
            TemplateIndex::from_json(&dup)
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
        let bad_url = index(HELLO).replace("https://", "file://");
        assert!(TemplateIndex::from_json(&bad_url).is_err());
    }
}
