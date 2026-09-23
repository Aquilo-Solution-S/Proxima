#![allow(dead_code)]

use async_trait::async_trait;
use proxima_core::llm::{EmbeddingClient, EmbeddingDim, LlmError};
use proxima_core::{Owner, OwnerRef, UserId};

/// Fixture embeddings sit on the default 1024-wide lane.
const EMBEDDING_DIM: usize = EmbeddingDim::D1024.width();

/// Model a successful verifier for a synthetic fixture principal.
///
/// # Panics
/// The supplied fixture must already contain server-resolved owner roles.
#[must_use]
pub fn authenticated_context(context: crate::AuthzContext) -> crate::AuthzContext {
    struct FixtureVerifier(proxima_core::AuthzContext);
    #[async_trait]
    impl proxima_core::Authenticator for FixtureVerifier {
        async fn authenticate(
            &self,
            _: &proxima_core::Credentials,
        ) -> Result<proxima_core::AuthzContext, proxima_core::AuthError> {
            Ok(self.0.clone())
        }
    }
    // This verifier does no I/O: exercise the public authentication seam
    // without creating or blocking a Tokio runtime in synchronous fixtures.
    let verifier = FixtureVerifier(context);
    let credentials = proxima_core::Credentials::Bearer("synthetic-fixture".into());
    let mut future = std::pin::pin!(proxima_core::authenticate(&verifier, &credentials));
    let mut task = std::task::Context::from_waker(std::task::Waker::noop());
    match std::future::Future::poll(future.as_mut(), &mut task) {
        std::task::Poll::Ready(result) => result.expect("fixture owner roles"),
        std::task::Poll::Pending => panic!("fixture verifier must complete synchronously"),
    }
}

#[derive(Debug, Clone)]
pub struct ConstantEmbedding {
    model_id: String,
    vector: Vec<f32>,
}

impl ConstantEmbedding {
    #[must_use]
    pub fn zero(model_id: impl Into<String>) -> Self {
        Self::filled(model_id, 0.0)
    }

    #[must_use]
    pub fn filled(model_id: impl Into<String>, value: f32) -> Self {
        Self {
            model_id: model_id.into(),
            vector: vec![value; EMBEDDING_DIM],
        }
    }

    #[must_use]
    pub fn prefixed(model_id: impl Into<String>, prefix: &[f32]) -> Self {
        let mut vector = vec![0.0; EMBEDDING_DIM];
        let prefix_len = prefix.len().min(EMBEDDING_DIM);
        vector[..prefix_len].copy_from_slice(&prefix[..prefix_len]);
        Self {
            model_id: model_id.into(),
            vector,
        }
    }
}

#[async_trait]
impl EmbeddingClient for ConstantEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(self.vector.clone())
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.vector.len()
    }
}

/// How a [`RefusingEmbedding`] says no.
///
/// The distinction is the whole point of the fixture: only one of these
/// two errors is self-evidently about the input.
#[derive(Debug, Clone, Copy)]
pub enum EmbedRefusal {
    /// [`LlmError::EmbedPermanent`] — the provider read the input and
    /// rejected it (HTTP 400/413/422 on a live provider, or a client-side
    /// `max_input_chars` guard).
    Permanent,
    /// [`LlmError::Embed`] — the ambiguous one. A local runner that an
    /// over-long input *kills* answers `400 {"error": "… EOF"}`, which is
    /// indistinguishable from a runner that was already down, so it is
    /// classified transient. This is the error production actually hit.
    Transient,
}

/// An embedding client that refuses inputs longer than
/// `embeds_up_to_chars` and embeds everything shorter.
///
/// The threshold is what makes the two failure worlds separable, because a
/// liveness probe is a very short input. A non-zero threshold is a provider
/// that is **up** and cannot take one particular text; zero refuses the
/// probe too and is a provider that is **down**. A client that only ever
/// returned errors could not tell those apart, which is exactly the
/// confusion under test.
#[derive(Debug, Clone)]
pub struct RefusingEmbedding {
    model_id: String,
    embeds_up_to_chars: usize,
    refusal: EmbedRefusal,
    vector: Vec<f32>,
}

impl RefusingEmbedding {
    /// A provider that is up but refuses anything longer than
    /// `embeds_up_to_chars`.
    #[must_use]
    pub fn provider_up(
        model_id: impl Into<String>,
        embeds_up_to_chars: usize,
        refusal: EmbedRefusal,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            embeds_up_to_chars,
            refusal,
            vector: vec![0.0; EMBEDDING_DIM],
        }
    }

    /// A provider that refuses every input, liveness probe included.
    #[must_use]
    pub fn provider_down(model_id: impl Into<String>, refusal: EmbedRefusal) -> Self {
        Self::provider_up(model_id, 0, refusal)
    }
}

#[async_trait]
impl EmbeddingClient for RefusingEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let chars = text.chars().count();
        if chars <= self.embeds_up_to_chars {
            return Ok(self.vector.clone());
        }
        let message = format!("refusing {chars} chars");
        Err(match self.refusal {
            EmbedRefusal::Permanent => LlmError::EmbedPermanent(message),
            EmbedRefusal::Transient => LlmError::Embed(message),
        })
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.vector.len()
    }
}

/// A host router a test rewires while the engine runs: a default route for
/// every Owner, overridden per Owner by a route or a refusal.
#[derive(Debug, Default)]
pub struct TestEmbeddingRouter {
    default: std::sync::RwLock<Option<proxima_core::llm::BoundEmbeddingClient>>,
    owners: std::sync::RwLock<
        std::collections::HashMap<Owner, Result<proxima_core::llm::EmbeddingRoute, String>>,
    >,
}

impl TestEmbeddingRouter {
    /// Route every Owner without an override to `client`.
    ///
    /// # Panics
    /// On a poisoned lock.
    pub fn set_default(&self, client: proxima_core::llm::BoundEmbeddingClient) {
        *self.default.write().expect("router lock") = Some(client);
    }

    /// Route `owner` to `client`, or to no client.
    ///
    /// # Panics
    /// On a poisoned lock.
    pub fn set_owner(&self, owner: Owner, client: Option<proxima_core::llm::BoundEmbeddingClient>) {
        self.set_route(
            owner,
            client.map_or_else(
                proxima_core::llm::EmbeddingRoute::none,
                proxima_core::llm::EmbeddingRoute::current,
            ),
        );
    }

    /// Route `owner` through `route` — a moving route, say.
    ///
    /// # Panics
    /// On a poisoned lock.
    pub fn set_route(&self, owner: Owner, route: proxima_core::llm::EmbeddingRoute) {
        self.owners
            .write()
            .expect("router lock")
            .insert(owner, Ok(route));
    }

    /// Refuse to route `owner`.
    ///
    /// # Panics
    /// On a poisoned lock.
    pub fn refuse(&self, owner: Owner, message: &str) {
        self.owners
            .write()
            .expect("router lock")
            .insert(owner, Err(message.to_string()));
    }
}

#[async_trait]
impl proxima_core::llm::EmbeddingRouter for TestEmbeddingRouter {
    async fn route(
        &self,
        owner: &Owner,
    ) -> Result<proxima_core::llm::EmbeddingRoute, proxima_core::llm::EmbeddingRouteError> {
        match self.owners.read().expect("router lock").get(owner) {
            Some(Ok(route)) => Ok(route.clone()),
            Some(Err(message)) => Err(proxima_core::llm::EmbeddingRouteError::new(message.clone())),
            None => Ok(self
                .default
                .read()
                .expect("router lock")
                .clone()
                .map_or_else(
                    proxima_core::llm::EmbeddingRoute::none,
                    proxima_core::llm::EmbeddingRoute::current,
                )),
        }
    }
}

#[must_use]
pub fn owner_fixture() -> Owner {
    OwnerRef::Personal(UserId::new(uuid::Uuid::nil()))
}

/// A Fact payload that declares itself listenable (issue #305).
///
/// Deliberately sidecar-free: the capture reads the payload's serde JSON
/// through the authorized draft, so a probe that needed a physical sidecar
/// table would be testing the sidecar registry instead of the capture.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ListenableProbeV1 {
    pub probe_id: uuid::Uuid,
    pub note: String,
}

impl proxima_core::FactPayload for ListenableProbeV1 {
    const SCHEMA_ID: &'static str = "probe/listenable-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "required": ["probe_id", "note"],
            "properties": {
                "probe_id": { "type": "string", "format": "uuid" },
                "note": { "type": "string" }
            }
        }))
    }
}

/// The non-listenable twin of [`ListenableProbeV1`]: identical in every
/// respect a write path can see, so a difference in outcome is the
/// declaration and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnlistenableProbeV1 {
    pub probe_id: uuid::Uuid,
    pub note: String,
}

impl proxima_core::FactPayload for UnlistenableProbeV1 {
    const SCHEMA_ID: &'static str = "probe/unlistenable-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::json!({ "type": "object" }))
    }
}

/// A listenable schema that forgot its JSON Schema — the shape freeze must
/// refuse with `ListenableWithoutSchema`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchemalessListenableProbeV1 {
    pub probe_id: uuid::Uuid,
}

impl proxima_core::FactPayload for SchemalessListenableProbeV1 {
    const SCHEMA_ID: &'static str = "probe/schemaless-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.probe_id.to_string()
    }
}

/// The fixture flavor the publication probes belong to.
///
/// A flavor of their own rather than three more `core/` schemas: freeze
/// cross-checks registrations against contracts, and a probe wearing the
/// kernel's prefix would have to be declared in flavor #0's contract —
/// which would ship a test fixture in the shipped catalog.
pub const PROBE_FLAVOR_ID: &str = "probe";

const fn probe_schema(
    name: &'static str,
    listenable_note: &'static str,
) -> proxima_core::flavor::contract::SchemaContract {
    use proxima_core::flavor::contract::{
        EmbeddingRecipe, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, TransferRule,
    };
    SchemaContract {
        id: SchemaRef::new(PROBE_FLAVOR_ID, name, 1),
        kind: proxima_core::verbs::schema::PayloadKind::Fact,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "a publication probe is not a retrievable surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a publication probe is not retrievable content",
        },
        transfer: TransferRule::RetainAtSource {
            why: listenable_note,
        },
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }
}

const PROBE_SCHEMAS: &[proxima_core::flavor::contract::SchemaContract] = &[
    probe_schema("listenable", "a probe owns no rows to move"),
    probe_schema("unlistenable", "a probe owns no rows to move"),
];

const SCHEMALESS_PROBE_SCHEMAS: &[proxima_core::flavor::contract::SchemaContract] =
    &[probe_schema("schemaless", "a probe owns no rows to move")];

/// The probe flavor's contract. Ordinal 90 keeps it clear of flavor #0 and
/// of any shipped flavor.
pub static PROBE_FLAVOR: proxima_core::flavor::contract::FlavorContract =
    proxima_core::flavor::contract::FlavorContract {
        flavor_id: PROBE_FLAVOR_ID,
        ordinal: 90,
        schemas: PROBE_SCHEMAS,
        state_surfaces: &[],
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
        projection: proxima_core::flavor::contract::ProjectionDecl::None {
            why: "the probe flavor declares no search surface",
        },
    };

/// The contract for the malformed probe: listenable, but with no
/// `json_schema()`. Registering it must fail freeze.
pub static SCHEMALESS_PROBE_FLAVOR: proxima_core::flavor::contract::FlavorContract =
    proxima_core::flavor::contract::FlavorContract {
        flavor_id: PROBE_FLAVOR_ID,
        ordinal: 91,
        schemas: SCHEMALESS_PROBE_SCHEMAS,
        state_surfaces: &[],
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
        projection: proxima_core::flavor::contract::ProjectionDecl::None {
            why: "the probe flavor declares no search surface",
        },
    };

/// A registry carrying flavor #0 plus the listenable/non-listenable probe
/// pair, frozen.
///
/// # Panics
///
/// Panics if the fixture registry does not freeze, which would mean the
/// fixture itself is malformed.
#[must_use]
pub fn probe_registry() -> proxima_core::FlavorRegistryFrozen {
    let mut registry = proxima_core::FlavorRegistry::new();
    registry.add_contract_or_panic_for_tests(&PROBE_FLAVOR);
    registry.add_fact_schema_or_panic_for_tests::<ListenableProbeV1>();
    registry.add_fact_schema_or_panic_for_tests::<UnlistenableProbeV1>();
    registry.freeze_or_panic_for_tests()
}
