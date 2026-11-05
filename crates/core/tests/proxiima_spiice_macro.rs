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

impl proxiima_core::PayloadProjection for DemoSelfPayload {
    const KEY_FIELDS: &'static [proxiima_core::KeyField<Self>] = &[];

    fn display(&self) -> proxiima_core::PayloadDisplay {
        proxiima_core::PayloadDisplay {
            body: Some(self.display_name.clone()),
            ..Default::default()
        }
    }

    fn durable(&self) -> Result<proxiima_core::DurablePayload, String> {
        let mut builder = proxiima_core::DurablePayloadBuilder::new();
        builder.field("display_name", &self.display_name)?;
        Ok(builder.finish())
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

impl proxiima_core::PayloadProjection for DemoOutputPayload {
    const KEY_FIELDS: &'static [proxiima_core::KeyField<Self>] = &[];

    fn display(&self) -> proxiima_core::PayloadDisplay {
        proxiima_core::PayloadDisplay {
            body: Some(self.summary.clone()),
            ..Default::default()
        }
    }

    fn durable(&self) -> Result<proxiima_core::DurablePayload, String> {
        let mut builder = proxiima_core::DurablePayloadBuilder::new();
        builder.field("summary", &self.summary)?;
        Ok(builder.finish())
    }
}

proxiima_core::proxiima_spiice! {
    name = "proxiima-test",
    perspective_schemas = [
        DemoSelfPayload,
        DemoOutputPayload,
    ],
}
