use std::path::PathBuf;

use proxima_core::assert_no_serde_json_value_fields;

#[test]
fn payload_modules_do_not_use_serde_json_value_fields() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_no_serde_json_value_fields(&[
        root.join("crates/core/src/citations.rs"),
        root.join("crates/core/src/memory/payloads"),
    ]);
}
