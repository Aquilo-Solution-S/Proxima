use crate::McpToolError;

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

pub fn memory_kind_for_edge(kind: Option<crate::EntityKind>) -> crate::EntityKind {
    match kind {
        Some(crate::EntityKind::Abstraction) => crate::EntityKind::Abstraction,
        Some(crate::EntityKind::Perspective) => crate::EntityKind::Perspective,
        None | Some(_) => crate::EntityKind::Fact,
    }
}
