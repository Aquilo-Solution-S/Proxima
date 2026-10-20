//! Authorized current-head listing for stateful Fact sidecars.

use proxima_core::verbs::query::{
    SidecarAtom, SidecarHeadsPage, SidecarHeadsRequest, SidecarHeadsScope,
};
use proxima_core::{AuthzContext, Engine, FactPayload, MemoryId, ToolError};

/// List the current heads of `P`'s natural-key series, newest admission first.
///
/// `key_prefix` supplies whole values for the first N natural-key columns in
/// frozen-contract order: an empty slice lists all series, one value narrows
/// the first column, and a full tuple selects one key per readable owner.
/// Table and column identifiers always come from the frozen registry.
///
/// A flavor-defined tombstone remains a current head. Superseded and
/// forgotten admissions are absent. Pass `next_cursor` back as `after` to
/// continue; each page observes the current heads at the time of its read.
///
/// # Errors
///
/// Returns authorization errors from the Engine, refuses a zero limit, a
/// prefix longer than the declared key, or a schema without a stateful Fact
/// sidecar, and propagates storage errors. Limits above
/// [`super::MAX_SIDECAR_HEAD_PAGE_LIMIT`] are clamped to that cap.
pub async fn sidecar_heads_by_key<P: FactPayload>(
    engine: &Engine,
    authz: &AuthzContext,
    scope: SidecarHeadsScope,
    key_prefix: &[SidecarAtom],
    after: Option<MemoryId>,
    limit: u32,
) -> Result<SidecarHeadsPage, ToolError> {
    Ok(engine
        .sidecar_heads_by_key(
            authz,
            &SidecarHeadsRequest {
                schema_id: P::schema_id(),
                scope,
                key_prefix: key_prefix.to_vec(),
                after,
                limit,
            },
        )
        .await?)
}
