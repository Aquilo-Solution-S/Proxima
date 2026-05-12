// Workspace review helper functions
use proxima_core::mcp::McpToolError;

use crate::mcp::emit_execution_request::normalize_text;
use crate::payloads::WorkspaceReviewFinding;

use super::types::{LoadedWorkspaceDecision, LoadedWorkspaceReview};

/// Validate workspace review findings.
///
/// # Errors
///
/// Returns an error if any finding field fails normalization.
pub fn validate_findings(findings: &[WorkspaceReviewFinding]) -> Result<(), McpToolError> {
    for finding in findings {
        normalize_text("finding.severity", &finding.severity, 1, 80)?;
        normalize_text("finding.message", &finding.message, 1, 2000)?;
        if let Some(path) = &finding.file_path {
            normalize_text("finding.file_path", path, 1, 500)?;
        }
    }
    Ok(())
}

/// Generate correction title from original title.
///
/// # Errors
///
/// Returns an error if the generated title fails normalization.
pub fn correction_title(title: &str) -> Result<String, McpToolError> {
    let prefixed = format!("Correct: {}", title.trim());
    let mut output = String::new();
    for ch in prefixed.chars().take(240) {
        output.push(ch);
    }
    normalize_text("title", &output, 1, 240)
}

/// Generate correction instructions from review/decision context.
///
/// # Errors
///
/// Returns an error if the generated instructions fail normalization.
pub fn correction_instructions(
    prior_instructions: &str,
    review: Option<&LoadedWorkspaceReview>,
    decision: Option<&LoadedWorkspaceDecision>,
    request_key: &str,
) -> Result<String, McpToolError> {
    let findings = if let Some(review) = review {
        if review.payload.findings.is_empty() {
            "none".to_string()
        } else {
            review
                .payload
                .findings
                .iter()
                .map(|finding| {
                    let location = match (&finding.file_path, finding.line) {
                        (Some(path), Some(line)) => format!("{path}:{line}"),
                        (Some(path), None) => path.clone(),
                        _ => "general".to_string(),
                    };
                    format!("- [{}] {}: {}", finding.severity, location, finding.message)
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    } else {
        "none".to_string()
    };
    let review_memory = review.map_or_else(
        || "none".into(),
        |review| review.memory_id.into_inner().to_string(),
    );
    let workspace_run = review
        .map(|review| review.payload.workspace_run_memory_id.to_string())
        .or_else(|| decision.map(|decision| decision.payload.workspace_run_memory_id.to_string()))
        .unwrap_or_else(|| "unknown".into());
    let review_summary = review.map_or("none", |review| review.payload.summary.as_str());
    let correction_notes = review
        .and_then(|review| review.payload.correction_instructions.as_deref())
        .or_else(|| decision.and_then(|decision| decision.payload.reason_text.as_deref()))
        .unwrap_or("none");
    let retry_decision = decision.map_or_else(
        || "none".into(),
        |decision| decision.memory_id.into_inner().to_string(),
    );
    let retry_reason = decision
        .and_then(|decision| decision.payload.reason_text.as_deref())
        .unwrap_or("none");
    let instructions = format!(
        "{}\n\nCorrection context:\nworkspace_review: {}\nworkspace_decision: {}\nworkspace_run: {}\nretry_key: {}\nreview_summary: {}\nretry_reason: {}\ncorrection_instructions: {}\nfindings:\n{}",
        prior_instructions.trim(),
        review_memory,
        retry_decision,
        workspace_run,
        request_key,
        review_summary,
        retry_reason,
        correction_notes,
        findings,
    );
    normalize_text("instructions", &instructions, 1, 20_000)
}
