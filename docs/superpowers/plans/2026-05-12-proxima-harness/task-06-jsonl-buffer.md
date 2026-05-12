# Task 2.4 — JSONL trace buffer

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/trace/jsonl.rs`

- [ ] **Step 1: Write the failing test**

Create the test file `crates/harness/tests/jsonl_buffer.rs`:

```rust
use proxima_harness::trace::jsonl::JsonlBuffer;
use serde_json::json;

#[test]
fn small_buffer_records_lines_in_order() {
    let mut buf = JsonlBuffer::with_capacity(64 * 1024);
    buf.append(&json!({"record":"start","invocation_id":"X"}));
    buf.append(&json!({"record":"round_start","round":0}));
    let snap = buf.snapshot();
    let text = std::str::from_utf8(&snap.bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"record\":\"start\""));
    assert!(lines[1].contains("\"record\":\"round_start\""));
    assert!(!snap.truncated);
}

#[test]
fn cap_hit_emits_truncated_marker_and_stops_appending() {
    let mut buf = JsonlBuffer::with_capacity(256);
    for i in 0..1000 {
        buf.append(&json!({"record":"round","i":i,"pad":"xxxxxxxxxxxxxxxxxx"}));
    }
    let snap = buf.snapshot();
    assert!(snap.truncated);
    let text = std::str::from_utf8(&snap.bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let last = lines.last().expect("at least one line");
    assert!(
        last.contains("\"record\":\"truncated\""),
        "last line should be the truncated marker, got {last}"
    );
    assert!(snap.bytes.len() <= 256 + 256);

    // Every line must be valid JSON — regression guard against the
    // earlier off-by-one in `write_truncated_marker` that left half a
    // record glued to the marker.
    for (idx, line) in lines.iter().enumerate() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("line {idx} is not valid JSON: {e}\nline = {line}"));
    }
}

/// Regression: when the buffer holds a few records and the next
/// append fits within the cap but the marker doesn't, the eviction
/// loop must drop **whole records** — never the trailing newline of
/// the last surviving record.
#[test]
fn cap_eviction_preserves_record_boundaries() {
    // Sized so the second record's append triggers the cap and the
    // marker eviction has to peel records off the tail.
    let mut buf = JsonlBuffer::with_capacity(100);
    buf.append(&json!({"r":0,"pad":"aaaaaaaaaaaaaaaaaa"}));
    buf.append(&json!({"r":1,"pad":"bbbbbbbbbbbbbbbbbb"}));
    buf.append(&json!({"r":2,"pad":"cccccccccccccccccc"}));
    let snap = buf.snapshot();
    assert!(snap.truncated);
    let text = std::str::from_utf8(&snap.bytes).unwrap();
    for (idx, line) in text.lines().enumerate() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("line {idx} is not valid JSON: {e}\nline = {line}"));
    }
}

#[test]
fn content_hash_is_stable_for_equal_byte_sequences() {
    let mut a = JsonlBuffer::with_capacity(1024);
    let mut b = JsonlBuffer::with_capacity(1024);
    a.append(&json!({"x":1}));
    a.append(&json!({"y":2}));
    b.append(&json!({"x":1}));
    b.append(&json!({"y":2}));
    assert_eq!(a.snapshot().content_hash, b.snapshot().content_hash);
}
```

Run: `cargo test -p proxima-harness --test jsonl_buffer`
Expected: FAIL with "no such item `JsonlBuffer`".

- [ ] **Step 2: Implement `JsonlBuffer`**

Replace `crates/harness/src/trace/jsonl.rs`:

```rust
//! In-memory JSONL transcript buffer with byte cap + truncate marker.
//!
//! Per spec §"Layer 1 — JSONL transcript", the buffer enforces a
//! per-invocation cap (default 5 MB, configurable per Owner). When
//! the cap is hit, the harness writes a final `truncated` marker
//! line and stops appending — the wake itself does not fail.

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
        let mut line = match serde_json::to_vec(record) {
            Ok(b) => b,
            Err(_) => return,
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
            // Peel **whole** records off the tail until the marker
            // fits, or until the buffer is empty. The buffer invariant
            // is "ends in '\n' (or is empty)"; truncating to `prev_nl
            // + 1` (one past the previous record terminator) preserves
            // it. An earlier version used `pop_at.saturating_sub(1)`
            // which removed only the trailing newline on the first
            // iteration, leaving a half-record glued to the marker
            // and producing invalid JSONL.
            while !self.bytes.is_empty()
                && self.bytes.len() + line.len() > self.cap_bytes
            {
                // bytes ends in '\n' here (invariant). Look for the
                // *previous* '\n' — the start of the trailing record's
                // terminator — and slice to one past it.
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

    /// Allow records that don't serialise via Serialize (e.g.
    /// pre-built `Value`). Identical semantics to `append`.
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
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p proxima-harness --test jsonl_buffer`
Expected: all 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/harness/src/trace crates/harness/tests/jsonl_buffer.rs
git commit -m "harness: JSONL transcript buffer with size cap"
```

