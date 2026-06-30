/// `const fn` byte-wise `str::starts_with` — used by `proxima_flavor!`
/// to compile-check schema / tool / trigger prefixes. `str::starts_with`
/// is not `const`, so the comparison is spelled out. See docs/08
/// §Schema namespacing: prefix violations reachable from associated
/// `const`s or literals are now caught at build time, not at `register`.
#[must_use]
pub const fn schema_id_has_prefix(id: &str, prefix: &str) -> bool {
    let (id, prefix) = (id.as_bytes(), prefix.as_bytes());
    if prefix.len() > id.len() {
        return false;
    }
    let mut i = 0;
    while i < prefix.len() {
        if id[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}
