use std::path::Path;

use tracing_chrome::{ChromeLayerBuilder, FlushGuard};
use tracing_subscriber::Registry;

pub fn layer(
    session_dir: &Path,
) -> (
    impl tracing_subscriber::Layer<Registry> + Send + Sync + 'static,
    FlushGuard,
) {
    let path = session_dir.join("engine.json");
    let (layer, guard) = ChromeLayerBuilder::new()
        .file(path)
        .include_args(true)
        .build();
    (layer, guard)
}
