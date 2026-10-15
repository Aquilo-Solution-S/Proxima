//! The built-in HTTP outbox sink (docs/18 §Built-in HTTP sink): one
//! [`DurableIntake`] that `POST`s each captured event, verbatim, to one URL.
//!
//! Configured from the environment only, and registered as the consumer
//! `http` only when `PROXIMA_OUTBOX_HTTP_URL` is set:
//!
//! - `PROXIMA_OUTBOX_HTTP_URL`: `https://…` (or `http://…` without a token).
//! - `PROXIMA_OUTBOX_HTTP_TOKEN_FILE`: re-read on every delivery and sent as
//!   `Authorization: Bearer <token>`, so a rotated token is picked up without
//!   a restart. Never cached, never logged.
//! - `PROXIMA_OUTBOX_HTTP_CA_FILE`: PEM bundle; when set, it is the *only*
//!   trust root (no built-in roots).
//!
//! One event per request; no batching, no broker client.

use std::path::{Path, PathBuf};
use std::time::Duration;

use proxima_core::publication::intake::{DurableIntake, Intake, IntakeError, ReceivedEvent};
use proxima_core::storage_ports::publication::ConsumerName;

use crate::ProximaError;

pub(crate) const ENV_URL: &str = "PROXIMA_OUTBOX_HTTP_URL";
pub(crate) const ENV_TOKEN_FILE: &str = "PROXIMA_OUTBOX_HTTP_TOKEN_FILE";
pub(crate) const ENV_CA_FILE: &str = "PROXIMA_OUTBOX_HTTP_CA_FILE";

/// The consumer name the built-in sink registers under.
pub(crate) const CONSUMER: &str = "http";

/// Under the runtime's 5 s `accept` bound, so the sink answers (with an
/// [`IntakeError`]) before the runtime abandons the call.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_millis(4_500);

/// The longest receiver body kept as a rejection reason, in characters.
const MAX_REASON_CHARS: usize = 512;

/// The most of a rejection body ever read: [`MAX_REASON_CHARS`] characters
/// of UTF-8. The rest is never read, so a receiver's body size never
/// becomes this process's allocation.
const MAX_REASON_BYTES: usize = MAX_REASON_CHARS * 4;

pub(crate) const CONTENT_TYPE: &str = "application/cloudevents+json";
pub(crate) const DELIVERY_COUNT_HEADER: &str = "Proxima-Delivery-Count";

/// The `http` consumer name.
///
/// # Panics
///
/// Never: `http` is a valid [`ConsumerName`].
pub(crate) fn consumer_name() -> ConsumerName {
    ConsumerName::new(CONSUMER).expect("`http` is a consumer name")
}

/// `POST`s each event to one receiver. Built once at boot.
pub struct HttpIntake {
    client: reqwest::Client,
    url: reqwest::Url,
    token_file: Option<PathBuf>,
}

impl std::fmt::Debug for HttpIntake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The URL may carry userinfo or a query; print the origin only.
        f.debug_struct("HttpIntake")
            .field("origin", &self.url.origin().ascii_serialization())
            .field("has_token_file", &self.token_file.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpIntake {
    /// Build the sink. Fails fast on a bad URL or an unreadable/invalid CA.
    ///
    /// # Errors
    ///
    /// [`ProximaError::Config`] naming the variable at fault.
    pub(crate) fn new(
        url: &str,
        token_file: Option<PathBuf>,
        ca_file: Option<&Path>,
    ) -> Result<Self, ProximaError> {
        let url = reqwest::Url::parse(url)
            .map_err(|err| ProximaError::Config(format!("{ENV_URL} is not a URL: {err}")))?;
        match url.scheme() {
            "https" => {}
            "http" if token_file.is_none() => {}
            "http" => {
                return Err(ProximaError::Config(format!(
                    "{ENV_URL} must be https:// when {ENV_TOKEN_FILE} is set: the bearer \
                     token is never sent in clear text"
                )));
            }
            other => {
                return Err(ProximaError::Config(format!(
                    "{ENV_URL} must be an http(s) URL, got scheme {other:?}"
                )));
            }
        }
        if url.host_str().is_none() {
            return Err(ProximaError::Config(format!("{ENV_URL} has no host")));
        }
        let mut builder = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(ca_file) = ca_file {
            let pem = std::fs::read(ca_file).map_err(|err| {
                ProximaError::Config(format!("{ENV_CA_FILE} could not be read: {err}"))
            })?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|err| {
                ProximaError::Config(format!("{ENV_CA_FILE} is not a PEM bundle: {err}"))
            })?;
            if certs.is_empty() {
                return Err(ProximaError::Config(format!(
                    "{ENV_CA_FILE} holds no certificate"
                )));
            }
            // Exclusively the configured CA: no built-in or platform roots.
            builder = builder.tls_certs_only(certs);
        }
        let client = builder.build().map_err(|err| {
            ProximaError::Config(format!("outbox HTTP client could not be built: {err}"))
        })?;
        Ok(Self {
            client,
            url,
            token_file,
        })
    }

    /// The sink the environment configures; `None` when the URL is unset.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub(crate) fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ProximaError> {
        let Some(url) = lookup(ENV_URL) else {
            return Ok(None);
        };
        let token_file = lookup(ENV_TOKEN_FILE).map(PathBuf::from);
        let ca_file = lookup(ENV_CA_FILE).map(PathBuf::from);
        Self::new(&url, token_file, ca_file.as_deref()).map(Some)
    }

    async fn bearer(&self) -> Result<Option<String>, IntakeError> {
        let Some(path) = &self.token_file else {
            return Ok(None);
        };
        // Re-read on every delivery: a rotated token takes effect at once.
        let token = tokio::fs::read_to_string(path)
            .await
            .map_err(|err| IntakeError::new(format!("{ENV_TOKEN_FILE} unreadable: {err}")))?;
        let token = token.trim();
        if token.is_empty() {
            return Err(IntakeError::new(format!("{ENV_TOKEN_FILE} is empty")));
        }
        Ok(Some(format!("Bearer {token}")))
    }
}

/// What one receiver status means for the record.
///
/// - 2xx: the receiver made the event durable → [`Intake::Accepted`].
/// - 400, 422: the receiver durably refused this event → [`Intake::Rejected`].
/// - Anything else (401, 403, 408, 429, 5xx, 3xx, other 4xx): no decision →
///   [`IntakeError`], and the runtime retries. Retrying is the safe default
///   for a status this table does not name.
pub(crate) fn classify(status: reqwest::StatusCode, body: &str) -> Result<Intake, IntakeError> {
    if status.is_success() {
        return Ok(Intake::Accepted);
    }
    let code = status.as_u16();
    if matches!(code, 400 | 422) {
        let detail: String = body.trim().chars().take(MAX_REASON_CHARS).collect();
        let detail = if detail.is_empty() {
            status.canonical_reason().unwrap_or("rejected").to_string()
        } else {
            detail
        };
        return Ok(Intake::Rejected {
            reason: format!("HTTP {code}: {detail}"),
        });
    }
    Err(IntakeError::new(format!(
        "outbox receiver answered HTTP {code}; retrying"
    )))
}

#[async_trait::async_trait]
impl DurableIntake for HttpIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        let bearer = self.bearer().await?;
        let mut request = self
            .client
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, CONTENT_TYPE)
            .header(DELIVERY_COUNT_HEADER, event.delivered_count.to_string())
            .body(event.raw.clone());
        if let Some(bearer) = bearer {
            request = request.header(reqwest::header::AUTHORIZATION, bearer);
        }
        let response = request.send().await.map_err(|err| {
            // `without_url`: the URL may carry a credential.
            IntakeError::new(format!(
                "outbox receiver unreachable: {}",
                err.without_url()
            ))
        })?;
        let status = response.status();
        let body = if matches!(status.as_u16(), 400 | 422) {
            reason_body(response).await
        } else {
            String::new()
        };
        classify(status, &body)
    }
}

/// The first [`MAX_REASON_BYTES`] of a rejection body, read chunk by chunk
/// and decoded lossily. A body that fails mid-read keeps what arrived.
async fn reason_body(mut response: reqwest::Response) -> String {
    let mut bytes = Vec::with_capacity(MAX_REASON_BYTES);
    while bytes.len() < MAX_REASON_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let wanted = MAX_REASON_BYTES - bytes.len();
                bytes.extend_from_slice(&chunk[..chunk.len().min(wanted)]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
#[path = "outbox_http_tests.rs"]
pub(crate) mod tests;
