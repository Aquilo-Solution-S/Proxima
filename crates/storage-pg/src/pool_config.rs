//! Postgres connection-pool policy, resolved once before pool construction.

use std::time::Duration;

use proxima_core::StorageError;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const DEFAULT_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_STATEMENT_TIMEOUT: Duration = Duration::from_mins(5);
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(10);
const DEFAULT_MAX_LIFETIME: Duration = Duration::from_mins(30);

/// Postgres pool and per-connection timeout policy.
///
/// All defaults are finite. `Duration::ZERO` preserves the environment
/// variables' existing zero semantics: statement timeout is omitted (Postgres
/// disables it), while pool durations are passed to `SQLx` unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgPoolConfig {
    pub max_connections: u32,
    pub statement_timeout: Duration,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_lifetime: Duration,
}

impl Default for PgPoolConfig {
    fn default() -> Self {
        Self {
            max_connections: DEFAULT_MAX_CONNECTIONS,
            statement_timeout: DEFAULT_STATEMENT_TIMEOUT,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_lifetime: DEFAULT_MAX_LIFETIME,
        }
    }
}

impl PgPoolConfig {
    /// Read pool policy from the process environment, applying finite defaults.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when a configured value is not a
    /// valid non-negative integer, or when max connections is zero.
    pub fn from_env() -> Result<Self, StorageError> {
        Ok(Self::from_lookup(&proxima_core::process_env)?.unwrap_or_default())
    }

    /// Read pool policy from an injected lookup.
    ///
    /// `None` means the lookup is silent, which lets layered builders preserve
    /// a programmatic base. Empty and whitespace-only values are unset. An
    /// explicitly supplied value equal to a shipped default is still an
    /// environment answer and therefore returns `Some`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when a configured value is not a
    /// valid non-negative integer, or when max connections is zero.
    pub fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, StorageError> {
        let (config, configured) = Self::resolve(lookup)?;
        Ok(configured.then_some(config))
    }

    /// Validate a programmatically constructed policy.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when max connections is zero.
    pub fn validate(self) -> Result<Self, StorageError> {
        if self.max_connections == 0 {
            return Err(StorageError::Unavailable(
                "PROXIMA_PG_MAX_CONNECTIONS=0 is not a usable pool size; a connection pool needs at least one connection"
                    .into(),
            ));
        }
        Ok(self)
    }

    fn resolve(lookup: &impl Fn(&str) -> Option<String>) -> Result<(Self, bool), StorageError> {
        let defaults = Self::default();
        let (max_connections, max_connections_configured) = env_int_with_presence(
            lookup,
            "PROXIMA_PG_MAX_CONNECTIONS",
            defaults.max_connections,
        )?;
        let (statement_timeout_ms, statement_timeout_configured) = env_int_with_presence(
            lookup,
            "PROXIMA_PG_STATEMENT_TIMEOUT_MS",
            u64::try_from(defaults.statement_timeout.as_millis())
                .expect("default statement timeout fits u64 milliseconds"),
        )?;
        let (acquire_timeout_secs, acquire_timeout_configured) = env_int_with_presence(
            lookup,
            "PROXIMA_PG_ACQUIRE_TIMEOUT_SECS",
            defaults.acquire_timeout.as_secs(),
        )?;
        let (idle_timeout_secs, idle_timeout_configured) = env_int_with_presence(
            lookup,
            "PROXIMA_PG_IDLE_TIMEOUT_SECS",
            defaults.idle_timeout.as_secs(),
        )?;
        let (max_lifetime_secs, max_lifetime_configured) = env_int_with_presence(
            lookup,
            "PROXIMA_PG_MAX_LIFETIME_SECS",
            defaults.max_lifetime.as_secs(),
        )?;
        let config = Self {
            max_connections,
            statement_timeout: Duration::from_millis(statement_timeout_ms),
            acquire_timeout: Duration::from_secs(acquire_timeout_secs),
            idle_timeout: Duration::from_secs(idle_timeout_secs),
            max_lifetime: Duration::from_secs(max_lifetime_secs),
        }
        .validate()?;
        Ok((
            config,
            max_connections_configured
                || statement_timeout_configured
                || acquire_timeout_configured
                || idle_timeout_configured
                || max_lifetime_configured,
        ))
    }

    pub(crate) fn connect_options(self, url: &str) -> Result<PgConnectOptions, StorageError> {
        let mut options: PgConnectOptions = url.parse().map_err(|error: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {error}"))
        })?;
        if !self.statement_timeout.is_zero() {
            options = options.options([(
                "statement_timeout",
                self.statement_timeout.as_millis().to_string(),
            )]);
        }
        Ok(options)
    }

    pub(crate) fn pool_options(self) -> PgPoolOptions {
        PgPoolOptions::new()
            .max_connections(self.max_connections)
            .acquire_timeout(self.acquire_timeout)
            .idle_timeout(self.idle_timeout)
            .max_lifetime(self.max_lifetime)
    }
}

/// Parse one pool integer while retaining whether the lookup supplied a
/// non-empty value. Value equality with the default cannot answer that
/// presence question: an explicit default still outranks flavor configuration.
fn env_int_with_presence<T: std::str::FromStr>(
    lookup: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: T,
) -> Result<(T, bool), StorageError> {
    let Some(value) = proxima_core::env_value(lookup, key) else {
        return Ok((default, false));
    };
    value
        .parse()
        .map(|parsed| (parsed, true))
        .map_err(|_| StorageError::Unavailable(format!("invalid integer {key}={value}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn defaults_are_the_existing_finite_pool_policy() {
        assert_eq!(
            PgPoolConfig::default(),
            PgPoolConfig {
                max_connections: 10,
                statement_timeout: Duration::from_mins(5),
                acquire_timeout: Duration::from_secs(5),
                idle_timeout: Duration::from_mins(10),
                max_lifetime: Duration::from_mins(30),
            }
        );
        assert_eq!(PgPoolConfig::from_lookup(&env(&[])).unwrap(), None);
    }

    #[test]
    fn explicit_shipped_default_is_not_silent() {
        assert_eq!(
            PgPoolConfig::from_lookup(&env(&[("PROXIMA_PG_MAX_CONNECTIONS", "10")])).unwrap(),
            Some(PgPoolConfig::default())
        );
    }

    #[test]
    fn injected_lookup_resolves_every_pool_setting() {
        let config = PgPoolConfig::from_lookup(&env(&[
            ("PROXIMA_PG_MAX_CONNECTIONS", "25"),
            ("PROXIMA_PG_STATEMENT_TIMEOUT_MS", "45000"),
            ("PROXIMA_PG_ACQUIRE_TIMEOUT_SECS", "7"),
            ("PROXIMA_PG_IDLE_TIMEOUT_SECS", "900\n"),
            ("PROXIMA_PG_MAX_LIFETIME_SECS", "2400"),
        ]))
        .unwrap()
        .expect("non-default pool policy");

        assert_eq!(
            config,
            PgPoolConfig {
                max_connections: 25,
                statement_timeout: Duration::from_secs(45),
                acquire_timeout: Duration::from_secs(7),
                idle_timeout: Duration::from_mins(15),
                max_lifetime: Duration::from_mins(40),
            }
        );
    }

    #[test]
    fn blank_values_are_unset() {
        assert_eq!(
            PgPoolConfig::from_lookup(&env(&[
                ("PROXIMA_PG_MAX_CONNECTIONS", ""),
                ("PROXIMA_PG_IDLE_TIMEOUT_SECS", "  \t "),
            ]))
            .unwrap(),
            None
        );
    }

    #[test]
    fn malformed_and_zero_pool_size_fail_at_resolution() {
        for (key, value, message) in [
            (
                "PROXIMA_PG_MAX_CONNECTIONS",
                "twenty",
                "invalid integer PROXIMA_PG_MAX_CONNECTIONS=twenty",
            ),
            (
                "PROXIMA_PG_MAX_LIFETIME_SECS",
                "-1",
                "invalid integer PROXIMA_PG_MAX_LIFETIME_SECS=-1",
            ),
            ("PROXIMA_PG_MAX_CONNECTIONS", "0", "at least one connection"),
        ] {
            let error = PgPoolConfig::from_lookup(&env(&[(key, value)]))
                .expect_err("invalid pool config must fail resolution");
            assert!(error.to_string().contains(message), "{key}: {error}");
        }
    }

    #[test]
    fn zero_preserves_each_duration_setting() {
        let config = PgPoolConfig::from_lookup(&env(&[
            ("PROXIMA_PG_STATEMENT_TIMEOUT_MS", "0"),
            ("PROXIMA_PG_ACQUIRE_TIMEOUT_SECS", "0"),
            ("PROXIMA_PG_IDLE_TIMEOUT_SECS", "0"),
            ("PROXIMA_PG_MAX_LIFETIME_SECS", "0"),
        ]))
        .unwrap()
        .expect("zero durations differ from finite defaults");

        assert!(config.statement_timeout.is_zero());
        assert!(config.acquire_timeout.is_zero());
        assert!(config.idle_timeout.is_zero());
        assert!(config.max_lifetime.is_zero());
        assert_eq!(
            config
                .connect_options("postgres://user:secret@localhost/proxima")
                .unwrap()
                .get_options(),
            None,
            "statement timeout zero omits the Postgres option"
        );
    }

    #[test]
    fn construction_uses_programmatic_policy_without_rendering_the_url() {
        let config = PgPoolConfig {
            max_connections: 3,
            statement_timeout: Duration::from_secs(17),
            acquire_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(19),
            max_lifetime: Duration::from_secs(23),
        };
        let options = config.pool_options();

        assert_eq!(options.get_max_connections(), 3);
        assert_eq!(options.get_acquire_timeout(), Duration::from_secs(2));
        assert_eq!(options.get_idle_timeout(), Some(Duration::from_secs(19)));
        assert_eq!(options.get_max_lifetime(), Some(Duration::from_secs(23)));
        assert_eq!(
            config
                .connect_options("postgres://user:secret@localhost/proxima")
                .unwrap()
                .get_options(),
            Some("-c statement_timeout=17000")
        );
    }
}
