//! In-process pub/sub for ingestion run progress keyed by `(Owner, RepoId)`.
//!
//! The persisted DB row is authoritative; this hub only caches the latest
//! stage snapshot for late subscribers and fans out live progress.

use std::collections::HashMap;
use std::sync::Arc;

use proxima_code::RepoIngestionRun;
use proxima_core::Owner;
use tokio::sync::{Mutex, RwLock, broadcast};
use uuid::Uuid;

use crate::commands::{IndexReportTs, IngestProgressTs, RepoIngestEventTs};

const CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone)]
pub struct RepoIngestHub {
    inner: Arc<RwLock<HashMap<Key, Arc<ChannelState>>>>,
}

type Key = (Owner, Uuid);

#[derive(Debug)]
struct ChannelState {
    snapshot: Mutex<Option<RepoIngestionRun>>,
    sender: broadcast::Sender<RepoIngestEventTs>,
}

impl RepoIngestHub {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn ensure(&self, owner: Owner, repo_id: Uuid) -> Arc<ChannelState> {
        let key = (owner, repo_id);
        {
            let map = self.inner.read().await;
            if let Some(ch) = map.get(&key) {
                return ch.clone();
            }
        }
        let mut map = self.inner.write().await;
        map.entry(key)
            .or_insert_with(|| {
                let (sender, _rx) = broadcast::channel(CHANNEL_CAPACITY);
                Arc::new(ChannelState {
                    snapshot: Mutex::new(None),
                    sender,
                })
            })
            .clone()
    }

    pub async fn snapshot(&self, owner: &Owner, repo_id: Uuid) -> Option<RepoIngestionRun> {
        let key = (owner.clone(), repo_id);
        let map = self.inner.read().await;
        let ch = map.get(&key)?;
        ch.snapshot.lock().await.clone()
    }

    /// Register a receiver and return the current cached snapshot.
    ///
    /// The receiver is created *before* the snapshot is read so that any
    /// event published concurrently with this call lands in the new
    /// receiver's queue rather than being dropped. The caller may see a
    /// duplicate Snapshot event if a `publish_snapshot` interleaves, but
    /// will never miss a terminal event for a short-lived run.
    pub async fn subscribe(
        &self,
        owner: Owner,
        repo_id: Uuid,
    ) -> (
        Option<RepoIngestionRun>,
        broadcast::Receiver<RepoIngestEventTs>,
    ) {
        let ch = self.ensure(owner, repo_id).await;
        let rx = ch.sender.subscribe();
        let snap = ch.snapshot.lock().await.clone();
        (snap, rx)
    }

    pub async fn publish_snapshot(&self, owner: Owner, run: RepoIngestionRun) {
        let ch = self.ensure(owner, run.repo_id).await;
        *ch.snapshot.lock().await = Some(run.clone());
        let _ = ch.sender.send(RepoIngestEventTs::Snapshot(run.into()));
    }

    pub async fn publish_progress(&self, owner: Owner, repo_id: Uuid, progress: IngestProgressTs) {
        let ch = self.ensure(owner, repo_id).await;
        let _ = ch.sender.send(RepoIngestEventTs::Progress(progress));
    }

    pub async fn publish_done(&self, owner: Owner, repo_id: Uuid, report: IndexReportTs) {
        let ch = self.ensure(owner, repo_id).await;
        let _ = ch.sender.send(RepoIngestEventTs::Done(report));
    }

    pub async fn publish_error(&self, owner: Owner, repo_id: Uuid, message: String) {
        let ch = self.ensure(owner, repo_id).await;
        let _ = ch.sender.send(RepoIngestEventTs::Error { message });
    }
}

impl Default for RepoIngestHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::IndexReportTs;
    use proxima_code::{RunStage, RunStatus};
    use proxima_core::{OrgId, Principal, UserId};
    use std::time::Duration;
    use tokio::sync::broadcast::error::TryRecvError;

    fn test_owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(Uuid::nil())),
            org_id: OrgId::new(Uuid::nil()),
        }
    }

    fn running_run(repo_id: Uuid) -> RepoIngestionRun {
        let now = time::OffsetDateTime::now_utc();
        RepoIngestionRun {
            run_id: Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_beef),
            repo_id,
            status: RunStatus::Running,
            stage: RunStage::Facts,
            commits_emitted: 0,
            files_emitted: 0,
            chunks_emitted: 0,
            chunks_reused: 0,
            chunks_tombstoned: 0,
            ast_edges_emitted: 0,
            abstractions_emitted: 0,
            embeddings_landed: 0,
            citations_emitted: 0,
            error_message: None,
            started_at: now,
            updated_at: now,
            finished_at: None,
        }
    }

    fn empty_report() -> IndexReportTs {
        IndexReportTs {
            commits_emitted: 0,
            commits_replayed: 0,
            files_present_emitted: 0,
            files_tombstoned: 0,
            chunks_emitted: 0,
            chunks_reused: 0,
            chunks_tombstoned: 0,
        }
    }

    /// Regression: a terminal event published immediately after
    /// `subscribe` returns must reach the new receiver. The prior
    /// implementation cloned the snapshot before calling
    /// `sender.subscribe()`, leaving a window where a `publish_done`
    /// could fire without any subscribers and be silently dropped.
    #[tokio::test]
    async fn subscribe_receives_event_published_immediately_after() {
        let hub = RepoIngestHub::new();
        let owner = test_owner();
        let repo_id = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_dead);

        hub.publish_snapshot(owner.clone(), running_run(repo_id))
            .await;

        let (snap, mut rx) = hub.subscribe(owner.clone(), repo_id).await;
        assert!(snap.is_some(), "cached snapshot should be returned");

        hub.publish_done(owner, repo_id, empty_report()).await;

        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("rx.recv timed out — Done event was dropped");
        assert!(matches!(ev, Ok(RepoIngestEventTs::Done(_))));
    }

    /// Subscribing when no snapshot exists yet still yields a working
    /// receiver — the caller's DB fallback handles the snapshot, but
    /// subsequent driver events must arrive.
    #[tokio::test]
    async fn subscribe_without_cached_snapshot_still_delivers_events() {
        let hub = RepoIngestHub::new();
        let owner = test_owner();
        let repo_id = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_dead);

        let (snap, mut rx) = hub.subscribe(owner.clone(), repo_id).await;
        assert!(snap.is_none());
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        hub.publish_snapshot(owner, running_run(repo_id)).await;

        let ev = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("rx.recv timed out");
        assert!(matches!(ev, Ok(RepoIngestEventTs::Snapshot(_))));
    }
}
