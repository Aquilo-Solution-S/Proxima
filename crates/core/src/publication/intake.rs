//! The intake contract: what a host implements to receive captured Facts.
//!
//! The runtime drains the publication outbox into one host-provided
//! [`DurableIntake`]: it hands each captured event to
//! [`DurableIntake::accept`] and marks the record published only once
//! `accept` has returned. Delivery is at-least-once; deduplicate on
//! [`ReceivedEvent::id`] (docs/host-integration.md).

use std::collections::BTreeMap;

use bytes::Bytes;
use serde::de::Error as _;

use crate::MemoryId;
use crate::owner::OwnerId;
use crate::publication::SubjectPosition;
use crate::storage_ports::publication::ClaimedPublication;

/// The `CloudEvents` 1.0 structured envelope, as parsed from the captured bytes.
///
/// Deserialized rather than re-derived: the captured bytes are the contract.
/// An attribute this struct does not name lands in [`Self::extensions`]
/// rather than being refused — an intake pinned to today's exact attribute
/// set would break on the first extension a later release adds. A sink
/// verifying a signature still reads [`ReceivedEvent::raw`], the captured
/// bytes, and not a re-serialization of this view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CloudEventEnvelope {
    pub specversion: String,
    pub id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub datacontenttype: Option<String>,
    #[serde(default)]
    pub dataschema: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    /// The series the event is about (docs/host-integration.md).
    pub subject: Option<String>,
    /// The event's position in [`Self::subject`], 20 decimal digits (the
    /// `CloudEvents` sequence extension).
    pub sequence: Option<String>,
    #[serde(default)]
    pub proximaowner: Option<String>,
    #[serde(default)]
    pub proximamodel: Option<String>,
    /// Every context attribute the fields above do not name, in name order.
    /// Host-bound extension attributes arrive here; so does anything a
    /// later producer release adds. Values are whatever the `CloudEvents`
    /// JSON format allowed the producer to write.
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
    pub data: serde_json::Value,
}

/// One delivery of a captured event, with what a durable intake needs to be
/// idempotent.
///
/// Built by the runtime from a claimed outbox record, with
/// [`Self::from_record`]; a host reads it.
///
/// ```
/// use proxima_core::publication::{SubjectPosition, intake::ReceivedEvent};
/// fn position(event: &ReceivedEvent) -> SubjectPosition { event.position }
/// ```
///
/// ```compile_fail
/// use proxima_core::publication::intake::ReceivedEvent;
/// fn clear_position(event: &mut ReceivedEvent) { event.position = None; }
/// ```
#[derive(Debug, Clone)]
pub struct ReceivedEvent {
    /// The `CloudEvents` `id` (the bare memory UUID) — the dedup key.
    pub id: String,
    /// The owner the Fact was written under, from the outbox row.
    pub owner: OwnerId,
    /// How many times the outbox record has been claimed for delivery, this
    /// claim included. `> 1` means an earlier claim did not end in the mark:
    /// its delivery failed or timed out, the process stopped before the mark,
    /// or shutdown or a quiesce handed the claim back before `accept` was
    /// called. So it counts claims, not calls to the intake.
    pub delivered_count: u64,
    /// The captured envelope bytes, verbatim. A consumer verifying a
    /// signature checks these, not a re-serialization of [`Self::envelope`].
    pub raw: Bytes,
    /// [`Self::raw`], parsed.
    pub envelope: CloudEventEnvelope,
    /// The envelope's `subject` and `sequence`, typed: where this event
    /// stands among the events about the same series. Delivery does not
    /// follow it (docs/host-integration.md): keep the last `sequence` applied per
    /// `subject` and drop an event whose `sequence` is not greater. A missing
    /// or unpaired pair is malformed and refused by [`Self::from_record`].
    pub position: SubjectPosition,
}

/// How a [`ReceivedEvent::id`] is spelled: a Fact id in Proxima's canonical
/// wire form, or any other string.
///
/// This is a reading of the string, not a verdict on the event. `Fact`
/// proves syntax, not admission: it does not say a Fact with that id was
/// admitted, that its schema is listenable, or that the owner owns it.
/// Checking a signature on [`ReceivedEvent::raw`], and checking that id,
/// owner and `proximaowner` agree, stay with the intake.
///
/// The enum is closed, so a `match` over it must name `Opaque`; there is no
/// third kind of id in the outbox today.
///
/// ```
/// use proxima_core::publication::intake::EventIdentity;
///
/// fn describe(identity: &EventIdentity) -> &'static str {
///     match identity {
///         EventIdentity::Fact(_) => "spelled like a Fact id",
///         EventIdentity::Opaque(_) => "set aside",
///     }
/// }
/// # let _ = describe;
/// ```
///
/// Setting the `Opaque` case aside is not optional:
///
/// ```compile_fail
/// use proxima_core::publication::intake::EventIdentity;
///
/// fn fact_only(identity: &EventIdentity) {
///     match identity {
///         EventIdentity::Fact(_) => {}
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventIdentity {
    /// A lowercase hyphenated UUID, the spelling the producer emits.
    Fact(MemoryId),
    /// Every other id, byte for byte.
    Opaque(String),
}

impl ReceivedEvent {
    /// The typed reading of [`Self::id`], the dedup key. Stores nothing.
    ///
    /// `Fact(m)` only when `id` parses as a bare UUID and formatting `m` gives
    /// back exactly `id`: the UUID parser accepts upper-case, simple, braced
    /// and `urn:` UUID spellings, and the comparison is what refuses them.
    /// Every other string is `Opaque(id)` unchanged. The UUID version is not
    /// checked, because [`MemoryId`] does not constrain it.
    ///
    /// Two ids are equal exactly when their identities are equal, so
    /// deduplicating on the identity and on `id` agree.
    ///
    /// Syntax, not admission: `Fact` says the string is spelled like an
    /// admitted Fact's `t`, nothing more. See [`EventIdentity`].
    #[must_use]
    pub fn identity(&self) -> EventIdentity {
        match uuid::Uuid::parse_str(&self.id) {
            Ok(uuid) if uuid.to_string() == self.id => EventIdentity::Fact(MemoryId::new(uuid)),
            _ => EventIdentity::Opaque(self.id.clone()),
        }
    }
}

/// What an intake did with one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intake {
    Accepted,
    Rejected { reason: String },
}

/// A sink that could not decide. Distinct from
/// [`Intake::Rejected`] on purpose: rejection is a decision that was
/// durably recorded, this is a decision that was not made.
///
/// There is no retry hint: the runtime releases the record and retries it on
/// its own backoff (the poll interval, doubling up to 30 s while a pass
/// delivers nothing).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("durable intake failed: {message}")]
pub struct IntakeError {
    pub message: String,
}

impl IntakeError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The one thing a host implements to receive captured Facts.
///
/// The contract is the acknowledgement contract: `accept` MUST have made its
/// outcome durable before it returns, because the runtime marks the record
/// published as soon as it does, and a published record is never delivered
/// again. Both outcomes count — a rejection nobody recorded is
/// indistinguishable from an event that never arrived.
///
/// Deduplicate on [`ReceivedEvent::id`]. Redelivery is not an error case, it
/// is the normal cost of at-least-once: the process can stop between `accept`
/// returning and the record being marked.
///
/// `accept` is called one event at a time, and must return within a few
/// seconds (the runtime gives up on a call after 5 s and retries the event).
#[async_trait::async_trait]
pub trait DurableIntake: Send + Sync + std::fmt::Debug {
    /// Durably record an outcome for `event` and return which one.
    ///
    /// # Errors
    ///
    /// [`IntakeError`] when the outcome could NOT be made durable; the
    /// runtime then releases the record and delivers it again later.
    async fn accept(&self, event: &ReceivedEvent) -> Result<Intake, IntakeError>;
}

impl ReceivedEvent {
    /// The event for one delivery of a claimed outbox `record`.
    ///
    /// The one place the captured bytes are read: parsed here, once, and
    /// trusted from then on. `delivered_count` is the record's `attempts`,
    /// this claim included.
    ///
    /// # Errors
    ///
    /// The parse error when the captured bytes are not a `CloudEvents`
    /// envelope, or carry a `subject`/`sequence` pair not in the producer's
    /// spelling. The outbox seals them at capture, so this is corruption, not
    /// a case to deliver.
    pub fn from_record(record: &ClaimedPublication) -> Result<Self, serde_json::Error> {
        let envelope: CloudEventEnvelope = serde_json::from_slice(&record.envelope)?;
        let (Some(subject), Some(sequence)) = (&envelope.subject, &envelope.sequence) else {
            return Err(serde_json::Error::custom(
                "publication envelope is missing its subject/sequence position",
            ));
        };
        let position = SubjectPosition::parse(subject, sequence)
            .map_err(<serde_json::Error as serde::de::Error>::custom)?;
        Ok(Self {
            id: record.event_id.clone(),
            owner: crate::OwnerId::new(record.owner_id),
            delivered_count: u64::from(record.attempts),
            raw: Bytes::copy_from_slice(&record.envelope),
            envelope,
            position,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_ports::publication::ClaimToken;

    fn envelope_bytes(id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "specversion": "1.0",
            "id": id,
            "source": "urn:test",
            "type": "probe/listenable-v1",
            "subject": "0190a1b2-c3d4-8e5f-8a9b-00032e3f4a5b",
            "sequence": "00000000000000000001",
            "data": null,
        }))
        .expect("envelope serializes")
    }

    fn claimed(envelope: Vec<u8>) -> ClaimedPublication {
        ClaimedPublication {
            id: uuid::Uuid::nil(),
            event_id: "0190a1b2-c3d4-8e5f-8a9b-00032e3f4a5b".to_owned(),
            owner_id: uuid::Uuid::from_u128(7),
            schema_id: "probe/listenable".to_owned(),
            schema_version: 1,
            event_type: "probe/listenable-v1".to_owned(),
            envelope,
            digest: [0; 32],
            attempts: 2,
            claim: ClaimToken::new(uuid::Uuid::nil()),
            lease_expires_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_claimed_record_becomes_an_event_that_keeps_its_bytes_and_its_typed_owner() {
        let record = claimed(envelope_bytes("0190a1b2-c3d4-8e5f-8a9b-00032e3f4a5b"));
        let event = ReceivedEvent::from_record(&record).expect("a sealed envelope parses");
        assert_eq!(event.id, record.event_id);
        assert_eq!(
            event.owner,
            OwnerId::from(crate::GroupId::new(uuid::Uuid::from_u128(7)))
        );
        assert_eq!(
            event.delivered_count, 2,
            "the claim count is the delivery count"
        );
        assert_eq!(
            event.raw, record.envelope,
            "the captured bytes are kept verbatim"
        );
        assert_eq!(event.envelope.event_type, "probe/listenable-v1");
        assert_eq!(event.position.sequence.get(), 1);
    }

    fn envelope_with(subject: Option<&str>, sequence: Option<&str>) -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&envelope_bytes("0190a1b2-c3d4-8e5f-8a9b-00032e3f4a5b"))
                .expect("json");
        let object = value.as_object_mut().expect("object");
        match subject {
            Some(subject) => object.insert("subject".into(), subject.into()),
            None => object.remove("subject"),
        };
        match sequence {
            Some(sequence) => object.insert("sequence".into(), sequence.into()),
            None => object.remove("sequence"),
        };
        serde_json::to_vec(&value).expect("serializes")
    }

    #[test]
    fn a_position_is_read_once_and_a_malformed_one_is_corruption() {
        let subject = "0190a1b2-c3d4-8e5f-8a9b-00032e3f4a5b";
        let event = ReceivedEvent::from_record(&claimed(envelope_with(
            Some(subject),
            Some("00000000000000000003"),
        )))
        .expect("a sealed position parses");
        let position = event.position;
        assert_eq!(position.subject.to_string(), subject);
        assert_eq!(position.sequence.get(), 3);
        assert_eq!(
            event.envelope.extensions.get("sequence"),
            None,
            "sequence is a named attribute, not an unknown extension"
        );

        for (subject, sequence) in [
            (None, None),
            (Some(subject), None),
            (None, Some("00000000000000000003")),
            (Some(subject), Some("3")),
            (Some("not-a-uuid"), Some("00000000000000000003")),
        ] {
            let error = ReceivedEvent::from_record(&claimed(envelope_with(subject, sequence)))
                .expect_err("refused");
            assert_eq!(error.classify(), serde_json::error::Category::Data);
        }
    }

    #[test]
    fn bytes_that_are_not_an_envelope_are_refused_where_they_are_read() {
        let record = claimed(b"not json at all".to_vec());
        assert!(ReceivedEvent::from_record(&record).is_err());
    }

    const FACT_UUID: &str = "0190a1b2-c3d4-8e5f-8a9b-00032e3f4a5b";

    fn received(id: &str) -> ReceivedEvent {
        let raw = Bytes::from(envelope_bytes(id));
        let envelope = serde_json::from_slice(&raw).expect("envelope parses");
        ReceivedEvent {
            id: id.to_owned(),
            owner: OwnerId::from(crate::UserId::new(uuid::Uuid::nil())),
            delivered_count: 1,
            raw,
            envelope,
            position: SubjectPosition::new(uuid::Uuid::nil(), std::num::NonZeroU64::MIN),
        }
    }

    /// UUID spellings the parser reads whose form is not canonical.
    fn parseable_but_not_canonical() -> Vec<String> {
        let upper = FACT_UUID.to_uppercase();
        let simple = FACT_UUID.replace('-', "");
        vec![
            upper,
            simple,
            format!("{{{FACT_UUID}}}"),
            format!("urn:uuid:{FACT_UUID}"),
        ]
    }

    fn opaque_ids() -> Vec<String> {
        let upper = FACT_UUID.to_uppercase();
        let mut ids = parseable_but_not_canonical();
        ids.extend([
            format!("A:{FACT_UUID}"),
            format!("P:{FACT_UUID}"),
            format!("G:{FACT_UUID}"),
            format!("f:{FACT_UUID}"),
            format!("urn:uuid:{FACT_UUID}"),
            format!("F:{FACT_UUID}"),
            upper,
            String::new(),
            "F:".to_owned(),
            format!(" F:{FACT_UUID}"),
            format!("F:{FACT_UUID} "),
            format!("F: {FACT_UUID}"),
            format!("F:{FACT_UUID}\n"),
            format!("F:F:{FACT_UUID}"),
            "evt-0001".to_owned(),
        ]);
        ids
    }

    #[test]
    fn a_canonical_fact_id_reads_as_a_fact() {
        let id = FACT_UUID.to_owned();
        let uuid = uuid::Uuid::parse_str(FACT_UUID).expect("fixture uuid");
        let identity = received(&id).identity();
        let EventIdentity::Fact(memory) = identity else {
            panic!("a canonical Fact id must read as Fact, got {identity:?}");
        };
        assert_eq!(memory, MemoryId::new(uuid));
        assert_eq!(memory.into_inner().to_string(), id);
    }

    #[test]
    fn every_other_spelling_reads_as_opaque_and_unchanged() {
        for id in opaque_ids() {
            assert_eq!(
                received(&id).identity(),
                EventIdentity::Opaque(id.clone()),
                "{id:?}"
            );
        }
    }

    #[test]
    fn the_canonical_comparison_is_what_refuses_parseable_spellings() {
        // Without this the opaque cases above could be passing on the
        // core parser alone, and the re-format comparison would be dead code.
        for id in parseable_but_not_canonical() {
            assert!(
                uuid::Uuid::parse_str(&id).is_ok(),
                "{id:?} must parse, or the comparison is not exercised"
            );
        }
    }

    #[test]
    fn identities_are_equal_exactly_when_ids_are_equal() {
        let mut ids = opaque_ids();
        ids.push(FACT_UUID.to_owned());
        ids.push(uuid::Uuid::from_u128(1).to_string());
        ids.push(uuid::Uuid::nil().to_string());
        // The same id twice, so the `true` direction is exercised too.
        ids.push(FACT_UUID.to_owned());
        let events: Vec<_> = ids.iter().map(|id| received(id)).collect();
        for first in &events {
            for second in &events {
                assert_eq!(
                    first.identity() == second.identity(),
                    first.id == second.id,
                    "{:?} vs {:?}",
                    first.id,
                    second.id
                );
            }
        }
    }
}
