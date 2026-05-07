use async_trait::async_trait;
use proxima_core::{
    AbstractionPayload, FactPayload, LlmCaps, ModelTier, Owner, PersonalityFlavor,
    PersonalitySelfDraft, PerspectivePayload, SchemaId, SchemaVersion, WakeFilter,
};

use crate::payloads::{
    CodeCommitSummarizerSelfV1, CodeDevelopmentPerspectiveV1, CodeEngineerSelfV1, CommitSummaryV1,
    CommitV1,
};

const COMMIT_SUMMARY_SYSTEM_PROMPT: &str = "You are a precise code-change summarizer running \
inside the Proxima causa-proxima substrate.\n\n\
The first user message is a typed wake-context JSON object. The triggering memory is a \
`proxima-code/commit-v1` Fact: call `core/fetch_memory` with its `triggering_memory_id` to \
read the commit and its associated facts.\n\n\
Then call `core/emit_abstraction` exactly once with:\n\
  - `schema_id` = \"proxima-code/commit-summary-v1\"\n\
  - `schema_version` = 1\n\
  - `payload` = a JSON object matching the schema with keys: `repo_id` (UUID), \
`commit_sha` (string), `summary` (1-3 sentences), `key_files` (string array, max 5), \
`change_kind` (one of: feature, fix, refactor, docs, test, chore, other).\n\n\
Do not call any other tool. After `core/emit_abstraction` succeeds, end your turn.";

const ENGINEER_SYSTEM_PROMPT: &str = "You are a senior development reviewer running inside \
the Proxima causa-proxima substrate.\n\n\
The first user message is a typed wake-context JSON object. The triggering memory is a \
`proxima-code/commit-summary-v1` Abstraction: call `core/fetch_memory` with its \
`triggering_memory_id` to read it.\n\n\
Then call `core/emit_perspective` exactly once with:\n\
  - `schema_id` = \"proxima-code/development-perspective-v1\"\n\
  - `schema_version` = 1\n\
  - `payload` = a JSON object matching the schema with keys: `repo_id` (UUID, may be null), \
`summary`, `pattern`, `risk`, `recommended_posture`, and `confidence` (0.0-1.0).\n\n\
Do not call any other tool. After `core/emit_perspective` succeeds, end your turn.";

#[derive(Debug, Default, Clone)]
pub struct CommitSummaryPersonality;

#[derive(Debug, Default, Clone)]
pub struct CodeEngineerPersonality;

#[async_trait]
impl PersonalityFlavor for CommitSummaryPersonality {
    fn personality_type_id(&self) -> &'static str {
        "proxima-code/commit-summary-v1"
    }

    fn self_schema(&self) -> SchemaId {
        SchemaId::new(CodeCommitSummarizerSelfV1::SCHEMA_ID.to_string())
    }

    fn default_self_payload(
        &self,
        _owner: &Owner,
        payload_overrides: Option<&serde_json::Value>,
    ) -> Result<PersonalitySelfDraft, proxima_core::ProtocolError> {
        let display_name = payload_overrides
            .and_then(|v| v.get("display_name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Commit Summarizer")
            .to_string();
        let purpose = payload_overrides
            .and_then(|v| v.get("purpose"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Summarize commits as Abstractions")
            .to_string();
        let payload = CodeCommitSummarizerSelfV1 {
            display_name: display_name.clone(),
            purpose,
        };
        Ok(PersonalitySelfDraft {
            schema_id: self.self_schema(),
            schema_version: SchemaVersion::new(CodeCommitSummarizerSelfV1::SCHEMA_VERSION),
            text: display_name,
            typed_payload: serde_json::to_value(payload)
                .map_err(|e| proxima_core::ProtocolError::internal(e.to_string()))?,
        })
    }

    fn system_prompt(&self) -> &'static str {
        COMMIT_SUMMARY_SYSTEM_PROMPT
    }

    fn writeable_schemas(&self) -> &'static [&'static str] {
        &[CommitSummaryV1::SCHEMA_ID]
    }

    fn writeable_relations(&self) -> &'static [&'static str] {
        &[]
    }

    fn default_wake_filters(&self) -> Vec<WakeFilter> {
        vec![WakeFilter::on_memory(SchemaId::new(
            CommitV1::SCHEMA_ID.to_string(),
        ))]
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Fast
    }

    fn requires(&self) -> LlmCaps {
        LlmCaps {
            tool_use: true,
            ..LlmCaps::none()
        }
    }
}

#[async_trait]
impl PersonalityFlavor for CodeEngineerPersonality {
    fn personality_type_id(&self) -> &'static str {
        "proxima-code/engineer-v1"
    }

    fn self_schema(&self) -> SchemaId {
        SchemaId::new(CodeEngineerSelfV1::SCHEMA_ID.to_string())
    }

    fn default_self_payload(
        &self,
        _owner: &Owner,
        payload_overrides: Option<&serde_json::Value>,
    ) -> Result<PersonalitySelfDraft, proxima_core::ProtocolError> {
        let display_name = payload_overrides
            .and_then(|v| v.get("display_name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Engineer")
            .to_string();
        let purpose = payload_overrides
            .and_then(|v| v.get("purpose"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Develop perspectives on code changes")
            .to_string();
        let payload = CodeEngineerSelfV1 {
            display_name: display_name.clone(),
            purpose,
        };
        Ok(PersonalitySelfDraft {
            schema_id: self.self_schema(),
            schema_version: SchemaVersion::new(CodeEngineerSelfV1::SCHEMA_VERSION),
            text: display_name,
            typed_payload: serde_json::to_value(payload)
                .map_err(|e| proxima_core::ProtocolError::internal(e.to_string()))?,
        })
    }

    fn system_prompt(&self) -> &'static str {
        ENGINEER_SYSTEM_PROMPT
    }

    fn writeable_schemas(&self) -> &'static [&'static str] {
        &[CodeDevelopmentPerspectiveV1::SCHEMA_ID]
    }

    fn writeable_relations(&self) -> &'static [&'static str] {
        &[]
    }

    fn default_wake_filters(&self) -> Vec<WakeFilter> {
        vec![WakeFilter::on_memory(SchemaId::new(
            CommitSummaryV1::SCHEMA_ID.to_string(),
        ))]
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Standard
    }

    fn requires(&self) -> LlmCaps {
        LlmCaps {
            tool_use: true,
            ..LlmCaps::none()
        }
    }
}
