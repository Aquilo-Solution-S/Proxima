CREATE TABLE proxima_agent_memory.utterance_v1 (
    memory_id uuid NOT NULL,
    speaker text NOT NULL,
    conversation_id text NOT NULL,
    text text NOT NULL,
    CONSTRAINT utterance_v1_conversation_id_nonempty CHECK ((length(btrim(conversation_id)) > 0)),
    CONSTRAINT utterance_v1_text_nonempty CHECK ((length(btrim(text)) > 0))
);

ALTER TABLE ONLY proxima_agent_memory.utterance_v1
    ADD CONSTRAINT utterance_v1_pkey PRIMARY KEY (memory_id);

ALTER TABLE ONLY proxima_agent_memory.utterance_v1
    ADD CONSTRAINT utterance_v1_memory_id_fkey FOREIGN KEY (memory_id) REFERENCES proxima_core.memories(memory_id);
