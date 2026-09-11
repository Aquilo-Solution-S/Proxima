//! A durable sink for Proxima's Fact stream (issue #305, docs/18).
//!
//! Run it against a broker the publisher is feeding:
//!
//! ```text
//! PROXIMA_NATS_URL=nats://127.0.0.1:4222 \
//! PROXIMA_INTAKE_PATH=/tmp/proxima-facts.jsonl \
//!   cargo run -p proxima-outbox-nats --example durable_intake
//! ```
//!
//! It exists to show what a sink OWES the stream, which is exactly two
//! things and no more:
//!
//! 1. **Make the outcome durable before acknowledging.** The journal retains
//!    the exact delivered bytes, a payload digest, and a separate decision
//!    timestamp. The append is `write` + `sync_all` — a buffered write that
//!    had not reached the disk when the process died would leave an
//!    acknowledged event that no longer exists. Everything the consumer does
//!    with an ACK is built on the sink having already committed.
//! 2. **Deduplicate on the `CloudEvents` id.** Delivery is at-least-once,
//!    always. The broker's `Nats-Msg-Id` window absorbs a republication
//!    within its duration; beyond it, and after any redelivery caused by a
//!    lost ACK, the SINK is the only thing that can tell a repeat from a
//!    new event. The id is `F:<uuid>` — the Fact's `t` — so it is stable
//!    across every republication of one event.
//!
//! Rejection is a THIRD outcome, distinct from failure: an event this sink
//! will never accept is recorded as rejected and acknowledged, because
//! leaving it unacknowledged would block the consumer on it forever. An
//! event the sink could not decide about returns an error instead, and the
//! consumer leaves it for redelivery.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use proxima_outbox_nats::{
    DurableIntake, Intake, IntakeError, NatsConsumerConfig, ReceivedEvent, ReferenceConsumer,
};
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;

/// Where the sink keeps its journal. One JSON object per line: the outcome
/// this sink committed for one event.
const ENV_PATH: &str = "PROXIMA_INTAKE_PATH";
const DEFAULT_PATH: &str = "proxima-facts.jsonl";

/// A file-backed sink: append the exact event bytes and outcome, flush them to
/// the disk, and only then let the consumer acknowledge.
#[derive(Debug)]
struct JsonlIntake {
    /// The file and the id index behind ONE lock. They are two views of a
    /// single fact — "this event has been recorded" — and a reader that
    /// could see the index updated before the bytes were on disk would
    /// acknowledge an event this sink does not have.
    state: Mutex<State>,
    path: PathBuf,
}

#[derive(Debug)]
struct State {
    file: File,
    decisions: HashMap<String, IndexedDecision>,
    conflicts: HashMap<(String, String), String>,
}

#[derive(Debug, Clone)]
struct IndexedDecision {
    payload_digest: String,
    outcome: Intake,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum JournalOutcome {
    Accepted,
    Rejected { reason: String },
}

impl JournalOutcome {
    fn from_intake(outcome: &Intake) -> Self {
        match outcome {
            Intake::Accepted => Self::Accepted,
            Intake::Rejected { reason } => Self::Rejected {
                reason: reason.clone(),
            },
        }
    }

    fn into_intake(self) -> Intake {
        match self {
            Self::Accepted => Intake::Accepted,
            Self::Rejected { reason } => Intake::Rejected { reason },
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "record_type", rename_all = "snake_case")]
enum JournalRecord {
    Decision {
        id: String,
        payload_digest: String,
        raw_base64: String,
        decision_at: String,
        subject: String,
        stream_sequence: u64,
        delivered_count: u64,
        #[serde(flatten)]
        outcome: JournalOutcome,
    },
    Conflict {
        id: String,
        payload_digest: String,
        raw_base64: String,
        decision_at: String,
        subject: String,
        stream_sequence: u64,
        delivered_count: u64,
        original_payload_digest: String,
        #[serde(flatten)]
        outcome: JournalOutcome,
    },
}

impl JsonlIntake {
    /// Open the journal and rebuild the outcome index from it.
    ///
    /// Rebuilding on start-up rather than trusting in-memory state is the
    /// point: a restarted sink that forgot what it had recorded would
    /// duplicate every event still inside the broker's redelivery window.
    /// Only an incomplete final append is discarded; a malformed complete
    /// record fails closed.
    fn open(path: &Path) -> io::Result<Arc<Self>> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        let (decisions, conflicts, truncate_at) = {
            let mut reader = BufReader::new(&file);
            let mut decisions = HashMap::new();
            let mut conflicts = HashMap::new();
            let mut offset = 0_u64;
            let mut truncate_at = None;
            loop {
                let mut line = Vec::new();
                let read = reader.read_until(b'\n', &mut line)?;
                if read == 0 {
                    break;
                }
                let terminated = line.last() == Some(&b'\n');
                if terminated {
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                }
                match serde_json::from_slice::<JournalRecord>(&line) {
                    Ok(record) => {
                        apply_record(record, &mut decisions, &mut conflicts)?;
                        offset = offset.saturating_add(read as u64);
                    }
                    Err(_error) if !terminated => {
                        // A crash can leave only the final append without its
                        // closing newline or with incomplete JSON. It is not
                        // a durable decision, so remove precisely that tail.
                        truncate_at = Some(offset);
                        break;
                    }
                    Err(error) => {
                        return Err(invalid_journal(format!(
                            "malformed complete JSONL record at byte {offset}: {error}"
                        )));
                    }
                }
            }
            (decisions, conflicts, truncate_at)
        };
        if let Some(offset) = truncate_at {
            file.set_len(offset)?;
        }
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                file,
                decisions,
                conflicts,
            }),
            path: path.to_path_buf(),
        }))
    }

    /// Look up, decide, and persist one outcome while holding the same lock.
    /// The index changes only after the journal append has been synced.
    fn decide_and_record(&self, event: &ReceivedEvent) -> io::Result<Intake> {
        let mut state = self.state.lock().expect("the journal lock is not poisoned");
        let payload_digest = payload_digest(&event.raw);
        if let Some(existing) = state.decisions.get(&event.id).cloned() {
            if existing.payload_digest == payload_digest {
                // Replay the original decision before consulting current
                // validation rules. A later release must not reinterpret a
                // durable historical decision.
                return Ok(existing.outcome);
            }

            let conflict_key = (event.id.clone(), payload_digest.clone());
            if let Some(reason) = state.conflicts.get(&conflict_key).cloned() {
                return Ok(Intake::Rejected { reason });
            }

            let reason = format!(
                "conflicting payload for event id {}: recorded digest {}, received {}",
                event.id, existing.payload_digest, payload_digest
            );
            let record = JournalRecord::Conflict {
                id: event.id.clone(),
                payload_digest: payload_digest.clone(),
                raw_base64: STANDARD.encode(&event.raw),
                decision_at: decision_timestamp(),
                subject: event.subject.clone(),
                stream_sequence: event.stream_sequence,
                delivered_count: event.delivered_count,
                original_payload_digest: existing.payload_digest,
                outcome: JournalOutcome::Rejected {
                    reason: reason.clone(),
                },
            };
            append_record(&mut state.file, &record)?;
            state.conflicts.insert(conflict_key, reason.clone());
            return Ok(Intake::Rejected { reason });
        }

        let outcome = if event.envelope.specversion == "1.0" {
            Intake::Accepted
        } else {
            Intake::Rejected {
                reason: format!(
                    "unsupported CloudEvents specversion {:?}",
                    event.envelope.specversion
                ),
            }
        };
        let record = JournalRecord::Decision {
            id: event.id.clone(),
            payload_digest: payload_digest.clone(),
            raw_base64: STANDARD.encode(&event.raw),
            decision_at: decision_timestamp(),
            subject: event.subject.clone(),
            stream_sequence: event.stream_sequence,
            delivered_count: event.delivered_count,
            outcome: JournalOutcome::from_intake(&outcome),
        };
        append_record(&mut state.file, &record)?;
        // The whole contract in one call: the consumer's ACK is only
        // allowed to mean anything because these bytes are on the disk
        // before it is sent.
        state.decisions.insert(
            event.id.clone(),
            IndexedDecision {
                payload_digest,
                outcome: outcome.clone(),
            },
        );
        Ok(outcome)
    }
}

#[async_trait::async_trait]
impl DurableIntake for JsonlIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        // The journal lookup happens before validation and the lookup,
        // decision, append, sync, and index update happen under one lock.
        match self.decide_and_record(event) {
            Ok(outcome) => {
                match &outcome {
                    Intake::Accepted => tracing_line(&format!(
                        "recorded {} ({})",
                        event.id, event.envelope.event_type
                    )),
                    Intake::Rejected { reason } => {
                        tracing_line(&format!("rejected {}: {reason}", event.id));
                    }
                }
                Ok(outcome)
            }
            Err(error) => Err(IntakeError::new(format!(
                "could not append to {}: {error}",
                self.path.display()
            ))),
        }
    }
}

fn append_record(file: &mut File, record: &JournalRecord) -> io::Result<()> {
    let line = serde_json::to_string(record)
        .map_err(|error| invalid_journal(format!("journal record is not serializable: {error}")))?;
    writeln!(file, "{line}")?;
    file.sync_all()
}

fn apply_record(
    record: JournalRecord,
    decisions: &mut HashMap<String, IndexedDecision>,
    conflicts: &mut HashMap<(String, String), String>,
) -> io::Result<()> {
    match record {
        JournalRecord::Decision {
            id,
            payload_digest,
            raw_base64,
            decision_at,
            outcome,
            ..
        } => {
            validate_timestamp(&decision_at)?;
            verify_raw_digest(&raw_base64, &payload_digest)?;
            if decisions
                .insert(
                    id.clone(),
                    IndexedDecision {
                        payload_digest,
                        outcome: outcome.into_intake(),
                    },
                )
                .is_some()
            {
                return Err(invalid_journal(format!(
                    "duplicate primary decision for event id {id}"
                )));
            }
        }
        JournalRecord::Conflict {
            id,
            payload_digest,
            raw_base64,
            decision_at,
            original_payload_digest,
            outcome,
            ..
        } => {
            validate_timestamp(&decision_at)?;
            verify_raw_digest(&raw_base64, &payload_digest)?;
            let Some(original) = decisions.get(&id) else {
                return Err(invalid_journal(format!(
                    "conflict record for event id {id} has no primary decision"
                )));
            };
            if original.payload_digest != original_payload_digest
                || original.payload_digest == payload_digest
            {
                return Err(invalid_journal(format!(
                    "conflict record for event id {id} does not name a distinct current payload"
                )));
            }
            let JournalOutcome::Rejected { reason } = outcome else {
                return Err(invalid_journal(format!(
                    "conflict record for event id {id} is not a rejection"
                )));
            };
            let key = (id, payload_digest);
            if conflicts.insert(key, reason).is_some() {
                return Err(invalid_journal(
                    "duplicate conflict decision in JSONL journal".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn verify_raw_digest(raw_base64: &str, expected_digest: &str) -> io::Result<()> {
    let raw = STANDARD
        .decode(raw_base64)
        .map_err(|error| invalid_journal(format!("raw event is not valid base64: {error}")))?;
    let actual_digest = payload_digest(&raw);
    if actual_digest != expected_digest {
        return Err(invalid_journal(format!(
            "raw event digest {actual_digest} does not match journal digest {expected_digest}"
        )));
    }
    Ok(())
}

fn payload_digest(raw: &[u8]) -> String {
    blake3::hash(raw).to_hex().to_string()
}

fn decision_timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC3339 is a valid fixed timestamp format")
}

fn validate_timestamp(value: &str) -> io::Result<()> {
    time::OffsetDateTime::parse(value, &Rfc3339)
        .map(|_| ())
        .map_err(|error| invalid_journal(format!("invalid decision timestamp {value:?}: {error}")))
}

fn invalid_journal(message: String) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn tracing_line(message: &str) {
    println!("durable_intake: {message}");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(config) = NatsConsumerConfig::from_env()? else {
        return Err("PROXIMA_NATS_URL is unset; nothing to consume".into());
    };
    let path = PathBuf::from(std::env::var(ENV_PATH).unwrap_or_else(|_| DEFAULT_PATH.to_owned()));
    let intake = JsonlIntake::open(&path)?;
    tracing_line(&format!(
        "consuming {} on {} into {}",
        config.stream,
        config.url,
        path.display()
    ));

    let consumer = ReferenceConsumer::connect(config, intake).await?;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stop.cancel();
        }
    });
    consumer.run(cancel).await;
    tracing_line("stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use proxima_outbox_nats::CloudEventEnvelope;
    use std::fs::{OpenOptions, read_to_string};
    use uuid::Uuid;

    fn test_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "proxima-durable-intake-{label}-{}.jsonl",
            Uuid::now_v7()
        ))
    }

    fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    fn event(id: &str, raw: &'static [u8], specversion: &str) -> ReceivedEvent {
        ReceivedEvent {
            id: id.to_owned(),
            subject: "proxima.fact.test".to_owned(),
            stream_sequence: 7,
            delivered_count: 1,
            raw: Bytes::from_static(raw),
            envelope: CloudEventEnvelope {
                specversion: specversion.to_owned(),
                id: id.to_owned(),
                source: "urn:test".to_owned(),
                event_type: "test/event".to_owned(),
                datacontenttype: None,
                dataschema: None,
                time: None,
                proximaowner: None,
                proximamodel: None,
                data: serde_json::json!({}),
            },
        }
    }

    fn records(path: &Path) -> Vec<JournalRecord> {
        read_to_string(path)
            .expect("the journal is readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("every retained line is valid JSON"))
            .collect()
    }

    #[tokio::test]
    async fn rejection_retains_reason_raw_bytes_and_decision_timestamp() {
        let path = test_path("rejection");
        let intake = JsonlIntake::open(&path).expect("the journal opens");
        let raw = b"not-json-\0-with-exact-bytes";
        let event = event("F:rejected", raw, "0.9");
        let outcome = intake.accept(&event).await.expect("rejection is durable");
        assert_eq!(
            outcome,
            Intake::Rejected {
                reason: "unsupported CloudEvents specversion \"0.9\"".to_owned()
            }
        );
        drop(intake);

        let journal = records(&path);
        let [
            JournalRecord::Decision {
                payload_digest: recorded_digest,
                raw_base64,
                decision_at,
                outcome: JournalOutcome::Rejected { reason },
                ..
            },
        ] = journal.as_slice()
        else {
            panic!("the rejection must be one primary decision record");
        };
        assert_eq!(STANDARD.decode(raw_base64).expect("raw is base64"), raw);
        assert_eq!(recorded_digest, &payload_digest(raw));
        assert_eq!(reason, "unsupported CloudEvents specversion \"0.9\"");
        validate_timestamp(decision_at).expect("the decision timestamp is valid");
        remove(&path);
    }

    #[tokio::test]
    async fn replay_returns_saved_decision_before_current_validation() {
        let path = test_path("replay");
        let intake = JsonlIntake::open(&path).expect("the journal opens");
        let raw = b"same-event-bytes";
        let first = event("F:replay", raw, "0.9");
        let saved = intake
            .accept(&first)
            .await
            .expect("first decision is durable");

        // The same raw bytes are now presented with a currently valid view.
        // The historical rejection must win over re-validation.
        let redelivery = event("F:replay", raw, "1.0");
        assert_eq!(
            intake
                .accept(&redelivery)
                .await
                .expect("saved decision is returned"),
            saved
        );
        drop(intake);
        assert_eq!(records(&path).len(), 1);
        remove(&path);
    }

    #[tokio::test]
    async fn conflicting_payload_is_retained_without_overwriting_original_decision() {
        let path = test_path("conflict");
        let intake = JsonlIntake::open(&path).expect("the journal opens");
        let original = event("F:conflict", b"original-bytes", "1.0");
        assert_eq!(
            intake.accept(&original).await.expect("original is durable"),
            Intake::Accepted
        );
        let conflicting = event("F:conflict", b"different-bytes", "1.0");
        let rejection = intake
            .accept(&conflicting)
            .await
            .expect("conflict rejection is durable");
        assert!(
            matches!(rejection, Intake::Rejected { ref reason } if reason.contains("conflicting payload"))
        );
        drop(intake);

        let journal = records(&path);
        assert!(matches!(
            journal.as_slice(),
            [
                JournalRecord::Decision {
                    outcome: JournalOutcome::Accepted,
                    ..
                },
                JournalRecord::Conflict {
                    outcome: JournalOutcome::Rejected { .. },
                    ..
                }
            ]
        ));
        let JournalRecord::Conflict {
            raw_base64,
            payload_digest: conflicting_digest,
            original_payload_digest,
            ..
        } = &journal[1]
        else {
            unreachable!("the conflict record was asserted above");
        };
        assert_eq!(
            STANDARD
                .decode(raw_base64)
                .expect("conflicting raw is base64"),
            b"different-bytes"
        );
        assert_eq!(conflicting_digest, &payload_digest(b"different-bytes"));
        assert_eq!(original_payload_digest, &payload_digest(b"original-bytes"));

        let restarted = JsonlIntake::open(&path).expect("the restarted journal opens");
        let replay = event("F:conflict", b"different-bytes", "0.9");
        assert_eq!(
            restarted
                .accept(&replay)
                .await
                .expect("saved conflict is returned before validation"),
            rejection
        );
        drop(restarted);
        assert_eq!(records(&path).len(), 2);
        remove(&path);
    }

    #[tokio::test]
    async fn restart_after_crash_before_ack_replays_one_durable_rejection() {
        let path = test_path("crash-before-ack");
        let raw = b"crash-safe-rejection";
        let first_event = event("F:crash-before-ack", raw, "0.9");
        let first = JsonlIntake::open(&path).expect("the journal opens");
        let saved = first
            .accept(&first_event)
            .await
            .expect("decision is synced");
        drop(first); // process crash here, before the broker ACK

        let restarted = JsonlIntake::open(&path).expect("the journal restarts");
        let redelivery = event("F:crash-before-ack", raw, "1.0");
        assert_eq!(
            restarted
                .accept(&redelivery)
                .await
                .expect("redelivery returns the saved decision"),
            saved
        );
        drop(restarted);
        assert_eq!(records(&path).len(), 1);
        remove(&path);
    }

    #[tokio::test]
    async fn interrupted_final_append_is_truncated_but_malformed_complete_record_fails_closed() {
        let path = test_path("interrupted");
        let intake = JsonlIntake::open(&path).expect("the journal opens");
        let event = event("F:complete", b"complete", "1.0");
        intake
            .accept(&event)
            .await
            .expect("the first decision is durable");
        drop(intake);
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the journal reopens for the interrupted append");
        file.write_all(br#"{"record_type":"decision","id":"partial"#)
            .expect("the partial append writes");
        file.sync_all()
            .expect("the partial append reaches the disk");
        drop(file);

        let restarted = JsonlIntake::open(&path).expect("the incomplete final line is discarded");
        assert_eq!(records(&path).len(), 1);
        drop(restarted);
        std::fs::write(&path, b"{not-json}\n").expect("the malformed record writes");
        let error = JsonlIntake::open(&path).expect_err("a malformed complete line is fatal");
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        remove(&path);
    }
}
