//! Graph endpoints carry bare UUIDs; redaction retains its explicit sentinels.

use crate::mcp::McpToolCtx;
use crate::{EdgeEndpoint, EdgeTargetProjection, EntityRef};

pub(crate) const REDACTED_TARGET: &str = "redacted target";
pub(crate) const UNAVAILABLE_TARGET: &str = "unavailable target";

pub(crate) fn format_endpoint(_ctx: &McpToolCtx, endpoint: EdgeEndpoint) -> String {
    match endpoint.entity {
        EntityRef::Memory(id) => id.into_inner().to_string(),
        EntityRef::Goal(id) => id.into_inner().to_string(),
    }
}

pub(crate) fn format_target_projection(ctx: &McpToolCtx, target: EdgeTargetProjection) -> String {
    match target {
        EdgeTargetProjection::Visible { target } => format_endpoint(ctx, target),
        EdgeTargetProjection::Redacted => REDACTED_TARGET.into(),
        EdgeTargetProjection::Unavailable => UNAVAILABLE_TARGET.into(),
    }
}
