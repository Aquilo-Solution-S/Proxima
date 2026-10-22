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
/// configuration read should go through it.
#[must_use]
pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// An environment lookup over fixed `pairs`, for a host that supplies its
/// configuration by value instead of from the process environment, and for
/// tests. When a key appears more than once, the later pair wins.
///
/// ```
/// use proxima_core::{env_from_pairs, env_value};
///
/// let env = env_from_pairs(&[("MODE", "a"), ("MODE", "b")]);
/// assert_eq!(env_value(&env, "MODE").as_deref(), Some("b"));
/// assert_eq!(env_value(&env, "MISSING"), None);
/// ```
pub fn env_from_pairs<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        pairs
            .iter()
            .rev()
            .find(|(candidate, _)| *candidate == key)
            .map(|(_, value)| (*value).to_owned())
    }
}

/// The boolean a setting's value spells: `1`, `true`, `yes` or `on` is true;
/// `0`, `false`, `no` or `off` is false; case and surrounding whitespace do
/// not matter. Anything else is `None`, and the caller names the setting in
/// its own error. Every boolean setting of every crate reads its words here,
/// so one setting cannot mean different things to different readers.
///
/// ```
/// use proxima_core::parse_bool_word;
///
/// assert_eq!(parse_bool_word(" On\n"), Some(true));
/// assert_eq!(parse_bool_word("NO"), Some(false));
/// assert_eq!(parse_bool_word("maybe"), None);
/// ```
#[must_use]
pub fn parse_bool_word(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{env_from_pairs as env, env_value, parse_bool_word};

    #[test]
    fn boolean_words_are_the_documented_set() {
        for word in ["1", "true", "YES", " On\n"] {
            assert_eq!(parse_bool_word(word), Some(true), "{word:?}");
        }
        for word in ["0", "False", "no", "OFF"] {
            assert_eq!(parse_bool_word(word), Some(false), "{word:?}");
        }
        for word in ["", "2", "y", "enabled", "tru e"] {
            assert_eq!(parse_bool_word(word), None, "{word:?}");
        }
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
}
