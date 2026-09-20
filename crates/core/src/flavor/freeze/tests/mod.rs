//! Freeze tests: one module per partition of [`super`], plus the smoke
//! test that the registry a real binary composes still freezes.

mod cross_checks;
mod dispatcher;
mod fixtures;
mod leg_fixtures;
mod projection_fixtures;
mod scopes;

use crate::FlavorRegistry;

/// The counterpart to every refusal in [`cross_checks`]: the registry as
/// the binary actually composes it, with core's contract in place, freezes.
///
/// The failure surfaces the error. Each case in that table is a contract
/// cross-check with its own message, and this is the test that fires when
/// one of them fires on the SHIPPED registry — the single most useful
/// moment to be told which. A bare `assert!(..is_ok())` reports "assertion
/// failed" and throws the reason away.
#[test]
fn the_shipped_registry_freezes() {
    if let Err(err) = FlavorRegistry::new().try_freeze() {
        panic!("the registry the binary composes must freeze: {err}");
    }
}
