use std::collections::HashSet;

use proxima_core::{MemoryId, ToolCtx, ToolError};

use crate::payloads::{AcceptanceCriterionV1, AcceptanceVerifierKind};

use super::super::CodeToolCtxExt;
use super::types::{ExecutionPlanItemArgs, ExecutionPlanItemKind};

pub(super) fn normalize_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, ToolError> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len < min || len > max {
        return Err(ToolError::InvalidInput(format!(
            "{field} must be {min}..={max} chars"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn validate_acceptance_criteria(
    criteria: Vec<AcceptanceCriterionV1>,
) -> Result<Vec<AcceptanceCriterionV1>, ToolError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(criteria.len());
    for mut criterion in criteria {
        criterion.key = normalize_text("acceptance_criteria.key", &criterion.key, 1, 80)?;
        criterion.description = normalize_text(
            "acceptance_criteria.description",
            &criterion.description,
            1,
            1000,
        )?;
        if !criterion
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(ToolError::InvalidInput(
                "acceptance_criteria.key must contain only ASCII letters, digits, '-' or '_'"
                    .into(),
            ));
        }
        if !seen.insert(criterion.key.clone()) {
            return Err(ToolError::InvalidInput(format!(
                "duplicate acceptance criterion key: {}",
                criterion.key
            )));
        }
        validate_acceptance_verifier_spec(&criterion)?;
        out.push(criterion);
    }
    Ok(out)
}

pub(super) fn validate_plan_items(
    items: Vec<ExecutionPlanItemArgs>,
) -> Result<Vec<ExecutionPlanItemArgs>, ToolError> {
    if items.is_empty() || items.len() > 20 {
        return Err(ToolError::InvalidInput(
            "items must contain 1..=20 plan requests".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut prior = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for mut item in items {
        item.key = normalize_text("items.key", &item.key, 1, 80)?;
        if !item
            .key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(ToolError::InvalidInput(
                "items.key must contain only ASCII letters, digits, '-' or '_'".into(),
            ));
        }
        if !seen.insert(item.key.clone()) {
            return Err(ToolError::InvalidInput(format!(
                "duplicate item key: {}",
                item.key
            )));
        }
        item.title = normalize_text("items.title", &item.title, 1, 240)?;
        item.instructions = normalize_text("items.instructions", &item.instructions, 1, 20_000)?;
        item.idempotency_key =
            normalize_text("items.idempotency_key", &item.idempotency_key, 1, 240)?;
        item.acceptance_criteria = validate_acceptance_criteria(item.acceptance_criteria)?;
        item.test_criteria = validate_acceptance_criteria(item.test_criteria)?;
        match item.kind {
            ExecutionPlanItemKind::Implementation => {
                if !item.test_criteria.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "implementation item {} must not set test_criteria",
                        item.key
                    )));
                }
            }
            ExecutionPlanItemKind::Test => {
                if !item.acceptance_criteria.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "test item {} must not set acceptance_criteria",
                        item.key
                    )));
                }
                if item.test_criteria.is_empty() {
                    return Err(ToolError::InvalidInput(format!(
                        "test item {} must set test_criteria",
                        item.key
                    )));
                }
                if !item
                    .test_criteria
                    .iter()
                    .any(|criterion| criterion.required)
                {
                    return Err(ToolError::InvalidInput(format!(
                        "test item {} must include at least one required test criterion",
                        item.key
                    )));
                }
            }
        }
        let mut item_deps = HashSet::new();
        let mut normalized_deps = Vec::with_capacity(item.depends_on.len());
        for dep in &item.depends_on {
            let dep = normalize_text("items.depends_on[]", dep, 1, 80)?;
            if !prior.contains(&dep) {
                return Err(ToolError::InvalidInput(format!(
                    "{} item {} depends on {}, but dependencies must reference earlier item keys",
                    item.kind.as_str(),
                    item.key,
                    dep
                )));
            }
            if !item_deps.insert(dep.clone()) {
                return Err(ToolError::InvalidInput(format!(
                    "item {} repeats dependency {}",
                    item.key, dep
                )));
            }
            normalized_deps.push(dep);
        }
        item.depends_on = normalized_deps;
        prior.insert(item.key.clone());
        out.push(item);
    }
    Ok(out)
}

fn validate_acceptance_verifier_spec(criterion: &AcceptanceCriterionV1) -> Result<(), ToolError> {
    match criterion.verifier_kind {
        AcceptanceVerifierKind::FileExists => {
            let path = criterion.verifier_spec.path.as_deref().ok_or_else(|| {
                ToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.path is required for file_exists",
                    criterion.key
                ))
            })?;
            let _ = normalize_text("acceptance_criteria.verifier_spec.path", path, 1, 1000)?;
        }
        AcceptanceVerifierKind::Command => {
            let command = criterion.verifier_spec.command.as_ref().ok_or_else(|| {
                ToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.command is required for command",
                    criterion.key
                ))
            })?;
            if command.is_empty() {
                return Err(ToolError::InvalidInput(format!(
                    "acceptance_criteria.{}.verifier_spec.command must not be empty",
                    criterion.key
                )));
            }
            for part in command {
                let _ =
                    normalize_text("acceptance_criteria.verifier_spec.command[]", part, 1, 2000)?;
            }
        }
        AcceptanceVerifierKind::BrowserSmoke
        | AcceptanceVerifierKind::DiffScope
        | AcceptanceVerifierKind::ReviewerOnly => {}
    }
    Ok(())
}

pub(super) fn resolve_evidence(ctx: &ToolCtx, raw: &[String]) -> Result<Vec<MemoryId>, ToolError> {
    raw.iter()
        .map(|value| ctx.resolve_fact_memory(value))
        .collect()
}
