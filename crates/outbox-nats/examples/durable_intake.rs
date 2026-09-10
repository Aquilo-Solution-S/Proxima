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
//! 1. **Make the outcome durable before acknowledging.** The append is
//!    `write` + `sync_all` — a buffered write that had not reached the disk
//!    when the process died would leave an acknowledged event that no
//!    longer exists. Everything the consumer does with an ACK is built on
//!    the sink having already committed.
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

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use proxima_outbox_nats::{
    DurableIntake, Intake, IntakeError, NatsConsumerConfig, ReceivedEvent, ReferenceConsumer,
};
use tokio_util::sync::CancellationToken;

/// Where the sink keeps its journal. One JSON object per line: the outcome
/// this sink committed for one event.
const ENV_PATH: &str = "PROXIMA_INTAKE_PATH";
const DEFAULT_PATH: &str = "proxima-facts.jsonl";

/// A file-backed sink: append the outcome, flush it to the disk, and only
/// then let the consumer acknowledge.
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
    seen: HashSet<String>,
}

impl JsonlIntake {
    /// Open the journal and rebuild the dedup index from it.
    ///
    /// Rebuilding on start-up rather than trusting an in-memory set is the
    /// point: a restarted sink that forgot what it had recorded would
    /// duplicate every event still inside the broker's redelivery window.
    fn open(path: &Path) -> std::io::Result<Arc<Self>> {
        let seen = match File::open(path) {
            Ok(existing) => {
                let mut seen = HashSet::new();
                for line in BufReader::new(existing).lines() {
                    let line = line?;
                    if let Some(id) = id_of(&line) {
                        seen.insert(id);
                    }
                }
                seen
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(error) => return Err(error),
        };
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Arc::new(Self {
            state: Mutex::new(State { file, seen }),
            path: path.to_path_buf(),
        }))
    }

    /// Append one outcome and make it durable. Returns `false` when the id
    /// was already recorded — a repeat, not new work.
    fn record(&self, event: &ReceivedEvent, outcome: &str) -> std::io::Result<bool> {
        let mut state = self.state.lock().expect("the journal lock is not poisoned");
        if state.seen.contains(&event.id) {
            return Ok(false);
        }
        let line = serde_json::json!({
            "id": event.id,
            "subject": event.subject,
            "stream_sequence": event.stream_sequence,
            "delivered_count": event.delivered_count,
            "outcome": outcome,
            "type": event.envelope.event_type,
            "source": event.envelope.source,
            "dataschema": event.envelope.dataschema,
            "time": event.envelope.time,
            "owner": event.envelope.proximaowner,
            "data": event.envelope.data,
        });
        writeln!(state.file, "{line}")?;
        // The whole contract in one call: the consumer's ACK is only
        // allowed to mean anything because these bytes are on the disk
        // before it is sent.
        state.file.sync_all()?;
        state.seen.insert(event.id.clone());
        Ok(true)
    }
}

#[async_trait::async_trait]
impl DurableIntake for JsonlIntake {
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError> {
        // A sink decides on the envelope's declared type, never on the
        // subject: the subject is a routing convenience the broker can be
        // reconfigured to change, the type is part of the sealed event.
        if event.envelope.specversion != "1.0" {
            return Ok(Intake::Rejected {
                reason: format!(
                    "unsupported CloudEvents specversion {:?}",
                    event.envelope.specversion
                ),
            });
        }
        match self.record(event, "accepted") {
            Ok(true) => {
                tracing_line(&format!(
                    "recorded {} ({})",
                    event.id, event.envelope.event_type
                ));
                Ok(Intake::Accepted)
            }
            Ok(false) => {
                // Already durable. Acknowledging again is correct and
                // costs nothing; refusing would stall the stream.
                tracing_line(&format!("duplicate {} ignored", event.id));
                Ok(Intake::Accepted)
            }
            Err(error) => Err(IntakeError::new(format!(
                "could not append to {}: {error}",
                self.path.display()
            ))),
        }
    }
}

fn id_of(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("id")?
        .as_str()
        .map(ToOwned::to_owned)
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
