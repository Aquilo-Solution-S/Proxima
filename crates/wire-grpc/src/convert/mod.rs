//! Conversion between proto wire types and `proxima_core` types.
//!
//! All fallible conversions return `Result<_, tonic::Status>`. Helper
//! converters are emitted exhaustively across the verb surface; some
//! are not yet referenced by `service.rs` (`Subscribe` / `EventIngest`
//! Goal-row paths land in A2.3+) — they're kept here as the contract
//! changeover point so the whole conversion table is in one module.

#![allow(
    dead_code,
    clippy::needless_pass_by_value,
    clippy::unnecessary_wraps,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::too_many_lines
)]

pub mod errors;
pub mod inference;
pub mod messages;
pub mod primitives;
pub mod refs;
pub mod rows;

#[allow(unused_imports)]
pub use errors::*;
#[allow(unused_imports)]
pub use inference::*;
#[allow(unused_imports)]
pub use messages::*;
#[allow(unused_imports)]
pub use primitives::*;
#[allow(unused_imports)]
pub use refs::*;
#[allow(unused_imports)]
pub use rows::*;
