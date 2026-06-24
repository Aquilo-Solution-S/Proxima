use proxima_core::{EdgePayload, RelationClass};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EdgeCallsV1 {
    pub callsite_byte_start: u32,
    pub callsite_byte_end: u32,
    pub callee_name: String,
    pub is_dynamic: bool,
}

impl EdgePayload for EdgeCallsV1 {
    const SCHEMA_ID: &'static str = "proxima-code/calls";
    const SCHEMA_VERSION: u32 = 1;
    const RELATION_CLASS: RelationClass = RelationClass::Structural;
    fn sidecar_table() -> &'static str {
        "proxima_code.code_calls_v1"
    }
}
