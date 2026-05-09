//! Phase 1 stub for the Code flavor's workspace runner.
//!
//! Phase 3 fills this in with worktree creation, recipe rendering,
//! Fact emission, and the WorkspaceRunnerSource event source. Until
//! then, `prepare` returns `WorkspaceRunnerError::Unimplemented` so
//! `wake/fire.rs` reproduces today's `failure_reason =
//! "workspace_mode_not_yet_implemented"` finalize state.

use proxima_core::{
    WorkspaceOutcome, WorkspacePrepareInput, WorkspacePreparedRun, WorkspaceRunRecord,
    WorkspaceRunner, WorkspaceRunnerError,
};

#[derive(Debug, Default)]
pub struct CodeWorkspaceRunner;

#[async_trait::async_trait]
impl WorkspaceRunner for CodeWorkspaceRunner {
    async fn prepare(
        &self,
        _input: WorkspacePrepareInput<'_>,
    ) -> Result<WorkspacePreparedRun, WorkspaceRunnerError> {
        Err(WorkspaceRunnerError::Unimplemented)
    }

    async fn finalize(
        &self,
        _prepared: WorkspacePreparedRun,
        _outcome: WorkspaceOutcome,
    ) -> Result<WorkspaceRunRecord, WorkspaceRunnerError> {
        // finalize is unreachable in Phase 1 because prepare always
        // returns Unimplemented. Keep the contract honest in case a
        // future call site bypasses prepare.
        Err(WorkspaceRunnerError::Unimplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Structural assertion: CodeWorkspaceRunner satisfies the
    /// trait bound. Avoids constructing real Owner/WakeToken values
    /// (WakeToken is minted via the engine's WakeTokenStore, not a
    /// public constructor - see crates/core/src/wake/token_store.rs).
    /// Phase 3 lands the behaviour-level tests against a real PG
    /// harness when the runner does meaningful work.
    #[test]
    fn runner_implements_trait() {
        fn assert_impl<R: WorkspaceRunner>(_: &R) {}
        let runner = CodeWorkspaceRunner;
        assert_impl(&runner);
    }
}
