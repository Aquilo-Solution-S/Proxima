use super::{PayloadKind, SchemaId, SchemaVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlavorRegistryError {
    DuplicateSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    DuplicateTool {
        name: &'static str,
    },
    DuplicateFlavor {
        flavor_id: String,
    },
    DuplicateDependencyRule {
        schema_id: String,
    },
    InvalidCapabilityTag {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
        tag: String,
        message: String,
    },
    InvalidToolName {
        name: &'static str,
        expected_prefix: String,
        message: String,
    },
    DuplicateOwnerResolver,
    SchemaIngressMismatch {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    UnregisteredSchemaCapabilityTags {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A registered MCP tool has no resolvable behaviour declaration, so the
    /// owner-role gate cannot tell a read from a write and has to assume a
    /// write. Substrate tools may answer through the core manifest; a flavor
    /// tool has only `ANNOTATIONS`.
    UndeclaredToolBehavior {
        name: &'static str,
    },
    /// A tool whose `Args` is an internally tagged enum — so its schema
    /// carries `x-proxima-actions` and MCP clients see a dispatcher —
    /// declared no `ACTION_ARG_SPECS`. Nothing then enumerates its
    /// actions: the scope gate falls back to whole-tool grants, the
    /// catalog lists none, REST serves no action route, and arguments are
    /// validated against every variant's fields merged together.
    DispatcherWithoutActionSpecs {
        name: &'static str,
    },
    /// A tool's `ACTION_ARG_SPECS` and its schemars-derived
    /// `x-proxima-actions` do not describe the same dispatcher.
    InvalidActionSpecs {
        name: &'static str,
        message: String,
    },
}

impl std::fmt::Display for FlavorRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSchema {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "duplicate schema registered: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::DuplicateTool { name } => {
                write!(f, "duplicate tool name registered: {name}")
            }
            Self::DuplicateFlavor { flavor_id } => {
                write!(f, "duplicate flavor descriptor registered: {flavor_id}")
            }
            Self::DuplicateDependencyRule { schema_id } => {
                write!(
                    f,
                    "duplicate dependency satisfaction rule for schema {schema_id}"
                )
            }
            Self::InvalidCapabilityTag {
                schema_id,
                schema_version,
                kind,
                tag,
                message,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} has invalid capability tag {tag:?}: {message}"
            ),
            Self::InvalidToolName {
                name,
                expected_prefix,
                message,
            } => write!(
                f,
                "tool name {name:?} is invalid for prefix {expected_prefix:?}: {message}"
            ),
            Self::DuplicateOwnerResolver => f.write_str("duplicate owner resolver registered"),
            Self::SchemaIngressMismatch {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} has mismatched typed-ingress registration"
            ),
            Self::UnregisteredSchemaCapabilityTags {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema capability tags reference unregistered schema: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::UndeclaredToolBehavior { name } => write!(
                f,
                "tool {name} declares no ANNOTATIONS, so the owner-role gate cannot tell a read \
                 from a write and will demand write access; set `const ANNOTATIONS` on the tool"
            ),
            Self::DispatcherWithoutActionSpecs { name } => write!(
                f,
                "tool {name} has an internally tagged `Args` (its schema carries \
                 x-proxima-actions) but declares no ACTION_ARG_SPECS, so nothing enumerates its \
                 actions: set `const ACTION_ARG_SPECS` on the tool, or give it a plain struct \
                 `Args`"
            ),
            Self::InvalidActionSpecs { name, message } => {
                write!(
                    f,
                    "tool {name} has inconsistent ACTION_ARG_SPECS: {message}"
                )
            }
        }
    }
}

impl std::error::Error for FlavorRegistryError {}
