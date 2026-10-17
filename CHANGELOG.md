# Changelog

All notable changes to the open MeshVault engine crates. Tags are `oss-vX.Y.Z`.

## oss-v0.1.0 (unreleased)

First release: the engine contract and the tools to test it.

### Added

- `meshvault-engine`: the `Engine` trait with peers, folders, status, events and errors, and the rules
  every implementation follows (idempotent calls, local state only, no backend detail across the trait,
  confined paths, ordered events for every subscriber).
- `meshvault-engine-test`: `FakeEngine`, an in-memory engine with hooks to simulate peers connecting.
- `meshvault-engine-test`: 22 contract tests and the `engine_contract_tests!` macro that runs them against
  any `Engine`.
- GPL-3.0 licence, README, CONTRIBUTING (CLA required) and `workspace.Cargo.toml`.
