//! The shared fixture vocabulary: a minimal flavor, the builders that
//! shape one, and the registration helpers that push it into a registry.
//!
//! Every refusal fixture in this tree is one edit away from a contract that
//! freezes, which is the point — a fixture that differs from the accepted
//! shape in two places proves only whichever check runs first.

use crate::flavor::contract::{
    CounterRule, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape,
    ProjectionDecl, Provenance, ResourceContract, SchemaContract, SchemaRef, SearchProjectionDecl,
    Surface, ToolContract, TransferRule,
};
use crate::verbs::schema::{PayloadKind, SchemaInfo};
use crate::{FlavorRegistry, SchemaId, SchemaVersion};

pub(super) const FIXTURE_FLAVOR: &str = "test-flavor";

pub(super) const fn contract(
    ordinal: u16,
    schemas: &'static [SchemaContract],
    tools: &'static [ToolContract],
    resources: &'static [ResourceContract],
) -> FlavorContract {
    FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
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
    }
}

pub(super) const fn schema(id: SchemaRef, transfer: TransferRule) -> SchemaContract {
    SchemaContract {
        id,
        kind: PayloadKind::CitedObject,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "a fixture, not a surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }
}

pub(super) const RESOURCE: ResourceContract = ResourceContract {
    uri_template: "proxima://fixture",
    path: "fixture",
    name: "proxima-fixture",
    title: "Fixture",
    description: "a resource no flavor may declare",
    scope_key: "resource:fixture",
    is_template: false,
    read_only: true,
    reads: &[],
};

/// Nothing declared at all: used for the two cases the *registrations*
/// have to disagree with.
pub(super) static EMPTY: FlavorContract = contract(7, &[], &[], &[]);
pub(super) static DUPLICATE_ORDINAL: FlavorContract = contract(0, &[], &[], &[]);
pub(super) static DECLARES_A_RESOURCE: FlavorContract = contract(7, &[], &[], &[RESOURCE]);
pub(super) static FOREIGN_SCHEMA: FlavorContract = contract(
    7,
    &[schema(
        SchemaRef::new("some-other-flavor", "thing", 1),
        TransferRule::StaysOnKey,
    )],
    &[],
    &[],
);
pub(super) static UNENFORCED_REFUSAL: FlavorContract = contract(
    7,
    &[schema(
        SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        TransferRule::NotTransferable {
            why: "says so and nothing else",
            enforced_by: &[],
        },
    )],
    &[],
    &[],
);
pub(super) static UNREGISTERED_SCHEMA: FlavorContract = contract(
    7,
    &[schema(
        SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
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
pub(super) static CITATION_WITH_A_SIDECAR: FlavorContract = contract(
    7,
    &[SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::CitationMapping,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "a fixture, not a surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
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
) -> FlavorContract {
    FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
        ordinal: 7,
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
    }
}

pub(super) const fn erase_fixture(
    surfaces: &'static [Surface],
    bespoke: &'static [&'static str],
) -> FlavorContract {
    FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
        ordinal: 7,
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
    }
}

pub(super) static UNREGISTERED_TOOL: FlavorContract = contract(
    7,
    &[],
    &[ToolContract {
        wire_name: "test_flavor_absent",
        actions: &[],
    }],
    &[],
);

/// The registration says nothing dispatches; the declaration names an
/// action. `actions` is a copy of `McpActionArgSpec::action`, and a copy
/// that has drifted is worse than no copy: palette scope keys are
/// `"<wire_name>:<action>"`, so the drifted list is read by people
/// composing palettes against actions the dispatcher will not route.
pub(super) static TOOL_ACTIONS_DISAGREE: FlavorContract = contract(
    8,
    &[],
    &[ToolContract {
        wire_name: "test_flavor_flat",
        actions: &["compose"],
    }],
    &[],
);

/// The flat fixture tool, named by its flavor's contract.
pub(super) static TOOL_AGREES: FlavorContract = contract(
    11,
    &[],
    &[ToolContract {
        wire_name: "test_flavor_flat",
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
    }];

/// An MCP tool registration under the fixture flavor, so a contract's tool
/// declaration has a registration to disagree with.
///
/// Flat unless `action_arg_specs` is non-empty. A dispatcher fixture carries
/// no `x-proxima-actions` extension, so it only reaches the checks that run
/// before `validate_dispatcher_action_specs` — which is where the behaviour
/// check sits.
pub(super) fn register_fixture_tool(
    registry: &mut FlavorRegistry,
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
        name: "test_flavor_flat",
        description: "a fixture's tool",
        origin: crate::mcp::McpToolOrigin::Flavor(FIXTURE_FLAVOR.to_owned()),
        produces_schema_ids: &[],
        args_schema: serde_json::json!({ "type": "object" }),
        output_schema: serde_json::json!({ "type": "object" }),
        action_arg_specs,
        argv_action_specs: &[],
        effect,
        audience: crate::mcp::McpToolAudience::Shared,
        call,
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

pub(super) fn register_fixture_schema(registry: &mut FlavorRegistry, name: &str, table: &str) {
    register_fixture_schema_of_kind(registry, name, table, PayloadKind::Fact);
}

pub(super) fn register_fixture_schema_of_kind(
    registry: &mut FlavorRegistry,
    name: &str,
    table: &str,
    kind: PayloadKind,
) {
    let schema_id = SchemaId::new(format!("{FIXTURE_FLAVOR}/{name}-v1"));
    let schema_version = SchemaVersion::new(1);
    registry.schemas.push(SchemaInfo {
        schema_id: schema_id.clone(),
        schema_version,
        kind,
        filter_keys: Vec::new(),
        sidecar_table: Some(table.to_owned()),
        natural_key_columns: Vec::new(),
        tombstone: None,
        has_typed_ingress: true,
        cited_object_schema: None,
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
        });
}

/// The two schemas every uniformity fixture declares.
pub(super) fn register_uniformity_schemas(registry: &mut FlavorRegistry) {
    register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
    register_fixture_schema(registry, "other", "test_flavor.other_v1");
}

/// A registration with no contract entry: consistent enough to reach
/// `validate_contracts` (opaque kinds are allowed to have no typed
/// ingress, and it declares no ingress entry to mismatch).
pub(super) fn registration_without_a_contract() -> SchemaInfo {
    SchemaInfo {
        schema_id: SchemaId::new(format!("{FIXTURE_FLAVOR}/thing-v1")),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::CitedObject,
        filter_keys: Vec::new(),
        sidecar_table: None,
        natural_key_columns: Vec::new(),
        tombstone: None,
        has_typed_ingress: false,
        cited_object_schema: None,
        listenable: false,
    }
}
