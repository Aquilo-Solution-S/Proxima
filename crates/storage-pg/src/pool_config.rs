//! Postgres connection-pool policy, resolved once before pool construction.

use std::time::Duration;

use proxima_core::StorageError;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// Per pool. A serving host holds two pools (runtime and platform), so the
/// default host opens at most ten backends. A store's work rarely needs more
/// concurrent transactions than this; past the server's saturation point,
/// more connections lower throughput rather than raise it.
const DEFAULT_MAX_CONNECTIONS: u32 = 5;
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

    /// Build a lazy pool under this policy, opening no connection yet.
    ///
    /// Every serving pool a host builds goes through this policy, the
    /// platform pool included, so `PROXIMA_PG_*` bounds all of them. The first
    /// acquire opens the first connection; constructors that validate the
    /// pool (`PgPlatformScope::new`) hold that one connection for every check.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Unavailable`] when max connections is zero or
    /// the URL does not parse.
    pub fn connect_lazy(self, url: &str) -> Result<sqlx::PgPool, StorageError> {
        let config = self.validate()?;
        Ok(config
            .pool_options()
            .connect_lazy_with(config.connect_options(url)?))
    }

    pub(crate) fn connect_options(self, url: &str) -> Result<PgConnectOptions, StorageError> {
        let mut options: PgConnectOptions = url.parse().map_err(|error: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {error}"))
        })?;
        // SQLx sends `extra_float_digits=2` as a startup parameter by default.
        // Postgres 12 and later already print the shortest exact float text
        // at the default of 1, and a transaction-mode pooler such as
        // PgBouncer refuses startup parameters it does not know, so the
        // connection is refused before it starts. Binary results, which
        // prepared queries use, are unaffected either way.
        options = options.extra_float_digits(None::<i8>);
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
    fn defaults_are_the_finite_pool_policy() {
        assert_eq!(
            PgPoolConfig::default(),
            PgPoolConfig {
                max_connections: 5,
                statement_timeout: Duration::from_mins(5),
                acquire_timeout: Duration::from_secs(5),
                idle_timeout: Duration::from_mins(10),
                max_lifetime: Duration::from_mins(30),
            }
        );
        assert_eq!(PgPoolConfig::from_lookup(&env(&[])).unwrap(), None);
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

    /// A lazily built pool carries the whole policy onto the server: the pool
    /// bound, the statement timeout as a startup option, and no
    /// `extra_float_digits` startup parameter (the server keeps its default
    /// of 1), which a transaction-mode pooler would refuse.
    #[tokio::test]
    async fn a_lazy_pool_applies_the_policy_on_its_connections() {
        let Some(url) = proxima_pg_testkit::admin_url_or_skip() else {
            return;
        };
        let pool = PgPoolConfig {
            max_connections: 2,
            statement_timeout: Duration::from_millis(7_250),
            ..PgPoolConfig::default()
        }
        .connect_lazy(&url)
        .expect("lazy pool");
        assert_eq!(pool.size(), 0, "a lazy pool opens nothing up front");
        assert_eq!(pool.options().get_max_connections(), 2);
        let mut first = pool.acquire().await.expect("first connection");
        let (timeout, float_digits): (String, String) = sqlx::query_as(
            "SELECT current_setting('statement_timeout'), current_setting('extra_float_digits')",
        )
        .fetch_one(&mut *first)
        .await
        .expect("session settings");
        assert_eq!(timeout, "7250ms");
        assert_eq!(float_digits, "1", "no extra_float_digits startup parameter");
        let second = pool.acquire().await.expect("second connection");
        assert_eq!(pool.size(), 2);
        drop((first, second));
        pool.close().await;
    }
}
