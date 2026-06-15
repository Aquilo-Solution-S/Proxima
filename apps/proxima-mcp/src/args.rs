use std::net::SocketAddr;

use proxima_core::{OrgId, Owner, Principal, UserId};
use uuid::Uuid;

pub const DEFAULT_BIND: &str = "127.0.0.1:31415";
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

pub const USAGE: &str = "\
Usage: proxima-mcp [OPTIONS]

Proxima Streamable HTTP MCP server.

Required:
  --owner-user <UUID>      Fixed owner principal
  --owner-org  <UUID>      Fixed org id
  --master-token <UUID>    Local bearer token (or PROXIMA_MCP_MASTER_TOKEN);
                           clients send Authorization: Bearer pxm_<token>

Optional:
  --database-url <URL>     Postgres URL (defaults to DATABASE_URL or proxima_dev)
  --bind <ADDR:PORT>       Bind address (default: 127.0.0.1:31415; loopback only)
  -h, --help               Print this message

Endpoint:
  http://127.0.0.1:31415/mcp

Tools:
  core/search_memories
  core/open
  core/remember
  core/derive
  core/link
";

#[derive(Debug, Clone)]
pub struct McpConfig {
    pub database_url: String,
    pub owner: Owner,
    pub bind: SocketAddr,
    pub master_token: Option<Uuid>,
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
    let bind = match bind {
        Some(bind) => bind,
        None => DEFAULT_BIND.parse().map_err(|err| {
            ArgsError::Invalid(format!("invalid default bind {DEFAULT_BIND:?}: {err}"))
        })?,
    };
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
        assert_eq!(cfg.bind.to_string(), DEFAULT_BIND);
        assert_eq!(cfg.master_token, Some(uuid::Uuid::nil()));
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
}
