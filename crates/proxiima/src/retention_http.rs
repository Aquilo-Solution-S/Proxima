//! The stock host's retention erase (docs/data-and-erasure.md): one route the
//! home calls with its retention's cutoff, which this host runs under its own
//! system authority, so a spiice writes no code for it.
//!
//! The call is the home's only when its forwarded credential vouches for it:
//! a verified forwarded owner and the signed claim `"vouch": "retention"`.
//! The forwarder's own role is below admin and never erases; an ordinary
//! caller, an unvouched forwarded call and a claim this host does not know
//! are refused before anything is read.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use proxiima_core::authz::SystemAuthority;
use proxiima_core::verbs::own_erase::{EraseMode, SeriesEraseError, SeriesSelection};
use proxiima_core::{AuthzContext, Engine, ErrorCode, OwnerId, SchemaId};
use proxiima_mcp_server::McpAuthContext;
use serde::{Deserialize, Serialize};

/// The route the home's retention tick calls.
pub const RETENTION_ERASE_PATH: &str = "/retention/erase";

/// The signed claim that makes a forwarded call the home's retention.
const VOUCH_CLAIM: &str = "vouch";

/// What a forwarded credential vouches for, parsed where it enters. The
/// home owns the set; a value this host does not know is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vouch {
    Retention,
}

impl Vouch {
    fn of(authz: &AuthzContext) -> Result<Option<Self>, Refusal> {
        match authz
            .verified_claims()
            .and_then(|claims| claims.get(VOUCH_CLAIM))
        {
            None => Ok(None),
            Some(serde_json::Value::String(vouch)) if vouch == "retention" => {
                Ok(Some(Self::Retention))
            }
            Some(_) => Err(Refusal::NotVouched(
                "the credential vouches for something else",
            )),
        }
    }
}

/// One erase: every series of `schema_id` aged past `cutoff` by its own
/// clock, for the vouched owner.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EraseRequest {
    schema_id: String,
    #[serde(with = "time::serde::rfc3339")]
    cutoff: time::OffsetDateTime,
}

/// What the erase destroyed, summed over its pages.
#[derive(Debug, Default, Serialize)]
struct EraseAnswer {
    owner: String,
    schema_id: String,
    pages: u64,
    series_erased: u64,
    versions_erased: u64,
    referencing_series: u64,
    blobs_removed: u64,
    cold_objects_pending: u64,
    dangling_pins: u64,
}

/// A JSON answer, as the route sends every reply.
fn json(status: StatusCode, body: &impl Serialize) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Debug)]
enum Refusal {
    NotVouched(&'static str),
    BadRequest(String),
    UnknownSchema(String),
    Erase(SeriesEraseError),
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::NotVouched(why) => (StatusCode::FORBIDDEN, why.to_owned()),
            Self::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            Self::UnknownSchema(schema) => (
                StatusCode::NOT_FOUND,
                format!("no spiice of this store declares {schema}"),
            ),
            Self::Erase(SeriesEraseError::Refused(refusal)) => {
                (StatusCode::CONFLICT, refusal.to_string())
            }
            Self::Erase(SeriesEraseError::Retryable(why)) => {
                (StatusCode::SERVICE_UNAVAILABLE, format!("retry: {why}"))
            }
            Self::Erase(SeriesEraseError::Protocol(err)) => (
                match err.code {
                    ErrorCode::Forbidden => StatusCode::FORBIDDEN,
                    ErrorCode::InvalidArgument => StatusCode::BAD_REQUEST,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                },
                err.message,
            ),
        };
        json(status, &serde_json::json!({ "error": message }))
    }
}

#[derive(Clone)]
struct RetentionErase {
    engine: Arc<Engine>,
    authority: Arc<SystemAuthority>,
}

/// The route, for the runtime's protected router: its bearer auth runs first.
pub(crate) fn router(engine: Arc<Engine>, authority: Arc<SystemAuthority>) -> Router {
    Router::new()
        .route(RETENTION_ERASE_PATH, post(erase))
        .with_state(RetentionErase { engine, authority })
}

async fn erase(
    State(host): State<RetentionErase>,
    Extension(auth): Extension<McpAuthContext>,
    body: Bytes,
) -> Result<Response, Refusal> {
    let owner = vouched_owner(&auth)?;
    let request: EraseRequest =
        serde_json::from_slice(&body).map_err(|err| Refusal::BadRequest(err.to_string()))?;
    // A retention keeps at least a day, so its cutoff lies in the past; a
    // later one would take the schema whole.
    if request.cutoff > time::OffsetDateTime::now_utc() {
        return Err(Refusal::BadRequest(
            "the cutoff lies after this host's clock".into(),
        ));
    }
    let schema = SchemaId::new(request.schema_id);
    let spiice = spiice_of(&host.engine, &schema)?;
    let mut answer = EraseAnswer {
        owner: owner.to_string(),
        schema_id: schema.to_string(),
        ..EraseAnswer::default()
    };
    loop {
        let mut unit = host
            .engine
            .system_unit_of_work(&host.authority)
            .map_err(|err| Refusal::Erase(err.into()))?;
        let receipt = unit
            .erase_own_series(
                spiice,
                owner,
                SeriesSelection::OccurredBefore {
                    schema: schema.clone(),
                    cutoff: request.cutoff,
                },
                EraseMode::Erase,
            )
            .await
            .map_err(Refusal::Erase)?;
        unit.commit()
            .await
            .map_err(|err| Refusal::Erase(err.into()))?;
        answer.pages += 1;
        answer.series_erased += receipt.series_erased;
        answer.versions_erased += receipt.versions_erased;
        answer.referencing_series += receipt.referencing_series;
        answer.blobs_removed += receipt.blobs_removed;
        answer.cold_objects_pending += receipt.cold_objects_pending;
        answer.dangling_pins += receipt.dangling_pins;
        if !receipt.more_remaining {
            return Ok(json(StatusCode::OK, &answer));
        }
    }
}

/// The owner a vouched forwarded credential names, which the auth layer has
/// already bound the request to.
fn vouched_owner(auth: &McpAuthContext) -> Result<OwnerId, Refusal> {
    let owner = auth
        .authz
        .verified_forwarded_owner()
        .ok_or(Refusal::NotVouched(
            "only the home's forwarded retention may erase here",
        ))?;
    match Vouch::of(&auth.authz)? {
        Some(Vouch::Retention) if owner == auth.owner => Ok(owner),
        Some(Vouch::Retention) => Err(Refusal::NotVouched("the request is bound to another owner")),
        None => Err(Refusal::NotVouched(
            "a forwarded call without the home's retention vouch does not erase",
        )),
    }
}

/// The registered spiice that declares `schema`: the one whose erase may
/// reach it. `erase_own_series` refuses a core schema, or one that is not a
/// Fact, Abstraction or Perspective.
fn spiice_of(engine: &Engine, schema: &SchemaId) -> Result<&'static str, Refusal> {
    engine
        .registry()
        .contracts()
        .iter()
        .find(|contract| {
            contract
                .schemas
                .iter()
                .any(|declared| declared.id.schema_id() == *schema)
        })
        .map(|contract| contract.spiice_id)
        .ok_or_else(|| Refusal::UnknownSchema(schema.to_string()))
}
