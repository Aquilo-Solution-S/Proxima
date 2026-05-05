use std::str::FromStr;

use prost_types::Timestamp;
use tonic::Status;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// IDs and primitives
// ---------------------------------------------------------------------------

pub fn uuid_from_proto(s: &str) -> Result<Uuid, Status> {
    Uuid::from_str(s).map_err(|e| Status::invalid_argument(format!("invalid UUID: {e}")))
}

pub fn uuid_to_proto(u: Uuid) -> String {
    u.to_string()
}

// ---------------------------------------------------------------------------
// Timestamp conversions
// ---------------------------------------------------------------------------

pub fn timestamp_from_proto(ts: Option<Timestamp>) -> Result<time::OffsetDateTime, Status> {
    let ts = ts.ok_or_else(|| Status::invalid_argument("missing timestamp"))?;
    let total_nanos = i128::from(ts.seconds) * 1_000_000_000 + i128::from(ts.nanos);
    time::OffsetDateTime::from_unix_timestamp_nanos(total_nanos)
        .map_err(|e| Status::invalid_argument(format!("invalid timestamp: {e}")))
}

pub fn timestamp_to_proto(ts: time::OffsetDateTime) -> Timestamp {
    let nanos = ts.unix_timestamp_nanos();
    let seconds_i128 = nanos / 1_000_000_000;
    let nanos_part = (nanos % 1_000_000_000) as i32;
    Timestamp {
        seconds: i64::try_from(seconds_i128).unwrap_or(i64::MAX),
        nanos: nanos_part,
    }
}
