//! Wake the in-process background drains when a write commits.
//!
//! The outbox publisher and the embedding worker claim queue rows that
//! writes enqueue. Instead of claiming on a short fixed interval, they sleep
//! until a write commits in this process, and otherwise poll on a fallback
//! interval that backs off while their claims come back empty. A storage
//! adapter calls [`write_committed`] after it commits a write; a drain holds a
//! [`WriteWake`] and awaits [`WriteWake::changed`] beside its fallback sleep.
//!
//! The signal is process-wide and carries no data: any committed write wakes
//! every drain in the process, and a drain that wakes for a write it has no
//! work for pays one empty claim. A wake is a hint, never a source of truth.
//! Writes committed by another process (another replica, a command-line
//! tool) are not signalled; the fallback poll finds them.

use std::sync::LazyLock;

use tokio::sync::watch;

/// The process's one signal; [`Writes`] instances other than this one exist
/// only in tests, so a test never sees another test's writes.
static WRITES: LazyLock<Writes> = LazyLock::new(Writes::new);

/// Signal that a write committed. Call it after the commit returned, never
/// before: a drain woken earlier claims before the rows are visible and goes
/// back to sleep.
pub fn write_committed() {
    WRITES.committed();
}

/// A write-committed signal: an epoch every commit bumps.
struct Writes(watch::Sender<u64>);

impl Writes {
    fn new() -> Self {
        Self(watch::channel(0).0)
    }

    fn committed(&self) {
        self.0.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    fn subscribe(&self) -> WriteWake {
        WriteWake {
            writes: self.0.subscribe(),
        }
    }
}

/// One drain's view of [`write_committed`] signals.
#[derive(Debug)]
pub struct WriteWake {
    writes: watch::Receiver<u64>,
}

impl WriteWake {
    /// Subscribe. Writes committed before this call do not wake it.
    #[must_use]
    pub fn subscribe() -> Self {
        WRITES.subscribe()
    }

    /// Resolve once a write has committed since the last call (or since
    /// [`Self::subscribe`]). Several writes in between resolve it once.
    pub async fn changed(&mut self) {
        if self.writes.changed().await.is_err() {
            // The process signal is a static and never dropped.
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Writes;
    use std::time::Duration;

    // Each test owns its signal: the process one is shared by every test
    // running in this binary, whose writes would wake (or race) these.

    #[tokio::test]
    async fn a_commit_after_subscribing_wakes() {
        let writes = Writes::new();
        let mut wake = writes.subscribe();
        writes.committed();
        writes.committed();
        tokio::time::timeout(Duration::from_secs(1), wake.changed())
            .await
            .expect("a committed write wakes the subscriber");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), wake.changed())
                .await
                .is_err(),
            "two writes before one wait wake it once"
        );
    }

    #[tokio::test]
    async fn nothing_committed_since_subscribing_does_not_wake() {
        let writes = Writes::new();
        writes.committed();
        let mut wake = writes.subscribe();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), wake.changed())
                .await
                .is_err(),
            "a write committed before subscribing is not a wake"
        );
    }

    #[tokio::test]
    async fn the_process_signal_wakes_a_subscriber() {
        let mut wake = super::WriteWake::subscribe();
        super::write_committed();
        tokio::time::timeout(Duration::from_secs(1), wake.changed())
            .await
            .expect("write_committed wakes a WriteWake");
    }
}
