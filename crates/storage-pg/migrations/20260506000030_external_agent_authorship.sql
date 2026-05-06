ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_operator_kind_values_chk;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_operator_kind_values_chk
    CHECK (operator_kind IS NULL
           OR operator_kind IN ('FtoA', 'AtoP', 'ExternalAgent'));

ALTER TABLE proxima_core.edges
    DROP CONSTRAINT edges_authorship_kind_chk;

ALTER TABLE proxima_core.edges
    ADD CONSTRAINT edges_authorship_kind_chk
    CHECK (authorship_kind IN (
        'EventSource','OperatorFtoA','OperatorAtoP','OperatorAtoGoal',
        'PerspectiveLink','User','Engine','ExternalAgent'
    ));
