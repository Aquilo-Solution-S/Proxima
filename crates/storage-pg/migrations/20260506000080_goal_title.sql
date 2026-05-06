ALTER TABLE proxima_core.goals
    ADD COLUMN title text;

UPDATE proxima_core.goals
SET title = text
WHERE title IS NULL;

ALTER TABLE proxima_core.goals
    ALTER COLUMN title SET NOT NULL;
