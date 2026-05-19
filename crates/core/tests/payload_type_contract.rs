use std::path::PathBuf;

use proxima_core::assert_no_serde_json_value_fields;

#[test]
fn payload_modules_do_not_use_serde_json_value_fields() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_no_serde_json_value_fields(&[
        root.join("crates/core/src/approval.rs"),
        root.join("crates/core/src/citations.rs"),
        root.join("crates/core/src/inquiry.rs"),
        root.join("crates/core/src/intervention.rs"),
        root.join("crates/core/src/mcp/core_tools/payload.rs"),
        root.join("crates/core/src/wake/trace/mod.rs"),
    ]);
}
