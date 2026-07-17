//! Shared opaque-cursor wire plumbing.
//!
//! Every paginated surface hands out the same kind of token: a JSON
//! document base64url-encoded without padding, carrying a format version
//! and a binding to the query that produced it, decoded fail-closed. The
//! per-surface `Wire*Cursor` payload shapes differ (and must stay
//! byte-stable), so this module centralizes the plumbing — token
//! encode/decode and the two caller-facing error messages — while each
//! surface keeps its own payload struct and binding semantics.
//!
//! New cursors should use [`FingerprintedCursor`], the `{v, fp, c}`
//! envelope `core_search_memories` established: `fp` is a canonical
//! fingerprint over everything that shapes the result set or its order
//! (page size stays out so it may vary between pages), and `c` is the
//! typed keyset resume point.

use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A cursor decode failure carrying its caller-facing message. Always a
/// caller-input fault: converts to `InvalidInput` for both the core MCP
/// error and the flavor-facing tool error.
#[derive(Debug)]
pub struct CursorError(String);

impl CursorError {
    #[must_use]
    pub fn into_message(self) -> String {
        self.0
    }
}

impl From<CursorError> for crate::mcp::McpToolError {
    fn from(err: CursorError) -> Self {
        Self::InvalidInput(err.0)
    }
}

impl From<CursorError> for crate::ToolError {
    fn from(err: CursorError) -> Self {
        Self::InvalidInput(err.0)
    }
}

/// A syntactically broken token: bad base64, bad JSON, or a version this
/// build does not speak. `source` completes the sentence — e.g.
/// `"proxima://edges page"` or `"core_search_memories response"`.
#[must_use]
pub fn malformed_cursor(source: &str) -> CursorError {
    CursorError(format!(
        "malformed cursor: pass next_cursor from a previous {source}"
    ))
}

/// A well-formed token bound to a different query. `rebind_hint` completes
/// the sentence — e.g. `"repeat the state filter that produced it"`.
#[must_use]
pub fn cursor_query_mismatch(rebind_hint: &str) -> CursorError {
    CursorError(format!("cursor does not match this query: {rebind_hint}"))
}

/// Encode a wire cursor payload: JSON then base64url without padding.
///
/// # Panics
///
/// Panics when `wire` fails to serialize — cursor payloads are plain
/// data structs, so this indicates a programming error, never input.
#[must_use]
pub fn encode_token<T: Serialize>(wire: &T) -> String {
    let json = serde_json::to_vec(wire).expect("cursor payload serializes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Decode a wire cursor payload; `None` on bad base64 or bad JSON.
/// Callers map `None` to their surface's [`malformed_cursor`] error and
/// keep their own version/binding checks.
#[must_use]
pub fn decode_token<T: DeserializeOwned>(raw: &str) -> Option<T> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.as_bytes())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Canonical 16-hex-char fingerprint over a canonicalized query shape.
/// Callers serialize the binding tuple themselves (sorting any sets so
/// equivalent filters fingerprint identically) and pass the JSON string.
#[must_use]
pub fn fingerprint(canon: &str) -> String {
    blake3::hash(canon.as_bytes()).to_hex()[..16].to_string()
}

/// The standard fingerprint-bound envelope `{v, fp, c}`.
#[derive(Debug, Serialize, Deserialize)]
struct WireEnvelope<C> {
    v: u8,
    fp: String,
    c: C,
}

/// Codec for the standard envelope: a format version plus the two
/// message fragments that name the surface. Declare one `const` per
/// paginated surface.
#[derive(Debug, Clone, Copy)]
pub struct FingerprintedCursor {
    /// Wire format version; bump on any payload shape change.
    pub version: u8,
    /// Completes "pass `next_cursor` from a previous …".
    pub source: &'static str,
    /// Completes "cursor does not match this query: …".
    pub rebind_hint: &'static str,
}

impl FingerprintedCursor {
    /// Mint a token binding `cursor` to `fingerprint`.
    #[must_use]
    pub fn encode<C: Serialize>(&self, fingerprint: &str, cursor: &C) -> String {
        encode_token(&WireEnvelope {
            v: self.version,
            fp: fingerprint.to_string(),
            c: cursor,
        })
    }

    /// Decode a token, failing closed on malformed input, a version
    /// mismatch, or a fingerprint from a different query shape.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError`] with a caller-facing message.
    pub fn decode<C: DeserializeOwned>(
        &self,
        fingerprint: &str,
        raw: &str,
    ) -> Result<C, CursorError> {
        let wire: WireEnvelope<C> =
            decode_token(raw).ok_or_else(|| malformed_cursor(self.source))?;
        if wire.v != self.version {
            return Err(malformed_cursor(self.source));
        }
        if wire.fp != fingerprint {
            return Err(cursor_query_mismatch(self.rebind_hint));
        }
        Ok(wire.c)
    }
}

#[cfg(test)]
mod tests {
    use super::{FingerprintedCursor, decode_token, encode_token, fingerprint};

    const CODEC: FingerprintedCursor = FingerprintedCursor {
        version: 1,
        source: "proxima://example page",
        rebind_hint: "repeat the example filter that produced it",
    };

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct ExampleCursor {
        created_at_nanos: i128,
        id: uuid::Uuid,
    }

    #[test]
    fn fingerprinted_round_trip() {
        let cursor = ExampleCursor {
            created_at_nanos: 1_700_000_000_000_000_000,
            id: uuid::Uuid::now_v7(),
        };
        let fp = fingerprint("[\"example\"]");
        let token = CODEC.encode(&fp, &cursor);
        let decoded: ExampleCursor = CODEC.decode(&fp, &token).expect("round trip");
        assert_eq!(decoded, cursor);
    }

    #[test]
    fn fingerprint_mismatch_fails_closed() {
        let cursor = ExampleCursor {
            created_at_nanos: 0,
            id: uuid::Uuid::now_v7(),
        };
        let token = CODEC.encode(&fingerprint("[\"a\"]"), &cursor);
        let err = CODEC
            .decode::<ExampleCursor>(&fingerprint("[\"b\"]"), &token)
            .expect_err("different fingerprint must fail");
        assert!(err.into_message().starts_with("cursor does not match"));
    }

    #[test]
    fn malformed_and_wrong_version_fail_closed() {
        let fp = fingerprint("[]");
        let err = CODEC
            .decode::<ExampleCursor>(&fp, "not-base64!!")
            .expect_err("garbage must fail");
        assert!(err.into_message().starts_with("malformed cursor"));

        let newer = FingerprintedCursor {
            version: 2,
            ..CODEC
        };
        let cursor = ExampleCursor {
            created_at_nanos: 0,
            id: uuid::Uuid::now_v7(),
        };
        let token = newer.encode(&fp, &cursor);
        let err = CODEC
            .decode::<ExampleCursor>(&fp, &token)
            .expect_err("future version must fail");
        assert!(err.into_message().starts_with("malformed cursor"));
    }

    #[test]
    fn token_layer_round_trips_flat_payloads() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Flat {
            v: u8,
            state: Option<String>,
        }
        let flat = Flat {
            v: 1,
            state: Some("Active".into()),
        };
        let token = encode_token(&flat);
        assert_eq!(decode_token::<Flat>(&token), Some(flat));
        assert_eq!(decode_token::<Flat>("!!!"), None);
    }
}
