//! Deterministic JSON serialization for operator invocation keys.

/// Recursively key-sorted, whitespace-stripped JSON serialization.
///
/// Used for A2P `context_hash` material. This deliberately avoids
/// relying on map insertion order from upstream serializers.
pub fn canonical_json<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let v = serde_json::to_value(value)?;
    let mut out = String::new();
    write_canonical(&v, &mut out);
    Ok(out)
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => out.push_str(&Value::String(s.clone()).to_string()),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(e, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            let mut keys: Vec<_> = o.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&o[*k], out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Fixture {
        b: u32,
        a: String,
        c: Vec<u32>,
    }

    #[test]
    fn key_order_stable() {
        let f = Fixture {
            b: 2,
            a: "x".into(),
            c: vec![1, 2],
        };
        assert_eq!(canonical_json(&f).unwrap(), r#"{"a":"x","b":2,"c":[1,2]}"#);
    }

    #[test]
    fn nested_key_order_stable() {
        let v = serde_json::json!({
            "z": 1,
            "a": { "b": true, "a": null },
            "list": [{ "y": 2, "x": 1 }]
        });
        assert_eq!(
            canonical_json(&v).unwrap(),
            r#"{"a":{"a":null,"b":true},"list":[{"x":1,"y":2}],"z":1}"#
        );
    }
}
