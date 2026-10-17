//! Test-only pause points: a test arms one for a single series handle, the
//! production path parks there once, and the test decides when it resumes.
//! Two barriers, no timing: `reached` returns once the path is parked, and
//! `resume` lets it continue.

use std::sync::{Arc, Mutex};

use tokio::sync::Barrier;
use uuid::Uuid;

/// In a Memory admission, right after its `t` is minted.
pub(crate) const AFTER_SERIES_MINT: &str = "after-series-mint";

/// In a group membership removal or demotion, inside its transaction and
/// right before the group-tree lock. Keyed by the group id.
pub(crate) const BEFORE_GROUP_LOCK: &str = "before-group-lock";

/// In an owner export, after the snapshot transaction committed and before
/// any cold object is read. Keyed by the owner id.
pub(crate) const AFTER_EXPORT_SNAPSHOT: &str = "after-export-snapshot";

type Armed = (&'static str, Uuid, Arc<Barrier>, Arc<Barrier>);

static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

/// The test's half of an armed pause point.
pub(crate) struct Gate {
    reached: Arc<Barrier>,
    resume: Arc<Barrier>,
}

impl Gate {
    /// Wait until the production path is parked at the point.
    pub(crate) async fn reached(&self) {
        self.reached.wait().await;
    }

    /// Let the parked path continue.
    pub(crate) async fn resume(&self) {
        self.resume.wait().await;
    }
}

/// Arm `point` for the next path that reaches it with `key`. It fires once.
pub(crate) fn arm(point: &'static str, key: Uuid) -> Gate {
    let reached = Arc::new(Barrier::new(2));
    let resume = Arc::new(Barrier::new(2));
    ARMED
        .lock()
        .expect("pause point registry")
        .push((point, key, reached.clone(), resume.clone()));
    Gate { reached, resume }
}

/// Park here if a test armed `point` for `key`; otherwise return at once.
pub(crate) async fn reach(point: &'static str, key: Uuid) {
    let armed = {
        let mut armed = ARMED.lock().expect("pause point registry");
        armed
            .iter()
            .position(|(p, k, _, _)| *p == point && *k == key)
            .map(|index| armed.swap_remove(index))
    };
    if let Some((_, _, reached, resume)) = armed {
        reached.wait().await;
        resume.wait().await;
    }
}
