//! `dev-idp` — a real OIDC issuer, on loopback, for self-hosted Proxima.
//!
//! Proxima authenticates MCP callers one way: an RS256 bearer, verified
//! against a JWKS discovered from the configured issuer, with `(iss, sub)`
//! mapped to a user through an explicit subject map. That is the only auth
//! path, deliberately — there is no dev bypass in the server, and adding one
//! would mean the thing you test locally is not the thing you deploy.
//!
//! So the local story is a local *issuer*, not a local exception. This binary
//! is that issuer: it generates an RSA key, serves
//! `/.well-known/openid-configuration` and a JWKS, and mints a bearer. The
//! server verifies it with exactly the code an operator runs against Entra,
//! Zitadel, or Auth0.
//!
//! It binds loopback only and refuses anything else. Plaintext HTTP is
//! acceptable here for the same reason it is acceptable for a local model
//! endpoint: the bearer never leaves the host. See
//! `crates/auth-oidc/src/config.rs::validate_https_url`, which admits
//! loopback HTTP for issuers and — only for a loopback issuer — its JWKS.
//!
//! The signing key is persisted (0600) so tokens survive a restart; without
//! that, every restart would silently invalidate the bearer sitting in your
//! agent's MCP config. Use `--ephemeral` to keep it in memory instead.
//!
//! ```text
//! cargo run -p proxima-dev-idp
//! ```
//!
//! Never deploy this. It mints tokens for anyone who can run it.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use uuid::Uuid;

const DEFAULT_BIND: &str = "127.0.0.1:31416";
const DEFAULT_AUDIENCE: &str = "proxima-mcp";
const DEFAULT_SUBJECT: &str = "proxima-dev";
const DEFAULT_TTL_DAYS: u64 = 30;
const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:31415";
const KID: &str = "dev-idp";

/// Namespace for deriving a stable user id from a subject name. Fixed so a
/// given `--subject` always maps to the same Proxima user across restarts and
/// across machines — otherwise a restart would strand every memory you wrote
/// under the previous id.
const USER_ID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x70, 0x72, 0x6f, 0x78, 0x69, 0x6d, 0x61, 0x2d, 0x64, 0x65, 0x76, 0x2d, 0x69, 0x64, 0x70, 0x00,
]);

const USAGE: &str = "\
Usage: dev-idp [OPTIONS]

A loopback OIDC issuer for running Proxima locally. Serves JWKS discovery and
prints a ready-to-paste bearer plus the exact env Proxima needs.

Options:
  --bind <ADDR:PORT>    Issuer bind address (loopback only) [default: 127.0.0.1:31416]
  --audience <AUD>      Token audience, must match PROXIMA_OIDC_AUDIENCE [default: proxima-mcp]
  --subject <SUB>       Token subject [default: proxima-dev]
  --user-id <UUID>      Proxima user id for that subject [default: derived from --subject]
  --ttl-days <N>        Token lifetime in days [default: 30]
  --server-url <URL>    Proxima MCP base URL, for the printed client config
                        [default: http://127.0.0.1:31415]
  --key-file <PATH>     Signing key location [default: $HOME/.proxima/dev-idp.pkcs8]
  --ephemeral           Keep the signing key in memory; tokens die with this process
  -h, --help            Print this message

Never deploy this binary. It issues tokens to whoever can reach it.
";

#[derive(Debug)]
struct Config {
    bind: SocketAddr,
    audience: String,
    subject: String,
    user_id: Uuid,
    ttl_days: u64,
    server_url: String,
    key_file: Option<PathBuf>,
}

#[derive(Debug)]
enum ArgError {
    Help,
    Message(String),
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Help => f.write_str(USAGE),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

fn default_key_file() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".proxima/dev-idp.pkcs8"))
}

fn parse_args(argv: impl IntoIterator<Item = String>) -> Result<Config, ArgError> {
    let mut bind = DEFAULT_BIND.to_string();
    let mut audience = DEFAULT_AUDIENCE.to_string();
    let mut subject = DEFAULT_SUBJECT.to_string();
    let mut user_id: Option<Uuid> = None;
    let mut ttl_days = DEFAULT_TTL_DAYS;
    let mut server_url = DEFAULT_SERVER_URL.to_string();
    let mut key_file = default_key_file();
    let mut ephemeral = false;

    let mut rest = argv.into_iter();
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .ok_or_else(|| ArgError::Message(format!("{flag} requires a value")))
        };
        match arg.as_str() {
            "-h" | "--help" => return Err(ArgError::Help),
            "--bind" => bind = value("--bind")?,
            "--audience" => audience = value("--audience")?,
            "--subject" => subject = value("--subject")?,
            "--server-url" => server_url = value("--server-url")?,
            "--key-file" => key_file = Some(PathBuf::from(value("--key-file")?)),
            "--ephemeral" => ephemeral = true,
            "--user-id" => {
                let raw = value("--user-id")?;
                user_id =
                    Some(raw.parse().map_err(|_| {
                        ArgError::Message(format!("--user-id is not a UUID: {raw}"))
                    })?);
            }
            "--ttl-days" => {
                let raw = value("--ttl-days")?;
                ttl_days = raw
                    .parse()
                    .map_err(|_| ArgError::Message(format!("--ttl-days is not a number: {raw}")))?;
            }
            other => return Err(ArgError::Message(format!("unknown argument {other}"))),
        }
    }

    let bind: SocketAddr = bind
        .parse()
        .map_err(|_| ArgError::Message(format!("--bind is not an address:port: {bind}")))?;
    if !bind.ip().is_loopback() {
        return Err(ArgError::Message(format!(
            "--bind must be loopback; {bind} would expose a token-minting issuer on the network"
        )));
    }
    if subject.trim().is_empty() {
        return Err(ArgError::Message("--subject must not be empty".into()));
    }

    Ok(Config {
        bind,
        audience,
        user_id: user_id
            .unwrap_or_else(|| Uuid::new_v5(&USER_ID_NAMESPACE, subject.trim().as_bytes())),
        subject: subject.trim().to_string(),
        ttl_days,
        server_url: server_url.trim_end_matches('/').to_string(),
        key_file: if ephemeral { None } else { key_file },
    })
}

/// Load the persisted signing key, generating and storing one on first run.
///
/// A generated key is written 0600 before it is used, so a partial write can
/// never leave a world-readable signing key behind.
fn load_or_create_key(path: Option<&PathBuf>) -> Result<RsaKeyPair, String> {
    let Some(path) = path else {
        return RsaKeyPair::generate(KeySize::Rsa2048)
            .map_err(|_| "failed to generate an RSA key".to_string());
    };

    if let Ok(bytes) = std::fs::read(path) {
        return RsaKeyPair::from_pkcs8(&bytes).map_err(|err| {
            format!(
                "{} is not a usable PKCS#8 RSA key ({err}); delete it to regenerate",
                path.display()
            )
        });
    }

    let key =
        RsaKeyPair::generate(KeySize::Rsa2048).map_err(|_| "failed to generate an RSA key")?;
    let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der<'_>>::as_der(&key)
        .map_err(|_| "failed to serialize the generated key")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    file.write_all(der.as_ref())
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(key)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// The JWKS this issuer publishes: one RSA key, by `n`/`e`, exactly the shape
/// `HttpJwksResolver` materializes.
fn jwks(key: &RsaKeyPair) -> String {
    let components: PublicKeyComponents<Vec<u8>> = key.public_key().into();
    serde_json::json!({
        "keys": [{
            "kid": KID,
            "kty": "RSA",
            "alg": "RS256",
            "use": "sig",
            "n": URL_SAFE_NO_PAD.encode(&components.n),
            "e": URL_SAFE_NO_PAD.encode(&components.e),
        }]
    })
    .to_string()
}

/// Mint an RS256 bearer for `config`'s subject.
fn mint(key: &RsaKeyPair, issuer: &str, config: &Config) -> Result<String, String> {
    let now = now_secs();
    let header = serde_json::json!({ "alg": "RS256", "kid": KID, "typ": "JWT" });
    let claims = serde_json::json!({
        "iss": issuer,
        "aud": config.audience,
        "sub": config.subject,
        "iat": now,
        "nbf": now,
        "exp": now + config.ttl_days * 24 * 60 * 60,
    });
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).map_err(|e| e.to_string())?),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|e| e.to_string())?),
    );
    let mut signature = vec![0; key.public_modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| "failed to sign the dev token".to_string())?;
    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(&signature)
    ))
}

fn json_route(
    body: Arc<String>,
) -> impl Fn() -> std::future::Ready<axum::response::Response> + Clone {
    move || {
        let body = Arc::clone(&body);
        std::future::ready(
            (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "application/json; charset=utf-8",
                )],
                body.as_str().to_owned(),
            )
                .into_response(),
        )
    }
}

use axum::response::IntoResponse as _;

fn print_instructions(config: &Config, issuer: &str, token: &str, key_file: Option<&PathBuf>) {
    let Config {
        audience,
        subject,
        user_id,
        server_url,
        ttl_days,
        ..
    } = config;
    println!("dev-idp — loopback OIDC issuer for Proxima. Do not deploy.\n");
    println!("  issuer    {issuer}");
    println!("  audience  {audience}");
    println!("  subject   {subject} -> user {user_id}");
    println!("  token     valid for {ttl_days} days");
    match key_file {
        Some(path) => println!("  key       {} (0600, reused on restart)", path.display()),
        None => println!("  key       in memory — this token dies with this process"),
    }
    println!(
        "\n1. Point Proxima at this issuer, in another shell:\n\
         \n\
         export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima\n\
         export PROXIMA_OIDC_ISSUER={issuer}\n\
         export PROXIMA_OIDC_AUDIENCE={audience}\n\
         export PROXIMA_OIDC_SUBJECT_MAP={subject}:{user_id}\n\
         export PROXIMA_PUBLIC_URL={server_url}\n\
         export PROXIMA_TOOL_PROFILE=full\n\
         cargo run -p proxima-mcp\n"
    );
    println!(
        "2. Connect your coding agent:\n\
         \n\
         claude mcp add --transport http proxima {server_url}/mcp \\\n\
         \x20 --header \"Authorization: Bearer {token}\" \\\n\
         \x20 --header \"X-Proxima-Owner: personal:{user_id}\"\n"
    );
    println!("Serving. Ctrl-C to stop.");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(ArgError::Help) => {
            print!("{USAGE}");
            return Ok(());
        }
        Err(error) => {
            eprintln!("error: {error}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let key = load_or_create_key(config.key_file.as_ref())?;
    let issuer = format!("http://{}", config.bind);
    let token = mint(&key, &issuer, &config)?;

    let discovery = Arc::new(
        serde_json::json!({
            "issuer": issuer,
            "jwks_uri": format!("{issuer}/keys"),
            "response_types_supported": ["id_token"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        })
        .to_string(),
    );
    let keys = Arc::new(jwks(&key));

    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(json_route(discovery)),
        )
        .route("/keys", axum::routing::get(json_route(keys)));

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    print_instructions(&config, &issuer, &token, config.key_file.as_ref());

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Result<Config, ArgError> {
        parse_args(raw.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn defaults_are_loopback_and_derive_a_stable_user_id() {
        let a = args(&[]).expect("defaults parse");
        let b = args(&[]).expect("defaults parse");
        assert!(a.bind.ip().is_loopback());
        assert_eq!(a.user_id, b.user_id, "user id must survive a restart");
        assert_eq!(a.subject, DEFAULT_SUBJECT);
    }

    #[test]
    fn distinct_subjects_get_distinct_users() {
        let a = args(&["--subject", "alice"]).expect("parse");
        let b = args(&["--subject", "bob"]).expect("parse");
        assert_ne!(a.user_id, b.user_id);
    }

    /// A token-minting issuer on a routable interface would hand anyone who
    /// can reach it a valid Proxima identity.
    #[test]
    fn refuses_a_non_loopback_bind() {
        let err = args(&["--bind", "0.0.0.0:31416"]).expect_err("must refuse");
        assert!(
            matches!(&err, ArgError::Message(m) if m.contains("loopback")),
            "{err}"
        );
    }

    #[test]
    fn explicit_user_id_wins_over_the_derived_one() {
        let id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("uuid");
        let config = args(&["--user-id", &id.to_string()]).expect("parse");
        assert_eq!(config.user_id, id);
    }

    #[test]
    fn rejects_malformed_values() {
        assert!(args(&["--user-id", "nope"]).is_err());
        assert!(args(&["--ttl-days", "soon"]).is_err());
        assert!(args(&["--bind", "not-an-address"]).is_err());
        assert!(args(&["--nonsense"]).is_err());
        assert!(args(&["--bind"]).is_err());
    }

    /// The published JWKS must be exactly what Proxima's resolver
    /// materializes: `kty: RSA` with base64url `n`/`e`.
    #[test]
    fn jwks_carries_base64url_rsa_components() {
        let key = RsaKeyPair::generate(KeySize::Rsa2048).expect("generate");
        let parsed: serde_json::Value = serde_json::from_str(&jwks(&key)).expect("json");
        let jwk = &parsed["keys"][0];
        assert_eq!(jwk["kty"], "RSA");
        assert_eq!(jwk["alg"], "RS256");
        assert_eq!(jwk["kid"], KID);
        let n = jwk["n"].as_str().expect("n");
        let e = jwk["e"].as_str().expect("e");
        assert!(!n.contains('=') && !n.contains('+') && !n.contains('/'));
        assert!(URL_SAFE_NO_PAD.decode(n).is_ok());
        assert_eq!(URL_SAFE_NO_PAD.decode(e).expect("e decodes"), vec![1, 0, 1]);
    }

    /// The minted token must carry the claims the authenticator validates:
    /// issuer, audience, subject, and a future expiry.
    #[test]
    fn minted_token_carries_the_validated_claims() {
        let key = RsaKeyPair::generate(KeySize::Rsa2048).expect("generate");
        let config = args(&["--subject", "alice", "--audience", "proxima-test"]).expect("parse");
        let token = mint(&key, "http://127.0.0.1:31416", &config).expect("mint");

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "compact JWS serialization");
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).expect("header"))
                .expect("json");
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"], KID);

        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).expect("claims"))
                .expect("json");
        assert_eq!(claims["iss"], "http://127.0.0.1:31416");
        assert_eq!(claims["aud"], "proxima-test");
        assert_eq!(claims["sub"], "alice");
        assert!(claims["exp"].as_u64().expect("exp") > now_secs());
        assert!(claims["nbf"].as_u64().expect("nbf") <= now_secs());
    }

    /// A persisted key must round-trip, or every restart silently invalidates
    /// the bearer sitting in the agent's MCP config.
    #[test]
    fn persisted_key_is_reused_across_runs() {
        let dir = std::env::temp_dir().join(format!("dev-idp-{}", Uuid::now_v7()));
        let path = dir.join("key.pkcs8");
        let first = load_or_create_key(Some(&path)).expect("first run generates");
        let second = load_or_create_key(Some(&path)).expect("second run reuses");
        assert_eq!(
            jwks(&first),
            jwks(&second),
            "the same key must be published on restart"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
