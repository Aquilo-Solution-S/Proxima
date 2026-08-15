use std::collections::HashSet;

use proxima_core::{MemoryId, ToolCtx, ToolError};

use crate::payloads::{AcceptanceCriterionV1, AcceptanceVerifierKind};

use super::super::CodeToolCtxExt;
use super::types::{ExecutionPlanItemArgs, ExecutionPlanItemKind};

/// Trim `value` and check it against `1..=max` characters.
///
/// Delegates to [`proxima_core::validate_trimmed_len`] rather than
/// repeating the check, so this flavor and the substrate refuse a blank
/// value in the same words. Floor is 1 (no `min` parameter).
///
/// # Errors
///
/// [`ToolError::InvalidInput`] when `value` is empty after trimming, or
/// longer than `max` characters.
pub(super) fn normalize_text(field: &str, value: &str, max: usize) -> Result<String, ToolError> {
    proxima_core::validate_trimmed_len(field, value, max).map(str::to_string)
}

pub(super) fn validate_acceptance_criteria(
    criteria: Vec<AcceptanceCriterionV1>,
) -> Result<Vec<AcceptanceCriterionV1>, ToolError> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(criteria.len());
    for mut criterion in criteria {
        criterion.key = normalize_text("acceptance_criteria.key", &criterion.key, 80)?;
        criterion.description = normalize_text(
            "acceptance_criteria.description",
            &criterion.description,
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
        item.key = normalize_text("items.key", &item.key, 80)?;
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
        item.title = normalize_text("items.title", &item.title, 240)?;
        item.instructions = normalize_text("items.instructions", &item.instructions, 20_000)?;
        item.idempotency_key = normalize_text("items.idempotency_key", &item.idempotency_key, 240)?;
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
            let dep = normalize_text("items.depends_on[]", dep, 80)?;
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
            let _ = normalize_text("acceptance_criteria.verifier_spec.path", path, 1000)?;
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
                let _ = normalize_text("acceptance_criteria.verifier_spec.command[]", part, 2000)?;
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

#[cfg(test)]
mod tests {
    use super::normalize_text;
    use proxima_core::ToolError;

    fn message(result: Result<String, ToolError>) -> String {
        match result.expect_err("must be refused") {
            ToolError::InvalidInput(message) => message,
            other => panic!("must be invalid input, got {other:?}"),
        }
    }

    /// A blank value and an over-length one are different mistakes with
    /// different fixes.
    #[test]
    fn blank_and_oversized_do_not_share_one_message() {
        let blank = message(normalize_text("title", "   ", 240));
        let over = message(normalize_text("title", &"a".repeat(241), 240));
        assert_ne!(blank, over, "one message for two mistakes tells neither");
        assert!(
            !blank.contains("240"),
            "a blank value must not be quoted a bound it satisfies: {blank}"
        );
        assert!(
            over.contains("240") && over.contains("241"),
            "an oversized value must be told the cap and what it sent: {over}"
        );
    }

    /// The point of delegating is that the flavor and the substrate refuse
    /// the same input in the same words. Comparing the two messages directly
    /// is what stops a well-meaning local copy from reappearing.
    #[test]
    fn the_flavor_refuses_in_the_substrate_s_words() {
        for (value, max) in [("   ", 240), (&"a".repeat(241) as &str, 240)] {
            let flavor = message(normalize_text("title", value, max));
            let substrate = match proxima_core::validate_trimmed_len("title", value, max)
                .expect_err("must be refused")
            {
                ToolError::InvalidInput(message) => message,
                other => panic!("must be invalid input, got {other:?}"),
            };
            assert_eq!(
                flavor, substrate,
                "the flavor must refuse in the substrate's words, not its own",
            );
        }
    }

    /// The field name is the caller's only pointer back into the schema, so
    /// it has to survive delegation.
    #[test]
    fn the_message_names_the_field_that_failed() {
        assert!(
            message(normalize_text("items.instructions", "", 20_000))
                .starts_with("items.instructions")
        );
    }
}
