//! Bind keys for `core_episode_commit` (`remember:N` / `derive` / `stance:N` / `goal:N`).

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use crate::mcp::McpToolError;

/// One produced-node key on the episode wire: `remember:N`, `derive`,
/// `stance:N`, or `goal:N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EpisodeKey {
    Remember(usize),
    Derive,
    Stance(usize),
    Goal(usize),
}

/// Why a string is not an [`EpisodeKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EpisodeKeyParseError {
    /// A known prefix whose index is not a `usize`.
    NotIndex,
    /// Not `remember:N`, `derive`, `stance:N`, or `goal:N`.
    Unknown,
}

impl EpisodeKey {
    pub(super) fn parse(raw: &str) -> Result<Self, EpisodeKeyParseError> {
        raw.parse()
    }
}

impl fmt::Display for EpisodeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Remember(idx) => write!(f, "remember:{idx}"),
            Self::Derive => f.write_str("derive"),
            Self::Stance(idx) => write!(f, "stance:{idx}"),
            Self::Goal(idx) => write!(f, "goal:{idx}"),
        }
    }
}

impl FromStr for EpisodeKey {
    type Err = EpisodeKeyParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if let Some(idx) = raw.strip_prefix("remember:") {
            let idx = idx.parse().map_err(|_| EpisodeKeyParseError::NotIndex)?;
            return Ok(Self::Remember(idx));
        }
        if raw == "derive" {
            return Ok(Self::Derive);
        }
        if let Some(idx) = raw.strip_prefix("stance:") {
            let idx = idx.parse().map_err(|_| EpisodeKeyParseError::NotIndex)?;
            return Ok(Self::Stance(idx));
        }
        if let Some(idx) = raw.strip_prefix("goal:") {
            let idx = idx.parse().map_err(|_| EpisodeKeyParseError::NotIndex)?;
            return Ok(Self::Goal(idx));
        }
        Err(EpisodeKeyParseError::Unknown)
    }
}

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
        match EpisodeKey::parse(key) {
            Ok(EpisodeKey::Remember(idx)) => {
                ensure_in_range(key, idx, remember_len)?;
                out.remember.insert(idx);
            }
            Ok(EpisodeKey::Derive) => {
                if !has_derive {
                    return Err(McpToolError::InvalidInput(
                        "bind key derive is out of range".into(),
                    ));
                }
                out.derive = true;
            }
            Ok(EpisodeKey::Stance(idx)) => {
                ensure_in_range(key, idx, stance_len)?;
                out.stance.insert(idx);
            }
            Ok(EpisodeKey::Goal(idx)) => {
                ensure_in_range(key, idx, goal_len)?;
                out.goal.insert(idx);
            }
            Err(EpisodeKeyParseError::NotIndex) => {
                return Err(McpToolError::InvalidInput(format!(
                    "bind key {key} is not a produced-node index"
                )));
            }
            Err(EpisodeKeyParseError::Unknown) => {
                return Err(McpToolError::InvalidInput(format!(
                    "bind key {key} is not a produced node; use remember:<index>, derive, stance:<index>, or goal:<index>"
                )));
            }
        }
    }
    Ok(out)
}

fn ensure_in_range(key: &str, idx: usize, len: usize) -> Result<(), McpToolError> {
    if idx >= len {
        return Err(McpToolError::InvalidInput(format!(
            "bind key {key} is out of range"
        )));
    }
    Ok(())
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

    #[test]
    fn episode_keys_round_trip_wire_spellings() {
        for key in [
            EpisodeKey::Remember(0),
            EpisodeKey::Remember(12),
            EpisodeKey::Derive,
            EpisodeKey::Stance(3),
            EpisodeKey::Goal(7),
        ] {
            assert_eq!(key.to_string().parse(), Ok(key));
        }
        assert_eq!(
            "remember:x".parse::<EpisodeKey>(),
            Err(EpisodeKeyParseError::NotIndex)
        );
        assert_eq!(
            "other".parse::<EpisodeKey>(),
            Err(EpisodeKeyParseError::Unknown)
        );
    }
}
