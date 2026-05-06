//! Code A→P operator: recent commit summaries to development posture.

use async_trait::async_trait;
use proxima_core::operators::{
    A2PContext, A2PContextSpec, A2POperator, AbstractionRow, NewPerspective, OperatorError,
};
use proxima_core::{
    AbstractionPayload, LlmCaps, ModelTier, PerspectivePayload, SchemaId, SchemaVersion,
};
use serde::Deserialize;

use crate::payloads::{CodeDevelopmentPerspectiveV1, CommitSummaryV1};

const SYSTEM_PROMPT: &str = "You are a senior development reviewer. \
Given recent commit summaries, infer the current development perspective. \
Output ONLY a JSON object with keys: summary, pattern, risk, \
recommended_posture, confidence. confidence is a number from 0 to 1.";

const PROMPT_VERSION: &str = "v1";
const OPERATOR_ID: &str = "proxima-code/development-perspective";
const INPUT_LIMIT: usize = 64;

#[derive(Debug, Deserialize)]
struct LlmOutput {
    summary: String,
    pattern: String,
    risk: String,
    recommended_posture: String,
    confidence: f32,
}

#[derive(Debug, Default, Clone)]
pub struct CodeDevelopmentPerspectiveOperator;

impl CodeDevelopmentPerspectiveOperator {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl A2POperator for CodeDevelopmentPerspectiveOperator {
    fn operator_id(&self) -> &'static str {
        OPERATOR_ID
    }

    fn output_schema_id(&self) -> &'static str {
        CodeDevelopmentPerspectiveV1::SCHEMA_ID
    }

    fn output_schema_version(&self) -> u32 {
        CodeDevelopmentPerspectiveV1::SCHEMA_VERSION
    }

    fn prompt_version(&self) -> &'static str {
        PROMPT_VERSION
    }

    fn consumes(&self, schema_id: &SchemaId) -> bool {
        schema_id.as_str() == CommitSummaryV1::SCHEMA_ID
    }

    fn context(&self) -> A2PContextSpec {
        A2PContextSpec {
            kind: "on_ingest".into(),
            key: OPERATOR_ID.into(),
            label: "Code development perspective".into(),
        }
    }

    fn input_limit(&self) -> usize {
        INPUT_LIMIT
    }

    async fn run(&self, ctx: A2PContext<'_>) -> Result<Vec<NewPerspective>, OperatorError> {
        let summaries = decode_commit_summaries(ctx.abstractions)?;
        if summaries.len() < 3 {
            return Ok(Vec::new());
        }

        let repo_id = shared_repo_id(&summaries);
        let user_prompt = render_user_prompt(&summaries);
        let raw = ctx.llm.complete_json(SYSTEM_PROMPT, &user_prompt).await?;
        let parsed: LlmOutput = serde_json::from_value(raw.clone()).map_err(|e| {
            OperatorError::OutputValidation(format!(
                "LLM output failed schema decode: {e}; raw: {raw}"
            ))
        })?;
        let confidence = parsed.confidence.clamp(0.0, 1.0);

        let payload = CodeDevelopmentPerspectiveV1 {
            repo_id,
            summary: parsed.summary,
            pattern: parsed.pattern,
            risk: parsed.risk,
            recommended_posture: parsed.recommended_posture,
            confidence,
        };
        let text = render_text(&payload);
        let embedding = ctx.embed.embed(&text).await?;
        let typed_payload = serde_json::to_value(&payload).map_err(|e| {
            OperatorError::Internal(format!("serialize CodeDevelopmentPerspectiveV1: {e}"))
        })?;

        Ok(vec![NewPerspective {
            schema_id: SchemaId::new(CodeDevelopmentPerspectiveV1::SCHEMA_ID.to_string()),
            schema_version: SchemaVersion::new(CodeDevelopmentPerspectiveV1::SCHEMA_VERSION),
            text,
            typed_payload,
            provenance: ctx.abstractions.iter().map(|a| a.memory_id).collect(),
            embedding,
            embedding_model_id: ctx.embed.model_id().to_string(),
        }])
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Deep
    }

    fn requires(&self) -> LlmCaps {
        LlmCaps {
            json_mode: true,
            ..LlmCaps::none()
        }
    }
}

fn decode_commit_summaries(
    rows: &[AbstractionRow],
) -> Result<Vec<(CommitSummaryV1, proxima_core::MemoryId, String)>, OperatorError> {
    let mut out = Vec::new();
    for row in rows {
        if row.schema_id.as_str() != CommitSummaryV1::SCHEMA_ID {
            continue;
        }
        let payload: CommitSummaryV1 = serde_json::from_value(row.payload_json.clone())
            .map_err(|e| OperatorError::Internal(format!("decode CommitSummaryV1: {e}")))?;
        out.push((payload, row.memory_id, row.text.clone()));
    }
    Ok(out)
}

fn shared_repo_id(
    summaries: &[(CommitSummaryV1, proxima_core::MemoryId, String)],
) -> Option<uuid::Uuid> {
    let first = summaries.first()?.0.repo_id;
    summaries
        .iter()
        .all(|(s, _, _)| s.repo_id == first)
        .then_some(first)
}

fn render_user_prompt(summaries: &[(CommitSummaryV1, proxima_core::MemoryId, String)]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "Recent commit summaries:");
    for (summary, _, text) in summaries {
        let _ = writeln!(
            out,
            "\n- {} [{}]\n  {}\n  key_files={:?}\n  text={}",
            summary.commit_sha, summary.change_kind, summary.summary, summary.key_files, text
        );
    }
    out
}

fn render_text(payload: &CodeDevelopmentPerspectiveV1) -> String {
    format!(
        "{}\n\nPattern: {}\nRisk: {}\nPosture: {}\nConfidence: {:.2}",
        payload.summary,
        payload.pattern,
        payload.risk,
        payload.recommended_posture,
        payload.confidence
    )
}
