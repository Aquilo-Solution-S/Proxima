use super::{PayloadKind, SchemaId, SchemaVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlavorRegistryError {
    DuplicateSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    DuplicateRelation {
        relation: String,
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
    InvalidRelationDescriptor {
        relation: String,
        message: String,
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
    UnregisteredRelationPayload {
        relation: String,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
    },
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
    UnsatisfiableRelationTags {
        relation: String,
        side: &'static str,
    },
    /// A registered MCP tool has no resolvable behaviour declaration, so the
    /// owner-role gate cannot tell a read from a write and has to assume a
    /// write. Substrate tools may answer through the core manifest; a flavor
    /// tool has only `ANNOTATIONS`.
    UndeclaredToolBehavior {
        name: &'static str,
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
            Self::DuplicateRelation { relation } => {
                write!(f, "duplicate relation descriptor registered: {relation}")
            }
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
            Self::InvalidRelationDescriptor { relation, message } => {
                write!(f, "relation descriptor {relation} is invalid: {message}")
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
            Self::UnregisteredRelationPayload {
                relation,
                schema_id,
                schema_version,
            } => write!(
                f,
                "relation descriptor {relation} references unregistered EdgePayload schema {schema_id} v{schema_version}"
            ),
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
            Self::UnsatisfiableRelationTags { relation, side } => write!(
                f,
                "relation descriptor {relation} has unsatisfiable {side} required capability tags"
            ),
            Self::UndeclaredToolBehavior { name } => write!(
                f,
                "tool {name} declares no ANNOTATIONS, so the owner-role gate cannot tell a read \
                 from a write and will demand write access; set `const ANNOTATIONS` on the tool"
            ),
        }
    }
}

impl std::error::Error for FlavorRegistryError {}
