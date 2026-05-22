//! Proxima engine core.
pub mod approval;
pub mod auth;
pub mod canonical_json;
pub mod chat;
pub mod citations;
pub mod cursor;
pub mod dependency;
pub mod embedding_settings;
pub mod engine;
pub mod error;
pub mod flavor;
pub mod harness;
pub mod ids;
pub mod inference;
pub mod intervention;
pub mod llm;
pub mod mcp;
pub mod models;
pub mod outbox;
pub mod owner;
pub mod payload;
pub mod payload_contract;
pub mod personality;
pub mod relation;
pub mod secrets;
pub mod storage;
pub mod verbs;
pub mod wake;
pub mod workspace_run;

pub use approval::*;
pub use auth::*;
pub use canonical_json::canonical_json;
pub use chat::*;
pub use citations::*;
pub use cursor::*;
pub use dependency::*;
pub use embedding_settings::*;
pub use engine::*;
pub use error::*;
pub use flavor::*;
pub use ids::*;
pub use inference::{
    BindInferenceTierRequest, BindInferenceTierResponse, ChatGPTCodexConfig, InferenceTargetConfig,
    InferenceTargetKind, InferenceTargetRow, InferenceTierBindingRow, ListInferenceTargetsRequest,
    ListInferenceTierBindingsRequest, MistralChatConfig, OpenAIChatConfig, OpenAIResponsesConfig,
    RegisterInferenceTargetRequest, RegisterInferenceTargetResponse, RemoveInferenceTargetRequest,
    RemoveInferenceTargetResponse,
};
pub use intervention::*;
pub use llm::*;
pub use mcp::{
    Handle, HandleTable, McpAuthorContext, McpCallFn, McpTool, McpToolCtx, McpToolDescriptor,
    McpToolError, MemoryHandleClass, OutputMode,
};
pub use models::*;
pub use outbox::*;
pub use owner::*;
pub use payload::*;
pub use payload_contract::assert_no_serde_json_value_fields;
pub use personality::workspace::{
    WorkspaceFinalizeInput, WorkspaceOutcome, WorkspacePrepareInput, WorkspacePreparedRun,
    WorkspaceRunRecord, WorkspaceRunner, WorkspaceRunnerError,
};
pub use personality::*;
pub use relation::*;
pub use secrets::*;
pub use storage::*;
pub use workspace_run::*;

// Re-export verb modules for convenience.
pub use verbs::*;

// Re-export subscribe types explicitly for external use.
pub use verbs::schema::FlavorRegistryFrozen;
pub use verbs::subscribe::{ChangeEventStream, SubscribeRequest};

/// Expands to `&'static str` with the calling crate's name
/// prefixed. Per docs/08 §Schema namespacing.
///
/// `proxima_schema_id!("commit")` in a crate named
/// `proxima-code` evaluates to `"proxima-code/commit"`.
#[macro_export]
macro_rules! proxima_schema_id {
    ($short:literal) => {
        ::std::concat!(::std::env!("CARGO_PKG_NAME"), "/", $short)
    };
}

/// Build-time registration macro. v1 subset — supports
/// `fact_schemas`, `abstraction_schemas`, `perspective_schemas`,
/// `goal_schemas`, `edge_schemas`, `relations`, `mcp_tools`.
/// Expands to a
/// `pub fn register(registry: &mut FlavorRegistry)` that adds each
/// schema / relation.
///
/// Prefix enforcement is tiered. Schema, `mcp_tools`, and
/// `workspace_triggers` prefixes resolve to associated `const`s or
/// literals, so they are checked by a `const` assertion — a misprefix
/// fails the build. `relations` and `dependency_satisfaction_rules`
/// carry their prefix on a runtime expression (a `RelationDescriptor`
/// field, a trait-object method), so those prefixes are asserted in
/// `register` and fail at startup instead.
///
/// `edge_schemas` registers `EdgePayload` impls; `relations`
/// registers `RelationDescriptor` literals — typed relations
/// must reference an edge schema also listed in `edge_schemas`,
/// cross-checked at `FlavorRegistry::freeze`.
///
/// Build-time owns the *capability vocabulary* (`LlmCaps`,
/// `EmbedCaps`) and operator `requires` declarations; specific
/// `(vendor, model_id)` bindings are runtime configuration, not
/// flavor authorship. New models plug in at runtime.
///
/// Future verbs (sources, operators) land as the
/// underlying systems materialize. Reject unknown keys at expansion
/// time to keep authors honest.
///
/// ```ignore
/// proxima_flavor! {
///     name = "proxima-code",
///     fact_schemas = [ CommitV1, FileChangeV1 ],
///     edge_schemas = [ EdgeCallsV1 ],
///     relations = [ RelationDescriptor::typed(
///         "proxima-code/calls",
///         RelationClass::Structural,
///         SchemaRef::new(SchemaId::new("proxima-code/calls".into()),
///                        SchemaVersion::new(1)),
///     ) ],
/// }
/// ```
///
#[macro_export]
macro_rules! proxima_flavor {
    // Internal: register one schema-kind list, compile-checking each
    // `SCHEMA_ID` carries the flavor prefix. The check is a `const`
    // assertion — a misprefixed `SCHEMA_ID` fails the build, not the
    // first boot. Collapses what were five byte-identical per-kind arms
    // in the main rule below.
    (@schemas $registry:ident $name:literal $trait:ident $add:ident [ $($ty:ty),* $(,)? ]) => {
        $(
            const _: () = ::std::assert!(
                $crate::schema_id_has_prefix(
                    <$ty as $crate::$trait>::SCHEMA_ID,
                    ::std::concat!($name, "/"),
                ),
                ::std::concat!(
                    ::std::stringify!($trait), " SCHEMA_ID for ",
                    ::std::stringify!($ty),
                    " must start with \"", $name, "/\"",
                ),
            );
            $registry.$add::<$ty>();
        )*
    };
    (
        name = $name:literal
        $(, display_name = $display_name:literal)?
        $(, fact_schemas = [ $($fact:ty),* $(,)? ])?
        $(, abstraction_schemas = [ $($abs:ty),* $(,)? ])?
        $(, perspective_schemas = [ $($persp:ty),* $(,)? ])?
        $(, goal_schemas = [ $($goal:ty),* $(,)? ])?
        $(, edge_schemas = [ $($edge:ty),* $(,)? ])?
        $(, relations = [ $($rel:expr),* $(,)? ])?
        $(, mcp_tools = [ $($tool:ty),* $(,)? ])?
        $(, workspace_runner = $workspace_runner:ty)?
        $(, workspace_triggers = [ $($workspace_trigger_schema:literal),* $(,)? ])?
        $(, dependency_satisfaction_rules = [ $($dependency_rule:ty),* $(,)? ])?
        $(,)?
    ) => {
        /// Generated by `proxima_flavor!`. Composite binaries
        /// call this once per linked flavor at startup.
        pub fn register(registry: &mut $crate::FlavorRegistry) {
            // Used by the `relations` / `dependency_satisfaction_rules`
            // arms, whose prefix-bearing value is a runtime expression
            // and so cannot be checked at `const` time like schemas are.
            #[allow(dead_code)]
            const EXPECTED_PREFIX: &str = ::std::concat!($name, "/");
            {
                #[allow(unused_assignments, unused_mut)]
                let mut display_name: &str = $name;
                $(display_name = $display_name;)?
                let author: ::std::option::Option<::std::string::String> =
                    ::std::option_env!("CARGO_PKG_AUTHORS")
                        .filter(|s: &&str| !s.is_empty())
                        .map(|s: &str| {
                            s.split(':').next().unwrap_or(s).trim().to_string()
                        });
                registry.add_flavor($crate::FlavorDescriptor {
                    flavor_id: $name.to_string(),
                    display_name: display_name.to_string(),
                    package_version: ::std::env!("CARGO_PKG_VERSION").to_string(),
                    author,
                    provenance: $crate::FlavorProvenance::Builtin,
                });
            }
            $($crate::proxima_flavor!(@schemas registry $name
                FactPayload add_fact_schema [ $($fact),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                AbstractionPayload add_abstraction_schema [ $($abs),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                PerspectivePayload add_perspective_schema [ $($persp),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                GoalPayload add_goal_schema [ $($goal),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                EdgePayload add_edge_schema [ $($edge),* ]);)?
            $($(
                {
                    let descriptor: $crate::RelationDescriptor = $rel;
                    assert!(
                        descriptor.relation.starts_with(EXPECTED_PREFIX),
                        "RelationDescriptor relation {:?} does not start with crate prefix {:?}",
                        descriptor.relation, EXPECTED_PREFIX,
                    );
                    registry.add_relation(descriptor);
                }
            )*)?
            $($(
                {
                    const _: () = ::std::assert!(
                        $crate::schema_id_has_prefix(
                            <$tool as $crate::McpTool>::NAME,
                            ::std::concat!($name, "/"),
                        ),
                        ::std::concat!(
                            "McpTool::NAME for ", ::std::stringify!($tool),
                            " must start with \"", $name, "/\"",
                        ),
                    );
                    registry.add_mcp_tool::<$tool>($name);
                }
            )*)?
            $(
                {
                    use $crate::WorkspaceRunner;
                    let runner: ::std::sync::Arc<dyn WorkspaceRunner> =
                        ::std::sync::Arc::new(<$workspace_runner as ::std::default::Default>::default());
                    registry.add_workspace_runner($name, runner);
                }
            )?
            $($(
                {
                    const _: () = ::std::assert!(
                        $crate::schema_id_has_prefix(
                            $workspace_trigger_schema, ::std::concat!($name, "/"),
                        ) || $crate::schema_id_has_prefix(
                            $workspace_trigger_schema, "proxima-core/",
                        ),
                        ::std::concat!(
                            "workspace trigger schema ", $workspace_trigger_schema,
                            " must start with \"", $name, "/\" or \"proxima-core/\"",
                        ),
                    );
                    registry.add_workspace_trigger($workspace_trigger_schema);
                }
            )*)?
            $($(
                {
                    use $crate::DependencySatisfactionRule;
                    let rule: ::std::sync::Arc<dyn DependencySatisfactionRule> =
                        ::std::sync::Arc::new(<$dependency_rule as ::std::default::Default>::default());
                    let schema_id = rule.target_schema_id();
                    assert!(
                        schema_id.starts_with(EXPECTED_PREFIX) || schema_id.starts_with("proxima-core/"),
                        "DependencySatisfactionRule schema {:?} does not start with crate prefix {:?}",
                        schema_id, EXPECTED_PREFIX,
                    );
                    registry.add_dependency_satisfaction_rule(schema_id, rule);
                }
            )*)?
        }
    };
}
