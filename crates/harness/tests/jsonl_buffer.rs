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

    for (idx, line) in lines.iter().enumerate() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("line {idx} is not valid JSON: {e}\nline = {line}"));
    }
}

#[test]
fn cap_eviction_preserves_record_boundaries() {
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
