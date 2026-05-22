-- Per-wake observation-sandbox evidence on the workspace_run Fact sidecar.
--
-- `sandbox_image` / `sandbox_container` identify the disposable container the
-- wake ran in; `wake_branch` is the branch its changes landed on;
-- `transcript_blob_hash` / `network_log_blob_hash` are blake3 hex hashes
-- addressing the wake transcript and egress network log on local disk.
--
-- All columns are nullable: host-mode wakes (no sandbox) and rows written
-- before this migration carry NULL, so the change preserves existing rows.
-- No CHECK vocabularies — these are evidence/identity strings, not closed
-- enumerations (AGENTS.md inv. 22). The hash columns are backend-agnostic:
-- a hosted deployment can swap disk for object storage with no schema change.
ALTER TABLE proxima_core.workspace_run_v1
    ADD COLUMN sandbox_image text,
    ADD COLUMN sandbox_container text,
    ADD COLUMN wake_branch text,
    ADD COLUMN transcript_blob_hash text,
    ADD COLUMN network_log_blob_hash text;
