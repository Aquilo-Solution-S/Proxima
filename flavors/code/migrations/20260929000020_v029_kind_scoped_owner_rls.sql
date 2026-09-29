-- v0.0.29: the code schema follows the kind rule (core 0023).
--
-- projection holds Fact (commit_v1) and Abstraction (commit_summary_v1,
-- code_chunk_v1) rows, and the two Self sidecars hold Perspective rows. As
-- memory_owner_tables they follow the parent memory's kind; before this file
-- any scope on the Fact lists read and wrote them.
SELECT proxima_core.install_owner_rls(
    'proxima_code',
    ARRAY['repo_ingestion_runs', 'repos'],
    ARRAY[
        'acceptance_criteria_v1', 'acceptance_criterion_v1', 'acceptance_summary_v1',
        'acceptance_verification_v1', 'code_chunk_call_v1', 'code_chunk_v1',
        'commit_summary_v1', 'commit_v1', 'development_perspective_v1',
        'execution_plan_item_v1', 'execution_plan_v1', 'execution_result_v1',
        'file_revision_v1', 'test_requested_criterion_v1', 'test_requested_v1',
        'test_result_v1', 'work_assignment_v1', 'work_requested_v1'
    ],
    '{}',
    ARRAY['commit_summarizer_self_v1', 'engineer_self_v1', 'projection']
);
