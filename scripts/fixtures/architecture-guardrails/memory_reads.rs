// Fixture for check_memory_reads: this file binds platform scope.
use crate::PgPlatformScope;

const UNMARKED: &str = "SELECT t FROM proxima_core.memory WHERE handle = $1";

const MARKED_SAME_LINE: &str = "SELECT t FROM proxima_core.memory /* FORGET-POLICY: live-only (fixture) */";

// FORGET-POLICY: includes-forgotten (fixture)
const MARKED_ABOVE: &str = "DELETE FROM proxima_core.memory WHERE t = $1";

const NOT_THE_SPINE: &str = "SELECT handle FROM proxima_core.memory_head";

const EMPTY_REASON: &str = "SELECT 1 FROM proxima_core.memory /* FORGET-POLICY: live-only () */";
