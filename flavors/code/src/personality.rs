use proxima_core::{
    Owner, PersonalityFlavor, PersonalitySelfDraft, PerspectivePayload, SchemaId, SchemaVersion,
};

use crate::payloads::{CodeCommitSummarizerSelfV1, CodeEngineerSelfV1};

#[derive(Debug, Default, Clone)]
pub struct CommitSummaryPersonality;

#[derive(Debug, Default, Clone)]
pub struct CodeEngineerPersonality;

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
}

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
}
