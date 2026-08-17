//! Which of a repository's paths ingest indexes.
//!
//! Scope belongs to the repo, not one ingest call: the incremental poller
//! lists arbitrary SHAs and must apply the same rule as the snapshot.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Most globs accepted per list.
///
/// Each compiles into the same automaton, so the cost is in pattern
/// compilation rather than per-path matching; this bounds what one
/// `register_repo` call can ask the server to build.
pub const MAX_SCOPE_GLOBS: usize = 64;

/// Longest single glob accepted. A path component is bounded by the
/// filesystem; a pattern far past that is not describing paths.
pub const MAX_SCOPE_GLOB_LEN: usize = 512;

/// A repository's path scope, as stored.
///
/// Kept separate from the compiled [`ScopeMatcher`] because this is what
/// crosses the database and the tool surface, and it must round-trip
/// verbatim: an operator who set `**/fixtures/**` reads back
/// `**/fixtures/**`, not whatever a matcher normalised it to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoScope {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl RepoScope {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Stable digest of the stored glob lists, in stored order.
    ///
    /// Snapshot cursors carry this so a same-tree re-run can no-op only
    /// when the scope has not changed.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"proxima-code-scope-v1\0");
        for pattern in &self.include {
            hasher.update(pattern.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(b"\x1e");
        for pattern in &self.exclude {
            hasher.update(pattern.as_bytes());
            hasher.update(b"\0");
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Compile for matching.
    ///
    /// # Errors
    ///
    /// [`ScopeError`] when a list is too long, a pattern is too long or
    /// empty, or a pattern is not a valid glob.
    pub fn compile(&self) -> Result<ScopeMatcher, ScopeError> {
        Ok(ScopeMatcher {
            include: compile_list("include_globs", &self.include)?,
            exclude: compile_list("exclude_globs", &self.exclude)?,
        })
    }
}

fn compile_list(field: &'static str, patterns: &[String]) -> Result<Option<GlobSet>, ScopeError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    if patterns.len() > MAX_SCOPE_GLOBS {
        return Err(ScopeError::TooManyGlobs {
            field,
            count: patterns.len(),
        });
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if pattern.trim().is_empty() {
            return Err(ScopeError::EmptyGlob { field });
        }
        if pattern.len() > MAX_SCOPE_GLOB_LEN {
            return Err(ScopeError::GlobTooLong {
                field,
                len: pattern.len(),
            });
        }
        // `literal_separator` is what makes `*` stop at `/` and `**` the
        // only way to cross a directory — the semantics an operator
        // writing `src/*.rs` already expects from .gitignore. Without it
        // `src/*.rs` would also match `src/a/b.rs`, so every scope would
        // be quietly wider than it reads.
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|err| ScopeError::InvalidGlob {
                field,
                pattern: pattern.clone(),
                reason: err.to_string(),
            })?;
        builder.add(glob);
    }
    let set = builder.build().map_err(|err| ScopeError::InvalidGlob {
        field,
        pattern: patterns.join(", "),
        reason: err.to_string(),
    })?;
    Ok(Some(set))
}

/// A compiled [`RepoScope`]. Build once per ingest, not once per path.
#[derive(Debug, Clone)]
pub struct ScopeMatcher {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl ScopeMatcher {
    /// A matcher that admits everything — the default scope.
    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            include: None,
            exclude: None,
        }
    }

    /// Whether `path` (repo-relative, `/`-separated, as git reports it) is
    /// indexed.
    ///
    /// No includes means every path is a candidate. Excludes win on
    /// conflict, which is the rule every tool carrying both lists uses:
    /// the narrower statement is the one the operator had to type twice.
    #[must_use]
    pub fn admits(&self, path: &str) -> bool {
        if let Some(include) = &self.include
            && !include.is_match(path)
        {
            return false;
        }
        if let Some(exclude) = &self.exclude
            && exclude.is_match(path)
        {
            return false;
        }
        true
    }

    /// Whether this matcher can reject anything at all. An ingest over an
    /// unscoped repo skips the per-path check entirely.
    #[must_use]
    pub fn admits_everything(&self) -> bool {
        self.include.is_none() && self.exclude.is_none()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScopeError {
    #[error("{field}: at most {MAX_SCOPE_GLOBS} globs, got {count}")]
    TooManyGlobs { field: &'static str, count: usize },
    #[error("{field}: a glob may not be empty")]
    EmptyGlob { field: &'static str },
    #[error("{field}: a glob may be at most {MAX_SCOPE_GLOB_LEN} chars, got {len}")]
    GlobTooLong { field: &'static str, len: usize },
    #[error("{field}: {pattern:?} is not a valid glob: {reason}")]
    InvalidGlob {
        field: &'static str,
        pattern: String,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{MAX_SCOPE_GLOBS, RepoScope, ScopeError, ScopeMatcher};

    fn scope(include: &[&str], exclude: &[&str]) -> ScopeMatcher {
        RepoScope {
            include: include.iter().map(|s| (*s).to_string()).collect(),
            exclude: exclude.iter().map(|s| (*s).to_string()).collect(),
        }
        .compile()
        .expect("valid scope")
    }

    #[test]
    fn fingerprint_changes_when_globs_change() {
        let empty = RepoScope::default().fingerprint();
        let excluded = RepoScope {
            include: Vec::new(),
            exclude: vec!["**/fixtures/**".into()],
        }
        .fingerprint();
        assert_ne!(empty, excluded);
        assert_eq!(empty, RepoScope::default().fingerprint());
    }

    #[test]
    fn an_empty_scope_admits_everything() {
        let matcher = scope(&[], &[]);
        assert!(matcher.admits_everything());
        assert!(matcher.admits("src/main.rs"));
        assert!(matcher.admits("packages/knip/fixtures/plugins/a/package.json"));
        assert!(ScopeMatcher::allow_all().admits_everything());
    }

    /// Exclude `**/fixtures/**` without touching sources beside them.
    #[test]
    fn excluding_fixtures_keeps_the_sources_beside_them() {
        let matcher = scope(&[], &["**/fixtures/**"]);
        assert!(!matcher.admits("packages/knip/fixtures/plugins/a/package.json"));
        assert!(!matcher.admits("fixtures/x.ts"));
        assert!(matcher.admits("packages/knip/src/index.ts"));
        assert!(matcher.admits("packages/knip/fixtures.ts"));
        assert!(!matcher.admits_everything());
    }

    #[test]
    fn an_include_list_makes_everything_else_out_of_scope() {
        let matcher = scope(&["src/**/*.rs"], &[]);
        assert!(matcher.admits("src/main.rs"));
        assert!(matcher.admits("src/a/b/c.rs"));
        assert!(!matcher.admits("src/main.ts"));
        assert!(!matcher.admits("docs/readme.md"));
    }

    /// The narrower statement wins. An operator who writes both lists has
    /// said the exclusion twice, so it is the one they meant.
    #[test]
    fn exclude_beats_include_on_a_path_matching_both() {
        let matcher = scope(&["src/**"], &["**/generated/**"]);
        assert!(matcher.admits("src/hand_written.rs"));
        assert!(!matcher.admits("src/generated/schema.rs"));
    }

    /// `*` stops at a separator, so `src/*.rs` is the top level only —
    /// what .gitignore has trained every operator to expect. Without
    /// `literal_separator` globset would match at any depth and a scope
    /// would silently be wider than it reads.
    #[test]
    fn a_single_star_does_not_cross_a_directory() {
        let matcher = scope(&["src/*.rs"], &[]);
        assert!(matcher.admits("src/main.rs"));
        assert!(!matcher.admits("src/deep/main.rs"));

        let deep = scope(&["src/**/*.rs"], &[]);
        assert!(deep.admits("src/deep/main.rs"));
    }

    #[test]
    fn malformed_and_oversized_lists_are_rejected_not_ignored() {
        let bad = RepoScope {
            include: vec!["src/[".to_string()],
            exclude: vec![],
        };
        assert!(matches!(bad.compile(), Err(ScopeError::InvalidGlob { .. })));

        let empty = RepoScope {
            include: vec!["  ".to_string()],
            exclude: vec![],
        };
        assert!(matches!(empty.compile(), Err(ScopeError::EmptyGlob { .. })));

        let many = RepoScope {
            include: (0..=MAX_SCOPE_GLOBS).map(|i| format!("a{i}/**")).collect(),
            exclude: vec![],
        };
        assert!(matches!(
            many.compile(),
            Err(ScopeError::TooManyGlobs { .. })
        ));

        let long = RepoScope {
            include: vec!["a".repeat(super::MAX_SCOPE_GLOB_LEN + 1)],
            exclude: vec![],
        };
        assert!(matches!(
            long.compile(),
            Err(ScopeError::GlobTooLong { .. })
        ));
    }

    /// Scope round-trips verbatim: an operator reads back what they wrote,
    /// not a normalisation of it.
    #[test]
    fn a_scope_is_stored_as_written() {
        let written = RepoScope {
            include: vec!["src/**".to_string()],
            exclude: vec!["**/fixtures/**".to_string()],
        };
        assert!(written.compile().is_ok());
        assert_eq!(written.include, vec!["src/**".to_string()]);
        assert_eq!(written.exclude, vec!["**/fixtures/**".to_string()]);
        assert!(!written.is_empty());
        assert!(RepoScope::default().is_empty());
    }
}
