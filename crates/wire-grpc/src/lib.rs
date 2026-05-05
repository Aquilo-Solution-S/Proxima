//! gRPC wire bridge for proxima-core.
//!
//! Generated proto types live under `proto::proxima::v1`; conversion
//! to/from `proxima_core` types is in `convert::{from_proto, to_proto}`.

#[allow(
    clippy::doc_markdown,
    clippy::large_enum_variant,
    clippy::trivially_copy_pass_by_ref,
    clippy::wildcard_imports,
    clippy::needless_lifetimes,
    clippy::derive_partial_eq_without_eq,
    clippy::default_trait_access,
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::too_many_lines,
    clippy::similar_names,
    missing_debug_implementations
)]
pub mod proto {
    pub mod proxima {
        pub mod v1 {
            tonic::include_proto!("proxima.v1");
        }
    }
}

mod convert;
mod service;

pub use proto::proxima::v1 as pb;
pub use service::EngineGrpcServer;
