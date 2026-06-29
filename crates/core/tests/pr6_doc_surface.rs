use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn production_docs_do_not_advertise_retired_goal_self_wake_surfaces() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut offenders = Vec::new();
    for path in production_doc_paths(&root) {
        let text = fs::read_to_string(&path).expect("read production doc");
        let lower = text.to_lowercase();
        for retired in [
            concat!("core_", "personality"),
            concat!("core_", "wake"),
            concat!("target_", "personality"),
            "personality",
            concat!("personality", "_id"),
            "wake entry",
            "wake entries",
            "wake-entry",
            "read-scope",
            "read-scope matrix",
            "read scope",
            "`i:",
            "`w:",
        ] {
            if lower.contains(retired) {
                offenders.push(format!("{} contains {retired:?}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "production docs advertise retired PR6 surfaces:\n{}",
        offenders.join("\n")
    );
}

fn production_doc_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("README.md"), root.join("docs/lean/README.md")];
    collect_docs(&root.join("docs"), &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_docs(dir: &Path, out: &mut Vec<PathBuf>) {
    if is_ignored_doc_dir(dir) {
        return;
    }
    for entry in fs::read_dir(dir).expect("read docs dir") {
        let path = entry.expect("read docs entry").path();
        if path.is_dir() {
            collect_docs(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

fn is_ignored_doc_dir(dir: &Path) -> bool {
    let normalized = dir.to_string_lossy();
    normalized.ends_with("docs/lean") || normalized.contains("docs/superpowers")
}
