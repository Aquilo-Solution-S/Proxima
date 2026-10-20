//! Current note series through the generic, owner-scoped facade.

use proxima::flavor::{
    SidecarAtom, SidecarHeadsPage, SidecarHeadsScope, ToolError, sidecar_heads_by_key,
};
use proxima::{AuthzContext, Engine, MemoryId};

use crate::NoteV1;

/// List current note heads, optionally narrowed by a natural-key tuple prefix.
///
/// An empty prefix lists every visible note; one UUID selects a notebook;
/// that UUID followed by a text key selects one note series. Resume from the
/// returned cursor for the next page, newest admission first.
///
/// # Errors
/// Returns facade authorization, prefix, limit, and storage errors.
pub async fn note_heads(
    engine: &Engine,
    authz: &AuthzContext,
    scope: SidecarHeadsScope,
    key_prefix: &[SidecarAtom],
    after: Option<MemoryId>,
    limit: u32,
) -> Result<SidecarHeadsPage, ToolError> {
    sidecar_heads_by_key::<NoteV1>(engine, authz, scope, key_prefix, after, limit).await
}
