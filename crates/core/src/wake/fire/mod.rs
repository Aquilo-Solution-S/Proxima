//! Wake fire submodule - split from the original fire.rs for improved compile times.

pub mod finalize;
pub mod input;
pub mod outcome;
pub mod recipe;
pub mod resolve;

#[allow(clippy::module_inception)]
pub mod fire;

// Re-export the main entry point and types for backward compatibility
pub use fire::fire_wake_entry;
pub use input::FireWakeEntryInput;
pub use outcome::WakeInvocationFinalizeOutcome;
pub use resolve::ResolvedTarget;
