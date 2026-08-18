//! Bind keys for `core_episode_commit` (`remember:N` / `derive` / `stance:N` / `goal:N`).

use std::collections::HashSet;

use crate::mcp::McpToolError;

#[derive(Debug, Default)]
pub(super) struct BindSet {
    pub remember: HashSet<usize>,
    pub derive: bool,
    pub stance: HashSet<usize>,
    pub goal: HashSet<usize>,
}

pub(super) fn parse_bind(
    raw: &[String],
    remember_len: usize,
    has_derive: bool,
    stance_len: usize,
    goal_len: usize,
) -> Result<BindSet, McpToolError> {
    let mut out = BindSet::default();
    for key in raw {
        if let Some(idx) = key.strip_prefix("remember:") {
            let idx = parse_index(key, idx, remember_len)?;
            out.remember.insert(idx);
            continue;
        }
        if key == "derive" {
            if !has_derive {
                return Err(McpToolError::InvalidInput(
                    "bind key derive is out of range".into(),
                ));
            }
            out.derive = true;
            continue;
        }
        if let Some(idx) = key.strip_prefix("stance:") {
            let idx = parse_index(key, idx, stance_len)?;
            out.stance.insert(idx);
            continue;
        }
        if let Some(idx) = key.strip_prefix("goal:") {
            let idx = parse_index(key, idx, goal_len)?;
            out.goal.insert(idx);
            continue;
        }
        return Err(McpToolError::InvalidInput(format!(
            "bind key {key} is not a produced node; use remember:<index>, derive, stance:<index>, or goal:<index>"
        )));
    }
    Ok(out)
}

fn parse_index(key: &str, idx: &str, len: usize) -> Result<usize, McpToolError> {
    let idx: usize = idx.parse().map_err(|_| {
        McpToolError::InvalidInput(format!("bind key {key} is not a produced-node index"))
    })?;
    if idx >= len {
        return Err(McpToolError::InvalidInput(format!(
            "bind key {key} is out of range"
        )));
    }
    Ok(idx)
}

pub(super) fn reject_duplicate_keys(
    keys: impl IntoIterator<Item = Option<String>>,
    label: &str,
) -> Result<(), McpToolError> {
    let mut seen = HashSet::new();
    for key in keys.into_iter().flatten() {
        if !seen.insert(key.clone()) {
            return Err(McpToolError::InvalidInput(format!(
                "duplicate idempotency_key in one episode {label}: {key}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_keys_cover_each_produced_kind() {
        let bind = parse_bind(
            &[
                "remember:0".into(),
                "derive".into(),
                "stance:1".into(),
                "goal:0".into(),
            ],
            1,
            true,
            2,
            1,
        )
        .expect("valid keys");
        assert!(bind.remember.contains(&0));
        assert!(bind.derive);
        assert!(bind.stance.contains(&1));
        assert!(bind.goal.contains(&0));
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let err = reject_duplicate_keys([Some("k".into()), Some("k".into())], "remember")
            .expect_err("duplicates");
        assert!(err.to_string().contains("duplicate idempotency_key"));
    }

    #[test]
    fn bind_keys_reject_missing_slots() {
        assert!(parse_bind(&["derive".into()], 0, false, 0, 0).is_err());
        assert!(parse_bind(&["remember:3".into()], 1, false, 0, 0).is_err());
        assert!(parse_bind(&["stance:0".into()], 0, false, 0, 0).is_err());
        assert!(parse_bind(&["goal:0".into()], 0, false, 0, 0).is_err());
        assert!(parse_bind(&["other".into()], 1, false, 0, 0).is_err());
    }
}
