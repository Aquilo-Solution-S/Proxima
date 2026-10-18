-- Forget is a state (#488): a forgotten Memory keeps its row and is hidden
-- from owner reads by the read policy until it is restored or erased.
ALTER TABLE proxima_core.memory ADD COLUMN forgotten_at timestamptz NULL;
