use super::error::McpToolError;

/// Parse the wire form: one hyphenated UUID, without a class or store prefix.
///
/// # Errors
///
/// Returns invalid input for any other spelling.
pub fn parse_wire_uuid(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    if raw.len() != 36 {
        return Err(McpToolError::InvalidInput(
            "id must be a hyphenated UUID".into(),
        ));
    }
    uuid::Uuid::parse_str(raw)
        .map_err(|_| McpToolError::InvalidInput("id must be a hyphenated UUID".into()))
}

#[cfg(test)]
mod tests {
    use super::parse_wire_uuid;

    #[test]
    fn wire_ids_accept_only_bare_hyphenated_uuids() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(parse_wire_uuid(&id.to_string()).expect("UUID"), id);
        for raw in [
            format!("F:{id}"),
            format!("A:{id}"),
            format!("P:{id}"),
            format!("G:{id}"),
            format!("R:{id}"),
            id.simple().to_string(),
            format!("{{{id}}}"),
            format!("urn:uuid:{id}"),
            "not-a-uuid".into(),
        ] {
            assert!(parse_wire_uuid(&raw).is_err(), "{raw}");
        }
    }
}
