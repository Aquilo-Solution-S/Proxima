use proxima_core::{McpToolError, Owner, Principal};

pub fn owner_principal(owner: &Owner) -> (&'static str, uuid::Uuid) {
    match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    }
}

pub fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = owner_principal(owner);
    (kind, principal_id, owner.org_id.into_inner())
}

#[allow(clippy::needless_pass_by_value)]
pub fn map_storage(error: sqlx::Error) -> McpToolError {
    McpToolError::Storage(proxima_core::StorageError::Internal(error.to_string()))
}

pub fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>, McpToolError> {
    if tags.len() > 16 {
        return Err(McpToolError::InvalidInput("at most 16 tags".into()));
    }
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim().to_ascii_lowercase();
        if tag.is_empty() || tag.chars().count() > 48 {
            return Err(McpToolError::InvalidInput(
                "tag must be 1..=48 chars".into(),
            ));
        }
        out.push(tag);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn memory_kind_for_edge(kind: Option<&str>) -> &'static str {
    match kind {
        Some("Abstraction") => "Abstraction",
        Some("Perspective") => "Perspective",
        None | Some(_) => "Fact",
    }
}

pub fn layer(kind: Option<&str>) -> u8 {
    match kind {
        Some("Abstraction") => 1,
        Some("Perspective") => 2,
        None | Some(_) => 0,
    }
}

pub fn assert_layer_top_down(
    source_kind: Option<&str>,
    target_kind: Option<&str>,
) -> Result<(), McpToolError> {
    let source_layer = layer(source_kind);
    let target_layer = layer(target_kind);
    if source_layer < target_layer {
        return Err(McpToolError::LayeringViolation(format!(
            "layering: src layer {source_layer} < tgt layer {target_layer}"
        )));
    }
    Ok(())
}
