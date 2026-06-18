use std::net::SocketAddr;

use proxima_core::{OrgId, Owner, Principal, UserId};
use uuid::Uuid;

pub const DEFAULT_BIND: &str = "127.0.0.1:31415";
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

pub const USAGE: &str = "\
Usage:
  proxima-mcp [serve] [OPTIONS]
  proxima-mcp reconcile-embeddings [OPTIONS]

Proxima Streamable HTTP MCP server.

Required:
  --owner-user <UUID>      Fixed owner principal
  --owner-org  <UUID>      Fixed org id
  --master-token <UUID>    Local bearer token (or PROXIMA_MCP_MASTER_TOKEN);
                           clients send Authorization: Bearer pxm_<token>

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
  PROXIMA_PUBLIC_URL            Public base URL for OAuth discovery
  PROXIMA_OIDC_ISSUER           OIDC issuer / authorization server
  PROXIMA_OIDC_AUDIENCE         Expected token audience
  PROXIMA_OIDC_JWKS_URI         Optional explicit JWKS endpoint
  PROXIMA_OIDC_ALLOWED_SUBJECTS Optional comma-separated sub allowlist

Maintenance:
  reconcile-embeddings     Enqueue missing embedding jobs globally

Endpoint:
  http://127.0.0.1:31415/mcp

Tools:
  core/search_memories
  core/open
  core/remember
  core/derive
  core/link
";

pub const RECONCILE_USAGE: &str = "\
Usage: proxima-mcp reconcile-embeddings [OPTIONS]

Enqueue embedding jobs for embeddable memories that need the target model.

Optional:
  --database-url <URL>     Postgres URL (defaults to DATABASE_URL or proxima_dev)
  --model <ID>             Embedding model id (defaults to PROXIMA_EMBED_MODEL or Mistral default)
  --missing-only           Enqueue memories with no embedding at all (default)
  --include-stale          Also re-enqueue memories embedded only under another model
  --since <RFC3339>        Only scan memories created at/after the timestamp
  --limit <N>              Maximum memories to scan
  --drain                  Process queued jobs inline with the Mistral embedding client
  -h, --help               Print this message
";

#[derive(Debug, Clone)]
pub struct McpConfig {
    pub database_url: String,
    pub owner: Owner,
    pub bind: Option<SocketAddr>,
    pub master_token: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileConfig {
    pub database_url: String,
    pub model: Option<String>,
    pub scope: ReconcileScope,
    pub limit: Option<i64>,
    pub drain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileScope {
    MissingOnly,
    IncludeStale,
    Since(time::OffsetDateTime),
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error("help requested")]
    Help,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    UuidParse(#[from] uuid::Error),
}

impl ArgsError {
    #[must_use]
    pub fn is_help(&self) -> bool {
        matches!(self, Self::Help)
    }
}

/// # Errors
///
/// Returns `ArgsError` for help, unknown flags, missing values, missing
/// required owner fields, UUID parse errors, or unreadable current dir.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<McpConfig, ArgsError> {
    let mut owner_user: Option<Uuid> = None;
    let mut owner_org: Option<Uuid> = None;
    let mut database_url: Option<String> = None;
    let mut bind: Option<SocketAddr> = None;
    let mut master_token: Option<Uuid> = None;

    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "-h" | "--help" => return Err(ArgsError::Help),
            f => {
                let value = iter
                    .next()
                    .ok_or_else(|| ArgsError::Invalid(format!("flag {f} expects a value")))?;
                match f {
                    "--owner-user" => owner_user = Some(Uuid::parse_str(&value)?),
                    "--owner-org" => owner_org = Some(Uuid::parse_str(&value)?),
                    "--database-url" => database_url = Some(value),
                    "--master-token" => master_token = Some(parse_master_token(&value)?),
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

    let owner_user =
        owner_user.ok_or_else(|| ArgsError::Invalid("--owner-user required".into()))?;
    let owner_org = owner_org.ok_or_else(|| ArgsError::Invalid("--owner-org required".into()))?;
    let database_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    });
    let master_token = match master_token {
        Some(token) => Some(token),
        None => std::env::var("PROXIMA_MCP_MASTER_TOKEN")
            .ok()
            .map(|raw| parse_master_token(&raw))
            .transpose()?,
    };

    Ok(McpConfig {
        database_url,
        owner: Owner {
            principal: Principal::User(UserId::new(owner_user)),
            org_id: OrgId::new(owner_org),
        },
        bind,
        master_token,
    })
}

/// # Errors
///
/// Returns `ArgsError` for help, unknown flags, missing values, invalid
/// timestamp, negative limit, or unreadable env defaults.
pub fn parse_reconcile_args<I: IntoIterator<Item = String>>(
    args: I,
) -> Result<ReconcileConfig, ArgsError> {
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

    let database_url = database_url.unwrap_or_else(|| {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    });

    Ok(ReconcileConfig {
        database_url,
        model,
        scope,
        limit,
        drain,
    })
}

fn parse_master_token(raw: &str) -> Result<Uuid, uuid::Error> {
    let trimmed = raw.trim();
    let bare = trimmed.strip_prefix("pxm_").unwrap_or(trimmed);
    Uuid::parse_str(bare)
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
    fn missing_owner_is_rejected() {
        let err = parse_args(["--owner-org".into(), uuid::Uuid::nil().to_string()])
            .expect_err("missing owner-user");
        assert!(err.to_string().contains("--owner-user required"));
    }

    #[test]
    fn full_args_parse() {
        let cfg = parse_args([
            "--owner-user".into(),
            uuid::Uuid::nil().to_string(),
            "--owner-org".into(),
            uuid::Uuid::nil().to_string(),
            "--database-url".into(),
            "postgres://x/y".into(),
            "--master-token".into(),
            uuid::Uuid::nil().to_string(),
        ])
        .expect("valid args");
        assert_eq!(cfg.database_url, "postgres://x/y");
        assert!(cfg.bind.is_none());
        assert_eq!(cfg.master_token, Some(uuid::Uuid::nil()));
    }

    #[test]
    fn loopback_bind_parse() {
        let cfg = parse_args([
            "--owner-user".into(),
            uuid::Uuid::nil().to_string(),
            "--owner-org".into(),
            uuid::Uuid::nil().to_string(),
            "--bind".into(),
            "127.0.0.1:9999".into(),
        ])
        .expect("valid args");
        assert_eq!(
            cfg.bind,
            Some("127.0.0.1:9999".parse().expect("valid bind"))
        );
    }

    #[test]
    fn master_token_accepts_wire_prefix() {
        let cfg = parse_args([
            "--owner-user".into(),
            uuid::Uuid::nil().to_string(),
            "--owner-org".into(),
            uuid::Uuid::nil().to_string(),
            "--master-token".into(),
            "pxm_00000000-0000-0000-0000-000000000000".into(),
        ])
        .expect("valid args");
        assert_eq!(cfg.master_token, Some(uuid::Uuid::nil()));
    }

    #[test]
    fn non_loopback_bind_rejected() {
        let err = parse_args([
            "--owner-user".into(),
            uuid::Uuid::nil().to_string(),
            "--owner-org".into(),
            uuid::Uuid::nil().to_string(),
            "--bind".into(),
            "0.0.0.0:31415".into(),
        ])
        .expect_err("non-loopback");
        assert!(err.to_string().contains("loopback"));
    }

    #[test]
    fn reconcile_help_flag_returns_help() {
        let err = parse_reconcile_args(["--help".to_string()]).expect_err("help");
        assert!(err.is_help());
    }

    #[test]
    fn reconcile_args_parse_scope_and_limit() {
        let cfg = parse_reconcile_args([
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
    fn reconcile_since_parses_rfc3339() {
        let cfg = parse_reconcile_args(["--since".into(), "2026-06-16T10:00:00Z".into()])
            .expect("valid args");
        assert!(matches!(cfg.scope, ReconcileScope::Since(_)));
    }
}
