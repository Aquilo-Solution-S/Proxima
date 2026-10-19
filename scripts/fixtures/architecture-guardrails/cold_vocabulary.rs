// Self-test fixture for the #488 cold-path vocabulary rule. Not compiled.
// Five lines below must be flagged, one per banned spelling.
const A: &str = "SELECT t FROM proxima_core.cooled";
fn hydrate_memory() {}
const COLD_FORMAT: u8 = 1;
let key = cold_object_key(t);
let key = format!("cold/{t}");
// These stay allowed: the purge port keeps its names (D6).
let store: Arc<dyn ColdObjectStore> = blobs.cold_store();
sqlx::query("SELECT 1 FROM proxima_core.cold_purge_pending");
let outcome: ColdPurgeRetryOutcome = retry_cold_object_purges().await?;
// The object purge drains the queue.
