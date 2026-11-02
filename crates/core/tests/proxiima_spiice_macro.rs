use proxiima_core::PerspectivePayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct DemoSelfPayload {
    display_name: String,
}

impl PerspectivePayload for DemoSelfPayload {
    const SCHEMA_ID: &'static str = "proxiima-test/self-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxiima_test.self_v1"
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DemoOutputPayload {
    summary: String,
}

impl PerspectivePayload for DemoOutputPayload {
    const SCHEMA_ID: &'static str = "proxiima-test/out-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxiima_test.out_v1"
    }
}

proxiima_core::proxiima_spiice! {
    name = "proxiima-test",
    perspective_schemas = [
        DemoSelfPayload,
        DemoOutputPayload,
    ],
}
