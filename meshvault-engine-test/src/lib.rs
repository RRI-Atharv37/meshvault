//! Test kit for MeshVault engines.
//!
//! - [`FakeEngine`] is an in-memory [`Engine`](meshvault_engine::Engine) for
//!   testing code that sits on top of an engine.
//! - [`contract`] holds the tests every `Engine` must pass. Add this crate as a
//!   dev-dependency and call [`engine_contract_tests!`] with a way to build
//!   your engine. A backend that cannot take arbitrary peer IDs or folder
//!   locations describes itself with [`Fixtures`].

#![deny(missing_docs)]

pub mod contract;
mod fake;
mod fixtures;

pub use fake::FakeEngine;
pub use fixtures::Fixtures;
