-- Source-batch F→A idempotency is per invocation, not just per
-- (batch, operator). Prompt/model/personality changes are new runs.

ALTER TABLE proxima_core.source_batch_f2a
    ADD COLUMN model_id text,
    ADD COLUMN personality_id text,
    ADD COLUMN personality_state_hash bytea;

UPDATE proxima_core.source_batch_f2a f2a
SET model_id = m.model_id,
    personality_id = m.personality_id,
    personality_state_hash = m.personality_state_hash
FROM proxima_core.memories m
WHERE f2a.head_memory_id = m.memory_id;

UPDATE proxima_core.source_batch_f2a
SET model_id = COALESCE(model_id, 'unknown'),
    personality_id = COALESCE(personality_id, 'unknown'),
    personality_state_hash = COALESCE(
        personality_state_hash,
        decode(repeat('00', 32), 'hex')
    );

ALTER TABLE proxima_core.source_batch_f2a
    ALTER COLUMN model_id SET NOT NULL,
    ALTER COLUMN personality_id SET NOT NULL,
    ALTER COLUMN personality_state_hash SET NOT NULL,
    ADD CONSTRAINT source_batch_f2a_personality_state_hash_chk
        CHECK (octet_length(personality_state_hash) = 32);

ALTER TABLE proxima_core.source_batch_f2a
    DROP CONSTRAINT source_batch_f2a_pkey,
    ADD PRIMARY KEY (
        batch_id,
        operator_id,
        prompt_version,
        model_id,
        personality_id,
        personality_state_hash
    );
