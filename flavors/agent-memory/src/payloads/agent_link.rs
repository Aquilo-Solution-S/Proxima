use proxima_core::{EdgePayload, RelationClass};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLinkV1 {
    pub reason: String,
    pub confidence: u8,
}

impl EdgePayload for AgentLinkV1 {
    const SCHEMA_ID: &'static str = "proxima-agent-memory/agent-link-v1";
    const SCHEMA_VERSION: u32 = 1;
    const RELATION_CLASS: RelationClass = RelationClass::Structural;

    fn sidecar_table() -> &'static str {
        "proxima_agent_memory.agent_link_v1"
    }
}
