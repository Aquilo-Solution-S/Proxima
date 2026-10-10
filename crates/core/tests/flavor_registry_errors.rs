use std::sync::Arc;

use proxima_core::authz::OwnerResolver;
use proxima_core::error::ProtocolError;
use proxima_core::mcp::{
    McpActionArgSpec, McpTool, McpToolAudience, McpToolCtx, McpToolError, Replay, ToolEffect,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthzContext, FlavorDescriptor, FlavorProvenance, FlavorRegistry, FlavorRegistryError, Owner,
    SchemaId, SchemaVersion, ScopeKeyError, ScopeKeyPart,
};

#[derive(serde::Serialize, schemars::JsonSchema)]
struct EmptyOutput {}

struct OutputFixture<T>(std::marker::PhantomData<fn() -> T>);

impl<T> McpTool for OutputFixture<T>
where
    T: serde::Serialize + schemars::JsonSchema + Send + 'static,
{
    const NAME: &'static str = "proxima-test_output";
    const DESCRIPTION: &'static str = "output schema fixture";
    const EFFECT: Option<ToolEffect> = Some(ToolEffect::ReadOnly);
    type Args = EmptyArgs;
    type Output = T;

    fn call(
        _ctx: McpToolCtx,
        _args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async { Err(McpToolError::Other("schema-only fixture".to_owned())) })
    }
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
#[expect(dead_code, reason = "only the output schema is exercised")]
enum MixedOutput {
    Object { value: String },
    Scalar(String),
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
#[expect(dead_code, reason = "only the output schema is exercised")]
enum ObjectUnionOutput {
    Text { text: String },
    Count { count: u32 },
}

fn assert_output_registration_refused<T>()
where
    T: serde::Serialize + schemars::JsonSchema + Send + 'static,
{
    let mut registry = FlavorRegistry::new();
    let error = registry
        .try_add_mcp_tool::<OutputFixture<T>>("proxima-test")
        .expect_err("nonobject output types cannot register");
    assert!(matches!(
        error,
        FlavorRegistryError::InvalidToolOutputSchema {
            name: "proxima-test_output",
            ref message,
        } if message.contains("object")
    ));
    assert!(
        registry
            .freeze_or_panic_for_tests()
            .mcp_tool("proxima-test_output")
            .is_none()
    );
}

#[test]
fn registration_refuses_unit_sequence_scalar_mixed_and_unconstrained_outputs() {
    assert_output_registration_refused::<()>();
    assert_output_registration_refused::<Vec<String>>();
    assert_output_registration_refused::<String>();
    assert_output_registration_refused::<bool>();
    assert_output_registration_refused::<MixedOutput>();
    assert_output_registration_refused::<serde_json::Value>();
}

#[test]
fn registration_accepts_empty_objects_and_preserves_object_union_branches() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<OutputFixture<EmptyOutput>>("proxima-test")
        .unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    assert_eq!(
        frozen
            .mcp_tool("proxima-test_output")
            .unwrap()
            .output_schema["type"],
        "object"
    );
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<OutputFixture<ObjectUnionOutput>>("proxima-test")
        .unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    let schema = &frozen
        .mcp_tool("proxima-test_output")
        .unwrap()
        .output_schema;
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["anyOf"].as_array().unwrap().len(), 2);
    assert!(schema.get("properties").is_none());
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
struct EmptyArgs {}

struct DemoTool;

impl McpTool for DemoTool {
    const NAME: &'static str = "proxima-test_demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = EmptyOutput;

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async { Ok(EmptyOutput {}) })
    }
}

struct WrongPrefixTool;

impl McpTool for WrongPrefixTool {
    const NAME: &'static str = "wrong_demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = EmptyOutput;

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async { Ok(EmptyOutput {}) })
    }
}

struct ProviderUnsafeTool;

impl McpTool for ProviderUnsafeTool {
    const NAME: &'static str = "proxima-test/demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = EmptyOutput;

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async { Ok(EmptyOutput {}) })
    }
}

/// The effect every stub action below declares.
const STUB_EFFECT: ToolEffect = ToolEffect::Additive(Replay::NonIdempotent);

/// A stub's tool-level declaration: its effect when it registers flat, so
/// that `UndeclaredToolBehavior` (checked first at freeze) never stands in
/// for the dispatcher guard under test, and none when it dispatches, where
/// `DispatcherToolEffect` would.
const fn stub_tool_effect(specs: &[McpActionArgSpec]) -> Option<ToolEffect> {
    if specs.is_empty() {
        Some(STUB_EFFECT)
    } else {
        None
    }
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum TaggedArgs {
    Look {
        #[schemars(description = "what to look at")]
        id: String,
    },
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum WrongTagArgs {
    Look {
        #[schemars(description = "what to look at")]
        id: String,
    },
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum TwoActionArgs {
    Look {
        #[schemars(description = "what to look at")]
        id: String,
    },
    Touch {
        #[schemars(description = "what to touch")]
        id: String,
        #[schemars(description = "why")]
        note: String,
    },
}

/// An action whose name holds the scope-key delimiter: `tool:look:at` would
/// be read back as a leaf of the action `look`.
#[derive(schemars::JsonSchema, serde::Deserialize)]
#[serde(tag = "action")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum ColonActionArgs {
    #[serde(rename = "look:at")]
    LookAt {
        #[schemars(description = "what to look at")]
        id: String,
    },
}

/// An action name with a space, which no provider accepts in a tool name.
#[derive(schemars::JsonSchema, serde::Deserialize)]
#[serde(tag = "action")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum SpacedActionArgs {
    #[serde(rename = "look at")]
    LookAt {
        #[schemars(description = "what to look at")]
        id: String,
    },
}

macro_rules! stub_tool {
    ($tool:ident, $name:literal, $args:ty, $specs:expr) => {
        struct $tool;

        impl McpTool for $tool {
            const NAME: &'static str = $name;
            const DESCRIPTION: &'static str = "test";
            const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = $specs;
            const EFFECT: Option<ToolEffect> = stub_tool_effect($specs);
            type Args = $args;
            type Output = EmptyOutput;

            fn call(
                _ctx: McpToolCtx,
                _args: Self::Args,
            ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
                Box::pin(async { Ok(EmptyOutput {}) })
            }
        }
    };
}

stub_tool!(TaggedNoSpecsTool, "proxima-test_tagged", TaggedArgs, &[]);
/// A hand-written `JsonSchema` carrying the retired `x-proxima-actions`
/// extension. Only a tagged enum's derived schema makes a dispatcher; the
/// extension is an inert keyword now, so its specs describe nothing.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct LegacyExtensionArgs {
    value: String,
}

impl schemars::JsonSchema for LegacyExtensionArgs {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LegacyExtensionArgs".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["run"] },
                "value": { "type": "string" }
            },
            "required": ["action"],
            "additionalProperties": false,
            "x-proxima-actions": { "run": { "allowed_fields": ["value"] } }
        })
    }
}

stub_tool!(
    LegacyExtensionTool,
    "proxima-test_legacyext",
    LegacyExtensionArgs,
    &[McpActionArgSpec {
        action: "run",
        allowed_fields: &["value"],
        required_fields: &["value"],
        effect: ToolEffect::Additive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }]
);

stub_tool!(
    ColonActionTool,
    "proxima-test_colonaction",
    ColonActionArgs,
    &[McpActionArgSpec {
        action: "look:at",
        allowed_fields: &["id"],
        required_fields: &["id"],
        effect: ToolEffect::Additive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }]
);
stub_tool!(
    SpacedActionTool,
    "proxima-test_spacedaction",
    SpacedActionArgs,
    &[McpActionArgSpec {
        action: "look at",
        allowed_fields: &["id"],
        required_fields: &["id"],
        effect: ToolEffect::Additive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }]
);

stub_tool!(
    WrongTagTool,
    "proxima-test_wrongtag",
    WrongTagArgs,
    &[McpActionArgSpec {
        action: "look",
        allowed_fields: &["id"],
        required_fields: &["id"],
        effect: ToolEffect::Additive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }]
);
stub_tool!(
    FlatWithSpecsTool,
    "proxima-test_flatspecs",
    EmptyArgs,
    &[McpActionArgSpec {
        action: "look",
        allowed_fields: &[],
        required_fields: &[],
        effect: ToolEffect::Additive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }]
);
stub_tool!(
    DriftedActionsTool,
    "proxima-test_driftactions",
    TwoActionArgs,
    &[McpActionArgSpec {
        action: "look",
        allowed_fields: &["id"],
        required_fields: &["id"],
        effect: ToolEffect::Additive(Replay::NonIdempotent),
        audience: McpToolAudience::Shared,
    }]
);
stub_tool!(
    DriftedFieldsTool,
    "proxima-test_driftfields",
    TwoActionArgs,
    &[
        McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "touch",
            allowed_fields: &["id"],
            required_fields: &["id", "note"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
    ]
);
stub_tool!(
    ReadOnlyDispatcherTool,
    "proxima-test_readonlydispatch",
    TaggedArgs,
    &[McpActionArgSpec {
        action: "look",
        allowed_fields: &["id"],
        required_fields: &["id"],
        effect: ToolEffect::ReadOnly,
        audience: McpToolAudience::Shared,
    }]
);
// Two specs for one action, with identical field lists so the field-set loop
// has nothing to report either: only counting the specs catches this.
stub_tool!(
    DuplicateSpecsTool,
    "proxima-test_dupspecs",
    TaggedArgs,
    &[
        McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
        McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            effect: ToolEffect::Additive(Replay::NonIdempotent),
            audience: McpToolAudience::Shared,
        },
    ]
);

#[derive(Debug)]
struct TestResolver;

impl OwnerResolver for TestResolver {
    fn resolve(&self, _authz: &AuthzContext, requested: &Owner) -> Result<Owner, ProtocolError> {
        Ok(*requested)
    }
}

#[test]
fn duplicate_schema_is_typed_freeze_error() {
    let schema_id = SchemaId::new("proxima-test/duplicate".to_string());
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_opaque_schema(
            schema_id.clone(),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        )
        .unwrap();
    registry
        .try_add_opaque_schema(
            schema_id.clone(),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        )
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateSchema {
            schema_id: ref id,
            schema_version,
            kind: PayloadKind::CitedObject,
        } if id == &schema_id && schema_version == SchemaVersion::new(1)
    ));
}

#[test]
fn opaque_memory_and_goal_registration_is_a_typed_error() {
    for kind in [
        PayloadKind::Fact,
        PayloadKind::Abstraction,
        PayloadKind::Perspective,
        PayloadKind::Goal,
    ] {
        let schema_id = SchemaId::new(format!("proxima-test/opaque-{kind:?}"));
        let err = FlavorRegistry::new()
            .try_add_opaque_schema(schema_id.clone(), SchemaVersion::new(1), kind)
            .expect_err("only citation schemas may be opaque");
        assert!(matches!(
            err,
            FlavorRegistryError::OpaqueSchemaKind {
                schema_id: ref actual_id,
                schema_version,
                kind: actual_kind,
            } if actual_id == &schema_id
                && schema_version == SchemaVersion::new(1)
                && actual_kind == kind
        ));
    }
}

#[test]
fn duplicate_tool_is_typed_freeze_error() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<DemoTool>("proxima-test")
        .unwrap();
    registry
        .try_add_mcp_tool::<DemoTool>("proxima-test")
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateTool {
            name: "proxima-test_demo"
        }
    ));
}

#[test]
fn duplicate_flavor_is_typed_freeze_error() {
    let descriptor = FlavorDescriptor {
        flavor_id: "proxima-test".to_string(),
        display_name: "Proxima Test".to_string(),
        package_version: "0.0.0".to_string(),
        author: None,
        provenance: FlavorProvenance::Builtin,
    };
    let mut registry = FlavorRegistry::new();
    registry.try_add_flavor(descriptor.clone()).unwrap();
    registry.try_add_flavor(descriptor).unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateFlavor { flavor_id } if flavor_id == "proxima-test"
    ));
}

#[test]
fn duplicate_owner_resolver_is_typed_add_error() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_set_owner_resolver(Arc::new(TestResolver))
        .unwrap();

    let err = registry
        .try_set_owner_resolver(Arc::new(TestResolver))
        .unwrap_err();
    assert_eq!(err, FlavorRegistryError::DuplicateOwnerResolver);
}

#[test]
fn invalid_capability_tag_is_typed_add_error() {
    let schema_id = SchemaId::new("proxima-test/fact".to_string());
    let mut registry = FlavorRegistry::new();

    let err = registry
        .try_add_schema_capability_tags(
            PayloadKind::Fact,
            schema_id.clone(),
            SchemaVersion::new(1),
            ["NotValid"],
        )
        .unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidCapabilityTag {
            schema_id: ref id,
            schema_version,
            kind: PayloadKind::Fact,
            tag,
            ..
        } if id == &schema_id && schema_version == SchemaVersion::new(1) && tag == "NotValid"
    ));
}

#[test]
fn invalid_tool_names_are_typed_add_errors() {
    let mut registry = FlavorRegistry::new();
    let err = registry
        .try_add_mcp_tool::<WrongPrefixTool>("proxima-test")
        .unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidToolName {
            name: "wrong_demo",
            ..
        }
    ));

    let err = registry
        .try_add_mcp_tool::<ProviderUnsafeTool>("proxima-test")
        .unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidToolName {
            name: "proxima-test/demo",
            ..
        }
    ));
}

#[test]
fn unregistered_schema_capability_tags_are_typed_freeze_error() {
    let schema_id = SchemaId::new("proxima-test/missing".to_string());
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_schema_capability_tags(
            PayloadKind::Fact,
            schema_id.clone(),
            SchemaVersion::new(1),
            ["actor"],
        )
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::UnregisteredSchemaCapabilityTags {
            schema_id: ref id,
            schema_version,
            kind: PayloadKind::Fact,
        } if id == &schema_id && schema_version == SchemaVersion::new(1)
    ));
}

mod bad_opaque_prefix_flavor {
    proxima_core::proxima_flavor! {
        name = "proxima-test",
        display_name = "Proxima Test Bad Opaque",
        fact_schemas = [],
        abstraction_schemas = [],
        perspective_schemas = [],
        goal_schemas = [],
        opaque_cited_object_schemas = ["wrong-prefix/blob-v1"],
        opaque_citation_mapping_schemas = [],
        mcp_tools = [],
    }
}

/// Register one stub and try to seal.
fn freeze_error<T: McpTool>() -> FlavorRegistryError {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<T>("proxima-test")
        .expect("stub registration is valid");
    registry
        .try_freeze()
        .expect_err("an inconsistent dispatcher must not seal")
}

fn assert_invalid_argument_schema<T: McpTool>(needle: &str) {
    let err = freeze_error::<T>();
    assert!(
        matches!(err, FlavorRegistryError::InvalidActionSpecs { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains(needle), "{err}");
}

#[test]
fn a_hand_written_actions_extension_is_not_a_dispatcher() {
    assert_invalid_argument_schema::<LegacyExtensionTool>("not an internally tagged enum");
}

/// An internally tagged `Args` is what makes a client see a dispatcher —
/// the schema pass derives a dispatcher schema from it unconditionally. With
/// no `ACTION_ARG_SPECS` nothing enumerates the actions, so the scope gate
/// falls back to whole-tool grants and arguments are validated against every
/// variant's fields merged together. Boot is where that is caught.
#[test]
fn a_tagged_args_tool_without_action_specs_cannot_be_frozen() {
    let err = freeze_error::<TaggedNoSpecsTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::DispatcherWithoutActionSpecs {
                name: "proxima-test_tagged"
            }
        ),
        "got {err:?}",
    );
    assert!(err.to_string().contains("ACTION_ARG_SPECS"), "{err}");
}

/// The discriminator is a contract, not a preference: `ToolScope` keys are
/// `"{tool}:{action}"`, the gate and `validate_action_args` read
/// `args["action"]`, and the REST narrowed route injects `"action"`. A
/// dispatcher tagged on anything else is enumerated correctly and then
/// gated, validated, and routed as if it had no actions at all.
#[test]
fn a_dispatcher_tagged_on_something_other_than_action_cannot_be_frozen() {
    let err = freeze_error::<WrongTagTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::InvalidActionSpecs {
                name: "proxima-test_wrongtag",
                ..
            }
        ),
        "got {err:?}",
    );
    assert!(err.to_string().contains("must tag on `action`"), "{err}");
}

/// The other direction: specs on a tool whose `Args` is a plain struct.
/// Nothing derives an action schema for it, so the specs describe a
/// dispatcher that does not exist and `validate_action_args` would demand an
/// `action` field the type cannot carry.
#[test]
fn specs_without_a_tagged_args_type_cannot_be_frozen() {
    let err = freeze_error::<FlatWithSpecsTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::InvalidActionSpecs {
                name: "proxima-test_flatspecs",
                ..
            }
        ),
        "got {err:?}",
    );
    assert!(
        err.to_string().contains("not an internally tagged enum"),
        "{err}",
    );
}

/// A variant added to the enum and not to the specs: the client is told the
/// action exists, and the gate refuses it as unknown.
#[test]
fn a_dispatcher_whose_specs_drift_from_its_schema_cannot_be_frozen() {
    let err = freeze_error::<DriftedActionsTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::InvalidActionSpecs {
                name: "proxima-test_driftactions",
                ..
            }
        ),
        "got {err:?}",
    );
    let rendered = err.to_string();
    assert!(rendered.contains("look"), "{rendered}");
    assert!(rendered.contains("touch"), "{rendered}");
}

/// The action set agrees and the field sets do not — a field serde will
/// happily deserialize that `validate_action_args` rejects before it gets
/// the chance.
#[test]
fn a_dispatcher_whose_field_sets_drift_cannot_be_frozen() {
    let err = freeze_error::<DriftedFieldsTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::InvalidActionSpecs {
                name: "proxima-test_driftfields",
                ..
            }
        ),
        "got {err:?}",
    );
    let rendered = err.to_string();
    assert!(rendered.contains("allowed_fields"), "{rendered}");
    assert!(rendered.contains("note"), "{rendered}");
}

/// An action name is a scope key part: one outside the grammar is refused
/// when the registry freezes, naming the tool and the reason, not at the
/// first palette build. The delimiter case is the one that would make
/// `tool:look:at` read back as something else.
#[test]
fn a_dispatcher_action_outside_the_key_grammar_cannot_be_frozen() {
    for (err, name) in [
        (
            freeze_error::<ColonActionTool>(),
            "proxima-test_colonaction",
        ),
        (
            freeze_error::<SpacedActionTool>(),
            "proxima-test_spacedaction",
        ),
    ] {
        assert!(
            matches!(&err, FlavorRegistryError::InvalidScopeKey { name: refused, .. } if *refused == name),
            "got {err:?}",
        );
        assert!(err.to_string().contains(name), "{err}");
    }
    let colon = freeze_error::<ColonActionTool>();
    assert!(
        matches!(
            colon,
            FlavorRegistryError::InvalidScopeKey {
                reason: ScopeKeyError::Delimiter(ScopeKeyPart::Action),
                ..
            }
        ),
        "got {colon:?}",
    );
}

/// Per-action annotations: the action spec, not the parent, answers
/// read versus write.
#[test]
fn a_read_only_flavor_dispatcher_with_per_action_effects_freezes() {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<ReadOnlyDispatcherTool>("proxima-test");
    let frozen = registry.try_freeze().expect("per-action behavior seals");
    let descriptor = frozen
        .mcp_tool("proxima-test_readonlydispatch")
        .expect("dispatcher registered");
    assert!(descriptor.action_is_read_only("look"));
}

/// Substrate dispatchers use the same descriptor-owned action contract.
#[test]
fn a_substrate_dispatcher_with_a_read_only_action_still_freezes() {
    FlavorRegistry::default()
        .try_freeze()
        .expect("the substrate tools registered by `FlavorRegistry::default` seal");
}

/// A `BTreeSet` cannot report this: the two specs collapse to one member and
/// the action set matches the derived one exactly. The second spec is dead
/// weight — `validate_action_args` and the scope gate both take the first
/// match — so whichever of the two the author meant to be the contract, one
/// of them silently is not.
#[test]
fn duplicate_action_names_in_specs_cannot_be_frozen() {
    let err = freeze_error::<DuplicateSpecsTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::InvalidActionSpecs {
                name: "proxima-test_dupspecs",
                ..
            }
        ),
        "got {err:?}",
    );
    assert!(err.to_string().contains("duplicate"), "{err}");
}

#[test]
fn schema_ingress_mismatch_is_typed_register_error() {
    let mut registry = FlavorRegistry::new();
    let err = bad_opaque_prefix_flavor::register(&mut registry).unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::SchemaIngressMismatch {
            schema_id,
            schema_version,
            kind: PayloadKind::CitedObject,
        } if schema_id == SchemaId::new("wrong-prefix/blob-v1".to_string())
            && schema_version == SchemaVersion::new(1)
    ));
}
