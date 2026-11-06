//! The shared fixture vocabulary: a minimal spiice, the builders that
//! shape one, and the registration helpers that push it into a registry.
//!
//! Every refusal fixture in this tree is one edit away from a contract that
//! freezes, which is the point — a fixture that differs from the accepted
//! shape in two places proves only whichever check runs first.

use crate::spiice::contract::{
    CounterRule, EmbeddingRecipe, EraseRule, ExportRule, ForgetRule, KeyShape, ProjectionDecl,
    Provenance, ResourceContract, SchemaContract, SchemaRef, SearchProjectionDecl, SpiiceContract,
    Surface, ToolContract, TransferRule,
};
use crate::verbs::schema::{PayloadKind, SchemaInfo};
use crate::{SchemaId, SchemaVersion, SpiiceRegistry};

pub(super) const FIXTURE_SPIICE: &str = "test-spiice";

pub(in crate::spiice::freeze) const fn contract(
    ordinal: u16,
    schemas: &'static [SchemaContract],
    tools: &'static [ToolContract],
    resources: &'static [ResourceContract],
) -> SpiiceContract {
    SpiiceContract {
        retired_facts: &[],
        spiice_id: FIXTURE_SPIICE,
        ordinal,
        schemas,
        state_surfaces: &[],
        scopes: &[],
        kernel_surfaces: &[],
        tools,
        resources,
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
        projection: ProjectionDecl::None {
            why: "a fixture registry has no schema that is a search surface",
        },
        evolutions: &[],
    }
}

pub(in crate::spiice::freeze) const fn schema(
    id: SchemaRef,
    transfer: TransferRule,
) -> SchemaContract {
    SchemaContract {
        id,
        kind: PayloadKind::CitedObject,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "a fixture, not a surface",
            tag_column: None,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: crate::NaturalKeyDeclaration::EMPTY,
    }
}

pub(super) static TAG_WITHOUT_MEMORY_SURFACE: SpiiceContract = contract(
    7,
    &[SchemaContract {
        id: SchemaRef::new(FIXTURE_SPIICE, "thing", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("test_spiice.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "tag validation fixture",
            tag_column: Some("tags"),
        },
        embedding: EmbeddingRecipe::Never {
            why: "tag validation fixture",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: crate::NaturalKeyDeclaration::EMPTY,
    }],
    &[],
    &[],
);

pub(super) const RESOURCE: ResourceContract = ResourceContract {
    uri_template: "proxiima://fixture",
    path: "fixture",
    name: "proxiima-fixture",
    title: "Fixture",
    description: "a resource no spiice may declare",
    scope_key: "resource:fixture",
    is_template: false,
    read_only: true,
    reads: &[],
};

/// Nothing declared at all: used for the two cases the *registrations*
/// have to disagree with.
pub(super) static EMPTY: SpiiceContract = contract(7, &[], &[], &[]);
pub(super) static DUPLICATE_ORDINAL: SpiiceContract = contract(0, &[], &[], &[]);
pub(super) static DECLARES_A_RESOURCE: SpiiceContract = contract(7, &[], &[], &[RESOURCE]);
pub(super) static FOREIGN_SCHEMA: SpiiceContract = contract(
    7,
    &[schema(
        SchemaRef::new("some-other-spiice", "thing", 1),
        TransferRule::StaysOnKey,
    )],
    &[],
    &[],
);
pub(super) static UNENFORCED_REFUSAL: SpiiceContract = contract(
    7,
    &[schema(
        SchemaRef::new(FIXTURE_SPIICE, "thing", 1),
        TransferRule::NotTransferable {
            why: "says so and nothing else",
            enforced_by: &[],
        },
    )],
    &[],
    &[],
);
pub(super) static UNREGISTERED_SCHEMA: SpiiceContract = contract(
    7,
    &[schema(
        SchemaRef::new(FIXTURE_SPIICE, "thing", 1),
        TransferRule::StaysOnKey,
    )],
    &[],
    &[],
);
/// A citation payload that declared a table of its own.
///
/// The shared-blob dedupe arm repoints a citation at a new `blob` row,
/// and finds the columns to repoint by following foreign keys. This
/// table's `cited_object_id` would hold a `blob_id` with no FK saying
/// so, so the remap would walk past it and leave the rows pointing at
/// the wrong owner's blob after a transfer.
pub(super) static CITATION_WITH_A_SIDECAR: SpiiceContract = contract(
    7,
    &[SchemaContract {
        id: SchemaRef::new(FIXTURE_SPIICE, "thing", 1),
        kind: PayloadKind::CitedObject,
        sidecar_table: Some("test_spiice.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "a fixture, not a surface",
            tag_column: None,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: crate::NaturalKeyDeclaration::EMPTY,
    }],
    &[],
    &[],
);
/// The shape a fixture surface takes when the fixture is about the
/// erase: exportable and owned, so nothing but the erase rule is under
/// test.
pub(super) const fn state_surface(
    table: &'static str,
    key: KeyShape,
    erase: EraseRule,
    export: ExportRule,
) -> Surface {
    Surface {
        table,
        key,
        owner_column: Some("owner_id"),
        transfer: TransferRule::StaysOnKey,
        erase,
        export,
        forget: ForgetRule::Keep {
            why: "a fixture, not a memory",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a fixture contributes to no receipt",
        },
        completeness: None,
    }
}

/// `erase_fixture`'s twin for the transfer partition: the surfaces are
/// declared non-erasing so the erase check cannot fire first and mask
/// the case under test.
pub(super) const fn transfer_fixture(
    surfaces: &'static [Surface],
    bespoke: &'static [&'static str],
) -> SpiiceContract {
    transfer_fixture_at(7, surfaces, bespoke)
}

/// [`transfer_fixture`] at a chosen ordinal. A bespoke-leg mismatch is a
/// core-only question (only core may name legs), so those fixtures sit at
/// [`crate::spiice::contract::CORE_ORDINAL`].
pub(super) const fn transfer_fixture_at(
    ordinal: u16,
    surfaces: &'static [Surface],
    bespoke: &'static [&'static str],
) -> SpiiceContract {
    SpiiceContract {
        retired_facts: &[],
        spiice_id: FIXTURE_SPIICE,
        ordinal,
        schemas: &[],
        state_surfaces: surfaces,
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: bespoke,
        projection: ProjectionDecl::None {
            why: "a fixture registry has no schema that is a search surface",
        },
        evolutions: &[],
    }
}

pub(super) const fn erase_fixture(
    surfaces: &'static [Surface],
    bespoke: &'static [&'static str],
) -> SpiiceContract {
    erase_fixture_at(7, surfaces, bespoke)
}

/// [`erase_fixture`] at a chosen ordinal; see [`transfer_fixture_at`].
pub(super) const fn erase_fixture_at(
    ordinal: u16,
    surfaces: &'static [Surface],
    bespoke: &'static [&'static str],
) -> SpiiceContract {
    SpiiceContract {
        retired_facts: &[],
        spiice_id: FIXTURE_SPIICE,
        ordinal,
        schemas: &[],
        state_surfaces: surfaces,
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: bespoke,
        bespoke_transfer_legs: &[],
        projection: ProjectionDecl::None {
            why: "a fixture registry has no schema that is a search surface",
        },
        evolutions: &[],
    }
}

pub(super) static UNREGISTERED_TOOL: SpiiceContract = contract(
    7,
    &[],
    &[ToolContract {
        wire_name: "test_spiice_absent",
        actions: &[],
    }],
    &[],
);

/// The registration says nothing dispatches; the declaration names an
/// action. `actions` is a copy of `McpActionArgSpec::action`, and a copy
/// that has drifted is worse than no copy: palette scope keys are
/// `"<wire_name>:<action>"`, so the drifted list is read by people
/// composing palettes against actions the dispatcher will not route.
pub(super) static TOOL_ACTIONS_DISAGREE: SpiiceContract = contract(
    8,
    &[],
    &[ToolContract {
        wire_name: "test_spiice_flat",
        actions: &["compose"],
    }],
    &[],
);

/// The flat fixture tool, named by its spiice's contract.
pub(super) static TOOL_AGREES: SpiiceContract = contract(
    11,
    &[],
    &[ToolContract {
        wire_name: "test_spiice_flat",
        actions: &[],
    }],
    &[],
);

/// One action for a fixture dispatcher.
pub(super) const FIXTURE_ACTIONS: &[crate::mcp::McpActionArgSpec] =
    &[crate::mcp::McpActionArgSpec {
        action: "look",
        allowed_fields: &[],
        required_fields: &[],
        effect: crate::mcp::ToolEffect::ReadOnly,
        audience: crate::mcp::McpToolAudience::Shared,
        title: "Test action",
    }];

/// An MCP tool registration under the fixture spiice, so a contract's tool
/// declaration has a registration to disagree with.
///
/// Flat unless `action_arg_specs` is non-empty. A dispatcher fixture carries
/// no dispatcher schema, so it only reaches the checks that run
/// before `validate_dispatcher_action_specs` — which is where the behaviour
/// check sits.
pub(super) fn register_fixture_tool(
    registry: &mut SpiiceRegistry,
    action_arg_specs: &'static [crate::mcp::McpActionArgSpec],
    effect: Option<crate::mcp::ToolEffect>,
) {
    // Registrations live for the process; a leaked closure gives the
    // descriptor the `'static` call handle its field type demands.
    let call: crate::mcp::McpCallFn = Box::leak(Box::new(
        |_ctx: crate::mcp::McpToolCtx, _args: serde_json::Value| {
            Box::pin(async { Err(crate::mcp::McpToolError::Other("a fixture".to_owned())) })
                as futures::future::BoxFuture<'static, _>
        },
    ));
    registry.mcp_tools.push(crate::mcp::McpToolDescriptor {
        name: "test_spiice_flat",
        description: "a fixture's tool",
        origin: crate::mcp::McpToolOrigin::Spiice(FIXTURE_SPIICE.to_owned()),
        produces_schema_ids: &[],
        args_schema: serde_json::json!({ "type": "object" }),
        dispatcher_schema: None,
        output_schema: serde_json::json!({ "type": "object" }),
        action_arg_specs,
        argv_action_specs: &[],
        effect,
        audience: crate::mcp::McpToolAudience::Shared,
        call,
        title: "Test tool",
    });
}

/// A flat tool's effect that `validate_tools_declare_behavior` accepts, so
/// a fixture reaches the check it is about.
pub(super) const FIXTURE_EFFECT: Option<crate::mcp::ToolEffect> = Some(
    crate::mcp::ToolEffect::Additive(crate::mcp::Replay::NonIdempotent),
);

/// A fixture's ingress. Never called: the registries these fixtures build
/// are meant to be REFUSED, so nothing reaches a payload parser.
pub(super) fn fixture_ingress(
    _payload: &serde_json::Value,
) -> Result<crate::verbs::schema::ProtocolPayload, String> {
    Err("a fixture, not an ingress".to_owned())
}

pub(super) fn register_fixture_schema(registry: &mut SpiiceRegistry, name: &str, table: &str) {
    register_fixture_schema_with_key(
        registry,
        name,
        table,
        PayloadKind::Fact,
        crate::NaturalKeyDeclaration::EMPTY,
    );
}

pub(super) fn register_fixture_schema_of_kind(
    registry: &mut SpiiceRegistry,
    name: &str,
    table: &str,
    kind: PayloadKind,
) {
    register_fixture_schema_with_key(
        registry,
        name,
        table,
        kind,
        crate::NaturalKeyDeclaration::EMPTY,
    );
}

pub(super) fn register_fixture_schema_with_key(
    registry: &mut SpiiceRegistry,
    name: &str,
    table: &str,
    kind: PayloadKind,
    natural_key_declaration: crate::NaturalKeyDeclaration,
) {
    let schema_id = SchemaId::new(format!("{FIXTURE_SPIICE}/{name}-v1"));
    let schema_version = SchemaVersion::new(1);
    registry.schemas.push(SchemaInfo {
        schema_id: schema_id.clone(),
        schema_version,
        kind,
        filter_keys: Vec::new(),
        sidecar_table: Some(table.to_owned()),
        natural_key_columns: natural_key_declaration
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        tombstone: None,
        has_typed_ingress: true,
        listenable: false,
    });
    registry
        .protocol_ingress
        .push(crate::verbs::schema::ProtocolPayloadIngressEntry {
            schema_id,
            schema_version,
            kind,
            ingress: fixture_ingress,
            json_schema: None,
            natural_key_declaration,
        });
}

pub(super) use key_payloads::{
    DuplicateKeyFact, NaturalKeyFixtureFact, SameKeyDifferentKindFact, SameKeyDifferentTypeFact,
};

/// The two schemas every uniformity fixture declares.
pub(super) fn register_uniformity_schemas(registry: &mut SpiiceRegistry) {
    register_fixture_schema(registry, "thing", "test_spiice.thing_v1");
    register_fixture_schema(registry, "other", "test_spiice.other_v1");
}

/// A registration with no contract entry: consistent enough to reach
/// `validate_contracts` (opaque kinds are allowed to have no typed
/// ingress, and it declares no ingress entry to mismatch).
pub(super) fn registration_without_a_contract() -> SchemaInfo {
    SchemaInfo {
        schema_id: SchemaId::new(format!("{FIXTURE_SPIICE}/thing-v1")),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitedObject,
        filter_keys: Vec::new(),
        sidecar_table: None,
        natural_key_columns: Vec::new(),
        tombstone: None,
        has_typed_ingress: false,
        listenable: false,
    }
}

#[cfg(test)]
mod key_payloads {
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub(crate) struct NaturalKeyFixtureFact {
        pub(super) thing_key: String,
    }

    impl crate::FactPayload for NaturalKeyFixtureFact {
        const SCHEMA_ID: &'static str = "test-spiice/thing-v1";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            self.thing_key.as_bytes().to_vec()
        }
        fn render(&self) -> String {
            self.thing_key.clone()
        }
    }

    impl crate::PayloadProjection for NaturalKeyFixtureFact {
        const KEY_FIELDS: &'static [crate::KeyField<Self>] =
            &[crate::KeyField::text("thing_key", |payload| {
                &payload.thing_key
            })];
        fn display(&self) -> crate::PayloadDisplay {
            crate::PayloadDisplay {
                body: Some(self.thing_key.clone()),
                ..Default::default()
            }
        }
        fn durable(&self) -> Result<crate::DurablePayload, String> {
            let mut builder = crate::DurablePayloadBuilder::new();
            builder.field("thing_key", &self.thing_key)?;
            Ok(builder.finish())
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub(crate) struct SameKeyDifferentTypeFact {
        pub(super) thing_key: String,
    }

    impl crate::FactPayload for SameKeyDifferentTypeFact {
        const SCHEMA_ID: &'static str = "test-spiice/thing-v1";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            self.thing_key.as_bytes().to_vec()
        }
        fn render(&self) -> String {
            self.thing_key.clone()
        }
    }

    impl crate::PayloadProjection for SameKeyDifferentTypeFact {
        const KEY_FIELDS: &'static [crate::KeyField<Self>] =
            &[crate::KeyField::text("thing_key", |payload| {
                &payload.thing_key
            })];

        fn display(&self) -> crate::PayloadDisplay {
            crate::PayloadDisplay {
                body: Some(self.thing_key.clone()),
                ..Default::default()
            }
        }

        fn durable(&self) -> Result<crate::DurablePayload, String> {
            let mut builder = crate::DurablePayloadBuilder::new();
            builder.field("thing_key", &self.thing_key)?;
            Ok(builder.finish())
        }
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub(crate) struct SameKeyDifferentKindFact {
        pub(super) thing_key: i32,
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    pub(crate) struct DuplicateKeyFact {
        pub(super) first: String,
        pub(super) second: String,
    }

    impl crate::FactPayload for DuplicateKeyFact {
        const SCHEMA_ID: &'static str = "test-spiice/thing-v1";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            self.first.as_bytes().to_vec()
        }
        fn render(&self) -> String {
            self.first.clone()
        }
    }

    impl crate::PayloadProjection for DuplicateKeyFact {
        const KEY_FIELDS: &'static [crate::KeyField<Self>] = &[
            crate::KeyField::text("thing_key", |payload| &payload.first),
            crate::KeyField::text("thing_key", |payload| &payload.second),
        ];

        fn display(&self) -> crate::PayloadDisplay {
            crate::PayloadDisplay::default()
        }

        fn durable(&self) -> Result<crate::DurablePayload, String> {
            let mut builder = crate::DurablePayloadBuilder::new();
            builder.field("first", &self.first)?;
            builder.field("second", &self.second)?;
            Ok(builder.finish())
        }
    }

    impl crate::FactPayload for SameKeyDifferentKindFact {
        const SCHEMA_ID: &'static str = "test-spiice/thing-v1";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            self.thing_key.to_be_bytes().to_vec()
        }
        fn render(&self) -> String {
            self.thing_key.to_string()
        }
    }

    impl crate::PayloadProjection for SameKeyDifferentKindFact {
        const KEY_FIELDS: &'static [crate::KeyField<Self>] =
            &[crate::KeyField::i32("thing_key", |payload| {
                payload.thing_key
            })];

        fn display(&self) -> crate::PayloadDisplay {
            crate::PayloadDisplay {
                body: Some(self.thing_key.to_string()),
                ..Default::default()
            }
        }

        fn durable(&self) -> Result<crate::DurablePayload, String> {
            let mut builder = crate::DurablePayloadBuilder::new();
            builder.field("thing_key", &self.thing_key)?;
            Ok(builder.finish())
        }
    }
}
