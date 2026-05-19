use std::path::PathBuf;

use proxima_core::assert_no_serde_json_value_fields;

#[test]
fn payload_modules_do_not_use_serde_json_value_fields() {
    assert_no_serde_json_value_fields(&[
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/payloads")
    ]);
}
