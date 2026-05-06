use std::path::PathBuf;
use std::sync::OnceLock;

static SESSION_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn dir() -> Option<&'static PathBuf> {
    SESSION_DIR
        .get_or_init(|| {
            std::env::var_os("PROXIMA_PERF_SESSION_DIR")
                .map(PathBuf::from)
                .filter(|p| p.exists())
        })
        .as_ref()
}
