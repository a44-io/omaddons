# omaddons

A World of Warcraft addon manager for Linux, written as a single Rust binary.

The goal is one tool that finds every WoW client on the machine (Battle.net under Wine or Proton, Steam, Lutris, Heroic and Bottles prefixes, and private-server clients from 1.12.1 to 5.4.8) and manages addons for each of them through a journaled, undoable transaction engine. The CLI has a stable JSON contract that coding agents can drive directly. An Omarchy bar widget will render the same CLI output for humans.

## Status

Pre-alpha. The repository holds the crate scaffold and the local quality gate; no command does anything yet. The binary builds and exits 0.

Work is tracked with beads (`br`). The design, the decisions behind it and the milestone roadmap live in the pinned vision bead:

```bash
br show omad-vision-8qa
```

Milestone 1 (in progress) is the working slice: discovery, the TOC load-rule engine, the transaction engine, keyless GitHub, git, URL and local sources, and the core CLI contract.

## Building

Linux only. The toolchain is pinned in `rust-toolchain.toml`, and rustup picks it up automatically.

```bash
cargo build
./target/debug/omaddons
```

## The local gate

There is no CI. Every commit that changes code runs the gate first and is committed only when it passes:

```bash
argc gate          # fmt, clippy -D warnings, tests, ubs on changed files,
                   # cargo deny when Cargo.toml, Cargo.lock or deny.toml changed
argc gate --all    # also run cargo deny unconditionally
```

`argc gate` calls `scripts/gate.sh`, which can also be run directly. It needs `cargo`, `ubs` and `cargo-deny` on `PATH`.

## Layout

```
src/main.rs        entry point
src/lib.rs         module tree
src/app/           one module per command
src/cli.rs         clap definitions
src/output/        JSON envelope, human output, stderr events
src/error.rs       error and warning code registries
src/model.rs       domain types
src/discover/      client discovery and the target registry
src/rules.rs       product, family and ClientRules tables
src/toc.rs         TOC parser and load rules
src/inventory.rs   folder classification
src/sources/       addon sources
src/resolve/       candidate selection, verification, catalog
src/net/           HTTP, host allowlist, net.log
src/fsx/           the single filesystem writer
src/engine/        the transaction engine
src/state/         config, registry, records, history
src/politeness.rs  network and scan politeness constants
src/limits.rs      safety limits
tests/             integration tests
```

Most modules hold only a doc comment so far.
