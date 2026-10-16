//! Shared test fixtures for the migration-reset adoption boundary.

pub fn pre_reset_pairs(entries: &[(i64, &str)]) -> Vec<(i64, Vec<u8>)> {
    entries
        .iter()
        .map(|(version, checksum)| (*version, decode_hex(checksum)))
        .collect()
}

pub fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("hex"), 16).expect("hex"))
        .collect()
}
