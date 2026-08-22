#![allow(dead_code)]

// Each integration-test binary independently includes this module via
// `mod common;`. Items unused by a particular binary would otherwise trip
// `dead_code` even though another binary uses them.

/// Read an env var gating a local-only or PG-backed e2e test.
///
/// Locally (or on any runner without `CI=true`), a missing/empty value
/// skips the test cleanly. Under `CI=true`, a missing value is a hard test
/// failure instead of a silent skip.
///
/// GitHub Actions exports `DATABASE_URL` / `PROXIMA_TEST_DATABASE_URL` and
/// runs the PG/OIDC e2e lane against live pgvector/pg18
/// (`.github/workflows/ci.yml`); this keeps non-GHA CI runners and local
/// `CI=true` runs from letting that lane go dark unnoticed.
pub fn require_env_or_skip(name: &str) -> Option<String> {
    require_env_or_skip_with(name, proxima_core::process_env)
}

/// Env-lookup-parameterized core, so unit tests can exercise every branch
/// without mutating real process env —
/// other `#[tokio::test]` functions in this same binary read `DATABASE_URL`
/// / `CI` concurrently, and cargo's default parallel test threads would
/// otherwise race a mutation against them.
fn require_env_or_skip_with(name: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    // Both reads go through `env_value` (whitespace is unset; `CI=" true "`
    // still means CI).
    match proxima_core::env_value(&lookup, name) {
        Some(value) => Some(value),
        None if proxima_core::env_value(&lookup, "CI").as_deref() == Some("true") => {
            panic!("{name} required under CI=true")
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_value_when_present_and_non_empty() {
        let lookup = |key: &str| (key == "DATABASE_URL").then(|| "postgres://x/y".to_string());
        assert_eq!(
            require_env_or_skip_with("DATABASE_URL", lookup),
            Some("postgres://x/y".to_string())
        );
    }

    #[test]
    fn returns_none_when_absent_and_ci_unset() {
        assert_eq!(require_env_or_skip_with("DATABASE_URL", |_| None), None);
    }

    #[test]
    fn returns_none_when_empty_and_ci_unset() {
        let lookup = |key: &str| (key == "DATABASE_URL").then(String::new);
        assert_eq!(require_env_or_skip_with("DATABASE_URL", lookup), None);
    }

    #[test]
    fn returns_none_when_whitespace_only_and_ci_unset() {
        let lookup = |key: &str| (key == "DATABASE_URL").then(|| "   ".to_string());
        assert_eq!(require_env_or_skip_with("DATABASE_URL", lookup), None);
    }

    /// Whitespace-only is unset; under `CI=true` that panics.
    #[test]
    #[should_panic(expected = "DATABASE_URL required under CI=true")]
    fn panics_when_whitespace_only_and_ci_true() {
        let lookup = |key: &str| match key {
            "DATABASE_URL" => Some(" \t ".to_string()),
            "CI" => Some("true".to_string()),
            _ => None,
        };
        require_env_or_skip_with("DATABASE_URL", lookup);
    }

    #[test]
    fn trims_a_value_carrying_a_trailing_newline() {
        let lookup = |key: &str| (key == "DATABASE_URL").then(|| "postgres://x/y\n".to_string());
        assert_eq!(
            require_env_or_skip_with("DATABASE_URL", lookup),
            Some("postgres://x/y".to_string())
        );
    }

    #[test]
    fn returns_none_when_absent_and_ci_false() {
        let lookup = |key: &str| (key == "CI").then(|| "false".to_string());
        assert_eq!(require_env_or_skip_with("DATABASE_URL", lookup), None);
    }

    #[test]
    #[should_panic(expected = "DATABASE_URL required under CI=true")]
    fn panics_when_absent_and_ci_true() {
        let lookup = |key: &str| (key == "CI").then(|| "true".to_string());
        require_env_or_skip_with("DATABASE_URL", lookup);
    }
}
