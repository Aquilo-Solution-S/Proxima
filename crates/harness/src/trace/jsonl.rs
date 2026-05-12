//! In-memory JSONL transcript buffer with byte cap + truncate marker.
//!
//! Per spec §"Layer 1 — JSONL transcript", the buffer enforces a
//! per-invocation cap. When the cap is hit, the harness writes a final
//! `truncated` marker line and stops appending — the wake itself does
//! not fail.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug)]
pub struct JsonlBuffer {
    bytes: Vec<u8>,
    cap_bytes: usize,
    truncated: bool,
    line_count: u64,
}

#[derive(Debug, Clone)]
pub struct JsonlSnapshot {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub line_count: u64,
    pub content_hash: [u8; 32],
}

impl JsonlBuffer {
    #[must_use]
    pub fn with_capacity(cap_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(cap_bytes.min(64 * 1024)),
            cap_bytes,
            truncated: false,
            line_count: 0,
        }
    }

    /// Append one JSON-serialisable record as a single line.
    /// Once `truncated == true`, further `append` calls are no-ops.
    pub fn append<T: Serialize>(&mut self, record: &T) {
        if self.truncated {
            return;
        }
        let Ok(mut line) = serde_json::to_vec(record) else {
            return;
        };
        line.push(b'\n');
        if self.bytes.len() + line.len() > self.cap_bytes {
            self.write_truncated_marker(self.bytes.len() + line.len());
            return;
        }
        self.bytes.extend_from_slice(&line);
        self.line_count += 1;
    }

    fn write_truncated_marker(&mut self, attempted_total: usize) {
        self.truncated = true;
        let marker = serde_json::json!({
            "record": "truncated",
            "reason": "size_cap",
            "cap_bytes": self.cap_bytes,
            "attempted_total": attempted_total,
        });
        if let Ok(mut line) = serde_json::to_vec(&marker) {
            line.push(b'\n');
            // Peel whole records off the tail until the marker fits.
            while !self.bytes.is_empty() && self.bytes.len() + line.len() > self.cap_bytes {
                debug_assert_eq!(self.bytes.last().copied(), Some(b'\n'));
                let trailing = self.bytes.len() - 1;
                let new_len = self.bytes[..trailing]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |i| i + 1);
                self.bytes.truncate(new_len);
            }
            self.bytes.extend_from_slice(&line);
            self.line_count += 1;
        }
    }

    /// Allow records that don't serialise via Serialize (e.g. pre-built `Value`).
    pub fn append_value(&mut self, v: &Value) {
        self.append(v);
    }

    #[must_use]
    pub fn snapshot(&self) -> JsonlSnapshot {
        let content_hash = *blake3::hash(&self.bytes).as_bytes();
        JsonlSnapshot {
            bytes: self.bytes.clone(),
            truncated: self.truncated,
            line_count: self.line_count,
            content_hash,
        }
    }

    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}
