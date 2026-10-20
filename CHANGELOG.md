# Changelog

All notable changes to the open MeshVault engine crates. Tags are `oss-vX.Y.Z`.

## oss-v0.2.0

### Added

- `meshvault-core`: BLAKE3 `Hash`, FastCDC chunking with 256 KiB / 1 MiB / 4 MiB bounds, `chunk_list_hash`,
  and the `Clock` trait with `SystemClock` and `FakeClock`.
- `meshvault-core`: `RelPath`, the single path normaliser (NFC, case-insensitive `PathKey`, reserved names,
  drive and UNC prefixes, `..`, control characters, length and depth limits).
- `meshvault-core`: the `Fs` trait with `SimFs` (durable and volatile state, crash and fault injection) and
  `WindowsFs` (local NTFS only; refuses other volumes, never follows links, durable moves).
- `meshvault-core`: the per-folder SQLite `Index` (WAL, full sync, integrity check on open, folder and schema checks).
- `meshvault-core`: `Scanner` and `check_folder`. A deletion is reported only under a directory that was
  listed without error; permission, lock and read errors keep the old version. Files still being written are
  deferred, and moves and case-only renames are detected.
- `meshvault-core`: `IgnoreSet`, and a watcher layer (`Watch`, `ReconcileTimer`, `NotifyWatchSource`) where any
  overflow or error triggers a full scan.

## oss-v0.1.1

### Added

- `meshvault-engine-test`: `Fixtures` and the `fixtures:` form of the macro, so a real backend can describe
  its peer ID shape, folder location and settle time.

### Changed

- Contract: a real backend may report `UpToDate` by itself after a scan. The tests wait for the engine to
  go quiet and ignore `UpToDate` when comparing the states the caller asked for.
- Docs and README describe the engine's direction and roadmap.

## oss-v0.1.0

First release: the engine contract and the tools to test it.

### Added

- `meshvault-engine`: the `Engine` trait with peers, folders, status, events and errors, and the rules
  every implementation follows (idempotent calls, local state only, no backend detail across the trait,
  confined paths, ordered events for every subscriber).
- `meshvault-engine-test`: `FakeEngine`, an in-memory engine with hooks to simulate peers connecting.
- `meshvault-engine-test`: 22 contract tests and the `engine_contract_tests!` macro that runs them against
  any `Engine`.
- GPL-3.0 licence, README, CONTRIBUTING (CLA required) and `workspace.Cargo.toml`.
