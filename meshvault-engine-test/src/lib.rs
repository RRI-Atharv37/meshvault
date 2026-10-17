//! Test kit for MeshVault engines.
//!
//! - [`FakeEngine`] is an in-memory [`Engine`](meshvault_engine::Engine) for
//!   testing code that sits on top of an engine.
//! - [`contract`] holds the tests every `Engine` must pass. Add this crate as a
//!   dev-dependency and call [`engine_contract_tests!`] with a way to build
//!   your engine.

#![deny(missing_docs)]

pub mod contract;
mod fake;

pub use fake::FakeEngine;
