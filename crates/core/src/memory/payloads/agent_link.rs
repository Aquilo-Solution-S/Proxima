use crate::{EdgePayload, RelationClass};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLinkV1 {
    pub reason: String,
    pub confidence: u8,
}

impl EdgePayload for AgentLinkV1 {
    const SCHEMA_ID: &'static str = "core/agent-link-v1";
    const SCHEMA_VERSION: u32 = 1;
    const RELATION_CLASS: RelationClass = RelationClass::Interpretive;

    fn sidecar_table() -> &'static str {
        "proxima_core.agent_link_v1"
    }
}
