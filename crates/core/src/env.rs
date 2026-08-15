//! One rule for reading a configuration value out of an environment lookup.
//!
//! Every configuration reader in the workspace wants the same thing: trim the
//! value, and treat what is left of an empty one as "not configured". That rule
//! was written three times — in the facade's auth module, in the binary's arg
//! parsing, and in the blob store's config — and bypassed by
//! `RuntimeBuilder::apply_lookup`, which read nine variables raw. The result was
//! one process answering two ways about the same variable: `PROXIMA_EMBED_*`
//! read an empty value as unset while `PROXIMA_EXPOSE_NETWORK=` aborted boot
//! with `must be a boolean, got ""`, and `DATABASE_URL=` reached the pool as an
//! empty connection string.
//!
//! Not to be confused with [`crate::secrets::EnvResolver`], which deliberately
//! takes the opposite stance: an `env:`-scheme secret resolves an empty variable
//! *successfully*, as a present-but-empty secret, and leaves rejection to the
//! caller. That is right for a secret — an empty credential is a value someone
//! configured, and only the consumer knows whether it is legal. It is wrong for
//! a bucket name or a bind address, where empty can only mean "unset".

/// Read `key` through `lookup`, trimmed, treating an empty result as unset.
///
/// This is the rule for configuration variables: an operator who writes
/// `FOO=` or leaves whitespace has not configured `FOO`, and gets the same
/// behaviour as leaving it out. Returning `None` rather than `Some("")` keeps
/// that decision in one place instead of at each parse site, where forgetting
/// it turns "unset" into a parse error that names a value the operator never
/// typed.
///
/// `lookup` is any environment source — `|key| std::env::var(key).ok()` for the
/// process environment, or an injected map in tests and embedded hosts.
///
/// ```
/// use proxima_core::env_value;
///
/// let env = |key: &str| match key {
///     "SET" => Some("  value  ".to_string()),
///     "BLANK" => Some("   ".to_string()),
///     _ => None,
/// };
/// assert_eq!(env_value(&env, "SET").as_deref(), Some("value"));
/// assert_eq!(env_value(&env, "BLANK"), None);
/// assert_eq!(env_value(&env, "MISSING"), None);
/// ```
#[must_use]
pub fn env_value(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

/// The process environment as a lookup, for callers with no injected source.
///
/// [`env_value`] takes a lookup so that hosts and tests can supply their own;
/// the overwhelmingly common argument is still the real environment, and it
/// was written out as `|key| std::env::var(key).ok()` at eight call sites
/// across five crates. The closure cannot drift — anything else fails to
/// typecheck — so this is not a correctness fix. It is one grep-able name for
/// "the process environment enters the program here", which is the question
/// asked when auditing what a deployment can influence.
///
/// Pass it by name, not by call: it is an ordinary `fn` item and coerces
/// wherever an `impl Fn(&str) -> Option<String>` is wanted.
///
/// ```
/// use proxima_core::{env_value, process_env};
///
/// let _configured = env_value(&process_env, "PATH").is_some();
/// ```
///
/// Note this is the *raw* reader: it does not trim and does not treat an
/// empty value as unset. Those are [`env_value`]'s job, and every
/// configuration read should go through it. The two are separate because
/// [`crate::secrets::EnvResolver`] deliberately wants the raw rule.
#[must_use]
pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

#[cfg(test)]
mod tests {
    use super::{env_value, process_env};

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn absent_is_unset() {
        assert_eq!(env_value(&env(&[]), "FOO"), None);
    }

    #[test]
    fn empty_is_unset() {
        assert_eq!(env_value(&env(&[("FOO", "")]), "FOO"), None);
    }

    #[test]
    fn whitespace_only_is_unset() {
        assert_eq!(env_value(&env(&[("FOO", " \t\n ")]), "FOO"), None);
    }

    /// Trailing newline from a here-doc or secret mount is trimmed, not rejected.
    #[test]
    fn surrounding_whitespace_is_trimmed_not_rejected() {
        assert_eq!(
            env_value(&env(&[("FOO", "900\n")]), "FOO").as_deref(),
            Some("900")
        );
    }

    #[test]
    fn interior_whitespace_is_preserved() {
        assert_eq!(
            env_value(&env(&[("FOO", " a b ")]), "FOO").as_deref(),
            Some("a b")
        );
    }

    /// `process_env` composes with `env_value` as a plain `fn` item — the
    /// property every call site depends on, since all eight pass it by name
    /// rather than calling it.
    ///
    /// Deliberately does not `set_var`: mutating the process environment
    /// would race every other test in this binary that reads one (`setenv` is
    /// not thread-safe against a concurrent `getenv`, which is why Rust 2024
    /// made it `unsafe`), and the trimming rule already has exhaustive
    /// coverage above against an injected lookup.
    #[test]
    fn process_env_composes_with_env_value_by_name() {
        const UNSET: &str = "PROXIMA_ENV_RS_PROCESS_ENV_UNSET";
        assert_eq!(
            process_env(UNSET),
            std::env::var(UNSET).ok(),
            "process_env is exactly the process environment, nothing more"
        );
        assert_eq!(env_value(&process_env, UNSET), None);
    }
}
