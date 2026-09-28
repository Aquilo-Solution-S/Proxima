//! The `AsyncAPI` export against the three things it describes (issue #388):
//! the official `AsyncAPI` 3.0.0 schema, the events capture seals, and the
//! subjects the publisher sends them on.
//!
//! `fixtures/asyncapi-3.0.0.schema.json` is `schemas/3.0.0.json` from
//! asyncapi/spec-json-schemas at tag v6.11.1 (Apache-2.0), unmodified; see
//! `fixtures/README.md`.

#[allow(dead_code)]
mod common;

use std::num::NonZeroU32;
use std::time::Duration;

use boon::{Compiler, Draft, SchemaIndex, Schemas};
use proxima_core::flavor::contract::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::publication::{
    PublicationDraft, PublicationExtensions, PublicationLimits, PublicationPlan, SealedPublication,
};
use proxima_core::storage_ports::publication::{PublicationOutboxPort, PublisherId};
use proxima_core::test_fixtures::{ListenableProbeV1, PROBE_FLAVOR, UnlistenableProbeV1};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    FactPayload, FlavorRegistry, FlavorRegistryFrozen, Owner, OwnerRef, SchemaVersion, UserId,
};
use proxima_outbox_nats::{
    ASYNCAPI_VERSION, AsyncApiError, AsyncApiInfo, CONTENT_TYPE_CLOUDEVENTS,
    DEFAULT_SUBJECT_PREFIX, asyncapi_document, subject_for, type_token,
};
use proxima_storage_pg::test_fixtures::fresh_pg;
use serde_json::{Value, json};
use uuid::Uuid;

const ASYNCAPI_SCHEMA: &str = include_str!("fixtures/asyncapi-3.0.0.schema.json");
const ASYNCAPI_SCHEMA_ID: &str = "http://asyncapi.com/definitions/3.0.0/asyncapi.json";
const DOCUMENT_URL: &str = "https://proxima.test/asyncapi.json";

// A second listenable schema in schemars' default shape: a `$schema`, and
// a nested type behind `"$ref": "#/$defs/Step"` that only resolves once the
// export rebases it. (Plain comments: a doc comment would become the
// schema's `description`.)
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct StepFinishedV1 {
    run: String,
    step: Step,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct Step {
    name: String,
    attempt: u32,
}

impl FactPayload for StepFinishedV1 {
    const SCHEMA_ID: &'static str = "probe2/step-finished-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.run.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.run.clone()
    }

    fn json_schema() -> Option<Value> {
        Some(serde_json::to_value(schemars::schema_for!(Self)).expect("the schema serializes"))
    }
}

/// `probe2` rather than a descriptive name: `probe2/…` sorts AFTER
/// `probe/…` by schema id but BEFORE it by type token (`2` < `_`), so the
/// ordering test can tell the two keys apart.
static STEP_FLAVOR: FlavorContract = FlavorContract {
    flavor_id: "probe2",
    ordinal: 92,
    schemas: &[SchemaContract {
        id: SchemaRef::new("probe2", "step-finished", 1),
        kind: PayloadKind::Fact,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "an AsyncAPI fixture is not a retrievable surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "an AsyncAPI fixture is not retrievable content",
        },
        transfer: TransferRule::RetainAtSource {
            why: "an AsyncAPI fixture owns no rows to move",
        },
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "the AsyncAPI fixture declares no search surface",
    },
};

/// Two listenable schemas and one that is not, registered and frozen the
/// way a host's bundles are.
fn registry() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_contract(&PROBE_FLAVOR)
        .expect("probe contract");
    registry
        .try_add_fact_schema::<ListenableProbeV1>()
        .expect("listenable probe");
    registry
        .try_add_fact_schema::<UnlistenableProbeV1>()
        .expect("unlistenable probe");
    registry
        .try_add_contract(&STEP_FLAVOR)
        .expect("step contract");
    registry
        .try_add_fact_schema::<StepFinishedV1>()
        .expect("step schema");
    registry.try_freeze().expect("the fixture registry freezes")
}

fn info() -> AsyncApiInfo {
    AsyncApiInfo::new("asyncapi-test-host", "1.2.3")
}

fn document(registry: &FlavorRegistryFrozen) -> Value {
    asyncapi_document(registry, &info(), DEFAULT_SUBJECT_PREFIX).expect("the document builds")
}

fn official_asyncapi_schema() -> (Schemas, SchemaIndex) {
    let mut compiler = Compiler::new();
    compiler
        .add_resource(
            ASYNCAPI_SCHEMA_ID,
            serde_json::from_str(ASYNCAPI_SCHEMA).expect("the fixture is JSON"),
        )
        .expect("the fixture loads");
    let mut schemas = Schemas::new();
    let index = compiler
        .compile(ASYNCAPI_SCHEMA_ID, &mut schemas)
        .expect("the official schema compiles");
    (schemas, index)
}

/// One message's payload schema, resolved inside the whole document the way
/// an `AsyncAPI` reader resolves it, under the draft-07 base `AsyncAPI`
/// Schema Objects extend, with formats asserted.
fn payload_schema(document: &Value, message_key: &str) -> (Schemas, SchemaIndex) {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V7);
    compiler.enable_format_assertions();
    compiler
        .add_resource(DOCUMENT_URL, document.clone())
        .expect("the document loads");
    let mut schemas = Schemas::new();
    let index = compiler
        .compile(
            &format!("{DOCUMENT_URL}#/components/messages/{message_key}/payload"),
            &mut schemas,
        )
        .expect("the payload schema compiles");
    (schemas, index)
}

fn message_key<F: FactPayload>() -> String {
    format!("{}.v{}", type_token(F::SCHEMA_ID), F::SCHEMA_VERSION)
}

fn owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

/// The envelope capture seals for `data`, as JSON.
fn sealed<F: FactPayload>(data: &F, extensions: PublicationExtensions) -> Value {
    let plan = PublicationPlan::new(
        PublicationDraft::new(
            F::schema_id(),
            SchemaVersion::new(F::SCHEMA_VERSION),
            common::source(),
            owner(),
            Some("trusted/runner".to_owned()),
            extensions,
            serde_json::to_value(data).expect("the payload serializes"),
        ),
        PublicationLimits::default(),
    );
    let sealed = SealedPublication::seal(&plan, Uuid::now_v7()).expect("the envelope seals");
    serde_json::from_slice(&sealed.bytes).expect("the envelope is JSON")
}

fn step() -> StepFinishedV1 {
    StepFinishedV1 {
        run: "run-7".to_owned(),
        step: Step {
            name: "compile".to_owned(),
            attempt: 2,
        },
    }
}

#[test]
fn the_document_is_valid_asyncapi_3_0_0() {
    let (schemas, index) = official_asyncapi_schema();
    let document = document(&registry());
    if let Err(error) = schemas.validate(&document, index) {
        panic!("{error:#}\n{document:#}");
    }
    assert_eq!(document["asyncapi"], ASYNCAPI_VERSION);
    assert_eq!(
        document["info"],
        json!({ "title": "asyncapi-test-host", "version": "1.2.3" })
    );
    assert!(
        document.get("servers").is_none(),
        "topology is deployment's"
    );
}

#[test]
fn a_registry_without_a_listenable_schema_yields_a_valid_empty_document() {
    let registry = FlavorRegistry::new()
        .try_freeze()
        .expect("flavor #0 freezes");
    assert_eq!(registry.listenable_schema_ids().count(), 0);
    let document = document(&registry);
    let (schemas, index) = official_asyncapi_schema();
    if let Err(error) = schemas.validate(&document, index) {
        panic!("{error:#}\n{document:#}");
    }
    assert_eq!(document["channels"], json!({}));
    assert_eq!(document["operations"], json!({}));
    assert_eq!(document["components"]["messages"], json!({}));
}

#[test]
fn only_listenable_schemas_become_channels_and_messages() {
    let registry = registry();
    assert!(
        registry
            .schemas()
            .iter()
            .any(|schema| schema.schema_id.as_str() == UnlistenableProbeV1::SCHEMA_ID),
        "the non-listenable schema is registered, so its absence below means something"
    );
    let document = document(&registry);
    let keys = |value: &Value| -> Vec<String> {
        value.as_object().expect("a map").keys().cloned().collect()
    };
    // Schema-id order: `probe/…` before `probe2/…`, although their type
    // tokens sort the other way round.
    let channels = vec![
        type_token(ListenableProbeV1::SCHEMA_ID),
        type_token(StepFinishedV1::SCHEMA_ID),
    ];
    assert!(
        channels[1] < channels[0],
        "the fixture must tell the orders apart"
    );
    assert_eq!(keys(&document["channels"]), channels);
    assert_eq!(keys(&document["operations"]), channels);
    assert_eq!(
        keys(&document["components"]["messages"]),
        [
            message_key::<ListenableProbeV1>(),
            message_key::<StepFinishedV1>()
        ]
    );
    let text = document.to_string();
    assert!(!text.contains(UnlistenableProbeV1::SCHEMA_ID));
    assert!(!text.contains(&type_token(UnlistenableProbeV1::SCHEMA_ID)));

    let channel = &document["channels"][&channels[0]];
    assert_eq!(
        channel["address"],
        format!(
            "{DEFAULT_SUBJECT_PREFIX}.{{ownerKind}}.{{ownerId}}.{}",
            channels[0]
        )
    );
    assert_eq!(
        channel["parameters"]["ownerKind"]["enum"],
        json!(["personal", "group"])
    );
    assert_eq!(
        document["operations"][&channels[0]],
        json!({
            "action": "send",
            "channel": { "$ref": format!("#/channels/{}", channels[0]) },
            "messages": [{
                "$ref": format!(
                    "#/channels/{}/messages/{}",
                    channels[0],
                    message_key::<ListenableProbeV1>()
                ),
            }],
        })
    );
    let message = &document["components"]["messages"][message_key::<ListenableProbeV1>()];
    assert_eq!(message["contentType"], CONTENT_TYPE_CLOUDEVENTS);
    assert_eq!(
        message["payload"]["properties"]["data"],
        ListenableProbeV1::json_schema().expect("registered"),
        "a schema with no local reference is embedded verbatim"
    );
}

#[test]
fn two_calls_on_the_same_registry_are_byte_identical() {
    let render = || serde_json::to_string_pretty(&document(&registry())).expect("renders");
    assert_eq!(render(), render());
}

#[test]
fn an_invalid_subject_prefix_is_refused() {
    for prefix in ["", "proxima.>", "proxima.*", ".proxima", "proxima..fact"] {
        assert_eq!(
            asyncapi_document(&registry(), &info(), prefix),
            Err(AsyncApiError::InvalidSubjectPrefix {
                value: prefix.to_owned()
            }),
            "{prefix:?}"
        );
    }
}

#[test]
fn the_payload_schema_holds_the_envelope_and_the_registered_data() {
    let document = document(&registry());
    let (schemas, index) = payload_schema(&document, &message_key::<StepFinishedV1>());
    let valid = sealed(&step(), PublicationExtensions::new());
    if let Err(error) = schemas.validate(&valid, index) {
        panic!("{error:#}\n{valid:#}");
    }

    let refused = |label: &str, mutate: &dyn Fn(&mut Value)| {
        let mut event = valid.clone();
        mutate(&mut event);
        assert!(
            schemas.validate(&event, index).is_err(),
            "{label} must be refused: {event:#}"
        );
    };
    // Reaches `Step` only through the rebased `#/$defs/Step` reference.
    refused("a nested field of the wrong type", &|event| {
        event["data"]["step"]["attempt"] = json!("two");
    });
    refused("another schema's type", &|event| {
        event["type"] = json!(ListenableProbeV1::SCHEMA_ID);
    });
    refused("another version's dataschema", &|event| {
        event["dataschema"] = json!("proxima://schema/probe2/step-finished-v1/2");
    });
    refused("an id that is not a Fact's", &|event| {
        event["id"] = json!("A:0190a0d0-0000-7000-8000-000000000000");
    });
    refused("a time that is not a date-time", &|event| {
        event["time"] = json!("yesterday");
    });
    refused("a missing owner", &|event| {
        event.as_object_mut().expect("a map").remove("proximaowner");
    });
    refused("another specversion", &|event| {
        event["specversion"] = json!("0.3");
    });
}

/// The schema admits exactly the extension attributes capture lets a host
/// bind: the name and value checks run on both sides and must agree.
#[test]
fn extension_attributes_follow_the_binding_rules() {
    let document = document(&registry());
    let (schemas, index) = payload_schema(&document, &message_key::<ListenableProbeV1>());
    let probe = ListenableProbeV1 {
        probe_id: Uuid::now_v7(),
        note: "note".to_owned(),
    };
    let base = sealed(&probe, PublicationExtensions::new());

    let names: [&str; 12] = [
        "workflowid",
        "a",
        "x1",
        &"x".repeat(20),
        &"x".repeat(21),
        "",
        "Upper",
        "under_score",
        "subject",
        "data_base64",
        "proxima",
        "proximafoo",
    ];
    let values = [
        json!("wf-7"),
        json!(""),
        json!("bell\u{7}"),
        json!("x".repeat(256)),
        json!("x".repeat(257)),
        json!(0),
        json!(i32::MIN),
        json!(i32::MAX),
        json!(i64::from(i32::MAX) + 1),
        json!(1.5),
        json!(true),
        json!(null),
        json!(["a"]),
        json!({ "a": 1 }),
    ];
    for name in names {
        for value in &values {
            let bound = extension_value(value)
                .is_some_and(|value| PublicationExtensions::new().with(name, value).is_ok());
            let mut event = base.clone();
            event
                .as_object_mut()
                .expect("a map")
                .insert(name.to_owned(), value.clone());
            let described = schemas.validate(&event, index).is_ok();
            assert_eq!(
                described, bound,
                "extension {name:?} = {value}: schema says {described}, capture says {bound}"
            );
        }
    }

    let bound = PublicationExtensions::new()
        .with("workflowid", "wf-7")
        .and_then(|set| set.with("attempt", 3))
        .and_then(|set| set.with("dryrun", true))
        .expect("valid extensions");
    let event = sealed(&probe, bound);
    if let Err(error) = schemas.validate(&event, index) {
        panic!("{error:#}\n{event:#}");
    }
}

/// The JSON value as the one extension type capture could bind it as.
fn extension_value(value: &Value) -> Option<proxima_core::publication::ExtensionValue> {
    use proxima_core::publication::ExtensionValue;
    match value {
        Value::String(text) => Some(ExtensionValue::String(text.clone())),
        Value::Bool(flag) => Some(ExtensionValue::Boolean(*flag)),
        Value::Number(number) => number
            .as_i64()
            .and_then(|number| i32::try_from(number).ok())
            .map(ExtensionValue::Integer),
        _ => None,
    }
}

/// A Fact admitted through the outbox, claimed the way the publisher claims
/// it: its envelope validates against its message, and the subject the
/// publisher sends it on is its channel address with the parameters filled.
#[tokio::test]
async fn a_captured_fact_matches_its_message_and_its_channel_address() {
    let (pg, _db) = fresh_pg("asyncapi").await;
    let owner = owner();
    common::register_owner(pg.pool_for_tests(), &owner).await;
    let extensions = PublicationExtensions::new()
        .with("workflowid", "wf-7")
        .and_then(|set| set.with("attempt", 3))
        .expect("valid extensions");
    common::capture_probe(
        &pg,
        owner,
        common::source(),
        extensions,
        None,
        "captured",
        None,
    )
    .await
    .expect("the listenable Fact is admitted");
    let claimed = pg
        .claim(
            &PublisherId::new("asyncapi-test").expect("publisher id"),
            NonZeroU32::MIN,
            Duration::from_mins(1),
        )
        .await
        .expect("the claim runs");
    let [record] = claimed.as_slice() else {
        panic!("one captured record, got {}", claimed.len());
    };

    let document = document(&registry());
    let channel = &document["channels"][type_token(&record.event_type)];
    let message_ref = channel["messages"]
        .as_object()
        .and_then(|messages| messages.get(&message_key::<ListenableProbeV1>()))
        .expect("the channel carries the record's message");
    let message_key = message_ref["$ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/components/messages/"))
        .expect("a component message reference");
    let (schemas, index) = payload_schema(&document, message_key);
    let envelope: Value = serde_json::from_slice(&record.envelope).expect("the envelope is JSON");
    if let Err(error) = schemas.validate(&envelope, index) {
        panic!("{error:#}\n{envelope:#}");
    }

    let owner_kind = record.owner_kind.as_str();
    assert!(
        channel["parameters"]["ownerKind"]["enum"]
            .as_array()
            .expect("an enum")
            .contains(&json!(owner_kind))
    );
    let filled = channel["address"]
        .as_str()
        .expect("an address")
        .replace("{ownerKind}", owner_kind)
        .replace("{ownerId}", &record.owner_id.to_string());
    assert_eq!(
        filled,
        subject_for(
            DEFAULT_SUBJECT_PREFIX,
            owner_kind,
            record.owner_id,
            &record.event_type
        ),
        "the publisher sends on the channel's address"
    );
    pg.pool_for_tests().close().await;
}
