//! Canonical JSON byte writer for transient payload identity.
//!
//! Deterministic, sorted-key bytes for content-addressed identity (e.g.
//! operator idempotency keys). `serde_json` has no stable sorted-key
//! output — its `preserve_order` feature does the opposite (preserves
//! insertion order) — so this small, fully-tested writer owns those
//! bytes rather than pulling a heavier external JCS crate. Arrays keep
//! input order; primitive/number formatting delegates to `serde_json`.

/// Serialize a JSON value with recursively sorted object keys.
///
/// Arrays retain input order. Primitive formatting is delegated to
/// `serde_json`, including number formatting.
#[must_use]
pub fn canonical_json_bytes(value: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical_json(value, &mut out);
    out
}

fn write_canonical_json(value: &serde_json::Value, out: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => out.extend_from_slice(b"null"),
        serde_json::Value::Bool(true) => out.extend_from_slice(b"true"),
        serde_json::Value::Bool(false) => out.extend_from_slice(b"false"),
        serde_json::Value::Number(number) => {
            serde_json::to_writer(out, number).expect("serializing JSON number to Vec cannot fail");
        }
        serde_json::Value::String(text) => {
            serde_json::to_writer(out, text).expect("serializing JSON string to Vec cannot fail");
        }
        serde_json::Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out);
            }
            out.push(b']');
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            out.push(b'{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, key)
                    .expect("serializing JSON object key to Vec cannot fail");
                out.push(b':');
                write_canonical_json(
                    map.get(key)
                        .expect("object key collected from map must still exist"),
                    out,
                );
            }
            out.push(b'}');
        }
    }
}
