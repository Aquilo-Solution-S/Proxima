ALTER TABLE proxima_code.workspace_decision_v1
    DROP CONSTRAINT workspace_decision_v1_decision_chk;

ALTER TABLE proxima_code.workspace_decision_v1
    ADD CONSTRAINT workspace_decision_v1_decision_chk
        CHECK (decision IN ('rejected', 'retry_requested', 'accepted', 'merged'));
