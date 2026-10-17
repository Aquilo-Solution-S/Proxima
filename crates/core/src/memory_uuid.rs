//! Memory UUID layout and the process-wide monotonic generator.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

const MAX_MILLIS: u64 = (1 << 48) - 1;
const MAX_COUNTER: u16 = (1 << 12) - 1;
static GENERATOR: Mutex<Generator> = Mutex::new(Generator { previous: None });

#[derive(Debug, Default)]
struct Generator {
    previous: Option<(u64, u16)>,
}

impl Generator {
    fn mint(
        &mut self,
        clock_millis: u64,
        mut random: [u8; 16],
        store_number: u16,
        floor: Option<Uuid>,
    ) -> Option<Uuid> {
        let floor = floor.map(|floor| {
            let bytes = floor.as_bytes();
            (
                timestamp_millis(floor),
                u16::from_be_bytes([bytes[6], bytes[7]]) & MAX_COUNTER,
            )
        });
        let previous = self.previous.max(floor);
        let seed = u16::from_be_bytes([random[6], random[7]]) & MAX_COUNTER;
        let (millis, counter) = match previous {
            Some((millis, counter)) if clock_millis <= millis => {
                if counter == MAX_COUNTER {
                    (millis + 1, seed)
                } else {
                    (millis, counter + 1)
                }
            }
            Some(_) | None => (clock_millis, seed),
        };
        if millis > MAX_MILLIS {
            return None;
        }
        self.previous = Some((millis, counter));
        random[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
        random[6..8].copy_from_slice(&(0x8000 | counter).to_be_bytes());
        random[8] = (random[8] & 0x3f) | 0x80;
        random[10..12].copy_from_slice(&store_number.to_be_bytes());
        Some(Uuid::from_bytes(random))
    }
}

/// Mint a time-ordered `UUIDv8` carrying the configured store number.
///
/// All calls in this process share one timestamp and counter. Frozen or
/// regressing clocks keep that order; counter exhaustion advances logical
/// time by one millisecond without waiting for the clock.
///
/// # Panics
///
/// Panics if the system clock precedes the Unix epoch, or if the timestamp
/// exceeds the layout's 48-bit millisecond range.
#[must_use]
pub fn new_memory_uuid(store_number: u16) -> Uuid {
    mint_memory_uuid(store_number, None).expect("memory UUID timestamp exceeds 48 bits")
}

/// Mint a memory UUID strictly after an existing revision's UUID.
///
/// The revision's timestamp and counter are a floor for the same process-wide
/// generator as [`new_memory_uuid`]. Advancing beyond both fields keeps the
/// result greater regardless of the floor's store number or random bits, even
/// when another process's clock is ahead. Later ordinary mints retain that floor.
///
/// Returns `None` if advancing the floor or the clock would exceed the layout's
/// 48-bit timestamp range. A refused mint leaves the generator state unchanged.
///
/// # Panics
///
/// Panics if the system clock precedes the Unix epoch.
#[must_use]
pub fn new_memory_uuid_after(store_number: u16, floor: Uuid) -> Option<Uuid> {
    mint_memory_uuid(store_number, Some(floor))
}

fn mint_memory_uuid(store_number: u16, floor: Option<Uuid>) -> Option<Uuid> {
    let mut generator = GENERATOR
        .lock()
        .expect("memory UUID generator mutex poisoned");
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("memory UUID clock precedes Unix epoch")
        .as_millis();
    generator.mint(
        u64::try_from(millis).expect("memory UUID timestamp exceeds 48 bits"),
        *Uuid::new_v4().as_bytes(),
        store_number,
        floor,
    )
}

/// Read the issuing store's number from a memory UUID.
#[must_use]
pub const fn memory_store_number(id: Uuid) -> u16 {
    let bytes = id.as_bytes();
    u16::from_be_bytes([bytes[10], bytes[11]])
}

pub(crate) fn created_at(id: Uuid) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(timestamp_millis(id)) * 1_000_000)
        .expect("48-bit millisecond timestamp is representable")
}

fn timestamp_millis(id: Uuid) -> u64 {
    let bytes = id.as_bytes();
    u64::from_be_bytes([
        0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn randomness(counter: u16) -> [u8; 16] {
        let mut bytes = [0xff; 16];
        bytes[6..8].copy_from_slice(&counter.to_be_bytes());
        bytes
    }

    #[test]
    fn layout_carries_timestamp_version_variant_store_and_randomness() {
        let mut generator = Generator::default();
        let millis = 0x0192_f3a4_7b1c;
        let id = generator
            .mint(millis, randomness(0xa2e), 3, None)
            .expect("mint fixture");
        assert_eq!(id.to_string(), "0192f3a4-7b1c-8a2e-bfff-0003ffffffff");
        assert_eq!(memory_store_number(id), 3);
        assert_eq!(id.get_version_num(), 8);
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
        assert_eq!(
            created_at(id).unix_timestamp_nanos(),
            i128::from(millis) * 1_000_000
        );
        assert_eq!(crate::MemoryId::new(id).created_at(), created_at(id));
        for store in [0, 1, u16::MAX] {
            assert_eq!(
                memory_store_number(
                    generator
                        .mint(millis, [0; 16], store, None)
                        .expect("mint fixture")
                ),
                store
            );
        }
    }

    #[test]
    fn frozen_and_regressing_clocks_increment_until_logical_time_advances() {
        let mut generator = Generator::default();
        let first = generator
            .mint(100, randomness(MAX_COUNTER - 1), 3, None)
            .expect("mint fixture");
        let frozen = generator
            .mint(100, randomness(9), 5, None)
            .expect("mint fixture");
        let overflow = generator
            .mint(99, randomness(7), 3, None)
            .expect("mint fixture");
        let regressed = generator
            .mint(99, randomness(0), 3, None)
            .expect("mint fixture");
        let recovered = generator
            .mint(102, randomness(2), 3, None)
            .expect("mint fixture");
        let ids = [first, frozen, overflow, regressed, recovered];
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(frozen.as_bytes()[6..8], [0x8f, 0xff]);
        assert_eq!(overflow.as_bytes()[6..8], [0x80, 7]);
        assert_eq!(regressed.as_bytes()[6..8], [0x80, 8]);
        assert_eq!(recovered.as_bytes()[6..8], [0x80, 2]);
        assert_eq!(created_at(overflow).unix_timestamp_nanos(), 101_000_000);
    }

    #[test]
    fn an_observed_revision_floors_the_generator_across_clocks_and_stores() {
        for counter in [0, MAX_COUNTER - 1, MAX_COUNTER] {
            let floor = Generator::default()
                .mint(200, randomness(counter), u16::MAX, None)
                .expect("mint fixture");
            let mut generator = Generator::default();
            let earlier = generator.mint(100, [0; 16], 1, None).expect("mint fixture");
            let revision = generator
                .mint(99, [0; 16], 0, Some(floor))
                .expect("mint fixture");
            let following = generator.mint(98, [0; 16], 1, None).expect("mint fixture");
            assert!(earlier < floor);
            assert!(floor < revision);
            assert!(revision < following);
            assert_eq!(revision.get_version_num(), 8);
            assert_eq!(revision.get_variant(), uuid::Variant::RFC4122);
            assert_eq!(memory_store_number(revision), 0);
            assert_eq!(
                timestamp_millis(revision),
                200 + u64::from(counter == MAX_COUNTER)
            );
        }
    }

    #[test]
    fn an_older_revision_floor_does_not_rewind_process_state() {
        let floor = Generator::default()
            .mint(100, randomness(MAX_COUNTER), 1, None)
            .expect("mint fixture");
        let mut generator = Generator::default();
        let earlier = generator
            .mint(200, randomness(12), 2, None)
            .expect("mint fixture");
        let revision = generator
            .mint(99, [0; 16], 1, Some(floor))
            .expect("mint fixture");
        assert!(earlier < revision);
        assert_eq!(timestamp_millis(revision), 200);
        assert_eq!(revision.as_bytes()[6..8], [0x80, 13]);
    }

    #[test]
    fn an_exhausted_revision_floor_is_refused_without_changing_process_state() {
        let floor = Generator::default()
            .mint(MAX_MILLIS, randomness(MAX_COUNTER), 1, None)
            .expect("mint fixture");
        let mut generator = Generator::default();
        let earlier = generator.mint(100, [0; 16], 1, None).expect("mint fixture");
        let previous = generator.previous;
        assert!(generator.mint(100, [0; 16], 1, Some(floor)).is_none());
        assert_eq!(generator.previous, previous);
        let following = generator.mint(100, [0; 16], 1, None).expect("mint fixture");
        assert!(earlier < following);
        assert_eq!(timestamp_millis(following), 100);
    }

    #[test]
    fn full_timestamp_range_is_representable() {
        let mut generator = Generator::default();
        for millis in [0, MAX_MILLIS] {
            let id = generator
                .mint(millis, [0; 16], 0, None)
                .expect("mint fixture");
            assert_eq!(
                created_at(id).unix_timestamp_nanos(),
                i128::from(millis) * 1_000_000
            );
        }
    }

    #[test]
    fn clock_outside_timestamp_range_is_refused() {
        assert!(
            Generator::default()
                .mint(MAX_MILLIS + 1, [0; 16], 0, None)
                .is_none()
        );
    }

    #[test]
    fn exhausted_timestamp_range_is_refused() {
        let mut generator = Generator::default();
        assert!(
            generator
                .mint(MAX_MILLIS, randomness(MAX_COUNTER), 0, None)
                .is_some()
        );
        assert!(generator.mint(MAX_MILLIS, [0; 16], 0, None).is_none());
    }

    #[test]
    fn consecutive_calls_across_stores_are_ordered() {
        let ids: Vec<_> = [u16::MAX, 0, 7, 3]
            .into_iter()
            .map(new_memory_uuid)
            .collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn concurrent_calls_share_one_order_across_store_numbers() {
        let results: Vec<_> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|store| {
                    scope.spawn(move || {
                        let ids: Vec<_> = (0..256).map(|_| new_memory_uuid(store)).collect();
                        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
                        assert!(ids.iter().all(|id| memory_store_number(*id) == store));
                        ids
                    })
                })
                .collect();
            threads
                .into_iter()
                .flat_map(|thread| thread.join().expect("minting thread"))
                .collect()
        });
        let mut unique = results;
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 8 * 256);
    }
}
