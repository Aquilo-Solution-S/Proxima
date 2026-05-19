//! Build-time payload source checks shared by core and flavors.

use std::fs;
use std::path::{Path, PathBuf};

/// Assert that payload modules do not expose `serde_json::Value` fields.
///
/// This is a source-level contract helper for integration tests. It is not a
/// schema registry or runtime registration path.
///
/// # Panics
///
/// Panics when a path cannot be read or when an offending field is found.
pub fn assert_no_serde_json_value_fields(paths: &[PathBuf]) {
    let mut offenders = Vec::new();
    for path in paths {
        collect_offenders(path, &mut offenders);
    }

    assert!(
        offenders.is_empty(),
        "payload structs must not expose serde_json::Value fields:\n{}",
        offenders.join("\n")
    );
}

fn collect_offenders(path: &Path, offenders: &mut Vec<String>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("read payload dir") {
            let entry = entry.expect("read dir entry");
            collect_offenders(&entry.path(), offenders);
        }
        return;
    }
    if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
        return;
    }
    let text = fs::read_to_string(path).expect("read payload source");
    for (index, line) in text.lines().enumerate() {
        if is_forbidden_value_field(line) {
            offenders.push(format!("{}:{}", path.display(), index + 1));
        }
    }
}

fn is_forbidden_value_field(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with("//")
        || trimmed.starts_with("type Value")
        || trimmed.contains("fn ")
        || trimmed.contains("->")
    {
        return false;
    }
    trimmed.contains(": serde_json::Value")
        || trimmed.contains(": Option<serde_json::Value")
        || trimmed.contains(": Vec<serde_json::Value")
        || (trimmed.contains(": Value")
            && (trimmed.starts_with("pub ") || trimmed.starts_with("pub(crate) ")))
}
