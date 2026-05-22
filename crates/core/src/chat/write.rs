//! Pure helpers shared by the chat tools.
//!
//! All chat data access now goes through the [`ChatStore`] capability
//! trait; this module keeps only the storage-agnostic helpers — the
//! engine-storage accessor, deterministic Abstraction memory ids, the
//! edge-authorship rule, and input normalization.

use super::*;

/// Borrow the engine-backed `Storage` handle the chat tools need.
///
/// `McpToolCtx::engine` is `None` only in test scaffolds without a wired
/// engine; every chat tool requires one.
pub(super) fn chat_storage(ctx: &McpToolCtx) -> Result<&dyn Storage, McpToolError> {
    ctx.storage()
        .ok_or_else(|| McpToolError::Other("chat tools require an attached engine".into()))
}

pub(super) fn edge_authorship_for_ctx(ctx: &McpToolCtx) -> EdgeAuthorshipKind {
    if ctx.master_token_id.is_some() {
        EdgeAuthorshipKind::User
    } else {
        EdgeAuthorshipKind::ExternalAgent
    }
}

pub(super) fn chat_compaction_memory_id(
    owner: &Owner,
    thread_key: &str,
    idempotency_key: &str,
) -> uuid::Uuid {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let mut key = Vec::with_capacity(96 + thread_key.len() + idempotency_key.len());
    key.extend_from_slice(owner_kind.as_str().as_bytes());
    key.push(0);
    key.extend_from_slice(owner_id.as_bytes());
    key.push(0);
    key.extend_from_slice(owner_org_id.as_bytes());
    key.push(0);
    key.extend_from_slice(thread_key.as_bytes());
    key.push(0);
    key.extend_from_slice(idempotency_key.as_bytes());
    uuid::Uuid::new_v5(&CHAT_COMPACTION_DERIVED_NAMESPACE, &key)
}

pub(super) fn chat_summary_memory_id(
    owner: &Owner,
    thread_key: &str,
    request_memory_id: uuid::Uuid,
    idempotency_key: &str,
) -> uuid::Uuid {
    let (owner_kind, owner_id, owner_org_id) = owner_columns(owner);
    let mut key = Vec::with_capacity(128 + thread_key.len() + idempotency_key.len());
    key.extend_from_slice(owner_kind.as_str().as_bytes());
    key.push(0);
    key.extend_from_slice(owner_id.as_bytes());
    key.push(0);
    key.extend_from_slice(owner_org_id.as_bytes());
    key.push(0);
    key.extend_from_slice(thread_key.as_bytes());
    key.push(0);
    key.extend_from_slice(request_memory_id.as_bytes());
    key.push(0);
    key.extend_from_slice(idempotency_key.as_bytes());
    uuid::Uuid::new_v5(&CHAT_SUMMARY_DERIVED_NAMESPACE, &key)
}

pub(super) fn normalize_text(
    field: &str,
    value: &str,
    min: usize,
    max: usize,
) -> Result<String, McpToolError> {
    let trimmed = value.trim();
    if trimmed.len() < min || trimmed.len() > max {
        return Err(McpToolError::InvalidInput(format!(
            "{field} length must be between {min} and {max}"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn owner_columns(owner: &Owner) -> (OwnerPrincipalKind, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        Principal::User(user) => (OwnerPrincipalKind::User, user.into_inner()),
        Principal::Group(group) => (OwnerPrincipalKind::Group, group.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
