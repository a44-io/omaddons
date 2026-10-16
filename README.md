# omaddons

<div align="center">

**One binary that finds every World of Warcraft client on a Linux machine, installs only the addons each client will actually load, and can undo every change it makes.**

![status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-orange)
![platform: Linux](https://img.shields.io/badge/platform-Linux-blue)
![rust: 1.97.1](https://img.shields.io/badge/rust-1.97.1-dea584)
![accounts: none needed](https://img.shields.io/badge/accounts-none%20needed-brightgreen)
![telemetry: none](https://img.shields.io/badge/telemetry-none-brightgreen)

</div>

```text
$ omaddons status
anniversary  tbc                                           2.5.6.70006   live  anniversary (battlenet)  0 managed · 0 unmanaged · 0 protected
classic      mists                                         5.5.4.70268   live  classic (battlenet)      0 managed · 0 unmanaged · 0 protected
era          Classic Era · Hardcore · Season of Discovery  1.15.9.70003  live  era (battlenet)          0 managed · 0 unmanaged · 0 protected
forever      forever                                       1.60.1.70245  live  forever (battlenet)      0 managed · 12 unmanaged · 0 protected
retail       mainline                                      12.1.5.70391  live  retail (battlenet)       0 managed · 0 unmanaged · 0 protected
wotlk        wrath-legacy                                  3.3.5.12340   live  localhost                0 managed · 0 unmanaged · 23 protected
```

<sub>Real output from the development machine: five Battle.net clients in one Proton prefix and a 3.3.5a private-server client in another, found with no configuration.</sub>

<div align="center">
<h3>Build and install</h3>

```bash
git clone https://github.com/a44-io/omaddons.git && cd omaddons
cargo install --locked --path .
```

</div>

There is no packaged release, installer or prebuilt binary yet; see [Installation](#installation).

> [!NOTE]
> **Pre-alpha.** Milestone 1, the working slice, is done and runs daily on the development machine. Nothing is tagged, the repository is private, and the JSON contract freezes at the first tag (milestone 4). Milestone 2 has begun: `outdated`, `update`, `pin` and `sync` work, and installs bring their catalog dependencies; WoWInterface and Tukui are on the way. See [What works today](#what-works-today) and the [Roadmap](#roadmap).

---

## Contents

- [TL;DR](#tldr)
- [Quick example](#quick-example)
- [What works today](#what-works-today)
- [Design philosophy](#design-philosophy)
- [How omaddons compares](#how-omaddons-compares)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Concepts: targets, families and load rules](#concepts-targets-families-and-load-rules)
- [Sources and refs](#sources-and-refs)
- [The built-in catalog](#the-built-in-catalog)
- [Command reference](#command-reference)
- [The JSON contract](#the-json-contract)
- [Safety model](#safety-model)
- [Network and privacy](#network-and-privacy)
- [Configuration](#configuration)
- [Files and state](#files-and-state)
- [Architecture](#architecture)
- [Troubleshooting](#troubleshooting)
- [Limitations and known issues](#limitations-and-known-issues)
- [Roadmap](#roadmap)
- [Development](#development)
- [FAQ](#faq)
- [For coding agents](#for-coding-agents)
- [License](#license)

---

## TL;DR

**The problem.** On Linux, World of Warcraft lives inside Wine prefixes. Battle.net runs under Proton or umu in one prefix, while Steam, Lutris, Heroic and Bottles each keep their own, and a private-server client sits wherever it was unzipped. One machine can easily hold six clients, each with its own `Interface/AddOns`: retail, Mists Classic, Classic Era (which also runs Season of Discovery and Hardcore), Anniversary, WoW: Forever and a 3.3.5a server. Addon managers built for Windows want to be pointed at each folder, and the popular ones lean on CurseForge. Unzipping by hand installs builds the client never loads and half-replaces folders when something fails, and it cannot be undone. Coding agents get nothing they can parse.

**The solution.** omaddons finds every client with no configuration and reads its real version. It then installs only what that client will load: the client's own TOC rules are applied to the files inside the archive before anything is written. Every change is a journaled transaction. It is staged, swapped in atomically and recorded with its actor, its argv and the reason given, and `omaddons undo` reverts it, SavedVariables included. Every command can answer with one JSON envelope, and every error in it names a fix, as a runnable argv whenever the fix is a command.

### Why use omaddons?

| Feature | What it does |
|---|---|
| **Zero-config discovery** | Reads Battle.net's `product.db`, `.build.info` and `.flavor.info`, the Lutris, Heroic, Steam and Bottles configs, and scans `~/Games` and `~/.wine`. Knows all 12 Battle.net WoW products and identifies private-server clients 1.12.1 to 5.4.8 from their executable's version resource. |
| **The game's rules decide** | TOC suffix precedence (`_Mainline`, `_Vanilla`, `_Forever`, …), `## Interface:` lists, `AllowLoadGameType`, interface ranges per family and the legacy single-TOC rules, applied to what is actually inside the archive. A build for another flavor is never installed. |
| **Multi-flavor archives** | DBM and BigWigs ship retail-only boss modules in the same zip as their Classic modules. On Classic or Forever, the folders that client would not load are skipped and listed in `folders.skipped`. This happens when the publisher's `release.json` declares that client, or when the extra folders rule themselves out with their own TOCs. |
| **Transactions, not file copies** | Folders are staged on the same filesystem, then swapped in with `renameat2(RENAME_EXCHANGE)` under a journal. A crash leaves each folder wholly old or wholly new, and the next change command finishes the work or rolls it back. |
| **Undo any change** | Each install, remove and undo snapshots what it replaces. `undo` restores folders byte for byte and restores SavedVariables only where they changed. An undo is itself undoable, and `undo --last-by agent` reverts the last thing an agent did. |
| **Keyless** | Stable GitHub releases are read from `release.json` without the REST API, and git branches through GitHub's commits feed and codeload, without the git binary. No account, token or API key is needed for anything that works today. |
| **Safe around a running game** | With the default policy, updates that keep the loaded TOC can land while you play (`/reload` to pick them up). Removing a loaded addon is refused until the client exits, and SavedVariables are never written while it runs. |
| **Verifiable privacy** | A hard host allowlist is checked on every redirect hop. Every request that reaches the network and every git run is logged locally (`omaddons net-log`), `--offline` refuses all network access, and there is no telemetry. |
| **Built for agents** | `--json` gives exactly one envelope on stdout, success or failure. Error codes are typed, fixes come as `fix_argv` and `alternatives` argv arrays whenever they are commands, exit codes 0 to 6 are documented, and every change records its actor and argv. |

### Quick example

```bash
# Every client on the machine, no setup
omaddons status

# Preview, then install boss mods into retail
omaddons install dbm bigwigs -t retail --dry-run
omaddons install dbm bigwigs -t retail

# Season of Discovery runs in the Classic Era client: -t sod selects it
omaddons install questie -t sod

# A 3.3.5a private server: a git branch, fetched without the git binary
omaddons install gbb -t wotlk

# Anything on GitHub: a pasted link works
omaddons install https://github.com/tullamods/OmniCC -t retail

# What changed, by whom, and take it back
omaddons history
omaddons undo

# The same thing, for an agent
omaddons install plater -t forever --json --actor agent --reason "user asked for nameplates"
```

---

## What works today

Milestone 1 (the working slice) is complete. Everything in this README describes code that exists and is tested, unless it is marked as planned.

| Area | State |
|---|---|
| Discovery of Battle.net, Steam, Lutris, Heroic, Bottles and custom clients | ✅ Done |
| The TOC load-rule engine (`ClientRules`) for all 12 Battle.net products and five legacy private-server versions | ✅ Done |
| The transaction engine: plan, stage, journal, atomic swap, crash recovery, undo, retention | ✅ Done |
| Keyless sources: GitHub releases, git (GitHub without the binary, other hosts with it), direct URLs, local zips and folders | ✅ Done |
| A catalog of 18 verified addons (13 GitHub release entries, 5 legacy private-server addons from git) | ✅ Done |
| The CLI contract: one JSON envelope, typed errors with fixes (an argv whenever the fix is a command), exit codes 0 to 6, contract snapshot tests | ✅ Done (freezes in M4) |
| Commands: `status`, `install`, `remove`, `undo`, `history`, `list`, `targets`, `info`, `config`, `net-log`, `help` | ✅ Done |
| `outdated` (alias `check`): what has updates, on every client | ✅ Done (milestone 2) |
| `update` (alias `upgrade`): every client's updates in one run, undone with one `undo` | ✅ Done (milestone 2) |
| `pin` and `unpin`: hold an addon at its release through updates | ✅ Done (milestone 2) |
| Dependencies: what a TOC requires installs with it, from the catalog | ✅ Done (milestone 2; more sources to come) |
| `sync`: every client made to match `addons.toml`, idempotently | ✅ Done (milestone 2; `--locked` and `--prune` to come) |
| WoWInterface and Tukui, adopting other managers' addons, `export`, `search` | 🗓️ Milestone 2 (decomposed into 75 beads) |
| The Omarchy bar widget, Lua error capture, `WTF/` backups | 🗓️ Milestone 3 |
| `capabilities`, `doctor`, `robot-docs`, stored plans, the agent skill, the first tag | 🗓️ Milestone 4 |
| Optional CurseForge and Wago (bring your own key), release packaging | 🗓️ Milestone 5 |

`omaddons outdated` lists what has updates, and `omaddons update` installs them on every client, one transaction per client, all undone by one `omaddons undo`. Two kinds of addon are left alone. A `url:` ref's release is the URL itself, so there is nothing newer to fetch. A pinned addon (by `pin`, or installed with `--version`) stays where it is (`skipped`, reason `pinned`) until `unpin`.

---

## Design philosophy

**1. The game's rules decide, not metadata.** A host's flavor tags, a `release.json` and a file name order the candidates; they never make a folder install that the client would not load. What decides is the TOC file the client would load from each folder in the downloaded archive, judged by that client's rules row (suffixes, interface range, game types). So omaddons refuses the retail build of an addon for Classic Era even when the release metadata claims otherwise, and it installs a Classic module on Forever only if Forever would load it.

**2. Every change is a transaction.** There is one engine and one filesystem writer (`src/fsx/`). Every install, update, remove and undo stages its folders on the client's own filesystem and swaps them in under a journal and a lock held only for the commit window. Then it records the actor, the argv and the reason. A test in the gate fails if any `std::fs` write call appears outside `src/fsx/`.

**3. Keyless and private by default.** Everything that works today needs no account. The network is a closed allowlist of hosts, every request is logged locally, and `--offline` is absolute. CurseForge and Wago will be strictly opt-in, bring your own credential, and their data will never be cached.

**4. The CLI is the product.** Humans read the terminal output, agents read the JSON envelope, and the future Omarchy widget will render the same envelope without logic of its own. Commands never print: they return typed outcomes that one output layer renders, and clippy denies `print!`. Every error names its fix: an argv an agent can run, or a sentence when no command would help.

**5. Detection is data.** Products, the version-to-family table, interface ranges and client-rules rows are tables in `src/rules.rs`. Each can be patched from `config.toml`, so a new Classic client or a misdetected private server is a config change, not a release.

---

## How omaddons compares

| | **omaddons** | WowUp | CurseForge app | instawow | Unzip by hand |
|---|---|---|---|---|---|
| Interface | CLI + JSON | GUI (Electron) | GUI | CLI, optional GUI | File manager |
| Native on Linux | ✅ Linux only | ✅ | ✅ AppImage, .deb | ✅ | ✅ |
| Finds clients inside Wine/Proton, Steam, Lutris, Heroic and Bottles prefixes with no setup | ✅ | ⚠️ ¹ | ⚠️ ¹ | ⚠️ ¹ | ❌ |
| Private-server clients 1.12.1 to 5.4.8 as first-class targets | ✅ | ❌ | ❌ | ❌ | ⚠️ by hand |
| Checks the archive's TOCs against the client's load rules before writing | ✅ | ⚠️ host metadata | ⚠️ host metadata | ⚠️ host metadata | ❌ |
| Undo installs and removals byte for byte, SavedVariables included | ✅ | ❌ | ❌ | ⚠️ `rollback` of updates | ❌ |
| Update everything in one command | ⚠️ re-run `install`; `update` in M2 | ✅ | ✅ | ✅ | ❌ |
| GitHub releases | ✅ keyless | ✅ | ❌ | ✅ | ⚠️ |
| WoWInterface, Tukui | 🗓️ M2 | ✅ | ❌ | ✅ | ⚠️ |
| CurseForge | 🗓️ M5, opt-in | ✅ WowUp-CF build | ✅ | ✅ | ⚠️ |
| Wago Addons | 🗓️ M5, opt-in | ✅ | ❌ | ✅ | ⚠️ |
| Documented machine contract for agents | ✅ | ❌ | ❌ | ❌ | ❌ |

¹ They detect standard install locations; other clients are added by pointing the app at their folder.

This comparison is from each project's own documentation as of October 2026. Corrections are welcome.

**Use omaddons when:**

- you run several WoW clients on Linux, especially across Wine prefixes, launchers or private servers;
- you want every change to be reversible, with SavedVariables included;
- a coding agent or a script manages your addons and needs a stable, typed interface;
- you want to know and limit exactly which hosts are contacted.

**Use something else when:**

- you depend on CurseForge-only or Wago-only addons today (CurseForge app, WowUp-CF, WowUp);
- you want a graphical app (WowUp, the CurseForge app, or [strongbox](https://github.com/ogri-la/strongbox), which covers WoWInterface, GitHub and GitLab);
- you want one command that updates everything today, before milestone 2 (instawow, WowUp);
- you play on Windows or macOS (omaddons is Linux only, by design).

---

## Installation

### From source (the only way today)

#### Requirements

- Linux. Development and testing happen on x86_64; the dependency policy also covers aarch64.
- [rustup](https://rustup.rs). The toolchain is pinned to Rust 1.97.1 in `rust-toolchain.toml`, and rustup installs it automatically on the first build.
- Optional: `git` 2.32 or newer, needed only for `git:` refs on hosts other than github.com (see [`git_hosts`](#configuration)).

**Install into `~/.cargo/bin`:**

```bash
git clone https://github.com/a44-io/omaddons.git
cd omaddons
cargo install --locked --path .
omaddons --version          # omaddons 0.1.0
```

**Or build without installing:**

```bash
cargo build --locked --release
./target/release/omaddons status
```

The release profile uses thin LTO, a single codegen unit and stripped symbols.

### Why there is no installer or package

The repository is private, and the project deliberately has no CI, public distribution or public installer until it fully works on its owner's setup (vision decision D16). Shell completions are planned for milestone 4, and a local `PKGBUILD`, a man page and a version scheme for milestone 5. The public installer is reconsidered after that.

### Uninstall

```bash
cargo uninstall omaddons
rm -rf ~/.config/omaddons ~/.local/state/omaddons ~/.cache/omaddons
```

Deleting the state directory loses the history and every undo snapshot. Each client managed by omaddons also has an `Interface/.omaddons/` folder that holds its identity marker, staging area and folder snapshots. The addons themselves stay where they are.

---

## Quick start

**1. See what was found.** No configuration is needed:

```bash
omaddons status            # every client, its addons, running state, what needs attention
omaddons targets           # the same clients with their paths and registration state
```

**2. Add a client that discovery missed** (for example a private server unpacked outside `~/Games`):

```bash
omaddons targets add /mnt/games/turtle --name turtle
# or scan another root on every run:
omaddons config set extra_roots '["/mnt/games"]'
```

**3. Choose a default target**, so `install` needs no `-t`:

```bash
omaddons targets default retail
```

**4. Preview, then install:**

```bash
omaddons install dbm bigwigs --dry-run
omaddons install dbm bigwigs
```

**5. Check and, if needed, undo:**

```bash
omaddons list -t retail
omaddons history
omaddons undo
```

### A whole session

This is real output from a test home with a retail and a Classic Era client and two local addons; absolute paths are shortened to `~`. The installs come from a folder and a zip, so the session works offline.

```text
$ omaddons install ./HelloWorld ./Bar-2.1.0.zip -t retail --dry-run
retail: would be installed HelloWorld 1.0.0 (1 folders)
retail: would be installed Bar 2.1.0 (1 folders)

$ omaddons install ./HelloWorld ./Bar-2.1.0.zip -t retail --reason "set up retail UI"
retail: installed HelloWorld 1.0.0 (1 folders)
retail: installed Bar 2.1.0 (1 folders)
tx 01M51F4QXQRZQAP2KKSAKZ71QK · undo: omaddons undo 01M51F4QXQRZQAP2KKSAKZ71QK

$ omaddons list -t retail
retail (12.1.5.70391, mainline)
  Bar         2.1.0  file:~/Downloads/./Bar-2.1.0.zip  stable
  HelloWorld  1.0.0  file:~/Downloads/./HelloWorld     stable
  2 managed · 1 unmanaged · 1 protected · 0 linked

$ omaddons remove Bar -t retail --actor agent --reason "Bar conflicts with ElvUI"
retail: removed Bar 2.1.0 (1 folder)
tx 01M51F4QY5KBKP70YF3DZV69ZN · undo: omaddons undo 01M51F4QY5KBKP70YF3DZV69ZN

$ omaddons undo --last-by agent --actor agent --reason "the user wants Bar back"
reverted 01M51F4QY5… (remove by agent, 2026-10-15 23:22) on retail
restored 1 folders, removed 0 · SavedVariables: 0 restored, 0 unchanged
tx 01M51F4QYBQEXB2HECCHD1728N · undo this: omaddons undo 01M51F4QYBQEXB2HECCHD1728N

$ omaddons history
01M51F4QYB…  2026-10-15 23:22  agent  undo     retail  +1 ~0 -0  "the user wants Bar back"
01M51F4QY5…  2026-10-15 23:22  agent  remove   retail  +0 ~0 -1  "Bar conflicts with ElvUI"
01M51F4QXQ…  2026-10-15 23:22  human  install  retail  +2 ~0 -0  "set up retail UI"
```

And what a refusal looks like:

```text
$ omaddons install ./EraOnly.zip -t retail
error[wrong_flavor]: file:~/Downloads/./EraOnly.zip does not load on retail (Mainline)
fix: use an archive built for this client

$ omaddons install ./OldThing -t retail
error[unmanaged_folder]: OldThing is not managed by omaddons
fix: omaddons install ./OldThing -t retail --force

$ omaddons install ./Bar-2.1.0.zip
error[target_ambiguous]: ./Bar-2.1.0.zip is compatible with 2 targets: era, retail
fix: omaddons install ./Bar-2.1.0.zip -t era
  or: omaddons install ./Bar-2.1.0.zip -t retail
```

---

## Concepts: targets, families and load rules

### Targets

A **target** is one WoW client: one flavor folder such as `_retail_`, with its own `Interface/AddOns` and `WTF/`. Each target has:

- an **id** you type with `-t`: `retail`, `era`, `forever`, `wotlk`;
- a **key**, a ULID assigned on registration that never changes;
- a **family** derived from the client's version, which picks its load rules;
- zero or more **game modes**: Classic Era, Hardcore and Season of Discovery all run in the Classic Era client and share its addons.

A target is **registered** on its first change (or with `targets add`, `rename` or `default`). Registration writes `<client>/Interface/.omaddons/target.json`, so a moved or renamed install is recognized as the same target. A second install of the same flavor gets a qualified id such as `retail@steam`, `retail@lutris` or `retail@wine`. The first registered install keeps the bare id, and further collisions get `retail@steam-2`, `-3` and so on.

### Where discovery looks

Discovery runs whenever a command needs the clients: every command except `config`, `net-log`, `help` and `targets rm`. Each walk goes at most 4 levels deep, and all the walks in one run share a budget of 50,000 directories.

It first collects the Wine prefixes (directories containing `drive_c/`):

- the prefixes that **launcher configs** name:
  - Lutris: `games/*.yml` under the native config and data directories (prefix and exe);
  - Heroic: every `winePrefix` in `GamesConfig/*.json`, native and Flatpak;
  - Steam: the `compatdata/*/pfx` Proton prefixes of every library in `libraryfolders.vdf`, native and Flatpak (WoW is usually a non-Steam shortcut with its own compatdata id);
  - Bottles: every bottle, including bottles kept at a custom path, native and Flatpak;
- the **direct children of `~/Games`** that are prefixes;
- **`~/.wine`**.

Then it looks for clients in this order. The first route that finds a client decides how it was found and its qualifier.

1. **Battle.net's `product.db`** in every prefix (`drive_c/ProgramData/Battle.net/Agent/product.db`). Windows drive letters are mapped through `dosdevices/`, so a game on `D:` outside the prefix is found.
2. **Executables named by launcher configs** (Lutris's `exe:` lines).
3. **A walk of each launcher-named prefix's `drive_c`**, skipping `windows`, `users` and `ProgramData`.
4. **`extra_roots`** from `config.toml`, and the roots of manually added targets.
5. **A walk of the `drive_c` of each `~/Games` prefix and of `~/.wine`**, with the same skips.
6. **A plain walk of `~/Games`** for clients outside any prefix.

A directory with `.build.info` is a **Battle.net root**: each `_<flavor>_` folder in it becomes a target, identified by its `.flavor.info`, product.db or the folder table. A directory with `Interface/` and a `wow*.exe` is a **custom client**: its version comes from the executable's PE version resource, and its label comes from `realmlist.wtf`.

Parsed results are cached in `~/.cache/omaddons/discovery.json` and reused while the stamped files (`.build.info`, `.flavor.info`, exes, `Config.wtf`, `realmlist.wtf`, `product.db`) are unchanged. `targets --rescan` ignores the cache, and `status --cached` skips the scan entirely.

### Battle.net products

| Product code | Folder | Exe | Kind | Target id | Family (current version) | Game modes |
|---|---|---|---|---|---|---|
| `wow` | `_retail_` | WoW.exe | live | `retail` | mainline (12.x) | Retail |
| `wowt` | `_ptr_` | WoWT.exe | test | `ptr` | mainline | |
| `wowxptr` | `_xptr_` | WoWT.exe | test | `xptr` | mainline | |
| `wow_beta` | `_beta_` | WoWB.exe | test | `beta` | mainline | |
| `wow_classic` | `_classic_` | WoWClassic.exe | live | `classic` | mists (5.5.x) | Mists of Pandaria Classic (`mists-classic`, `mop`, `mop-classic`) |
| `wow_classic_ptr` | `_classic_ptr_` | WoWClassicT.exe | test | `classic-ptr` | mists | |
| `wow_classic_beta` | `_classic_beta_` | WowB.exe | test | `classic-beta` | forever (1.60.x) | |
| `wow_classic_era` | `_classic_era_` | WoWClassic.exe | live | `era` | vanilla (1.15.x) | Classic Era (`classic-era`), Hardcore (`hardcore`, `hc`), Season of Discovery (`sod`, `season-of-discovery`) |
| `wow_classic_era_ptr` | `_classic_era_ptr_` | WowClassicT.exe | test | `era-ptr` | tbc (2.5.x) | |
| `wow_anniversary` | `_anniversary_` | WoWClassic.exe | live | `anniversary` | tbc (2.5.x) | Burning Crusade Anniversary (`tbc-anniversary`, `bc-anniversary`) |
| `wow_classic_titan` | `_classic_titan_` | WoWClassic.exe | live | `titan` | titan (3.80.x, China) | |
| `wowf` | `_forever_` | Wow.exe | live | `forever` | forever (1.60.x) | WoW: Forever |

The product code does not decide the family; the version does. `wow_classic_era_ptr` runs TBC 2.5.x, and `wow_classic_beta` runs Forever 1.60.x. An unknown flavor folder still becomes a target. If its version cannot be read, it can receive installs only after you set its family (see [Configuration](#per-target-overrides)).

### Private-server (legacy) clients

| Client version | Family | Default id | Load rules |
|---|---|---|---|
| 1.12.1 (Vanilla) | `vanilla-legacy` | `vanilla` | plain `<Folder>.toc` only, single interface number |
| 2.4.3 (TBC) | `tbc-legacy` | `tbc` | same |
| 3.3.5a (WotLK) | `wrath-legacy` | `wotlk` | same |
| 4.3.4 (Cataclysm) | `cata-legacy` | `cata` | same |
| 5.4.8 (MoP) | `mists-legacy` | `mists` | same |

Legacy clients are found by their executable (`wow.exe`, then `wow-64.exe`, then any `wow*.exe`) next to an `Interface/` folder. Turtle WoW and other 1.12.1 servers are plain `vanilla-legacy` clients; name them with `targets add … --name turtle` or `targets rename`. While a legacy client runs, the default running policy refuses every change (see [Running clients](#running-clients)).

### Families and interface ranges

| Family | Interface range | Clients |
|---|---|---|
| `mainline` | 60000 and up | retail, PTR, XPTR, beta |
| `vanilla` | 11300–14999 | Classic Era (with Hardcore and Season of Discovery) |
| `forever` | 15000–19999 | WoW: Forever and its beta |
| `tbc` | 20500–20599 | Anniversary, the Classic Era PTR |
| `wrath` | 30400–30499 | Wrath Classic (3.4.x, no live product today) |
| `titan` | 38000–38099 | Titan (China) |
| `cata` | 40400–40499 | Cataclysm Classic (4.4.x, no live product today) |
| `mists` | 50500–50599 | Mists Classic |
| `vanilla-legacy` | 11200–11299 | 1.12.1 private servers |
| `tbc-legacy` | 20400–20499 | 2.4.3 private servers |
| `wrath-legacy` | 30300–30399 | 3.3.5a private servers |
| `cata-legacy` | 40300–40399 | 4.3.4 private servers |
| `mists-legacy` | 50400–50499 | 5.4.8 private servers |
| `unknown` | the client's own major.minor | anything else |

A client's interface number is `major×10000 + minor×100 (+ patch on modern clients)`, so 12.1.5 is 120105 and 3.3.5a is 30300. On legacy clients, `lastAddonVersion` in `WTF/Config.wtf` overrides the computed number.

### Choosing a target with `-t`

`-t` (alias `--flavor`) accepts any of these. The first kind that matches wins:

1. a target **key** (ULID);
2. a target **id**: `retail`, `era`, `retail@steam`, `turtle`;
3. a **product code**: `wow_classic_era`;
4. a **flavor folder**: `_classic_era_`;
5. a **game mode** or alias: `sod`, `hardcore`, `mop`;
6. a **family**: `mainline`, `vanilla`, `wrath-legacy`;
7. a **path**: the install root, the client directory or its AddOns folder.

Every target that the first matching kind matches is selected. `status`, `list` and `info` cover them all: `-t vanilla` reads every vanilla client, `-t mainline` every retail client. A command that changes one client (`install`, `remove`, and `history` and `undo`, which name one) refuses a selector that matches two with `target_ambiguous` (exit 3): the first candidate is the `fix`, and each other one is an `alternatives` entry.

An id beats a family, and the legacy default ids `vanilla`, `tbc`, `cata` and `mists` are also family names. With a 1.12.1 server present, `-t vanilla` selects it, not Classic Era. With a 5.4.8 server, `-t mists` selects it, not Mists Classic. Use `-t era` and `-t classic` (or `-t mop`) for the Battle.net clients, or rename the server's target.

`install` is the only command that picks a target when `-t` is absent. It uses config `default_target`, then the registry default (`targets default`, if that client is present), then the only live client. With several live clients and no default, it dry-runs the refs against every live client (test clients excluded) and installs only if exactly one client loads all of them. `status`, `list` and `history` cover every target unless `-t` narrows them. `undo` takes the newest transaction on any target. `remove` finds the target that manages the named addons, and asks for `-t` when more than one does.

### How omaddons decides what loads

For each top-level folder in the archive:

1. **Pick the TOC the client would load.** Candidates are `<Folder>.toc` and, on clients that support several TOCs, `<Folder>_<Suffix>.toc` or `<Folder>-<Suffix>.toc` (case-insensitive). Precedence follows the client's row: game suffix (e.g. `_Forever`, `_Vanilla`, `_Mists`, `_Standard`), then family suffix (`_Mainline` or `_Classic`), then the plain TOC. A suffix the row does not list never loads. Legacy clients read the plain TOC only.
2. **Game types.** On modern clients, an `AllowLoadGameType` that excludes this client, or an `ExcludeLoadGameType` that names it, means the folder does not load.
3. **Interface.** Comma-separated `## Interface:` lists are read where the client supports them; otherwise only the leading number counts. The folder loads if a value falls inside the client's family range. A newer interface loads. An older one gets an `out_of_date` warning, because the client loads it only with "Load out of date AddOns" enabled.

Then the archive as a whole:

- At least one folder must load.
- If the publisher's `release.json` declares this client for this very file, the folders the client would not load are skipped and listed in `folders.skipped`. That is how DBM's retail-only modules stay out of a Classic install.
- Otherwise the addon's own folder must load, and every skipped folder must rule itself out: a game-type gate, TOCs only for other clients, or a dependency on such a folder. A folder that merely lists another client's interface marks a build for another client, and the whole archive is refused.
- When no candidate loads, the error is `no_compatible_release`, with every asset tried and why in `error.details.tried` (JSON output). For a `url:`, `file:` or `--file` archive, it is `wrong_flavor`.

Release metadata also orders the candidates. First come assets whose metadata names this flavor (exact interface, then same major.minor, then any), then a file-name hint for it, then unannotated assets, then assets marked for other flavors. Within a tier, a full build comes before a `-nolib` build. Assets are downloaded and verified in that order until one is accepted. omaddons never falls back to an older release.

---

## Sources and refs

A **ref** names where an addon comes from. Everything that works today is keyless.

| You type | It becomes | Where the release comes from |
|---|---|---|
| `weakauras`, `dbm`, `bw` | a catalog entry | [the built-in catalog](#the-built-in-catalog), by name or alias (case-insensitive); an installed addon's name also works |
| `gh:Owner/Repo` or `Owner/Repo` | GitHub | the stable path reads `releases/latest` and its `release.json` from github.com, with **no REST API calls**. The REST API (60 requests an hour without a token) is used only for `--channel beta`/`alpha`, for releases without a usable `release.json` (their `.zip` assets become the candidates), and for pinned tags that lack one |
| `https://github.com/o/r` | `gh:o/r` | as above. `/releases/tag/T` pins tag `T`, `/releases/download/T/F` pins tag `T`, and `/tree/branch` becomes a `git:` ref |
| `git:https://github.com/o/r#branch` | git on GitHub | the commits Atom feed resolves the branch or tag to a commit, and codeload serves a zip of that exact commit (a rev the feed does not serve is fetched as a tag by name). **No git binary.** A 40-hex sha is used as is. No `#rev` means the default branch |
| `git:https://codeberg.org/o/r#tag` | git elsewhere | the `git` binary (2.32+), in a sandbox, only for hosts listed in `git_hosts`. Branches win over tags with the same name |
| `url:https://…/addon.zip` | a direct download | only from the always-allowed hosts (GitHub's, WoWInterface's and Tukui's); not `git_hosts` |
| `./addon.zip`, `./AddonFolder`, `file:path` | local | a zip (a typed path must end in `.zip`, and the magic bytes are checked) or a folder, read in place |
| `wowi:<id>`, `tukui:<slug>` and their links | WoWInterface, Tukui | parsed and refused with `source_disabled` ("arrives in M2"), with no network call, unless `--file` supplies the archive |
| `cf:<id>`, `wago:<id>`, CurseForge and Wago links | CurseForge, Wago | parsed and refused with `source_disabled` ("arrives in M5"), unless `--file` supplies the archive |
| an `https://` link to a `git_hosts` host, or ending in `.git` | `git:` | as `git:` above (`#fragment` becomes the rev) |
| any other `https://` link | `url:` | refused at plan time unless its host is allowlisted |
| `http://…` | | refused (`ref_invalid`), suggesting the https form |

**Channels.** `--channel stable|beta|alpha`. The default is the addon's recorded channel, then `default_channel`, then `stable`. On GitHub, `alpha` behaves like `beta`, and the newest 20 releases are considered.

**Pinning.** `--version V` installs exactly that release and pins it: a tag for `gh:` refs, or a rev for `git:` refs. On the `release.json` path, `1.2.3` also tries `v1.2.3`. It is refused for `url:` and `file:` refs. A pasted release link pins its tag the same way. Installing a pinned addon again without `--version` changes nothing and warns `addon_pinned`, without fetching its releases, even when a newer release exists. `--version W` moves it to `W`, still pinned. `--version` naming the release already installed pins it without reinstalling anything: a transaction of kind `pin` that records each folder and that `undo` unpins. A pin that another omaddons process commits while an install is planning also holds: the install re-checks it under the lock.

**What an update keeps.** Re-installing an installed addon keeps its pin and its recorded channel unless `--channel` names another. An addon installed only because another one needs it (`reason: dependency`, from milestone 2) becomes `explicit` when you install it by name. When the release to install is the one already there, these record changes still happen, as a transaction that moves no folder: `--channel beta` whose newest release is installed records the channel (item `edited`, `fields: ["channel"]`), and a repeat changes nothing.

**A browser download.** `--file ZIP` installs an archive you downloaded as the release of one ref, so it is recorded against that addon.

**Ambiguity.** `owner/repo` that also exists as a local path, or a name that matches two addons, gives `ref_ambiguous` with every candidate as a runnable `omaddons install …` command.

---

## The built-in catalog

18 entries are embedded in the binary. Each was checked against its publisher on 2026-10-14: the GitHub entries against their `releases/latest` tag, `release.json` and the addon roots inside the zip, and the legacy entries against their TOC's `## Interface` line. The last column is the families the publisher declares (in `release.json`, or the legacy TOC's interface). It is shown by `info` and does not restrict anything: TOC verification decides every install. The live catalog probe records where publishers and their own TOCs disagree. As of 2026-10-15, Plater declares titan but ships no TOC Titan loads, and RaiderIO declares only mainline but its TOCs also load on vanilla and mists.

| Name | Aliases | Ref | Declared families |
|---|---|---|---|
| WeakAuras | `wa`, `weakauras2` | `gh:WeakAuras/WeakAuras2` | vanilla, tbc, cata, mists, titan (**not retail**: 5.22.1 ships no Mainline TOC) |
| DBM | `deadlybossmods`, `dbm-core` | `gh:DeadlyBossMods/DeadlyBossMods` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| BigWigs | `bw` | `gh:BigWigsMods/BigWigs` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| LittleWigs | | `gh:BigWigsMods/LittleWigs` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| Plater | `plater-nameplates` | `gh:Tercioo/Plater-Nameplates` | mainline, vanilla, tbc, titan, mists, forever |
| Bartender4 | `bartender`, `bt4` | `gh:Nevcairiel/Bartender4` | mainline, vanilla, tbc, mists, forever |
| Questie | | `gh:Questie/Questie` | vanilla, tbc, wrath, mists, forever |
| OmniCC | | `gh:tullamods/OmniCC` | mainline, vanilla, tbc, titan, mists |
| Dominos | | `gh:tullamods/Dominos` | mainline, vanilla, tbc, mists, forever |
| Masque | | `gh:SFX-WoW/Masque` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| RaiderIO | `raider.io`, `rio` | `gh:RaiderIO/raiderio-addon` | mainline |
| MythicDungeonTools | `mdt` | `gh:Nnoggie/MythicDungeonTools` | mainline |
| AllTheThings | `att` | `gh:ATTWoWAddon/AllTheThings` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| pfUI | | `git:https://github.com/shagu/pfUI#master` | vanilla-legacy (1.12.1) |
| PlateBuffer | | `git:https://github.com/nullfoxh/PlateBuffer#master` | tbc-legacy (2.4.3) |
| GroupBulletinBoard | `gbb` | `git:https://github.com/fondlez/GroupBulletinBoard#main` | wrath-legacy (3.3.5a) |
| MikScrollingBattleText | `msbt` | `git:https://github.com/fondlez/MikScrollingBattleText-Cata#main` | cata-legacy (4.3.4) |
| InFlight | | `git:https://github.com/fondlez/InFlight-Taxi-Timer#main` | mists-legacy (5.4.8) |

**Your own entries.** Files in `~/.config/omaddons/catalog.d/*.toml` are applied in file-name order, in the same format. They can add an entry, replace one by name, or drop one with `remove = true`. A broken file is skipped whole with a `catalog_override_invalid` warning.

```toml
# ~/.config/omaddons/catalog.d/mine.toml
schema_version = "1.0"

[[addon]]
name = "MyBars"
aliases = ["mb"]
ref = "gh:example/MyBars"
families = ["mainline"]

[[addon]]
name = "Dominos"
remove = true
```

---

## Command reference

`omaddons help` lists the commands. `omaddons help <command>` or `omaddons <command> --help` shows every flag. For subcommands, use `omaddons targets add --help` or `omaddons help "targets add"` (quoted; see [Known issues](#known-issues)).

### Global options

Every global option works before or after the command, except `-V`/`--version`, which works only right after `omaddons`.

| Option | Meaning |
|---|---|
| `--json`, `--robot` | One JSON envelope on stdout (see [The JSON contract](#the-json-contract)) |
| `--format json\|human` | Overrides `--json`; default `human`, or `OMADDONS_FORMAT=json` |
| `-t, --target <TARGET>` (alias `--flavor`) | The client to act on: [any selector](#choosing-a-target-with--t) |
| `--dry-run` | Show the plan without changing anything (mutating commands) |
| `--actor human\|agent\|widget` | Who is acting, recorded in history. Default `OMADDONS_ACTOR`, else `human` when stdin is a terminal, else `agent`. In `history` it filters |
| `--reason <TEXT>` | Why, recorded verbatim in history (at most 200 characters) |
| `--offline` | Fail any network access (`OMADDONS_OFFLINE=1`) |
| `--while-running` | Allow folder changes while the client runs (never SavedVariables) |
| `--no-color` | No ANSI color (also `NO_COLOR`) |
| `--quiet` | Only errors on stderr |
| `-v, --verbose` | More detail: paths, and debug `log` events on stderr (repeatable) |
| `--progress json\|human\|none`, `--no-progress` | Progress of long commands on stderr (`install`, `update` and `outdated` today). Off with `--json`, in CI or when stderr is not a terminal, unless `--progress` names a mode; never with `--quiet` |
| `-h, --help` | Help |
| `-V, --version` | Version (`omaddons -V` only; after a command, `--version` is that command's flag or a usage error) |

### `status`

Triage: every client, its addon and folder counts, whether it is running, and what needs attention (a missing client, a pending journal, a degraded addon, a newer state version).

```bash
omaddons status               # scan, then report
omaddons status --cached      # no scan: the cached clients, re-checked on disk (fast; the widget's view)
omaddons status --check       # exit 6 when anything needs attention
omaddons status -v --json     # adds client_dir, install_root and exe_name
```

### `install <REF>...`

Install or update addons in one target. Each ref is fetched once and verified against the target's TOC rules before anything is written, and all addons for a target go into one transaction. Re-running `install` on an installed addon updates it to the newest release its channel allows. `url:` refs are never re-checked, and pinned addons stay where they are (`addon_pinned`; see [Pinning](#sources-and-refs)).

| Flag | Meaning |
|---|---|
| `--channel stable\|beta\|alpha` | Release channel (default: recorded, then `default_channel`, then stable) |
| `--version <V>` | Exactly this release, pinned: a tag for `gh:`, a rev for `git:` |
| `--file <ZIP>` | Use this downloaded archive as the release of the one given ref |
| `--force` | Take over a folder another addon owns, or replace an unmanaged folder (both undoable) |
| `--strict` | Exit 6 when only some of the addons install |
| `--refresh` | Fetch every release from its source, never from remembered metadata |
| `--no-deps` | Install only the named addons, not the ones their TOCs require (what they require is still checked, and warned about) |

```bash
omaddons install dbm bigwigs littlewigs -t retail
omaddons install questie -t hardcore
omaddons install gh:WeakAuras/WeakAuras2 --channel beta -t era
omaddons install gh:tullamods/Dominos --version 11.4.5 -t retail
omaddons install pfui -t turtle
omaddons install ./MyAddon -t retail --dry-run
omaddons install gh:someone/addon --file ~/Downloads/addon-1.2.zip -t retail
```

Each item's `status` is `installed`, `updated`, `unchanged` or `failed`, or, when only its record changed, `pinned`, `unpinned` or `edited` (with `fields`). A partial result (some addons failed) is still exit 0, with `partial: true` and each failure in `data.items[].error`. With `--strict` it is exit 6. Never-launched clients have no `Interface/AddOns` yet: the first install creates it inside its transaction and reports `creates_addons_dir: true`.

**Dependencies.** What an addon's TOC requires (`## Dependencies`, `## RequiredDeps`) installs in the same transaction when a keyless source ships it (today the catalog, including your `catalog.d` entries), from `default_channel` (never the requesting addon's channel). It is recorded as `reason: dependency` with `required_by`, and `undo` removes it with its requester. `## OptionalDeps` never installs anything. Some required folders install nothing:

- `Blizzard_*` folders and folders of the same archive.
- A folder that is already present. A managed owner gains the requester in `required_by`. For an unmanaged one, `commands` offers `omaddons list -t <id> --unmanaged`.

`missing_dependency` warns, and the run stays a success, when a required folder:

- cannot be resolved (no keyless source ships it, or several do);
- has only a build that is out of date for the client;
- is past a dependency limit (4 rounds, 32 dependencies per client);
- was not installed because of `--no-deps`.

A dependency that resolves but fails to install makes the run `partial`, with a warning naming it and what requires it. The requesting addon still installs.

### `update [ADDON]...`

Every managed addon on every client, or only the named ones, to its newest compatible release. `upgrade` is an alias, and bare `update` is the same as `update --all`.

```text
$ omaddons update
classic: updated 1 addon (tx 01M5Q2VA0J…) — /reload in game
  DBM  12.1.14 → 12.1.15
era: nothing to update (Questie is pinned)
retail: updated 2 addons (tx 01M5Q2VA1K…)
  DBM     12.1.14 → 12.1.15
  Plater  Plater-v661 → Plater-v662
Undo all of it with: omaddons undo
```

```bash
omaddons update                     # everything, everywhere
omaddons update dbm plater -t retail
omaddons update dbm --channel beta  # switch to beta and install its newest release
omaddons update --dry-run           # what would change, per client
omaddons update --strict            # exit 6 when only some addons update
```

What to update comes from the same statuses `outdated` shows, fetched once per ref (free inside 15 minutes; `--refresh` asks again). Each client with updates commits its own transaction, all under one run, so `omaddons undo` reverts every client and `omaddons undo <tx>` one of them. A failure on one client does not roll back another.

Some addons are left alone and say why (`skipped`, with `skip_reason`): `pinned` (by `pin` or `install --version`; naming it adds an `addon_pinned` warning whose fix is `omaddons unpin <addon> -t <target>`), `ahead`, `static` (a local file or URL), `unchecked`, and `blocked`. A newer release that turns out not to load on a client is `blocked`, not a failure; the verdict is remembered, so `outdated` shows it too. A fetch that fails fails its addons (`update` never installs from stale metadata) and makes the run `partial`. `unchanged` addons are counted in `summary` and listed with `-v`.

There is no `--version` and no `--force`: a given version is an install (`omaddons install <addon> --version V` or `omaddons pin <addon> --version V`, which pin it), and taking over another addon's folder is `omaddons install <addon> --force -t <target>`, which an `ownership_conflict` names as its fix.

### `pin <ADDON>` and `unpin <ADDON>`

Hold an addon at its release through `update`, and let it go again.

```text
$ omaddons pin questie
forever: pinned 1 addon (tx 01M5Q2VB2M…)
  Questie  pinned at bundle/v12.1.0+v1.0.6
undo: omaddons undo 01M5Q2VB2MZ0N4E6V7A8C9D1EF
$ omaddons unpin questie
forever: unpinned 1 addon (tx 01M5Q2VB3N…)
  Questie  unpinned at bundle/v12.1.0+v1.0.6
undo: omaddons undo 01M5Q2VB3NA1P5F7W8B9D0E2FG
forever: `omaddons update Questie -t forever` installs bundle/v12.2.0+v1.0.7
```

```bash
omaddons pin dbm                     # every client that has DBM
omaddons pin dbm -t retail
omaddons pin dbm --version 12.1.14   # install that release where DBM is installed, then pin it
omaddons unpin dbm
```

A pin changes only the record, never a game file, so it works while the game runs. Like every change it is a transaction (kind `pin`), with one per client under one run, so `omaddons undo` reverts it. An addon already pinned (or unpinned) is left as it is, with no transaction. A pin keeps the addon's channel and its reason (a pinned dependency stays a dependency). `--version` takes a tag (`gh:` refs) or a rev (`git:` refs). `url:` and local installs have no other release to choose, so `--version` is refused for them. After `unpin`, the last line says what `update` would install, from the remembered metadata and without a request.

### `sync [FILE]`

Make every client match a desired-state file: `addons.toml` in the config directory (`~/.config/omaddons/addons.toml`), or the file named.

```toml
schema_version = "1.0"

[family.mainline]                  # every live mainline client
addons = ["WeakAuras", "DBM", "gh:BigWigsMods/BigWigs"]

[[family.mainline.addon]]          # the table form, when an entry needs options
name = "Plater"
version = "Plater-v661"
pinned = true

[target."ptr"]                     # one client by id: the only way to reach a test client
addons = ["DBM"]
```

| Flag | Meaning |
|---|---|
| `--strict` | Exit 6 when only some of the addons sync |
| `--no-deps` | Install only the listed addons, not the ones their TOCs require |

**The file.**

- A family section applies to every live client of that family, and never to a test client (a PTR or beta).
- A `[target."<id>"]` section applies to that one client, and its entries replace family entries for the same addon.
- An entry is a name or a ref, read the way `install` reads its arguments. Installed names are looked up on that client only, and relative paths are read against the file's own directory.
- An entry in table form has these keys:
  - `name`, unless `ref` is given;
  - `ref`;
  - `channel`: absent means `stable`, whatever `default_channel` says;
  - `version`;
  - `pinned` (default `false`);
  - `dependency`: a version lock for a dependency, not a request.
- Unknown keys are warned about (`desired_unknown_key`), and so are sections naming no known client (`desired_unknown_target`).
- A file that cannot be read changes nothing: `desired_state_invalid`, exit 2, with the line.

**What sync does.**

- It installs what is missing.
- A pinned entry at another version installs that version, pinned.
- An entry on another channel switches the addon to it: a records-only change when that channel's newest release is the installed one.
- A pin that differs from the file is set or cleared without touching a folder.
- It never moves an installed addon to a newer release; that is `omaddons update`'s job.

So a second `sync` changes nothing and sends no request. Each client with changes gets one transaction of kind `sync`, all under one run, and `omaddons undo` reverts the whole sync. An entry that cannot be resolved fails on its own (`partial: true`), and so do two entries that name the same addon. What an addon's TOC requires installs with it, as for `install`.

### `remove <ADDON>...`

Remove managed addons from one target, as an undoable transaction. An addon is named by its name, its ref or a folder it owns.

```bash
omaddons remove DBM -t retail
omaddons remove gh:BigWigsMods/BigWigs -t era
```

Unmanaged, linked (symlinked) and `Blizzard_*` folders, and names that match no managed addon, are refused with `not_managed`.

An addon whose folders were all deleted by hand is forgotten: its record and index entries go, in a transaction that records each folder (`record` ops) and that `undo` reverts. Such a remove changes no folder, so it also goes through while the game runs.

### `undo [TX]`

Revert what a command did, as new transactions, SavedVariables included.

A command that changes several clients commits one transaction per client, and all of them share a run id. Bare `undo` reverts the most recent run, every one of its transactions, newest first. After a single-client command that is one transaction, as before. Those undo transactions share a run of their own, so a second `undo` reverts them all again.

| Flag / argument | Meaning |
|---|---|
| `TX` | One transaction: an id or a unique prefix of 8+ characters |
| `--run RUN` | Every transaction of one run (`history` shows run ids); with `-t`, only its transaction on that client |
| `-t TARGET` | The most recent transaction on that client only |
| `--last-by human\|agent\|widget` | This actor's most recent run (an undo counts too) |
| `--saved-variables auto\|restore\|keep` | `auto` (default) restores what changed and defers that restore while the game runs. `restore` refuses while the game runs. `keep` never touches SavedVariables |

```bash
omaddons undo
omaddons undo 01M51F4QY5KB
omaddons undo --run 01M51F4R2ZQX
omaddons undo -t retail
omaddons undo --last-by agent
omaddons undo 01M51F4QY5KB --saved-variables restore   # finish a deferred restore after quitting
```

When one transaction of a run cannot be undone (`undo_conflict`, say), the others still are. The result is then `partial`, and `transactions[]` in the JSON data holds one result per transaction. The fields before it describe the first transaction undone, as they always did. When every transaction fails, `undo` fails with the first error.

Undoing the newest change never conflicts with a later transaction. Undoing an older one is refused with `undo_conflict` if a later transaction touched the same folders, and the fix names the newer one to undo first. `undo_conflict` also covers a transaction already undone, and a folder it removed that was put back by hand.

### `history`

What changed, when, by whom and why, newest first.

```bash
omaddons history
omaddons history --limit 50 --since 2w
omaddons history --by agent --since 12h
omaddons history --run 01M51F4R2ZQX        # one run's transactions (a prefix of 8+ characters works)
omaddons history -t retail -v --json       # each folder operation (ops) instead of counts
```

`--since` takes `30m`, `12h`, `1d`, `2w` or an RFC 3339 time. The global `--actor` still filters too; given with a different `--by`, it is a usage error. Every line shows its run (`run` in JSON, `null` for transactions from before milestone 2, each of which is a run by itself).

Each line counts the folders added (`+`), replaced (`~`) and removed (`-`). A transaction that changed records without moving a folder, such as forgetting an addon whose folders were gone, also shows `=N` for the folders it recorded (`changes.recorded` in JSON).

### `list`

The addons in each target's AddOns folder.

```bash
omaddons list                              # every target: managed addons with version, ref, channel
omaddons list -t forever --unmanaged       # also unmanaged, linked and invalid folders, with title, version and load verdict
omaddons list -t wotlk --protected         # also Blizzard_* folders
```

### `outdated`

Which managed addons have updates, on every client (or the clients `-t` names), without changing anything. `check` is an alias.

```text
$ omaddons outdated
classic (5.5.4, mists)
  Bar  v1  → v2  gh:o/bar
  Foo  v1  → v2  gh:o/foo
retail (12.1.5, mainline)
  Bar  v1  → v2
  Foo  v1  → v2
  Pin  v1  pinned (latest v2)
4 updates on 2 clients · 1 pinned · checked just now (3 requests, 0 reused)
Run `omaddons update` to install them.
```

```bash
omaddons outdated -v              # every addon, current, static and unchecked ones included
omaddons outdated --check         # exit 6 (check_failed) when any update is available
omaddons outdated --refresh       # ask every source now, even inside the 15-minute window
omaddons outdated --verify        # download and verify each update not yet known to load
```

Each distinct ref and channel is fetched at most once, through the [release memo](#files-and-state). A check from the last 15 minutes is reused without a request, so a second run is free. `--offline` answers from the memo whatever its age, with a `stale_check` warning. Addons installed from a local file, a URL or `--file` have nothing upstream to compare and make no request (`static`).

The listing shows only what needs a decision; `-v` lists everything, and the summary always counts every status:

| Status | Meaning |
|---|---|
| `update_available` | A newer release exists. `confidence` says how likely it is to load: `verified` (a download proved it), `declared` (the publisher's metadata names this client), `likely`, `unlikely` |
| `pinned` | Installed with `--version`, and a newer release exists (`latest`) |
| `blocked` | The newest release is known not to load on this client (`blocked_reason`) |
| `ahead` | The installed release is newer than the newest one on its channel |
| `failed` | The source cannot answer for it (a deleted repository: fix `omaddons remove <name> -t <id>`, or a fetch error) |
| `current`, `static`, `unchecked` | Shown with `-v` only (`unchecked_reason`: `never_checked`, `offline`, `source_disabled`, `no_eligible_release`) |

When a fetch fails, a remembered answer is used and the warning says so; with nothing remembered, the item is `failed`. Either way the run is `partial`. `--verify` downloads only the updates not yet verified, in a dry run that takes no lock, and remembers the verdicts: a candidate that does not load becomes `blocked` from then on. `outdated` writes only the release memo, the download cache and `net.log`.

### `targets`

List the clients, or add, remove, rename and choose them.

```bash
omaddons targets                           # id, key, family or modes, version, kind, label, path
omaddons targets --rescan                  # ignore the discovery cache
omaddons targets add /mnt/games/turtle --name turtle   # a root, flavor folder, client dir, Interface or AddOns path
omaddons targets rename wotlk warmane
omaddons targets default retail            # install's default when -t is absent; config default_target still wins
omaddons targets rm warmane                # unregister; nothing in the client directory is touched
```

An id you choose (`--name`, `rename`) must match `^[a-z0-9][a-z0-9._-]*$`, be at most 32 characters, contain no `@`, and not be a key, family name or product code. Built-in default ids such as `vanilla` or `mists` are exempt, which is why they can shadow a family (see [Choosing a target](#choosing-a-target-with--t)).

### `info <REF>`

What an addon is: installed records on every target, the catalog entry, and the newest release.

```text
$ omaddons info weakauras --offline
WeakAuras  gh:WeakAuras/WeakAuras2
  catalog: folders WeakAuras, WeakAurasArchive, WeakAurasModelPaths, WeakAurasOptions, WeakAurasTemplates · families vanilla, tbc, cata, mists, titan
  latest: not checked (offline)
```

The newest release comes through the [release memo](#files-and-state): a check made in the last 15 minutes is reused without a request, and `--refresh` checks again anyway. With `--offline`, `latest` is the remembered answer whatever its age, with a `stale_check` warning, or `null` (as above) when nothing is remembered. A missing repository is remembered too, so asking again within the window costs nothing; a network or server error never is.

### `config`

Show or change `config.toml`. `set` edits the one value in place with `toml_edit`, keeping the file's comments and order, and validates the whole file before writing it.

```bash
omaddons config                                  # every key, its value, and "# default" where unset
omaddons config get running_policy
omaddons config set default_channel beta
omaddons config set git_hosts '["codeberg.org"]' # arrays and tables take TOML syntax
omaddons config set limits.snapshot_max_bytes 4GiB
omaddons config path
```

A mistyped key is a usage error whose fix is the closest real key, e.g. `omaddons config set default_channel beta`.

### `net-log`

Every network request and git subprocess omaddons made, with a summary by host.

```bash
omaddons net-log
omaddons net-log --since 1d --json
```

### `help [COMMAND]`

```bash
omaddons help
omaddons help install
omaddons targets add --help    # subcommands: --help, or omaddons help "targets add" (see Known issues)
```

---

## The JSON contract

`--json` (or `--robot`, `--format json`, `OMADDONS_FORMAT=json`) makes every command print **exactly one JSON object on one line on stdout**, on success and on failure, including usage errors and panics. Keys always come in the same order.

### Success

```json
{"ok":true,"schema_version":"1.0","tool_version":"0.1.0","command":"install","exit_code":0,"partial":false,
 "data":{"dry_run":false,"target":{"id":"retail","key":"01M51EX04HHWM46XE9F2B9JXED"},"tx":"01M51EX645RCVFN6Z4ZV5GRWVS","kind":"install",
   "items":[{"request":"./Bar-2.1.0.zip","ref":"file:/home/me/src/./Bar-2.1.0.zip","name":"Bar","status":"installed",
     "from_version":null,"to_version":"2.1.0","version_confidence":"toc","channel":"stable",
     "folders":{"added":["Bar"],"replaced":[],"removed":[],"skipped":[],"recorded":[]},"error":null}],
   "reload_required":"none","atomic_swap":true,"creates_addons_dir":false},
 "warnings":[],
 "commands":[{"action":"undo","command":"omaddons undo 01M51EX645RCVFN6Z4ZV5GRWVS","argv":["omaddons","undo","01M51EX645RCVFN6Z4ZV5GRWVS"],"destructive":false}],
 "meta":{"elapsed_ms":3,"offline":true}}
```

<sub>Wrapped for reading; the real output is one line.</sub>

### Failure

```json
{"ok":false,"schema_version":"1.0","tool_version":"0.1.0","command":"install","exit_code":3,
 "error":{"code":"target_ambiguous","message":"./Bar-2.1.0.zip is compatible with 2 targets: era, retail",
   "fix":"omaddons install ./Bar-2.1.0.zip -t era","fix_argv":["omaddons","install","./Bar-2.1.0.zip","-t","era"],
   "alternatives":[{"display":"omaddons install ./Bar-2.1.0.zip -t retail","argv":["omaddons","install","./Bar-2.1.0.zip","-t","retail"]}],
   "retryable":false,"details":{"candidates":["era","retail"]}},
 "warnings":[],"commands":[],"meta":{"elapsed_ms":1,"offline":true}}
```

### Fields

| Field | Type | Meaning |
|---|---|---|
| `ok` | bool | `true` exactly when `exit_code` is 0 |
| `schema_version` | string | `"1.0"` |
| `tool_version` | string | the binary's version |
| `command` | string | `install`, `targets add`, `config set`, … |
| `exit_code` | int | the process exit code |
| `partial` | bool | success with some failed items (present on success and on `check_failed`) |
| `data` | object | the command's result (success, and `check_failed`) |
| `error` | object | `code`, `message`, `fix` (display string), `fix_argv` (argv array, or `null` when the fix is prose), `alternatives` (`[{display, argv}]`), `retryable`, `details` (always an object) |
| `warnings` | array | `{code, message, fix, fix_argv}`. On a partial run, each failed item is mirrored here |
| `commands` | array | suggested next steps: `{action, command, argv, destructive}`, e.g. `undo` after a change, `list_unmanaged`, `forget_target` (destructive) |
| `meta` | object | `elapsed_ms`, `offline` (always `false` on usage errors; see Known issues) |

`check_failed` (exit 6) is the one failure that also carries `data` and `partial`, so `status --check --json` still returns the full status.

### Exit codes

| Exit | Name | Meaning |
|---|---|---|
| 0 | `ok` | Success, including partial success (`partial: true`, each failed item in `data`) |
| 1 | `failed` | Runtime failure: network, source, archive integrity, I/O, state, internal. `error.retryable` says whether to retry |
| 2 | `usage` | Unknown command or flag, bad argument, bad ref syntax |
| 3 | `not_found` | Target, ref, addon, plan or transaction not found, or ambiguous |
| 4 | `refused` | Safety refusal: running client, protected folder, ownership or case conflict, unsafe archive, wrong flavor, undo conflict, blocked host |
| 5 | `locked` | Another omaddons process held the lock past the 5-second timeout; retryable |
| 6 | `check_failed` | An opt-in assertion failed: `status --check` or `install --strict` |

A panic still produces one `internal` envelope and exit 1, never Rust's 101.

<details>
<summary><strong>Every error code (39)</strong></summary>

| Code | Exit | Retryable | When |
|---|---|---|---|
| `usage` | 2 | no | bad flag, value, `--since`, `--limit`, `--actor`, `--reason`, config key or id |
| `ref_invalid` | 2 | no | malformed ref: `gh:owner`, `git:` without https, `http://` |
| `desired_state_invalid` | 2 | no | `sync`'s `addons.toml` cannot be read: broken TOML, a bad value, an unknown family, an entry listed twice, or a newer schema (`details: {path, line, key, reason}`); also a failed `sync` item when two entries name the same addon |
| `target_not_found` | 3 | no | no target matches the selector; a known game mode whose client is missing |
| `target_ambiguous` | 3 | no | several targets fit; one alternative per candidate |
| `ref_ambiguous` | 3 | no | a name matches several addons or a local path |
| `not_found` | 3 | no | unknown name, path, tag, transaction or pruned snapshot; nothing to undo |
| `not_managed` | 3 | no | `remove` of an unmanaged, linked or `Blizzard_*` folder, or a name no managed addon answers to |
| `plan_not_found` | 3 | no | reserved for stored plans (M4) |
| `no_compatible_release` | 3 | no | no candidate loads on the client; `details.tried` lists them |
| `game_running` | 4 | no | the running policy refuses (fix appends `--while-running`), or `undo --saved-variables restore` while the client runs |
| `protected_folder` | 4 | no | the archive contains a `Blizzard_*` folder |
| `ownership_conflict` | 4 | no | another addon owns the folder; fix appends `--force` |
| `unmanaged_folder` | 4 | no | an unmanaged folder exists; fix appends `--force` |
| `case_collision` | 4 | no | a folder name differs only in case from an existing one |
| `wrong_flavor` | 4 | no | a `url:`, `file:` or `--file` archive does not load on the client |
| `unsafe_archive` | 4 | no | traversal, absolute paths, symlinks, devices, duplicate or case-colliding entries, the compression-ratio limit |
| `archive_too_large` | 4 | no | over the entry, uncompressed-size or download limit |
| `undo_conflict` | 4 | no | a later transaction touched the same folders, already undone, or a removed folder put back by hand |
| `host_blocked` | 4 | no | host not on the allowlist or in `git_hosts` |
| `scope_denied` | 4 | no | the filesystem writer refused a path outside its scope |
| `state_too_new` | 4 | no | a state file written by a newer omaddons |
| `locked` | 5 | yes | lock held past 5 s; `details.holder` |
| `check_failed` | 6 | no | `--check` or `--strict` |
| `network` | 1 | yes | connection failure |
| `rate_limited` | 1 | yes | GitHub REST budget exhausted, or any host's rate-limit answer (403/429 with no requests remaining, `Retry-After` over 60 s, a 429 that outlasts the retries); `details.reset_at` |
| `download_failed` | 1 | yes | download failure |
| `http_status` | 1 | 5xx only | unexpected status; `details.status` |
| `hash_mismatch` | 1 | first attempt | downloaded bytes did not match the expected hash |
| `plan_stale` | 1 | yes | another run changed the target between plan and commit |
| `offline` | 1 | no | `--offline` blocked a request |
| `bad_archive` | 1 | no | malformed, encrypted, or not Stored/Deflate |
| `distribution_disabled` | 1 | no | reserved for CurseForge (M5) |
| `source_disabled` | 1 | no | WoWInterface or Tukui (M2), CurseForge or Wago (M5); a `git:` ref on another host when `git` is missing or older than 2.32 |
| `auth_required` | 1 | no | reserved for credentialed sources |
| `auth_rejected` | 1 | no | reserved for credentialed sources |
| `state_corrupt` | 1 | no | an unreadable state or config file (with line and column) |
| `io` | 1 | no | filesystem error; `details.path` |
| `internal` | 1 | no | a bug or panic; fix is `omaddons status` |

</details>

<details>
<summary><strong>Every warning code (26)</strong></summary>

| Code | Meaning |
|---|---|
| `journal_recovered` | an interrupted transaction was finished or rolled back |
| `journal_pending` | an interrupted transaction waits for the next change command |
| `tx_in_progress` | another omaddons process is committing |
| `atomic_swap_unavailable` | the AddOns filesystem has no `renameat2` exchange; two journaled renames were used |
| `reload_required` | `/reload` in game to pick up the change |
| `saved_variables_deferred` | the game is running; restore SavedVariables later with the given command |
| `snapshot_cap_exceeded` | protected snapshots alone exceed the size cap |
| `retention_failed` | pruning failed after commit; the transaction stands |
| `out_of_date` | the TOC's interface is older than the client's; it loads only with "Load out of date AddOns" enabled |
| `missing_dependency` | a folder an installed addon's TOC requires was not installed: no keyless source ships it, several projects do, its only build is out of date, a dependency limit was reached, or `--no-deps`; the fix names a candidate when there is one |
| `toc_comma_on_legacy` | a comma interface list on a client that reads only the first number |
| `client_not_launched_since_patch` | `Config.wtf` predates the client's current version |
| `widget_outdated` | reserved for the widget (M3) |
| `state_version_older` | state migrates forward on the next write |
| `state_version_newer` | writes refused until omaddons is upgraded |
| `config_unknown_key` | unknown key in `config.toml`, with the closest valid key |
| `unknown_flavor` | an unrecognized flavor folder, or a custom client whose version maps to no family, became a target |
| `discovery_truncated` | discovery hit its directory budget; add the client with `targets add` |
| `target_key_duplicate` | two clients carry the same marker (a copied install) |
| `catalog_override_invalid` | a `catalog.d` file was skipped |
| `ownership_transferred` | `--force` moved a folder from one addon to another |
| `credential_file_mode` | reserved for credential files other users can read (no command reads credentials yet) |
| `addon_pinned` | a pinned addon was left at its release; the fix shows it with `info`, and `--version` moves it |
| `stale_check` | an offline answer came from the release memo, whatever its age; it names the oldest remembered check |
| `desired_unknown_key` | `addons.toml` has a key omaddons does not know (named, with its line); it is ignored |
| `desired_unknown_target` | an `addons.toml` `[target.<id>]` section names no known client; its entries are skipped |

</details>

### stderr events

When stderr is not a terminal, or `CI` is set, or `TERM=dumb`, stderr carries NDJSON events. Otherwise it carries human text.

```json
{"event":"error","code":"source_disabled","message":"wowi:5108: WoWInterface arrives in M2","fix":"omaddons help install","fix_argv":["omaddons","help","install"],"retryable":false,"exit_code":1}
{"event":"warning","code":"reload_required","message":"…","fix":"/reload in game","fix_argv":null}
{"event":"log","level":"debug","message":"discovery: scanned yes, roots reused 2, parsed 0, cache written no"}
```

Exactly one `error` event is emitted per failure, even with `--quiet`. `debug` log events need `-v`. `progress` events come from long commands (`install`, `update` and `outdated` today) when progress is on: `{"event":"progress","phase":"download","message":"gh:o/foo@v1 on retail","current":1,"total":2}`. The phases are `fetch`, `download`, `verify`, `stage` and `commit`. Within a phase, `current` counts up to a fixed `total`, and each item appears once. On a terminal in human mode, progress is one line that is rewritten in place.

### Stability

The envelope is `schema_version` 1.0. Until the first tag (milestone 4) the contract can still change. From that tag on, it is frozen. The contract suite (`tests/contract/`) runs a matrix of success and failure cases across the commands. It checks key order, `ok`/`exit_code` agreement and registered codes, and that every `fix_argv` and `commands[].argv` round-trips through shell quoting to its display string. 24 snapshots pin representative envelopes.

---

## Safety model

### Transactions

```text
 plan (no lock)              stage (no lock)                  commit window (global flock, milliseconds)
 ──────────────────────      ─────────────────────────────    ─────────────────────────────────────────────
 fetch each ref once         extract to                       recover leftover journals
 download → blob cache       <client>/Interface/.omaddons/    re-check the target's generation:
 inspect the archive           stage/<tx>/<Folder>/             changed and conflicting → plan_stale
 judge every TOC against     verify each file's size            already done by another run → no-op
   the client's rules          and SHA-256                    journal: prepared
 ownership + running-        one syncfs                       create Interface/AddOns if never launched
   policy checks                                              snapshot touched SavedVariables + state files
                                                              swap each folder (journal step per rename):
                                                                new      → RENAME_NOREPLACE
                                                                replaced → RENAME_EXCHANGE
                                                                old/removed → .omaddons/snapshots/<tx>/AddOns/
                                                              journal: swapped
                                                              write record, files index, history line
                                                              journal: committed → release the lock
                                                              then: status.json, cleanup, retention
```

- **Interrupted runs.** The next command that commits a change (install, remove or undo) recovers under the lock first. A journal that reached `swapped` rolls forward, and anything earlier rolls back. Each step records the device and inode of both paths, so rollback reverses a rename only if it actually happened. Read commands never recover anything: they report `journal_pending`, and `status` lists it under attention.
- **No atomic rename.** On filesystems without `renameat2` exchange (NTFS through ntfs-3g, some FUSE mounts), omaddons falls back to two journaled renames per folder and reports `atomic_swap: false` with an `atomic_swap_unavailable` warning. A cross-device rename is an error, never a fallback, and the staging area must be on the same filesystem as AddOns.
- **Concurrency.** Planning and staging take no lock, so a terminal and an agent (and later the widget) can work at once. The global lock (`~/.local/state/omaddons/lock`, an flock that is never renamed or deleted) is held for the commit window, and briefly for retention, `config set` and `targets` changes. Tests assert each commit holds it under 50 ms, and the M1 checkpoint measured at most 2 ms. A second run waits up to 5 seconds, then fails with `locked` (exit 5, retryable, `details.holder`).
- **Crash testing.** All 24 engine failpoints are armed as a real `abort` in a child process for first installs, installs, updates and removes. The state afterwards must be wholly old or wholly new, and at least ten of them must actually crash a scene. A failpoint off a scene's path has to let it succeed.

### Undo

- An undo is its own transaction (kind `undo`, `reverts: <tx>`), so it can be undone too.
- Every history line carries the run (command invocation) it belongs to. Bare `undo` reverts a whole run, one undo transaction per client in its own lock window. Runs are attribution, not atomicity: each client's transaction commits and undoes on its own.
- Folders come back byte for byte from `<client>/Interface/.omaddons/snapshots/<tx>/AddOns/`; tests compare tree digests. Each addon's record and files index are restored, while other addons' later changes stand.
- **SavedVariables.** Only the touched addons' own files (`WTF/…/SavedVariables/<Folder>.lua` and `.lua.bak`, account-wide and per character) are snapshotted, with a SHA-256 manifest. On undo, a file that is unchanged since is left alone, a changed one is restored, and one the new version created is moved aside. With `--saved-variables auto` and the game running, folders revert (if the running policy allows), and the SavedVariables restore is deferred. You get a `saved_variables_deferred` warning carrying the command to run after quitting: `omaddons undo <tx> --saved-variables restore`.
- **Conflicts.** `undo_conflict` (exit 4) when a later transaction touched the same folders (undo the newest first), when the transaction was already undone, or when a folder it removed has been put back by hand. A snapshot pruned by retention gives `not_found`.
- Registry changes and `target.json` are not reverted.

### Ownership and protected folders

| Folder | What happens |
|---|---|
| `Blizzard_*` (any case) | Never written, even with `--force`. Two independent layers refuse it: the planner and the filesystem writer's scope table |
| A symlink (your development checkout) | Never touched, even with `--force` (class `linked`) |
| A name that differs only in case from an existing folder | `case_collision`, even with `--force`. Wine folds case, so the game would see only one. An addon's own folder may change case on update |
| Two AddOns folders that differ only in case (`AddOns` and `addons`) | Refused. An existing lowercase `interface/addons` alone is used as the AddOns folder |
| Another managed addon's folder | `ownership_conflict`; `--force` transfers it and marks the old owner degraded |
| An unmanaged folder | `unmanaged_folder`; `--force` replaces it, and the snapshot keeps the original for undo |

Ownership comes only from omaddons' own records. Other managers' metadata is not read yet; adopting their addons is milestone 2.

### Hostile archives

Refused as `unsafe_archive` before anything reaches the client: path traversal (`..`), absolute paths, drive letters, control characters, symlinks, devices, FIFOs and sockets, duplicate entries, entries that collide when case is ignored, names that are both a file and a directory, entries that expand past their declared size, and a compression ratio over 200:1 once past 32 MiB. Archives with more than 20,000 entries or more than 1 GiB uncompressed are refused as `archive_too_large`. Encrypted entries and compression other than Stored or Deflate are `bad_archive`. Staged files are written with fixed modes (0644 and 0755).

### Running clients

Running clients are detected live from `/proc` (your own processes only). The executable name is matched case-insensitively, and the Wine path is mapped through the prefix's drive letters.

| `running_policy` | While the client runs |
|---|---|
| `safe` (default) | Adds and replacements that keep the loaded TOC go through, with `reload_required: "reload"` (`/reload` in game). Removing a folder the client loads, or an update that deletes or renames the TOC it loads, is refused with `game_running` until it exits. On legacy and unrecognized custom clients, every change is refused |
| `refuse` | Every change is refused |
| `allow` | Nothing is refused; the result says whether `/reload` or a restart is needed |

`--while-running` lifts these folder rules for one command, even under `refuse`. **SavedVariables are never written while the client runs**, under any policy or flag, because the game rewrites them on logout.

### Retention

Retention runs after every commit, for the target just changed:

- Snapshots younger than 24 hours and the target's most recent transaction are always kept.
- At most the newest 10 snapshots within 7 days are kept, the protected ones included; everything outside that set is pruned.
- If the kept set exceeds 2 GiB for the target, its oldest unprotected snapshots are pruned until it fits.
- History lines are never pruned.
- The download cache is an LRU capped at 1 GiB. Archives of releases installed on any target are pinned, so the cache can exceed the cap.
- Orphaned stage directories and in-flight locks older than an hour are swept.

All of these numbers live in `src/limits.rs`, and most can be changed in `[limits]`.

---

## Network and privacy

omaddons contacts **only** these hosts. The list is checked on every redirect hop and matched exactly (subdomains are never implied), and HTTP requests must be `https` on port 443.

| Host | Used for | State |
|---|---|---|
| `github.com` | `releases/latest`, `release.json`, commits feeds | used |
| `api.github.com` | REST fallback (beta and alpha channels, releases without `release.json`) | used |
| `codeload.github.com` | zips of exact commits for `git:` refs | used |
| `objects.githubusercontent.com`, `release-assets.githubusercontent.com` | release asset downloads | used |
| `api.mmoui.com`, `cdn.wowinterface.com` | WoWInterface | allowed for `url:` today; the source arrives in M2 |
| `api.tukui.org` | Tukui | allowed for `url:` today; the source arrives in M2 |
| `api.curseforge.com`, `edge.forgecdn.net`, `mediafilez.forgecdn.net` | CurseForge | off until enabled (M5) |
| `addons.wago.io` | Wago | off until enabled (M5) |
| every host in `git_hosts` | `git:` refs through the git binary | only when you list them |

**What a stable GitHub install costs:** zero REST calls. omaddons reads `releases/latest` without following its redirect, then fetches `release.json` and the asset. Both of those redirect to `release-assets.githubusercontent.com`, so `net.log` shows five lines per repository, plus one more download for each candidate that does not load. Requests are limited to 6 at a time overall and 3 per host. Retries cover 429, 5xx and network errors, up to 3 times with full-jitter backoff (0.5 s base, 8 s cap), and honor `Retry-After` up to 60 s.

**Tokens.** `OMADDONS_GITHUB_TOKEN`, if set, is sent only to `api.github.com` and raises the REST budget from 60 to 5,000 requests an hour. `GITHUB_TOKEN` is never read, and `Authorization` is dropped on any redirect to another host. With or without a token, omaddons stops sending REST requests once the response headers say 5 or fewer remain, and reports `rate_limited` with the reset time. The budget is tracked per run, so the first REST request of a run is always sent.

**The git subprocess** runs with a cleared environment. Only `PATH` and `HOME` are kept, plus `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_ALLOW_PROTOCOL=https`. It also gets no credential helper, no askpass, no redirects and https as the only protocol. Arguments go as argv (never through a shell), and the run is in its own process group, which is killed on timeout. URLs with embedded credentials are refused.

**`net.log`.** Every request that reaches the network (each redirect hop and each retry) and every git run against a repository is one JSON line in `~/.local/state/omaddons/net.log`, rotated once at 10 MiB. Requests refused before sending (offline, `host_blocked`) and the `git --version` probe are not logged. Query strings and headers are never written. An illustrative line:

```json
{"time":"2026-10-14T21:00:00Z","method":"GET","host":"github.com","path":"/WeakAuras/WeakAuras2/releases/latest","status":302,"bytes":0,"duration_ms":34,"purpose":"release-latest","via":"http","error":null}
```

Production purposes are `release-latest`, `release-json`, `rest-releases`, `commits-atom` and `asset` for HTTP. git lines have `"method":"GIT"`, `"via":"git"`, the exit code as `status`, and a purpose of `ls-remote`, `clone`, `init`, `fetch`, `checkout` or `rev-parse`.

**Offline.** `--offline` or `OMADDONS_OFFLINE=1` refuses all network access, git included. Local installs, `remove`, `undo` and every read command work offline. Release metadata a command fetched is remembered in the release memo; offline, `info` answers from it with a `stale_check` warning.

**Fetch once.** Within one command each ref is fetched once, however many targets and engine runs need it: `install` without `-t` probes every live client and then installs with the same answer. `install` always asks the source for the newest release; `info` reuses an answer up to 15 minutes old (`SOURCE_RECHECK_MIN_INTERVAL`) unless `--refresh` is given.

**Also:** TLS uses rustls with the bundled Mozilla root store (webpki-roots), not the system store. HTTP proxies are not supported. There is no telemetry.

---

## Configuration

`~/.config/omaddons/config.toml` (respects `XDG_CONFIG_HOME`). Every key is optional. Edit it by hand or with `omaddons config set`, which keeps comments and order. A broken file fails commands with `state_corrupt` citing line and column, and unknown keys only warn.

```toml
# Clients discovery cannot find: extra roots scanned on every run.
# Use absolute paths: "~" is not expanded (and not rejected either).
extra_roots = ["/mnt/games/turtle"]

# The target for install without -t: an id, key or any other -t form.
default_target = "retail"

# Release channel for new installs: "stable", "beta" or "alpha".
default_channel = "stable"

# While a client runs: "safe", "refuse" or "allow" (see Safety model).
running_policy = "safe"

# Extra https git hosts for git: refs, as bare host names. Needs git 2.32+.
git_hosts = ["codeberg.org"]

# Read today, used from milestone 3 (WTF backups and the widget's check interval).
auto_backup = "off"          # "off" or "daily"
check_interval_hours = 6     # 0–48; 0 checks only when the panel opens

[limits]                     # sizes are integers or "KiB" / "MiB" / "GiB" strings
archive_max_entries = 20000
archive_max_uncompressed = "1GiB"
archive_max_ratio = 200
download_max_bytes = "512MiB"    # values above 512 MiB do not take effect yet (Known issues)
tx_retention_count = 10
tx_retention_days = 7
snapshot_max_bytes = "2GiB"      # per target, beyond the 24-hour window
blob_cache_max_bytes = "1GiB"

# Per-target overrides, keyed by id or key.
[targets.wotlk]
label = "my 3.3.5a server"
```

| Key | Default | Meaning |
|---|---|---|
| `extra_roots` | `[]` | Extra discovery roots |
| `default_target` | none | `install`'s target when `-t` is absent; wins over `targets default` |
| `default_channel` | `stable` | Channel for new installs |
| `running_policy` | `safe` | `safe`, `refuse` or `allow` |
| `git_hosts` | `[]` | Hosts allowed for `git:` refs through the git binary |
| `auto_backup` | `off` | Read; used in milestone 3 |
| `check_interval_hours` | `6` | Read; used in milestone 3 |
| `limits.archive_max_entries` | `20000` | Entries per archive |
| `limits.archive_max_uncompressed` | `1GiB` | Expanded size per archive |
| `limits.archive_max_ratio` | `200` | Compression ratio, checked past 32 MiB |
| `limits.download_max_bytes` | `512MiB` | Largest download |
| `limits.tx_retention_count` | `10` | Snapshots kept per target within the window |
| `limits.tx_retention_days` | `7` | The retention window |
| `limits.snapshot_max_bytes` | `2GiB` | Snapshot total per target beyond 24 hours |
| `limits.blob_cache_max_bytes` | `1GiB` | Download cache (installed releases are pinned) |

### Per-target overrides

```toml
[targets.retail]
label = "main"               # shown in status and targets

[targets.mystery]
family = "wrath-legacy"      # a misdetected or unreadable client: re-picks its load rules
```

### Rules overrides (advanced)

The detection tables can be patched without a new release:

- `[client_rules.<row>]` (or `[rules.client_rules.<row>]`) patches one load-rules row; an unknown row name adds a new row, which then needs every required field. Rows are `mainline`, `mainline-12.0.7`, `mainline-10.2.7`, `mainline-9.1`, `forever`, `vanilla`, `tbc`, `wrath`, `titan`, `cata`, `mists`, the five `*-legacy` rows, `fallback-battlenet` and `fallback-custom`. The fields are `family`, `install_kind`, `version_min`, `version_max`, `game`, `game_types`, `multi_toc`, `comma_interface`, `game_suffixes`, `family_suffixes`, `legacy_suffixes`, `directive_conditionals`, `path_variables`, `interface_includes_patch` and `legacy`.
- `[rules]` can add or replace `products`, prepend `version_families` rules, and replace `family_ranges`.

Client-rules rows and `family_ranges` apply on the next command. The discovery cache does not notice changes to `products`, `version_families` or `interface_includes_patch`, so run `omaddons targets --rescan` after editing those.

### Environment variables

| Variable | Effect |
|---|---|
| `OMADDONS_ACTOR` | Default `--actor` |
| `OMADDONS_OFFLINE` | `1` (exactly) means `--offline` |
| `OMADDONS_FORMAT` | `json` means `--json` |
| `OMADDONS_GITHUB_TOKEN` | Bearer token for `api.github.com` only |
| `NO_COLOR`, `CI`, `TERM=dumb` | No color; `CI` and `TERM=dumb` also switch stderr to NDJSON |
| `HOME` | Required and absolute: `~/Games`, `~/.wine`, launcher configs |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | Move omaddons' config, state and cache (used only when absolute). `XDG_CONFIG_HOME` also moves the Lutris and Heroic config lookups |
| `XDG_DATA_HOME` | Moves the Lutris, Steam and Bottles data lookups; omaddons keeps no files there |

Debug builds also read `OMADDONS_FAILPOINTS`, `OMADDONS_HTTP_FIXTURES`, `OMADDONS_RECORD` and `OMADDONS_PROC_ROOT` for testing. Release builds ignore them.

---

## Files and state

| Path | What | Safe to delete? |
|---|---|---|
| `~/.config/omaddons/config.toml` | Settings | Yes (defaults apply) |
| `~/.config/omaddons/catalog.d/*.toml` | Your catalog overrides | Yes |
| `~/.local/state/omaddons/registry.json` | Registered targets and the default | Loses ids and registrations |
| `~/.local/state/omaddons/targets/<key>.json`, `<key>.files.json` | Per-target addon records, and every managed file's size and hash | Loses management |
| `~/.local/state/omaddons/history.jsonl` | One line per committed transaction | Loses history |
| `~/.local/state/omaddons/snapshots/<tx>/`, `journal/`, `inflight/`, `trash/` | Undo data for state files, journals, in-flight locks, staged deletions | No |
| `~/.local/state/omaddons/lock` | The global lock (never delete while running) | No |
| `~/.local/state/omaddons/status.json` | A derived summary for the future widget | Yes |
| `~/.local/state/omaddons/net.log`, `net.log.1` | The request log | Yes |
| `~/.cache/omaddons/blobs/`, `http/`, `discovery.json`, `tmp/`, `trash/` | Downloads, HTTP validators, the discovery cache, temporary downloads and git checkouts, staged deletions | Yes when no command is running; reinstalls then download again |
| `~/.cache/omaddons/releases.json` | The release memo: the newest release each fetch saw (at most 4,096 entries), failures retrying cannot change, and what downloading candidates taught (each archive's hash, and whether it loads on a client) | Yes; the next commands fetch and download again |
| `<client>/Interface/.omaddons/target.json` | The client's identity, so a moved install keeps its id | Loses identity |
| `<client>/Interface/.omaddons/snapshots/<tx>/` | `AddOns/` (replaced and removed folders) and `WTF/` (SavedVariables copies with a manifest): most of the undo data | Loses undo |
| `<client>/Interface/.omaddons/stage/`, `trash/` | Staging (same filesystem as AddOns) and staged deletions | No |

The state directory honors `XDG_STATE_HOME`, the cache `XDG_CACHE_HOME`, and the config `XDG_CONFIG_HOME`. State files carry a `schema_version`. A file written by a newer omaddons is read, never overwritten (`state_too_new`).

---

## Architecture

```text
                              omaddons <command> [--json]
                                           │
                        ┌──────────────────▼──────────────────┐
                        │ cli.rs: clap definitions → Command  │
                        └──────────────────┬──────────────────┘
                                           │
                        ┌──────────────────▼──────────────────┐
                        │ app/: one module per command        │
                        │ returns a typed Outcome; never      │
                        │ prints                              │
                        └──┬───────────────┬───────────────┬──┘
                           │               │               │
        ┌──────────────────▼───┐ ┌─────────▼────────┐ ┌────▼──────────────────────┐
        │ discover/            │ │ sources/         │ │ engine/                   │
        │ product.db, BPSV,    │ │ refs, GitHub,    │ │ plan → stage → journal →  │
        │ launchers, scan, PE, │ │ REST, atom, git, │ │ swap → commit; inspect,   │
        │ realmlist, /proc,    │ │ url, local       │ │ ownership, running        │
        │ registry, cache      │ │ resolve/: select,│ │ policy, undo, retention,  │
        │                      │ │ catalog          │ │ blob cache                │
        └──────────┬───────────┘ └─────────┬────────┘ └────┬──────────────────────┘
                   │      rules.rs + toc/: ClientRules     │
                   │      and load verdicts                │
                   │               │                       │
                   │     ┌─────────▼────────┐              │
                   │     │ net/: ureq +     │              │
                   │     │ rustls, allowlist│              │
                   │     │ guard, retry,    │              │
                   │     │ limits, net.log  │              │
                   │     └──────────────────┘              │
                   │                                       │
                   └──────────────────┬────────────────────┘
                                      │
                        ┌─────────────▼────────────────┐
                        │ fsx/: the single filesystem  │  write-scope table,
                        │ writer                       │  flock, renameat2,
                        └─────────────┬────────────────┘  reflink, prune, failpoints
                                      │
          ┌───────────────────────────┼──────────────────────────────┐
          ▼                           ▼                              ▼
 <client>/Interface/         ~/.local/state/omaddons/       ~/.cache/omaddons/
 AddOns/, .omaddons/         registry, records, history,    blobs, http validators,
 (stage, snapshots)          journals, snapshots, net.log   discovery cache
                                      │
                        ┌─────────────▼────────────────┐
                        │ output/: one JSON envelope   │ ← every Outcome and Error
                        │ or human text on stdout;     │
                        │ NDJSON or text on stderr     │
                        └──────────────────────────────┘
```

| Path | Role |
|---|---|
| `src/main.rs`, `src/lib.rs` | Entry point and module tree |
| `src/cli.rs` | clap definitions, global flags, argv for fixes |
| `src/app/` | One module per command, plus `world.rs` (discovery + registry for a run) and `engine.rs` (wiring) |
| `src/output/` | The JSON envelope, human rendering, stderr events, terminal detection |
| `src/error.rs` | The error and warning code registries and the exit-code table |
| `src/model.rs` | Domain types: refs, folder names, actors, channels, records |
| `src/discover/` | Client discovery, launcher readers, running detection, the target registry and cache |
| `src/rules.rs` | Products, the version-to-family table, family ranges, client-rules rows, game modes |
| `src/toc.rs`, `src/toc/loaded.rs` | TOC parser and the load verdict |
| `src/inventory.rs` | Folder classification: protected, linked, managed, unmanaged, invalid |
| `src/sources/` | Ref grammar and every source |
| `src/resolve/` | Candidate selection and the catalog |
| `src/net/` | HTTP client, allowlist, guard, retries, concurrency limits, validators, net.log, record/replay |
| `src/engine/` | The transaction engine |
| `src/fsx/` | The only code allowed to write to the filesystem |
| `src/state/` | Config, paths, registry, records, history, status |
| `src/politeness.rs`, `src/limits.rs` | Every network-politeness constant and every safety limit, each in one place |
| `catalog/catalog.toml` | The built-in catalog, embedded with `include_str!` |
| `tests/`, `fuzz/` | Integration, contract, crash, concurrency and live tests; cargo-fuzz targets |

**Invariants the build enforces:**

- Only `src/fsx/` writes to the filesystem (`tests/single_writer.rs` scans the source).
- Commands never print: `print!` and `eprint!` are denied by clippy.
- No `unwrap` or `expect` outside tests, and `unsafe` is forbidden.
- Nothing calls `std::process::exit`. Every exit goes through one table, and every error code used in `src/` must be registered (`tests/contract/exit_sites.rs`).
- tokio, reqwest, native-tls, OpenSSL and aws-lc are banned (`deny.toml`). Concurrency is scoped threads over ureq.

---

## Troubleshooting

### A client is missing from `status`

```bash
omaddons targets --rescan                         # ignore the discovery cache
omaddons targets add ~/path/to/client --name mine # a root, client dir, Interface or AddOns path
omaddons config set extra_roots '["/mnt/games"]'  # scan this root every run
```

Wine prefixes are found only as direct children of `~/Games`, as `~/.wine`, or where a launcher config names them. A nested prefix such as Heroic's `~/Games/Heroic/Prefixes/…` is found only through Heroic's own config. Outside prefixes, `~/Games` is walked 4 levels deep, and `extra_roots`, manually added roots and `product.db` paths are used as well. A custom client needs an `Interface/` folder next to its `wow*.exe`. A warning `discovery_truncated` means the 50,000-directory budget ran out; add the client by path.

### `target_ambiguous`: several clients match

Add `-t`. For `install`, you can also set a default once:

```bash
omaddons targets default retail
```

The `fix` is the first candidate, and with `--json` `error.alternatives` holds one runnable argv for each of the others.

### `no_compatible_release` or `wrong_flavor`

The addon has no build this client loads. With `--json`, `error.details.tried` lists each asset and why it was rejected. Check the catalog's families (`omaddons info <name>`), try `--channel beta`, or pin a release with `--version`. WeakAuras, for example, currently ships no retail build.

### `game_running`

The client is running, and one of these applies:

- the change would remove a folder it loads, or an update would drop or rename the TOC it loads;
- it is a legacy client, where the `safe` policy refuses every change;
- `running_policy` is `refuse`;
- you ran `undo --saved-variables restore`.

Quit the game and run the command again. For folder changes you can also add `--while-running`, but SavedVariables are never written while the game runs, whatever the flags.

### `unmanaged_folder` or `ownership_conflict`

A folder with that name already exists and omaddons does not own it, or another managed addon does. Replacing it is undoable:

```bash
omaddons install <ref> -t retail --force
```

### `saved_variables_deferred` after an undo

The folders were reverted, but the game was running, so SavedVariables were left alone. After quitting:

```bash
omaddons undo <tx> --saved-variables restore
```

### `locked` (exit 5)

Another omaddons process held the lock for more than 5 seconds. `error.details.holder` names its pid and command. It is retryable: run the command again.

### `rate_limited`

Usually the GitHub REST budget (60 an hour without a token) ran out; only the beta and alpha channels, releases without a usable `release.json` and some pinned tags use REST. Any host can also cause it, keyless github.com included: a 403 or 429 saying no requests remain, a `Retry-After` longer than 60 seconds, or a 429 that outlasts three retries. Wait for `details.reset_at`. For the REST budget, a token raises it to 5,000 an hour:

```bash
export OMADDONS_GITHUB_TOKEN=github_pat_…
```

### `host_blocked` for a git host

`config set` replaces the whole list, so include the hosts already in it:

```bash
omaddons config get git_hosts
omaddons config set git_hosts '["codeberg.org", "gitlab.com"]'
```

### `source_disabled`

WoWInterface and Tukui refs arrive in milestone 2, and CurseForge and Wago in milestone 5. Many popular addons also publish GitHub releases, so try `gh:Owner/Repo`. Otherwise, download the zip in a browser and install it as a local path (`./Addon.zip`). The same error for a `git:` ref on another host means `git` is missing or older than 2.32.

### `journal_pending` that does not clear

Journals are recovered by the next command that commits a change, not by read commands. If recovery finds a step that matches neither its before nor its after state, it stops and leaves the journal for manual repair. Every later change command then fails until it is fixed. `doctor`, which will automate this repair, is planned for milestone 4.

---

## Limitations and known issues

### What omaddons does not do (yet)

| Capability | State | Planned |
|---|---|---|
| WoWInterface, Tukui | ❌ refs parse and are refused | M2 |
| Dependency resolution (`## Dependencies`) | ⚠️ resolved from the catalog when installing; WoWInterface and Tukui lookups, bridge folders and the dependency items in output are on the way | M2 |
| Adopting addons installed by other managers | ❌ they show as unmanaged | M2 |
| `addons.toml`, `export`, `sync`, `copy` between clients | ⚠️ `sync` works; `sync --locked`, `sync --prune`, `export` and `copy` are on the way | M2 |
| `search` | ❌ catalog names and links only | M2 |
| Progress output | ⚠️ `install`, `update` and `outdated` report progress; the other long commands arrive with M2 | M2 |
| The Omarchy bar widget | ❌ the help text mentions it, the code does not exist yet | M3 |
| Lua error capture (BugGrabber), `WTF/` backups | ❌ | M3 |
| `capabilities`, `doctor`, `robot-docs`, stored plans, agent skill | ❌ | M4 |
| CurseForge, Wago | ❌ | M5, optional, bring your own key |
| Shell completions | ❌ | M4 |
| Packages, man page | ❌ build from source | M5 |
| Windows, macOS | ❌ | Never (non-goal) |
| A GUI, TUI, daemon or MCP server | ❌ | Not in v1 (non-goal) |
| Editing WeakAuras strings, ElvUI profiles or SavedVariables | ❌ | Never (non-goal) |

### Known limitations

- Discovery reads only the direct children of `~/Games` as prefixes, and Lutris only from its native install (no Flatpak reader). Steam's `shortcuts.vdf` is not read.
- Drive-letter mapping applies to clients that Battle.net's `product.db` names. A custom client on another drive letter needs `targets add` or `extra_roots`.
- Custom clients between the five legacy versions (2.0–2.3, 3.0–3.2, 4.0–4.2, 5.0–5.3) get family `unknown`, which matches only their exact major.minor.
- On unknown custom clients, the `safe` running policy behaves as for legacy clients and refuses every change while they run.
- The discovery cache does not notice changes to the `products` or `version_families` overrides, or a binary upgrade; run `targets --rescan`.
- The download cache reuses an asset whose SHA-256 is known: from the source (GitHub REST assets carry a digest) or remembered from an earlier download of that release's asset in the release memo. A release's assets are taken to be immutable, so a publisher re-uploading different bytes under the same tag and file name is not noticed while the old blob is cached (unless the source declares a different size or hash). Within one command, the same asset is still downloaded once per target that tries it. An unchanged release is never downloaded.
- A `url:` ref's release is its URL, so new content published at the same URL is never picked up.
- Private GitHub repositories and HTTP proxies are not supported.
- The legacy running behavior and several upstream file layouts (product.db, Steam and Bottles Flatpak paths) are marked UNVERIFIED in the code.

### Known issues

These are bugs, found while this README was written:

- `omaddons help targets add` (a nested command after `help`) is a usage error whose fix is `omaddons help help`. Use `omaddons targets add --help` or `omaddons help "targets add"`.
- Some `--json` error fixes (`--force`, `--while-running`) repeat `--json` inside `fix_argv`.
- Raising `limits.download_max_bytes` above 512 MiB has no effect; lowering it works.
- Addon identity is the name. Installing an addon whose name matches an installed addon from a different ref (another zip, `--file`, another repository) replaces it without `--force`, reports it as a fresh install (`from_version: null`), and switches the record to the new ref.
- A pasted `/releases/download/<tag>/<file>` link pins the tag, but the named file is not used to pick the asset.
- Usage errors ignore `--no-color` and report `meta.offline: false` even when offline. `install http://github.com/a/b -t retail` suggests an https fix without the `-t`, and `status --version` suggests itself.
- Human output says "1 folders", and `file:` refs keep a `./` component.
- `install --dry-run` creates empty `inflight/` and `trash/` directories in the state directory.

---

## Roadmap

| Milestone | Scope | State |
|---|---|---|
| **M1: working slice** | Discovery, ClientRules and the TOC engine, fsx and the transaction engine with undo, keyless GitHub/git/url/local sources, the core CLI contract | ✅ Done (2026-10-15) |
| Follow-up: Classic and Forever gaps | Multi-flavor archives, never-launched clients, game modes, real-machine tests for every live client, the catalog × flavor probe | 🔄 One check remains, to run during the Forever stress test on 2026-10-19: do the installed addons actually load in the live client? |
| **M2: updates and adoption** | `outdated`, `update`, `pin`/`unpin`; WoWInterface and Tukui; changelogs; dependencies; drift detection; tiered `adopt`; `addons.toml` with `export`, `sync` and `copy`; `search`; `auth github` | 🗓️ Planned and decomposed into 75 beads |
| **M3: the widget** | The Omarchy bar widget (QML, embedded in the binary, installed by `omaddons omarchy install`), `errors` (BugGrabber), `backup` of `WTF/` | 🗓️ Planned |
| **M4: the agent surface** | `capabilities`, `robot-docs`, `install-skill`, intent inference, the full `doctor`, stored plans (`--dry-run` then `apply`), `completions`, wire-name golden tests; **the first tag freezes the contract** | 🗓️ Planned |
| **M5: accounts and release** | Optional CurseForge and Wago (bring your own credential, never cached), fingerprint adoption; release engineering: versioning, CHANGELOG, a local PKGBUILD, packaged completions and a man page | 🗓️ Planned |

The authoritative design is the pinned vision bead. Its body is revision 2, and comments added after it supersede it.

```bash
br show omad-vision-8qa      # the vision: thesis, decisions D1–D34, milestones
br ready                     # work that is ready now
```

---

## Development

### The local gate

There is no CI (decision D16). Every commit that changes code (`*.rs`, `Cargo.*`, `rust-toolchain.toml`, `deny.toml`, lint config, the catalog, test fixtures and snapshots) passes the gate first:

```bash
argc gate          # cargo fmt --check, clippy --all-targets -D warnings, cargo test,
                   # ubs on changed files, cargo deny when Cargo.toml/Cargo.lock/deny.toml changed
argc gate --all    # also run cargo deny unconditionally
```

`argc gate` runs `scripts/gate.sh`, which can be run directly. It needs `cargo`, `git`, [`ubs`](https://github.com/Dicklesworthstone/ultimate_bug_scanner) and `cargo-deny`. Commits that touch only `.beads/`, README.md or CLAUDE.md may skip it.

Other tasks: `argc fmt`, `argc lint`, `argc test [filter]`.

### Tests

At the time of writing there are 760 tests: 464 unit tests in `src/`, 295 integration tests in 46 test binaries, and one doctest. They include 29 proptest properties. 28 are `#[ignore]`d and run only at checkpoints: live probes, real-machine tests, fixture recorders and child-process helpers.

| Suite | What it covers |
|---|---|
| `tests/contract/` | A matrix of success and failure envelopes across the commands: key order, exit codes, registered codes, fix argv round-trips (24 snapshots) |
| `tests/engine_crash.rs` | All 24 engine failpoints armed as real `abort`s in a child process |
| `tests/engine_concurrency.rs` | Parallel runs, duplicate installs, lock hold time under 50 ms |
| `tests/discover_*.rs` | The discovery matrix, launchers, scanning, custom clients, the cache |
| `tests/catalog.rs` | All 18 catalog entries replayed offline from recorded HTTP fixtures, with no REST calls |
| `tests/single_writer.rs` | Only `src/fsx/` writes to the filesystem |

Recorded HTTP fixtures live in `tests/fixtures/http/<scenario>/`. Debug builds replay them with `OMADDONS_HTTP_FIXTURES=<dir>` and re-record them with `OMADDONS_RECORD=1`. Recording never stores request headers, drops `set-cookie`, and strips secret query parameters from URLs and `location` headers.

### Milestone checkpoints (not part of the gate)

```bash
argc fuzz-all --time 600                                   # every fuzz target, 10 minutes each
OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored  # the real endpoints
OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1 \
  cargo test --test real_machine -- --ignored --test-threads=1   # this machine's clients; every addon change is undone
```

- **Live probes.** They include `catalog_matches_declared_flavors`, which builds a synthetic client for every live product and every legacy version and dry-runs each catalog addon on each. An addon must install exactly where its publisher declares it and nowhere else, with zero REST calls. The probe prints the addon × client matrix, and publishers whose `release.json` disagrees with their own TOCs are listed in it with a date.
- **Real-machine tests.** They run against the real home and undo every addon change they make. Target registrations and `target.json` markers written on the first change stay, by design.
- **Fuzzing.** There are nine cargo-fuzz targets: `archive`, `bottles`, `bpsv`, `heroic`, `lutris`, `product_db`, `ref_parse`, `steam_vdf` and `toc`. Each has committed seeds under `fuzz/seeds/`. They need `cargo-fuzz` and a nightly toolchain at least as new as the crate's `rust-version` (pick one with `--toolchain`). A crash input becomes a regression unit test in the owning parser.

### Work tracking

Work is tracked with beads (`br`), and agents follow `AGENTS.md`. `br ready` shows unblocked work, and `br show <id>` shows one bead.

---

## FAQ

### Why "omaddons"?

It is built alongside [Omarchy](https://omarchy.org), the Arch-based desktop that installs Battle.net through umu and Proton into `~/Games/battlenet`, and an Omarchy bar widget is planned for milestone 3. The CLI does not depend on Omarchy and works on any Linux desktop.

### Will it touch the addons I already have?

No. Folders it did not install are reported as unmanaged and left alone. Replacing one requires `--force`, and even that is undoable. Adopting existing addons into management is milestone 2.

### Is it safe to use while WoW is running?

Yes, by default. Updates that keep the loaded TOC land and tell you to `/reload`. Removing a loaded addon is refused until the game exits, and you run it again then. SavedVariables are never written while the game runs. Legacy private-server clients get no changes at all while running. See [Running clients](#running-clients).

### How do I update my addons?

Run `omaddons outdated` to see what has updates on every client, then `omaddons update` to install them (or `omaddons update <addon>` for some). Each client gets its own transaction, and `omaddons undo` reverts the whole update. `url:` refs are one exception: their release is the URL, so install a new URL instead. Pinned addons (`omaddons pin <addon>`, or installed with `--version`) are the other: they stay put until `omaddons unpin <addon>`.

### Does it work with Turtle WoW, Warmane or other private servers?

Any client from 1.12.1, 2.4.3, 3.3.5a, 4.3.4 or 5.4.8 with an `Interface/` folder is a first-class target, identified by its executable. Its label comes from `realmlist.wtf`. Server-specific patches are not special-cased: Turtle WoW is a `vanilla-legacy` client. Give it a name with `targets add <path> --name turtle` or `targets rename`.

### Can I use CurseForge addons?

Not through CurseForge yet; optional CurseForge support with your own key is planned for milestone 5. CurseForge's terms make keys non-transferable and forbid caching, so omaddons will never bundle a key or proxy. Many CurseForge addons also publish on GitHub (`gh:Owner/Repo`). You can also download a zip in the browser and install it as a local path.

### Does it phone home?

No. There is no telemetry. Only the hosts on the allowlist are contacted, every request that reaches the network is in `omaddons net-log`, and `--offline` makes network access impossible.

### Where does undo data live, and how long is it kept?

Old folders and SavedVariables copies go to `<client>/Interface/.omaddons/snapshots/`, on the same filesystem as the addons, and state-file copies go to `~/.local/state/omaddons/snapshots/`. Per target, everything younger than 24 hours and the latest change are always kept. Beyond those, at most the newest 10 changes within 7 days are kept, capped at 2 GiB.

### Can a coding agent drive it?

That is a primary design goal. Use `--json`, read `error.fix_argv` and `commands[].argv`, pass `--actor agent --reason "…"`, and keep `undo --last-by agent` as the escape hatch. An agent is also detected automatically: without `--actor`, a run whose stdin is not a terminal is recorded as `agent`. See [For coding agents](#for-coding-agents).

### Why no CI?

The repository is private and developed by its owner with coding agents. A local gate that must pass before every code commit replaces CI until the project is opened (decision D16).

### Does it support Windows or macOS?

No, by design. It relies on Linux specifics: `renameat2`, `/proc`, Wine prefix layouts and reflinks.

---

## For coding agents

```text
omaddons: WoW addon manager for Linux. One JSON envelope per command, typed errors with fixes as argv.

Core loop
  omaddons status --json                         # every client (target), running state, attention
  omaddons install <ref>... -t <target> --dry-run --json
  omaddons install <ref>... -t <target> --json --actor agent --reason "<why>"
  omaddons undo --last-by agent --json           # the escape hatch

Refs     catalog name | gh:Owner/Repo | git:https://…#rev | url:https://… | ./zip-or-folder
Targets  -t retail | era | sod | hardcore | classic | anniversary | forever | wotlk | <key> | <path>

Read the envelope
  ok, exit_code, partial, data, error{code, message, fix, fix_argv, alternatives[{display, argv}], retryable, details}
  warnings[{code, message, fix, fix_argv}], commands[{action, command, argv, destructive}]
  Run fix_argv / alternatives[].argv / commands[].argv as argv arrays; never parse the display strings.
  fix_argv is null when the fix is prose (read error.fix).

Exit codes  0 ok (check partial) · 1 failed (see retryable) · 2 usage · 3 not found/ambiguous
            4 refused (safety) · 5 locked (retry) · 6 --check/--strict failed

Rules
  - --dry-run first for anything destructive; --force only after reading the refusal.
  - Never write into Interface/AddOns yourself; every change must go through omaddons to stay undoable.
  - --offline for read-only work; net-log shows every request that reached the network.
  - Re-running install updates an addon (not url: refs, not pinned ones: addon_pinned); there is no update command before milestone 2.
  - Only install falls back to a default target; pass -t everywhere else when more than one client matches.

State   ~/.config/omaddons/config.toml · ~/.local/state/omaddons/ · ~/.cache/omaddons/
```

---

## License

There is no license yet. The repository is private, and all rights are reserved by its owner until a license is chosen when the project is opened. The dependency policy (`deny.toml`) admits only Apache-2.0, BSD-3-Clause, CDLA-Permissive-2.0, ISC, MIT, Unicode-3.0 and Zlib crates, so no dependency constrains that choice.
