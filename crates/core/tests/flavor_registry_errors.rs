use std::sync::Arc;

use proxima_core::authz::OwnerResolver;
use proxima_core::error::ProtocolError;
use proxima_core::mcp::{McpActionArgSpec, McpTool, McpToolAnnotations, McpToolCtx, McpToolError};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthzContext, DependencySatisfactionRule, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError, MemoryId, MemoryInspectPort, Owner, SchemaId, SchemaVersion, StorageError,
};

#[derive(schemars::JsonSchema, serde::Deserialize)]
struct EmptyArgs {}

struct DemoTool;

impl McpTool for DemoTool {
    const NAME: &'static str = "proxima-test_demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

struct WrongPrefixTool;

impl McpTool for WrongPrefixTool {
    const NAME: &'static str = "wrong_demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

struct ProviderUnsafeTool;

impl McpTool for ProviderUnsafeTool {
    const NAME: &'static str = "proxima-test/demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

/// The behaviour declaration every stub below shares, so that
/// `UndeclaredToolBehavior` (checked first at freeze) never stands in for
/// the dispatcher guard under test.
const STUB_ANNOTATIONS: Option<McpToolAnnotations> =
    Some(McpToolAnnotations::new().read_only(false).open_world(false));

/// The same declaration with the read/write answer flipped, for the one stub
/// whose tool-level `read_only` is the subject.
const READ_ONLY_ANNOTATIONS: Option<McpToolAnnotations> =
    Some(McpToolAnnotations::new().read_only(true).open_world(false));

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

macro_rules! stub_tool {
    ($tool:ident, $name:literal, $args:ty, $specs:expr) => {
        stub_tool!($tool, $name, $args, $specs, STUB_ANNOTATIONS);
    };
    ($tool:ident, $name:literal, $args:ty, $specs:expr, $annotations:expr) => {
        struct $tool;

        impl McpTool for $tool {
            const NAME: &'static str = $name;
            const DESCRIPTION: &'static str = "test";
            const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = $specs;
            const ANNOTATIONS: Option<McpToolAnnotations> = $annotations;
            type Args = $args;
            type Output = ();

            fn call(
                _ctx: McpToolCtx,
                _args: Self::Args,
            ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
                Box::pin(async { Ok(()) })
            }
        }
    };
}

stub_tool!(TaggedNoSpecsTool, "proxima-test_tagged", TaggedArgs, &[]);
stub_tool!(
    WrongTagTool,
    "proxima-test_wrongtag",
    WrongTagArgs,
    &[McpActionArgSpec {
        action: "look",
        allowed_fields: &["id"],
        required_fields: &["id"],
        annotations: None,
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
        annotations: None,
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
        annotations: None,
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
            annotations: None,
        },
        McpActionArgSpec {
            action: "touch",
            allowed_fields: &["id"],
            required_fields: &["id", "note"],
            annotations: None,
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
        annotations: READ_ONLY_ANNOTATIONS,
    }],
    READ_ONLY_ANNOTATIONS
);
stub_tool!(
    SilentActionDispatcherTool,
    "proxima-test_silentaction",
    TaggedArgs,
    &[McpActionArgSpec {
        action: "look",
        allowed_fields: &["id"],
        required_fields: &["id"],
        annotations: None,
    }],
    READ_ONLY_ANNOTATIONS
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
            annotations: None,
        },
        McpActionArgSpec {
            action: "look",
            allowed_fields: &["id"],
            required_fields: &["id"],
            annotations: None,
        },
    ]
);

/// A hand-written `JsonSchema` — the only way to reach the malformed-extension
/// branch. The derive path runs the flattener, which writes `x-proxima-actions`
/// as an object or not at all.
#[derive(serde::Deserialize)]
struct BogusExtensionArgs {}

impl schemars::JsonSchema for BogusExtensionArgs {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "BogusExtensionArgs".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {},
            "x-proxima-actions": "bogus",
        })
    }
}

// Empty specs on purpose: this is the shape that used to seal — the extension
// was read with `as_object()`, so a non-object one was indistinguishable from
// no extension at all and the tool passed as flat.
stub_tool!(
    BogusExtensionTool,
    "proxima-test_bogusext",
    BogusExtensionArgs,
    &[]
);

#[derive(Debug)]
struct TestRule(&'static str);

#[async_trait::async_trait]
impl DependencySatisfactionRule for TestRule {
    fn target_schema_id(&self) -> &'static str {
        self.0
    }

    async fn is_satisfied(
        &self,
        _storage: &dyn MemoryInspectPort,
        _owner: &Owner,
        _dependency_memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }
}

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
fn duplicate_dependency_rule_is_typed_freeze_error() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_dependency_satisfaction_rule("proxima-test/fact", Arc::new(TestRule("x")))
        .unwrap();
    registry
        .try_add_dependency_satisfaction_rule("proxima-test/fact", Arc::new(TestRule("x")))
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateDependencyRule { schema_id }
            if schema_id == "proxima-test/fact"
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

/// An internally tagged `Args` is what makes a client see a dispatcher —
/// the schema pass stamps `x-proxima-actions` onto it unconditionally. With
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

/// Per-action annotations remove the reason for the old flavor-only freeze
/// prohibition: the action spec, not the parent, answers read versus write.
#[test]
fn a_read_only_flavor_dispatcher_with_per_action_annotations_freezes() {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<ReadOnlyDispatcherTool>("proxima-test");
    let frozen = registry.try_freeze().expect("per-action behavior seals");
    let descriptor = frozen
        .mcp_tool("proxima-test_readonlydispatch")
        .expect("dispatcher registered");
    assert!(descriptor.action_is_read_only("look"));
}

#[test]
fn missing_action_annotations_freeze_as_write_without_inheriting_the_parent() {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<SilentActionDispatcherTool>("proxima-test");
    let frozen = registry
        .try_freeze()
        .expect("missing behavior fails closed");
    let descriptor = frozen
        .mcp_tool("proxima-test_silentaction")
        .expect("dispatcher registered");
    assert!(!descriptor.action_is_read_only("look"));
    assert!(!descriptor.is_read_only());
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

/// `x-proxima-actions` present but not an object is not the same answer as
/// absent. Read as absent — which `as_object()` did — a hand-written schema
/// carrying a dispatcher-shaped extension sealed as a flat tool.
#[test]
fn a_non_object_actions_extension_cannot_be_frozen() {
    let err = freeze_error::<BogusExtensionTool>();
    assert!(
        matches!(
            err,
            FlavorRegistryError::InvalidActionSpecs {
                name: "proxima-test_bogusext",
                ..
            }
        ),
        "got {err:?}",
    );
    let rendered = err.to_string();
    assert!(
        rendered.contains("malformed `x-proxima-actions`"),
        "{rendered}",
    );
    assert!(rendered.contains("bogus"), "{rendered}");
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
