-- M5.5 — Typed edge sidecar for code/calls relation.
-- One row per `proxima-code/calls` edge in `proxima_core.edges`.
-- Keyed on edge_id FK to the edge row.

CREATE TABLE proxima_code.code_calls_v1 (
    edge_id            uuid PRIMARY KEY
                         REFERENCES proxima_core.edges(edge_id),
    callsite_byte_start int  NOT NULL,
    callsite_byte_end   int  NOT NULL,
    callee_name         text NOT NULL,
    is_dynamic          bool NOT NULL,
    CONSTRAINT code_calls_v1_byte_range_chk
        CHECK (callsite_byte_end >= callsite_byte_start)
);
