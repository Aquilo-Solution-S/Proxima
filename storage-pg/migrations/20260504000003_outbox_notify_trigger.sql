-- M2.6 — emit pg_notify on every change_event insert so the
-- Rust outbox publisher can tail without polling.

CREATE FUNCTION proxima_core.notify_change_event() RETURNS trigger AS $$
BEGIN
    PERFORM pg_notify('proxima_change_event', NEW.seq::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER change_event_notify_trg
    AFTER INSERT ON proxima_core.change_event
    FOR EACH ROW EXECUTE FUNCTION proxima_core.notify_change_event();
