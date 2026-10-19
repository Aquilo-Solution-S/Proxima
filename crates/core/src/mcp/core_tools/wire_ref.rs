//! Graph endpoints carry bare UUIDs; redaction retains its explicit sentinels.

use crate::mcp::McpToolCtx;
use crate::{EdgeEndpoint, EdgeTargetProjection, EntityRef};

pub(crate) const REDACTED_TARGET: &str = "redacted target";
pub(crate) const FORGOTTEN_TARGET: &str = "forgotten target";

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
        EdgeTargetProjection::Forgotten => FORGOTTEN_TARGET.into(),
    }
}
