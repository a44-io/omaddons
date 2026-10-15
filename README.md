# omaddons

A World of Warcraft addon manager for Linux, written as a single Rust binary.

The goal is one tool that finds every WoW client on the machine (Battle.net under Wine or Proton, Steam, Lutris, Heroic and Bottles prefixes, and private-server clients from 1.12.1 to 5.4.8) and manages addons for each of them through a journaled, undoable transaction engine. The CLI has a stable JSON contract that coding agents can drive directly. An Omarchy bar widget will render the same CLI output for humans.

## Status

Milestone 1, the working slice, is done: discovery, the TOC load-rule engine, the transaction engine with undo, keyless sources (GitHub, git, URL, local files) and the core CLI contract. It is pre-alpha: nothing is tagged yet, and the JSON contract freezes with the first tag (milestone 4).

Work is tracked with beads (`br`). The design, the decisions behind it and the roadmap live in the pinned vision bead:

```bash
br show omad-vision-8qa
```

## What it does today

**Finds every client, with no configuration.** Discovery reads Battle.net's `product.db` and `.build.info`, scans `~/Games` (each Wine prefix's `drive_c`, and plain directories) and `~/.wine`, and reads launcher configs: Lutris, Heroic, Steam (every library in `libraryfolders.vdf`) and Bottles, native and Flatpak. It reads mapped drive letters, so a game on `D:` outside the prefix is found. Every Battle.net product is recognized, including WoW: Forever (`_forever_`) and its beta (`_classic_beta_`), and custom clients from 1.12.1 to 5.4.8 are identified from their executable. An unknown flavor folder still becomes a usable target. Each client is a target with an id such as `retail`, `era`, `forever` or `wotlk`; a second install of the same flavor gets a qualified id such as `retail@steam`.

**Installs what loads.** Every candidate archive is checked against the target client's own TOC load rules (suffixes such as `_Mainline`, `_Vanilla` or `_Forever`, Interface lists, and the legacy clients' rules) before anything is written. A build for another flavor is never installed; when nothing loads, the error says so and lists every candidate tried.

**Sources, all keyless:**

| Ref | Where the release comes from |
|---|---|
| `weakauras` | the built-in catalog of 18 addons, 5 of them legacy private-server addons |
| `gh:owner/repo` or a pasted GitHub link | the stable release's `release.json`, read from github.com without the REST API; the REST API (60 requests an hour unauthenticated) only for `--channel beta`/`alpha` and for releases without a `release.json`, whose zips are then listed |
| `git:https://github.com/o/r#branch` | the commits feed and codeload, without the `git` binary |
| `git:https://codeberg.org/o/r#tag` | the `git` binary, for hosts listed in `git_hosts` |
| `url:https://…/addon.zip` | a direct download from an allowed host |
| `./addon.zip`, `./AddonFolder` or `file:` | a local archive or folder |

**Commands:**

| Command | Example |
|---|---|
| `status` | `omaddons status` lists every client, its addons, whether it is running and what needs attention |
| `install` | `omaddons install weakauras dbm -t retail` installs or updates; `--dry-run` shows the plan |
| `remove` | `omaddons remove DBM -t retail` |
| `undo` | `omaddons undo` reverts the last change; `omaddons undo --last-by agent` reverts the last change an agent made |
| `history` | `omaddons history` shows what changed, when, by whom and why |
| `list` | `omaddons list -t wotlk` lists the folders in a target's AddOns directory |
| `targets` | `omaddons targets add ~/Games/turtle --name turtle`; also `rename`, `rm`, `default`, `--rescan` |
| `info` | `omaddons info bigwigs` shows installed records, the catalog entry and the newest release |
| `config` | `omaddons config set default_channel beta` edits `config.toml`, keeping its comments |
| `net-log` | `omaddons net-log` lists every network request and git subprocess |

Run `omaddons help <command>` for every flag.

## The JSON contract

Every command accepts `--json` (or `--robot`) and prints exactly one JSON envelope on stdout, on success and on failure:

```json
{"ok":true,"schema_version":"1.0","tool_version":"0.1.0","command":"install","exit_code":0,"partial":false,"data":{},"warnings":[],"commands":[{"action":"undo","argv":["omaddons","undo","01M4…"]}],"meta":{"elapsed_ms":412,"offline":false}}
```

A failure carries `error` with a `code`, a `message`, a `fix` and, when the fix is a command, its `fix_argv`, so an agent can run it without parsing a string. Progress events go to stderr as NDJSON when stderr is not a terminal. `--actor human|agent|widget` and `--reason` are recorded in history.

| Exit | Meaning |
|---|---|
| 0 | Success, including partial success (`partial: true`, with each failed item in `data`) |
| 1 | Runtime failure; `error.retryable` says whether to retry |
| 2 | Usage: unknown command or flag, bad argument, bad ref |
| 3 | Not found: target, ref, addon or transaction, or ambiguous |
| 4 | Safety refusal; `fix` names the flag or command |
| 5 | The lock is held by another run past the timeout; retryable |
| 6 | An opt-in check failed (`--strict` on a partial install) |

## Safety

- **Transactions.** Every change to a client runs under a journal: folders are staged, then swapped in atomically, so an interrupted run leaves each folder wholly old or wholly new, and the next run finishes or rolls it back.
- **Undo.** Each transaction snapshots what it replaces. `undo` restores folders byte for byte, and restores the client's SavedVariables only if they changed since; while the game runs, that restore waits.
- **Protected folders.** `Blizzard_*` folders are never written, and an addon folder whose name differs only in case from an existing one is refused.
- **Hostile archives.** Path traversal, symlinks, duplicate or case-colliding entries, and archives past the entry, size or compression-ratio limits are refused before anything is written.
- **Running clients.** By default (`running_policy = "safe"`) a running client can still receive updates that replace or add folders, and the result says `/reload` is needed. Removing or renaming a folder the game has loaded is refused until it exits, and SavedVariables restores wait for it. On legacy clients (1.12.1 to 5.4.8) `safe` refuses every change while the game runs. `refuse` blocks every change to a running client; `allow` blocks none.
- **Retention.** Snapshots are kept for at least a day and pruned after that by age, count and size; the download cache is bounded.

## Privacy

omaddons contacts only an allowlist of hosts: GitHub (`github.com`, `api.github.com`, `codeload.github.com` and its release-asset hosts) and any git hosts you list in `git_hosts`; the list also names WoWInterface and Tukui, which milestone 2 uses. Every request and git subprocess is written to `net.log` (`omaddons net-log`) without credentials. `--offline` (or `OMADDONS_OFFLINE=1`) refuses all network access. A GitHub token is used only if you set `OMADDONS_GITHUB_TOKEN`, and only for `api.github.com`. There is no telemetry.

## Files

- `~/.config/omaddons/config.toml`: settings, edited with `omaddons config`.
- `~/.local/state/omaddons/`: the target registry, per-target records, history, journals, snapshots and `net.log`.
- `~/.cache/omaddons/`: downloads and the discovery cache; deleting it is always safe.
- `<client>/Interface/.omaddons/target.json`: the client's identity, written on its first change, so a moved install is recognized.

The XDG variables move these as usual.

## Not there yet

- Milestone 2: `update` and `outdated`, WoWInterface and Tukui sources, dependencies, adopting addons installed by other managers, `export`/`sync` from a desired-state file, and search.
- Milestone 3: the Omarchy bar widget, Lua error capture, `WTF/` backups.
- Milestone 4: `capabilities` and the rest of the agent surface, the full `doctor`, stored plans, and the first tag, which freezes the contract.
- Milestone 5: optional CurseForge and Wago accounts, and release packaging.

## Building

Linux only. The toolchain is pinned in `rust-toolchain.toml`, and rustup picks it up automatically.

```bash
cargo build --release
./target/release/omaddons status
```

## The local gate

There is no CI. Every commit that changes code runs the gate first and is committed only when it passes:

```bash
argc gate          # fmt, clippy -D warnings, tests, ubs on changed files,
                   # cargo deny when Cargo.toml, Cargo.lock or deny.toml changed
argc gate --all    # also run cargo deny unconditionally
```

`argc gate` calls `scripts/gate.sh`, which can also be run directly. It needs `cargo`, `ubs` and `cargo-deny` on `PATH`.

Checks that are not part of the gate run at milestone checkpoints:

```bash
argc fuzz-all --time 600                                  # every fuzz target, 10 minutes each
OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored # the real endpoints
OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1 \
  cargo test --test real_machine -- --ignored --test-threads=1  # this machine's clients; every change is undone
```

Fuzzing needs `cargo-fuzz` and a nightly toolchain at least as new as the crate's `rust-version` (`--toolchain` picks one).

## Layout

```
src/main.rs        entry point
src/lib.rs         module tree
src/app/           one module per command
src/cli.rs         clap definitions
src/output/        JSON envelope, human output, stderr events
src/error.rs       error and warning code registries
src/model.rs       domain types
src/discover/      client discovery, launcher configs and the target registry
src/rules.rs       product, family and ClientRules tables
src/toc.rs         TOC parser and load rules
src/inventory.rs   folder classification
src/sources/       addon sources
src/resolve/       candidate selection and the catalog
src/net/           HTTP, host allowlist, net.log
src/fsx/           the single filesystem writer
src/engine/        the transaction engine: plan, journal, swap, undo, retention
src/state/         config, registry, records, history
src/politeness.rs  network and scan politeness constants
src/limits.rs      safety limits
catalog/           the built-in addon catalog
tests/             integration tests, fixtures and snapshots
fuzz/              cargo-fuzz targets and seeds
```
