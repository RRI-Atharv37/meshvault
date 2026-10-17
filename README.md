# MeshVault engine

[![CI](https://github.com/RRI-Atharv37/meshvault/actions/workflows/ci.yml/badge.svg)](https://github.com/RRI-Atharv37/meshvault/actions/workflows/ci.yml)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0--only-blue.svg)](LICENSE)

MeshVault is a file-sync product for small teams that work with big files, such as video and 3D projects.
This repository is its open engine: Rust crates that move files safely between trusted devices.

Moving bytes safely between devices is open. Turning that into a team product is closed.

## Architecture

```mermaid
flowchart TB
    subgraph closed["MeshVault app (closed source)"]
        app["Desktop app, team features, licensing"]
    end
    subgraph open["This repository (GPL-3.0)"]
        engine["meshvault-engine<br/>the Engine trait and its types"]
        test["meshvault-engine-test<br/>FakeEngine + contract tests"]
        impls["Engine implementations<br/>(planned)"]
    end
    app -->|talks only to| engine
    impls -->|implement| engine
    impls -.->|must pass| test
    test -->|tests| engine
```

The app talks to a sync backend only through the `Engine` trait. A backend can be swapped for another
without touching the app, as long as it passes the same contract tests. Backend names, ports, keys and
raw backend errors never cross the trait.

The split exists so that the part anyone could audit and reuse, the file-moving core, is open, while the
product built around it stays closed.

## Crates

| Crate | What it is |
|---|---|
| [`meshvault-engine`](meshvault-engine) | The `Engine` trait: peers, folders, status, events, errors, and the rules every implementation follows. |
| [`meshvault-engine-test`](meshvault-engine-test) | `FakeEngine`, an in-memory engine, and the contract tests every `Engine` must pass. |

## Engine roadmap

- [x] `Engine` contract and `FakeEngine` with contract tests
- [ ] Syncthing-backed engine (supervisor, REST client, event mapping)
- [ ] Two-engine end-to-end test: a file on one device appears on another
- [ ] Chunking and hashing: FastCDC and BLAKE3, with benchmarks
- [ ] Signed manifests, diff and apply with atomic writes and path safety
- [ ] Transport over QUIC with resume and relay fallback
- [ ] Encryption and device keys
- [ ] A native mesh engine behind the same trait
- [ ] Fuzzing, fault injection and a published threat model

## Testing an engine

Add the test crate as a dev-dependency, together with `tokio` (features `rt` and `macros`), then give the
macro an expression that builds a fresh engine:

```rust
meshvault_engine_test::engine_contract_tests!(MyEngine::start().await);
```

It generates one test per rule: peer add and remove, folder share, pause and resume, event order and
delivery, and idempotency.

## Building and testing

From the repository root:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Contributing and licence

Licensed GPL-3.0-only, see [LICENSE](LICENSE). Contributions need a signed CLA, see
[CONTRIBUTING.md](CONTRIBUTING.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).
