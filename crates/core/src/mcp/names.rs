/// Tool names exposed to LLM-hosted MCP clients must also be valid
/// provider function names. Internal ids use flavor-style `/`
/// separators, which some runners pass through unchanged.
#[must_use]
pub fn provider_safe_tool_name(canonical: &str) -> String {
    let mut out = String::with_capacity(canonical.len());
    let mut previous_dot = false;
    for ch in canonical.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.';
        let mapped = if safe { ch } else { '_' };
        if mapped == '.' {
            if previous_dot {
                out.push('_');
                previous_dot = false;
            } else {
                out.push(mapped);
                previous_dot = true;
            }
        } else {
            out.push(mapped);
            previous_dot = false;
        }
    }
    out
}

#[must_use]
pub fn tool_name_matches(canonical: &str, request_name: &str) -> bool {
    canonical == request_name || provider_safe_tool_name(canonical) == request_name
}
