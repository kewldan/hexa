//! YARA rules over a document (SRCH-09), using the pure-Rust `boreal` engine.

use boreal::Compiler;
use serde::Serialize;
use tauri::State;

use crate::error::{CmdError, CmdResult};
use crate::state::AppState;

/// Largest range scanned in one go.
const MAX_SCAN: u64 = 1 << 30;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YaraString {
    pub identifier: String,
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YaraRuleMatch {
    pub rule: String,
    pub namespace: String,
    pub tags: Vec<String>,
    pub strings: Vec<YaraString>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct YaraReport {
    pub matches: Vec<YaraRuleMatch>,
    pub warnings: Vec<String>,
}

/// Compiles `rules` and scans `[start, end)`; offsets are absolute.
#[tauri::command]
pub async fn yara_scan(
    state: State<'_, AppState>,
    id: u64,
    rules: String,
    start: u64,
    end: u64,
) -> CmdResult<YaraReport> {
    let tree = state.get(id)?.view.read().tree.clone();
    let end = end.min(tree.len());
    if end.saturating_sub(start) > MAX_SCAN {
        return Err(CmdError::invalid("range is larger than 1 GiB"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let data = tree.read_vec(start, (end - start) as usize)?;
        scan_bytes(&rules, &data, start)
    })
    .await
    .map_err(|e| CmdError::new("internal", e.to_string()))?
}

/// Compiles `rules` and scans `data`, reporting offsets from `base`.
pub fn scan_bytes(rules: &str, data: &[u8], base: u64) -> CmdResult<YaraReport> {
    let mut compiler = Compiler::new();
    let status = compiler
        .add_rules_str(rules)
        .map_err(|e| CmdError::new("yara", e.to_string()))?;
    let warnings = status.warnings().map(ToString::to_string).collect();
    let scanner = compiler.finalize();
    let result = match scanner.scan_mem(data) {
        Ok(r) | Err((_, r)) => r,
    };
    let matches = result
        .rules
        .iter()
        .map(|r| YaraRuleMatch {
            rule: r.name.to_owned(),
            namespace: r.namespace.to_owned(),
            tags: r
                .tags
                .iter()
                .map(|t| scanner.get_string_symbol(*t).to_owned())
                .collect(),
            strings: r
                .matches
                .iter()
                .flat_map(|s| {
                    s.matches.iter().map(|m| YaraString {
                        identifier: format!("${}", s.name),
                        offset: base + m.offset as u64,
                        len: m.length as u64,
                    })
                })
                .collect(),
        })
        .collect();
    Ok(YaraReport { matches, warnings })
}

#[cfg(test)]
mod tests {
    use super::scan_bytes;

    #[test]
    fn finds_strings_with_absolute_offsets() {
        let rules = r#"rule hello : greeting { strings: $a = "hello" condition: $a }
                       rule never { condition: false }"#;
        let r = scan_bytes(rules, b"xx hello hello", 0x100).unwrap();
        assert_eq!(r.matches.len(), 1);
        assert_eq!(r.matches[0].rule, "hello");
        assert_eq!(r.matches[0].tags, ["greeting"]);
        let offs: Vec<u64> = r.matches[0].strings.iter().map(|s| s.offset).collect();
        assert_eq!(offs, [0x103, 0x109]);
        assert!(scan_bytes("rule broken {", b"", 0).is_err());
    }
}
