use std::net::SocketAddr;

pub const DEFAULT_BIND: &str = "127.0.0.1:31415";
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

/// `DATABASE_URL`, or the dev default when it is not configured.
///
/// Written out once rather than at each of the four subcommand parsers that
/// need it, and read through [`proxima_core::env_value`] so `DATABASE_URL=`
/// falls back to the default instead of handing an empty connection string to
/// the pool.
fn database_url_from_env() -> String {
    proxima_core::env_value(&proxima_core::process_env, "DATABASE_URL")
        .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_string())
}

pub const USAGE: &str = "\
Usage:
  proxima-mcp [serve] [OPTIONS]
  proxima-mcp maintain-embeddings [OPTIONS]
  proxima-mcp maintain-retention [OPTIONS]

Proxima Streamable HTTP MCP server.

Auth:
  (OIDC)                   Set PROXIMA_OIDC_ISSUER + PROXIMA_OIDC_AUDIENCE
                           (see Environment) for bearer-JWT auth.

Optional:
  --database-url <URL>     Postgres URL (defaults to DATABASE_URL or proxima_dev)
  --bind <ADDR:PORT>       Bind address override (loopback only)
  -h, --help               Print this message

Environment:
  PROXIMA_MCP_BIND              Bind address; allows non-loopback with
                                PROXIMA_EXPOSE_NETWORK=true
                                (e.g. 0.0.0.0:8080)
  PROXIMA_EXPOSE_NETWORK        Permit non-loopback MCP exposure
  PROXIMA_ALLOWED_ORIGINS       Comma-separated CORS origin allowlist
  PROXIMA_ALLOWED_HOSTS         Comma-separated inbound Host allowlist
                                (hostnames or host:port). Defaults to the
                                host of PROXIMA_PUBLIC_URL + the allowed
                                origins; loopback is always permitted
  PROXIMA_PUBLIC_URL            Public base URL for OAuth discovery
  PROXIMA_OIDC_ISSUER           OIDC issuer / authorization server
  PROXIMA_OIDC_AUDIENCE         Expected token audience
  PROXIMA_OIDC_JWKS_URI         Optional explicit JWKS endpoint
  PROXIMA_OIDC_HTTP_TIMEOUT_SECONDS
                                Discovery/JWKS complete-request timeout
                                (default 10; range 1..=300 seconds)
  PROXIMA_OIDC_ALLOWED_SUBJECTS Optional comma-separated sub allowlist
                                (in addition to the subject map below, never
                                an identity source by itself)
  PROXIMA_OIDC_SUBJECT_MAP_JSON Issuer-aware (iss,sub)->user_id identity map:
                                a JSON array of {iss, sub, user_id} objects.
                                Required when PROXIMA_OIDC_ISSUER is set,
                                unless PROXIMA_OIDC_SUBJECT_MAP is given
                                instead.
  PROXIMA_OIDC_SUBJECT_MAP      Legacy single-issuer shorthand:
                                sub:<uuid>,sub2:<uuid2>; every entry binds
                                to PROXIMA_OIDC_ISSUER. Mutually exclusive
                                with PROXIMA_OIDC_SUBJECT_MAP_JSON.
  PROXIMA_TOOL_PROFILE          Tool profile: memory (fail-closed default) or
                                full (opt-in; adds core_publish/core_membership)
  PROXIMA_TOOL_ALLOW            Comma-separated canonical tool ids added to profile
  PROXIMA_TOOL_DENY             Comma-separated canonical tool ids removed from profile
  PROXIMA_EMBED_REQUEST_TIMEOUT_SECONDS
                                Complete /embeddings request timeout
                                (default 120; range 1..=3600)
  PROXIMA_EMBED_BATCH_SIZE      Texts per provider request
                                (default 32; range 1..=1024)
  PROXIMA_EMBED_WORKER_INTERVAL_SECONDS
                                Idle worker poll interval
                                (default 5; range 1..=3600)
  PROXIMA_EMBED_STALE_CLAIM_TIMEOUT_SECONDS
                                Processing-claim crash timeout
                                (default 900; range 1..=86400; must be
                                greater than request timeout)

Maintenance:
  maintain-embeddings      One self-healing pass: orphan sweep, reconcile
                           enqueue, optional inline drain, health report.
                           Cron-safe (skips if another pass holds the lock)
  maintain-retention       One retention pass: forget Facts past their
                           owner's retention window and/or prune old
                           change_event rows. Cron-safe; legal holds skip
  maintain-blobs           One reconcile pass over the S3 bucket: report
                           artefacts whose object is gone, objects no row
                           claims, and rows naming another store. Read-only

Endpoint:
  http://127.0.0.1:31415/mcp
  MCP initialize must include X-Proxima-Owner: personal:<uuid>,
  group:<uuid>, or world:00000000-0000-0000-0000-000000000001. The server
  binds that owner to Mcp-Session-Id.

Tools:
  core_search_memories
  core_recall
  core_think
  core_memory_spaces
  core_remember
  core_episode_commit
  core_forget
  core_record_utterance
  core_derive
  core_interpret
  core_goal
  core_fact
  core_membership
  core_publish
  core_upload
";

pub const MAINTAIN_USAGE: &str = "\
Usage: proxima-mcp maintain-embeddings [OPTIONS]

One embedding self-healing pass, in order: sweep orphaned embedding rows,
enqueue jobs for memories that need the target model, optionally drain the
queue inline, then print a health report (backlog, orphans, recall canary).
Passes are serialized by a Postgres advisory lock; when another pass holds
it, this run prints a skip notice and exits 0 — safe to fire from cron.

Optional:
  --database-url <URL>     Postgres URL (defaults to DATABASE_URL or proxima_dev)
  --model <ID>             Embedding model id (defaults to PROXIMA_EMBED_MODEL; one is required)
  --missing-only           Enqueue memories with no embedding at all (default)
  --include-stale          Also re-enqueue memories embedded only under another model
  --since <RFC3339>        Only scan memories created at/after the timestamp
  --limit <N>              Maximum memories to scan (omit for the full graph)
  --drain                  Process queued jobs inline with the configured embedding client
  -h, --help               Print this message
";

pub const MAINTAIN_BLOBS_USAGE: &str = "\
Usage: proxima-mcp maintain-blobs [OPTIONS]

One reconcile pass over the configured S3 bucket and the rows that name it.
READ-ONLY: it deletes nothing and repairs nothing, because the direction that
matters cannot be repaired from here — the bytes are gone, and only a bucket
version or a backup still has them.

Reports three separate numbers, which are three different problems:
  missing   an artefact the corpus claims to hold whose object is absent.
            A CITATION THAT CANNOT BE RESOLVED. This is the one to alert on
  orphans   objects no row claims: cost and retention, nothing broken
  foreign   rows naming another bucket or a key outside objects/. Neither
            loss nor waste - usually a legacy or hand-written locator

Requires the same PROXIMA_S3_* block the host runs with; the bucket is taken
from the environment rather than a flag so it cannot be pointed at a store
the database has no relation to.

Optional:
  --database-url <URL>     Postgres URL (defaults to DATABASE_URL or proxima_dev)
  -h, --help               Print this help
";

pub const RETENTION_USAGE: &str = "\
Usage: proxima-mcp maintain-retention [OPTIONS]

One retention pass over owner-scoped data. Passes are serialized by a
Postgres advisory lock; when another pass holds it, this run prints a
skip notice and exits 0 — safe to fire from cron. Every owner is
processed under its legal-hold gate: owners with an active legal/security
hold are skipped and reported.

At least one action flag is required — there are deliberately no
destruction defaults.

Actions (at least one):
  --enforce-fact-retention Forget (cool) Facts older than their owner's
                           configured retention window. Owners without a
                           configured window are untouched; MCP-call audit
                           Facts are never aged out (indefinite controller
                           evidence). Live enforcement requires the same
                           PROXIMA_S3_* block as the serving host
  --retry-cold-object-purges
                           Retry a bounded batch of exact object-store keys
                           left by committed compliance erases. Live retry
                           requires the serving host's PROXIMA_S3_* block
  --prune-change-events-older-than <DURATION>
                           Delete change_event rows older than the horizon
                           (e.g. 90d, 36h, 45m, 3600s)

Optional:
  --database-url <URL>     Postgres URL (defaults to DATABASE_URL or proxima_dev)
  --batch-size <N>         Rows per transaction (default 1000)
  --dry-run                Report what would be forgotten/pruned without
                           changing anything
  -h, --help               Print this message
";

#[derive(Clone)]
pub struct McpConfig {
    pub database_url: String,
    pub bind: Option<SocketAddr>,
}

impl std::fmt::Debug for McpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConfig")
            .field("database_url", &"<redacted>")
            .field("bind", &self.bind)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MaintainConfig {
    pub database_url: String,
    pub model: Option<String>,
    pub scope: ReconcileScope,
    pub limit: Option<i64>,
    pub drain: bool,
}

impl std::fmt::Debug for MaintainConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaintainConfig")
            .field("database_url", &"<redacted>")
            .field("model", &self.model)
            .field("scope", &self.scope)
            .field("limit", &self.limit)
            .field("drain", &self.drain)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileScope {
    MissingOnly,
    IncludeStale,
    Since(time::OffsetDateTime),
}

#[derive(Clone, PartialEq, Eq)]
pub struct RetentionConfig {
    pub database_url: String,
    pub enforce_fact_retention: bool,
    pub retry_cold_object_purges: bool,
    pub prune_change_events_older_than_seconds: Option<i64>,
    pub batch_size: i64,
    pub dry_run: bool,
}

impl std::fmt::Debug for RetentionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetentionConfig")
            .field("database_url", &"<redacted>")
            .field("enforce_fact_retention", &self.enforce_fact_retention)
            .field("retry_cold_object_purges", &self.retry_cold_object_purges)
            .field(
                "prune_change_events_older_than_seconds",
                &self.prune_change_events_older_than_seconds,
            )
            .field("batch_size", &self.batch_size)
            .field("dry_run", &self.dry_run)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error("help requested")]
    Help,
    #[error("{0}")]
    Invalid(String),
}

impl ArgsError {
    #[must_use]
    pub fn is_help(&self) -> bool {
        matches!(self, Self::Help)
    }
}

/// # Errors
///
/// Returns `ArgsError` for help, unknown flags, missing values, or unreadable
/// current dir.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<McpConfig, ArgsError> {
    let mut database_url: Option<String> = None;
    let mut bind: Option<SocketAddr> = None;

    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" => return Err(ArgsError::Help),
            f => {
                let value = iter
                    .next()
                    .ok_or_else(|| ArgsError::Invalid(format!("flag {f} expects a value")))?;
                match f {
                    "--database-url" => database_url = Some(value),
                    "--bind" => {
                        let parsed: SocketAddr = value.parse().map_err(|err| {
                            ArgsError::Invalid(format!("invalid --bind {value:?}: {err}"))
                        })?;
                        if !parsed.ip().is_loopback() {
                            return Err(ArgsError::Invalid(format!(
                                "--bind must be loopback, got {}",
                                parsed.ip()
                            )));
                        }
                        bind = Some(parsed);
                    }
                    other => return Err(ArgsError::Invalid(format!("unknown flag: {other}"))),
                }
            }
        }
    }

    let database_url = database_url.unwrap_or_else(database_url_from_env);

    Ok(McpConfig { database_url, bind })
}

/// # Errors
///
/// Returns `ArgsError` for help, unknown flags, missing values, invalid
/// timestamp, negative limit, or unreadable env defaults.
pub fn parse_maintain_args<I: IntoIterator<Item = String>>(
    args: I,
) -> Result<MaintainConfig, ArgsError> {
    let mut database_url: Option<String> = None;
    let mut model: Option<String> = None;
    let mut scope = ReconcileScope::MissingOnly;
    let mut limit: Option<i64> = None;
    let mut drain = false;

    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" => return Err(ArgsError::Help),
            "--drain" => drain = true,
            "--missing-only" => scope = ReconcileScope::MissingOnly,
            "--include-stale" => scope = ReconcileScope::IncludeStale,
            f => {
                let value = iter
                    .next()
                    .ok_or_else(|| ArgsError::Invalid(format!("flag {f} expects a value")))?;
                match f {
                    "--database-url" => database_url = Some(value),
                    "--model" => model = Some(value),
                    "--limit" => {
                        let parsed: i64 = value.parse().map_err(|err| {
                            ArgsError::Invalid(format!("invalid --limit {value:?}: {err}"))
                        })?;
                        if parsed < 0 {
                            return Err(ArgsError::Invalid("--limit must be nonnegative".into()));
                        }
                        limit = Some(parsed);
                    }
                    "--since" => {
                        let parsed = time::OffsetDateTime::parse(
                            &value,
                            &time::format_description::well_known::Rfc3339,
                        )
                        .map_err(|err| {
                            ArgsError::Invalid(format!("invalid --since {value:?}: {err}"))
                        })?;
                        scope = ReconcileScope::Since(parsed);
                    }
                    other => return Err(ArgsError::Invalid(format!("unknown flag: {other}"))),
                }
            }
        }
    }

    let database_url = database_url.unwrap_or_else(database_url_from_env);

    Ok(MaintainConfig {
        database_url,
        model,
        scope,
        limit,
        drain,
    })
}

/// What `maintain-blobs` needs, which is only where the database is.
///
/// The bucket comes from `PROXIMA_S3_*` and not from a flag, because it must
/// be the SAME bucket the running host writes to. A `--bucket` flag would
/// let an operator reconcile a database against a store it has no relation
/// to and be told, truthfully and uselessly, that every artefact is missing.
pub struct MaintainBlobsConfig {
    pub database_url: String,
}

impl std::fmt::Debug for MaintainBlobsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaintainBlobsConfig")
            .field("database_url", &"<redacted>")
            .finish()
    }
}

/// # Errors
///
/// Returns `ArgsError` for help, unknown flags, or a missing value.
pub fn parse_maintain_blobs_args<I: IntoIterator<Item = String>>(
    args: I,
) -> Result<MaintainBlobsConfig, ArgsError> {
    let mut database_url: Option<String> = None;
    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" => return Err(ArgsError::Help),
            f => {
                let value = iter
                    .next()
                    .ok_or_else(|| ArgsError::Invalid(format!("flag {f} expects a value")))?;
                match f {
                    "--database-url" => database_url = Some(value),
                    other => return Err(ArgsError::Invalid(format!("unknown flag: {other}"))),
                }
            }
        }
    }
    let database_url = database_url.unwrap_or_else(database_url_from_env);
    Ok(MaintainBlobsConfig { database_url })
}

/// # Errors
///
/// Returns `ArgsError` for help, unknown flags, missing values, an invalid
/// duration or batch size, or when no action flag is given — destruction
/// must be an explicit operator choice, so there is no default action.
pub fn parse_retention_args<I: IntoIterator<Item = String>>(
    args: I,
) -> Result<RetentionConfig, ArgsError> {
    let mut database_url: Option<String> = None;
    let mut enforce_fact_retention = false;
    let mut retry_cold_object_purges = false;
    let mut prune_older_than: Option<i64> = None;
    let mut batch_size: i64 = 1000;
    let mut dry_run = false;

    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" => return Err(ArgsError::Help),
            "--enforce-fact-retention" => enforce_fact_retention = true,
            "--retry-cold-object-purges" => retry_cold_object_purges = true,
            "--dry-run" => dry_run = true,
            f => {
                let value = iter
                    .next()
                    .ok_or_else(|| ArgsError::Invalid(format!("flag {f} expects a value")))?;
                match f {
                    "--database-url" => database_url = Some(value),
                    "--prune-change-events-older-than" => {
                        let seconds = parse_duration_seconds(&value).map_err(|err| {
                            ArgsError::Invalid(format!(
                                "invalid --prune-change-events-older-than {value:?}: {err}"
                            ))
                        })?;
                        prune_older_than = Some(seconds);
                    }
                    "--batch-size" => {
                        let parsed: i64 = value.parse().map_err(|err| {
                            ArgsError::Invalid(format!("invalid --batch-size {value:?}: {err}"))
                        })?;
                        if parsed < 1 {
                            return Err(ArgsError::Invalid("--batch-size must be positive".into()));
                        }
                        batch_size = parsed;
                    }
                    other => return Err(ArgsError::Invalid(format!("unknown flag: {other}"))),
                }
            }
        }
    }

    if !enforce_fact_retention && !retry_cold_object_purges && prune_older_than.is_none() {
        return Err(ArgsError::Invalid(
            "maintain-retention requires at least one action: --enforce-fact-retention, \
             --retry-cold-object-purges, and/or --prune-change-events-older-than <DURATION>"
                .into(),
        ));
    }

    let database_url = database_url.unwrap_or_else(database_url_from_env);

    Ok(RetentionConfig {
        database_url,
        enforce_fact_retention,
        retry_cold_object_purges,
        prune_change_events_older_than_seconds: prune_older_than,
        batch_size,
        dry_run,
    })
}

/// Parse an explicit-unit duration (`3600s`, `45m`, `36h`, `90d`, `12w`)
/// into seconds. A bare number is rejected on purpose: the value feeds a
/// destruction horizon, so the unit must be spelled out.
fn parse_duration_seconds(raw: &str) -> Result<i64, String> {
    let Some(unit) = raw.chars().last() else {
        return Err("empty duration".into());
    };
    let multiplier: i64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return Err("duration needs a unit suffix: s, m, h, d, or w".into()),
    };
    let digits = &raw[..raw.len() - unit.len_utf8()];
    let value: i64 = digits
        .parse()
        .map_err(|err| format!("invalid duration value {digits:?}: {err}"))?;
    if value < 1 {
        return Err("duration must be positive".into());
    }
    value
        .checked_mul(multiplier)
        .ok_or_else(|| "duration overflows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_flag_returns_help() {
        let err = parse_args(["--help".to_string()]).expect_err("help");
        assert!(err.is_help());
    }

    #[test]
    fn full_args_parse() {
        let cfg =
            parse_args(["--database-url".into(), "postgres://x/y".into()]).expect("valid args");
        assert_eq!(cfg.database_url, "postgres://x/y");
        assert!(cfg.bind.is_none());
    }

    #[test]
    fn loopback_bind_parse() {
        let cfg = parse_args(["--bind".into(), "127.0.0.1:9999".into()]).expect("valid args");
        assert_eq!(
            cfg.bind,
            Some("127.0.0.1:9999".parse().expect("valid bind"))
        );
    }

    #[test]
    fn non_loopback_bind_rejected() {
        let err = parse_args(["--bind".into(), "0.0.0.0:31415".into()]).expect_err("non-loopback");
        assert!(err.to_string().contains("loopback"));
    }

    #[test]
    fn maintain_blobs_help_flag_returns_help() {
        let err = parse_maintain_blobs_args(["--help".to_string()]).expect_err("help");
        assert!(err.is_help());
    }

    #[test]
    fn maintain_blobs_takes_a_database_url_and_refuses_a_bucket() {
        let cfg = parse_maintain_blobs_args(["--database-url".into(), "postgres://x/y".into()])
            .expect("valid args");
        assert_eq!(cfg.database_url, "postgres://x/y");

        // The bucket is deliberately NOT a flag: it must be the one the host
        // writes to, and a mismatched pair would report total loss with a
        // straight face. If this ever starts parsing, the usage text and the
        // reason in `MaintainBlobsConfig` are what to re-read.
        let err = parse_maintain_blobs_args(["--bucket".into(), "somewhere-else".into()])
            .expect_err("--bucket must not be accepted");
        assert!(!err.is_help(), "{err:?}");
    }

    #[test]
    fn maintain_help_flag_returns_help() {
        let err = parse_maintain_args(["--help".to_string()]).expect_err("help");
        assert!(err.is_help());
    }

    #[test]
    fn maintain_args_parse_scope_and_limit() {
        let cfg = parse_maintain_args([
            "--database-url".into(),
            "postgres://x/y".into(),
            "--model".into(),
            "custom-embed".into(),
            "--include-stale".into(),
            "--limit".into(),
            "50".into(),
            "--drain".into(),
        ])
        .expect("valid args");
        assert_eq!(cfg.database_url, "postgres://x/y");
        assert_eq!(cfg.model.as_deref(), Some("custom-embed"));
        assert_eq!(cfg.scope, ReconcileScope::IncludeStale);
        assert_eq!(cfg.limit, Some(50));
        assert!(cfg.drain);
    }

    #[test]
    fn maintain_since_parses_rfc3339() {
        let cfg = parse_maintain_args(["--since".into(), "2026-06-16T10:00:00Z".into()])
            .expect("valid args");
        assert!(matches!(cfg.scope, ReconcileScope::Since(_)));
    }

    #[test]
    fn retention_help_flag_returns_help() {
        let err = parse_retention_args(["--help".to_string()]).expect_err("help");
        assert!(err.is_help());
        assert!(RETENTION_USAGE.contains("PROXIMA_S3_*"));
    }

    #[test]
    fn retention_requires_an_action_flag() {
        let err = parse_retention_args(["--dry-run".to_string()]).expect_err("no action");
        assert!(err.to_string().contains("at least one action"));
    }

    #[test]
    fn retention_full_args_parse() {
        let cfg = parse_retention_args([
            "--database-url".into(),
            "postgres://x/y".into(),
            "--enforce-fact-retention".into(),
            "--prune-change-events-older-than".into(),
            "90d".into(),
            "--batch-size".into(),
            "250".into(),
            "--dry-run".into(),
        ])
        .expect("valid args");
        assert_eq!(cfg.database_url, "postgres://x/y");
        assert!(cfg.enforce_fact_retention);
        assert_eq!(
            cfg.prune_change_events_older_than_seconds,
            Some(90 * 86_400)
        );
        assert_eq!(cfg.batch_size, 250);
        assert!(cfg.dry_run);
    }

    #[test]
    fn retention_single_action_is_enough() {
        let cfg = parse_retention_args(["--enforce-fact-retention".to_string()])
            .expect("enforce alone is a valid pass");
        assert!(cfg.enforce_fact_retention);
        assert_eq!(cfg.prune_change_events_older_than_seconds, None);
        assert_eq!(cfg.batch_size, 1000);
        assert!(!cfg.dry_run);
    }

    #[test]
    fn retention_cold_purge_retry_is_an_explicit_action() {
        let cfg = parse_retention_args([
            "--retry-cold-object-purges".to_string(),
            "--batch-size".to_string(),
            "17".to_string(),
            "--dry-run".to_string(),
        ])
        .expect("retry alone is a valid bounded pass");
        assert!(cfg.retry_cold_object_purges);
        assert!(!cfg.enforce_fact_retention);
        assert_eq!(cfg.prune_change_events_older_than_seconds, None);
        assert_eq!(cfg.batch_size, 17);
        assert!(cfg.dry_run);
    }

    #[test]
    fn retention_duration_units_parse() {
        for (raw, seconds) in [
            ("3600s", 3_600),
            ("45m", 45 * 60),
            ("36h", 36 * 3_600),
            ("90d", 90 * 86_400),
            ("2w", 2 * 604_800),
        ] {
            let cfg = parse_retention_args(["--prune-change-events-older-than".into(), raw.into()])
                .expect("valid duration");
            assert_eq!(
                cfg.prune_change_events_older_than_seconds,
                Some(seconds),
                "{raw}"
            );
        }
    }

    #[test]
    fn retention_duration_rejects_bare_number_zero_and_junk() {
        for raw in ["90", "0d", "-3h", "", "d", "1.5h", "90x"] {
            let err = parse_retention_args(["--prune-change-events-older-than".into(), raw.into()])
                .expect_err(raw);
            assert!(err.to_string().contains("invalid"), "{raw}: {err}");
        }
    }

    #[test]
    fn retention_batch_size_must_be_positive() {
        let err = parse_retention_args([
            "--enforce-fact-retention".into(),
            "--batch-size".into(),
            "0".into(),
        ])
        .expect_err("zero batch");
        assert!(err.to_string().contains("--batch-size must be positive"));
    }
}
