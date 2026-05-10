-- Sidecar for the substrate-shipped audit Fact schema
-- `core/personality_config_changed_v1`. The schema is registered by
-- `FlavorRegistry::default()` at engine boot, so the query path will
-- LEFT JOIN this table for every memory query — it must exist even
-- before the first audit row lands.
--
-- Column names match the JSON keys produced by
-- `PersonalityConfigChangedV1`'s serde derive so a future ingest path
-- can populate rows via `jsonb_populate_record`. `before` and `after`
-- are SQL keywords; quote on access.
CREATE TABLE proxima_core.personality_config_changed_v1 (
    memory_id  uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    verb       text NOT NULL,
    "before"   jsonb,
    "after"    jsonb,
    subject    jsonb NOT NULL,
    caller     jsonb NOT NULL
);

CREATE INDEX idx_personality_config_changed_v1_subject_kind
    ON proxima_core.personality_config_changed_v1 ((subject ->> 'kind'));
CREATE INDEX idx_personality_config_changed_v1_subject_id
    ON proxima_core.personality_config_changed_v1 ((subject ->> 'id'));
CREATE INDEX idx_personality_config_changed_v1_verb
    ON proxima_core.personality_config_changed_v1 (verb);
