-- v0.0.19: the code schema's owner RLS through proxima_core.install_owner_rls.
--
-- Same classification as the v0.0.15 block, one policy change: that block
-- took an FK-parent table's first FK in creation order, so
-- execution_plan_v1 (an Abstraction sidecar) was keyed on
-- goal_activated_memory_id (a Fact) instead of its own t. A caller with Fact
-- read/write reached plan rows past the abstraction ceiling. The installer
-- keys every FK-parent table on the FK of its leading primary-key column.
SELECT proxima_core.install_owner_rls(
    'proxima_code',
    ARRAY['projection', 'repo_ingestion_runs', 'repos'],
    ARRAY[
        'acceptance_criteria_v1', 'acceptance_criterion_v1', 'acceptance_summary_v1',
        'acceptance_verification_v1', 'code_chunk_call_v1', 'code_chunk_v1',
        'commit_summary_v1', 'commit_v1', 'development_perspective_v1',
        'execution_plan_item_v1', 'execution_plan_v1', 'execution_result_v1',
        'file_revision_v1', 'test_requested_criterion_v1', 'test_requested_v1',
        'test_result_v1', 'work_assignment_v1', 'work_requested_v1'
    ],
    '{}',
    ARRAY['commit_summarizer_self_v1', 'engineer_self_v1']
);
