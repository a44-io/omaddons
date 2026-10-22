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
forever      forever                                       1.60.1.70245  live  forever (battlenet)      0 managed · 3 unmanaged · 0 protected · 3 adoptable
retail       mainline                                      12.1.5.70391  live  retail (battlenet)       2 managed · 0 unmanaged · 0 protected
wotlk        wrath-legacy                                  3.3.5.12340   live  localhost                0 managed · 0 unmanaged · 23 protected
Next: omaddons adopt -t forever
```

<sub>Output from a test home laid out like the development machine: five Battle.net clients in one Proton prefix and a 3.3.5a private-server client in another, found with no configuration.</sub>

<div align="center">
<h3>Build and install</h3>

```bash
git clone https://github.com/a44-io/omaddons.git && cd omaddons
cargo install --locked --path .
```

</div>

There is no packaged release, installer or prebuilt binary yet; see [Installation](#installation).

> [!NOTE]
> **Pre-alpha.** Milestones 1 (the working slice) and 2 (updates, dependencies, adoption and desired state) are done, the fixes from the post-M2 audit are in (the bugs its closing check found are listed under [Known issues](#known-issues)), and omaddons runs daily on the development machine. There, every client it manages (retail, Mists Classic, Classic Era, Anniversary, WoW: Forever and a 3.3.5a server) has been seen in game loading the addons it installed. Nothing is tagged, the repository is private, and the JSON contract freezes at the first tag (milestone 4). `outdated` (with changelogs), `update`, `pin`, `sync`, `export` and `copy` work, installs bring their dependencies (from the catalog, Tukui or WoWInterface), WoWInterface and Tukui (ElvUI) addons install and update, `search` and `auth github` work, and `adopt` brings addons installed by hand or by other managers under management. Milestone 3 (the Omarchy widget, Lua errors and `WTF/` backups) is under way: `errors` and `errors dismiss` read and acknowledge BugGrabber's Lua errors, `status` counts the new ones, `backup create` and `backup list` work, and checking and restoring backups come next. See [What works today](#what-works-today) and the [Roadmap](#roadmap).

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
| **Undo any change** | Each install, remove and undo snapshots what it replaces. `undo` restores folders as they were (file bytes and modes, directory modes, symlinks, empty directories) and restores SavedVariables only where they changed, until retention prunes its snapshot. An undo is itself undoable, and `undo --last-by agent` reverts the last thing an agent did. |
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

Milestones 1 (the working slice) and 2 (updates, dependencies, adoption and desired state) are complete, and milestone 3 has started. Everything in this README describes code that exists and is tested, unless it is marked as planned. Bugs the code has today, where it differs from what this README says, are listed under [Known issues](#known-issues).

| Area | State |
|---|---|
| Discovery of Battle.net, Steam, Lutris, Heroic, Bottles and custom clients | ✅ Done |
| The TOC load-rule engine (`ClientRules`) for all 12 Battle.net products and five legacy private-server versions | ✅ Done |
| The transaction engine: plan, stage, journal, atomic swap, crash recovery, undo, retention | ✅ Done |
| Keyless sources: GitHub releases, git (GitHub without the binary, other hosts with it), direct URLs, local zips and folders | ✅ Done |
| A catalog of 62 verified addons (46 GitHub release entries, 9 WoWInterface entries, ElvUI and Tukui from Tukui, 5 legacy private-server addons from git) | ✅ Done |
| The CLI contract: one JSON envelope, typed errors with fixes (an argv whenever the fix is a command), exit codes 0 to 6, contract snapshot tests | ✅ Done (freezes in M4) |
| Commands: `status`, `install`, `remove`, `undo`, `history`, `list`, `targets`, `info`, `config`, `net-log`, `help` | ✅ Done |
| `outdated` (alias `check`): what has updates, on every client | ✅ Done (milestone 2) |
| `adopt`: where unmanaged addons come from, whether they are exactly a release, and bringing them under management as they are (undoable) | ✅ Done (milestone 2) |
| `update` (alias `upgrade`): every client's updates in one run, undone with one `undo` | ✅ Done (milestone 2) |
| `pin` and `unpin`: hold an addon at its release through updates | ✅ Done (milestone 2) |
| Drift: `status`, `list` and `info` name managed addons whose files changed or went missing; `install --reinstall` repairs one from the download cache | ✅ Done (milestone 2) |
| Dependencies: what a TOC requires installs with it, from the catalog, Tukui or WoWInterface; `required_by` kept true on update and remove, orphans reported | ✅ Done (milestone 2) |
| `sync`: every client made to match `addons.toml`, idempotently; `--locked` reproduces exact versions, `--prune` removes what the file drops | ✅ Done (milestone 2) |
| `export`: every client's addon set as one `addons.toml`, printed or written with a backup | ✅ Done (milestone 2) |
| WoWInterface: `wowi:` refs and links, keyless, every download checked against its MD5 (a file published without one installs with a `download_unverified` warning) | ✅ Done (milestone 2) |
| Tukui (ElvUI and Tukui): `tukui:` refs and links, keyless, every download checked against its `ETag` MD5 (a download without a usable one installs with a `download_unverified` warning) | ✅ Done (milestone 2) |
| `search` (alias `find`): the catalog, WoWInterface, Tukui and GitHub at once, with a compatibility hint per client | ✅ Done (milestone 2) |
| Changelogs: `info --changelog` and `outdated --changelog`, from GitHub's Atom feeds, WoWInterface and Tukui | ✅ Done (milestone 2) |
| `copy`: one client's addons installed on another where builds exist, the rest skipped and said why | ✅ Done (milestone 2) |
| `auth github`: an opt-in GitHub token (5,000 REST requests an hour), stored from standard input or `gh`, checked and cleared | ✅ Done (milestone 2) |
| In-game load checks: retail, Mists Classic, Classic Era, Anniversary, WoW: Forever and 3.3.5a each wrote the SavedVariables of the addons omaddons installed, so the client loaded them | ✅ Observed on the development machine (2026-10-20 and 21) |
| `backup create`: a copy of each client's `WTF/`, made without the global lock (copy-on-write on btrfs and xfs), that says whether `WTF/` held still while it was copied and which addon versions produced it; `--verify` hashes it. `backup list`: each client's backups, what each proves, and the space they take | 🔄 Milestone 3 (checking and restoring backups are next) |
| `errors`: which addon most likely throws Lua errors, from BugGrabber's data on every client (3.3.5a included), with the attribution's method and confidence, new errors per database, and nothing an addon wrote shown unless asked for; `errors dismiss` marks what was read as seen, bound to that read; `status` shows each client's new errors, and `status --cached` counts them without opening BugGrabber's files | 🔄 Milestone 3 |
| The Omarchy bar widget, BugGrabber from a new keyless `wowace:` source, checking and restoring `WTF/` backups, automatic backups | 🗓️ Milestone 3 |
| `capabilities`, `doctor`, `robot-docs`, stored plans, the agent skill, the first tag | 🗓️ Milestone 4 |
| Optional CurseForge and Wago (bring your own key), release packaging | 🗓️ Milestone 5 |

`omaddons outdated` lists what has updates, and `omaddons update` installs them on every client, one transaction per client, all undone by one `omaddons undo`. Two kinds of addon are left alone. A `url:` ref's release is the URL itself, so there is nothing newer to fetch. A pinned addon (by `pin`, or installed with `--version`) stays where it is (`skipped`, reason `pinned`) until `unpin`.

---

## Design philosophy

**1. The game's rules decide, not metadata.** A host's flavor tags, a `release.json` and a file name order the candidates; they never make a folder install that the client would not load. What decides is the TOC file the client would load from each folder in the downloaded archive, judged by that client's rules row (suffixes, interface range, game types). So omaddons refuses the retail build of an addon for Classic Era even when the release metadata claims otherwise, and it installs a Classic module on Forever only if Forever would load it.

**2. Every change is a transaction.** There is one engine and one filesystem writer (`src/fsx/`). Every install, update, remove and undo stages its folders on the client's own filesystem and swaps them in under a journal and a lock held only for the commit window. Then it records the actor, the argv and the reason. A test in the gate fails if any `std::fs` write call appears outside `src/fsx/`.

**3. Keyless and private by default.** Everything that works today needs no account. The network is a closed allowlist of hosts, every request is logged locally, and `--offline` is absolute. CurseForge and Wago will be strictly opt-in, bring your own credential, and their data will never be cached.

**4. The CLI is the product.** Humans read the terminal output, agents read the JSON envelope, and the future Omarchy widget will render the same envelope without logic of its own. Commands never print: they return typed outcomes that one output layer renders, and clippy denies `print!`. Every error names its fix: an argv an agent can run, or a sentence when no command would help.

**5. Detection is data.** Products, the version-to-family table, interface ranges and client-rules rows are tables in `src/rules.rs`. Each can be patched from `config.toml`: a new product code, a changed version range or load rule for a known family, or a misdetected private server is a config change, not a release. A new family (an expansion line with its own `release.json` flavor) still needs a release, because families are a closed set in code.

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
| Update everything in one command | ✅ `update`, every client in one run | ✅ | ✅ | ✅ | ❌ |
| GitHub releases | ✅ keyless | ✅ | ❌ | ✅ | ⚠️ |
| WoWInterface, Tukui | ✅ keyless | ✅ | ❌ | ✅ | ⚠️ |
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

Deleting the state directory loses the history and every undo snapshot. Each client managed by omaddons also has an `Interface/.omaddons/` folder that holds its identity marker, staging area, folder snapshots and `WTF/` backups; deleting it loses them. The addons themselves stay where they are.

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
retail: would install HelloWorld 1.0.0 (1 folder)
retail: would install Bar 2.1.0 (1 folder)

$ omaddons install ./HelloWorld ./Bar-2.1.0.zip -t retail --reason "set up retail UI"
retail: installed HelloWorld 1.0.0 (1 folder)
retail: installed Bar 2.1.0 (1 folder)
tx 01M59Z5RTVMBBHR4FKNZ3FN39K · undo: omaddons undo 01M59Z5RTVMBBHR4FKNZ3FN39K

$ omaddons list -t retail
retail (12.1.5.70391, mainline)
  Bar         2.1.0    file:~/Downloads/Bar-2.1.0.zip  stable
  HelloWorld  1.0.0    file:~/Downloads/HelloWorld     stable
  2 managed · 1 unmanaged · 1 protected · 0 linked

$ omaddons remove Bar -t retail --actor agent --reason "Bar conflicts with ElvUI"
warning: On retail, 01M59Z5RWG5MH458YF524HJPG5's snapshot holds the only copy of Bar: `omaddons undo 01M59Z5RWG5MH458YF524HJPG5` brings them back until retention prunes it (kept at least 24 hours; then pruned once 10 newer transactions on this client exist, after 7 days, or past the 2 GiB snapshot cap)
  fix: omaddons undo 01M59Z5RWG5MH458YF524HJPG5
retail: removed Bar 2.1.0 (1 folder)
tx 01M59Z5RWG5MH458YF524HJPG5 · undo: omaddons undo 01M59Z5RWG5MH458YF524HJPG5

$ omaddons undo --last-by agent --actor agent --reason "the user wants Bar back"
reverted 01M59Z5RWG5MH458… (remove by agent, 2026-10-19 06:36) on retail
restored 1 folder, removed 0 · SavedVariables: 0 restored, 0 unchanged
tx 01M59Z5RX2ECD3E5ZVXFYZPVH0 · undo this: omaddons undo 01M59Z5RX2ECD3E5ZVXFYZPVH0

$ omaddons history
01M59Z5RX2ECD3E5…  2026-10-19 06:36  agent  undo     retail  +1 ~0 -0  "the user wants Bar back"
01M59Z5RWG5MH458…  2026-10-19 06:36  agent  remove   retail  +0 ~0 -1  "Bar conflicts with ElvUI"
01M59Z5RTVMBBHR4…  2026-10-19 06:36  human  install  retail  +2 ~0 -0  "set up retail UI"
```

And what a refusal looks like:

```text
$ omaddons install ./EraOnly.zip -t retail
error[wrong_flavor]: file:~/Downloads/EraOnly.zip does not load on retail (Mainline)
fix: use an archive built for this client

$ omaddons install gh:o/oldthing -t retail
error[unmanaged_folder]: OldThing is not managed by omaddons
fix: omaddons adopt OldThing --ref gh:o/oldthing -t retail
  or: omaddons install gh:o/oldthing -t retail --force

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

Discovery runs whenever a command needs the clients: every command except `auth`, `config`, `net-log`, `help` and `targets rm`. Each walk goes at most 4 levels deep, and all the walks in one run share a budget of 50,000 directories.

It first collects the Wine prefixes (directories containing `drive_c/`):

- the prefixes that **launcher configs** name:
  - Lutris: `games/*.yml` under the config and data directories, native and Flatpak (prefix and exe);
  - Heroic: every `winePrefix` in `GamesConfig/*.json` (a Proton game's `pfx` inside it), the shared prefix and the prefixes in its per-game prefix folder (`~/Games/Heroic/Prefixes` unless `config.json` names another), and the exes of sideloaded apps, native and Flatpak;
  - Steam: the `compatdata/*/pfx` Proton prefixes of every library in `libraryfolders.vdf`, native, Flatpak and Snap (WoW is usually a non-Steam shortcut with its own compatdata id);
  - Bottles: every bottle, including a bottles directory moved in Bottles' preferences and bottles created at a custom path, native and Flatpak;
- the prefixes of **running WoW and Battle.net processes** (their `WINEPREFIX`, your own processes only), when the prefix is under your home: a client in a nested prefix such as `~/Games/umu/umu-default` is found while it or the Battle.net app runs, and remembered after they exit (route 8 below). Under Wine the app runs as `CrBrowserMain`, so it is recognized by its agent, an `Agent.exe` run from Battle.net's `ProgramData` folder;
- the **direct children of `~/Games`** that are prefixes;
- **`~/.wine`**.

Then it looks for clients in this order. The first route that finds a client decides how it was found and its qualifier.

1. **Battle.net's `product.db`** in every prefix (`drive_c/ProgramData/Battle.net/Agent/product.db`). Windows drive letters are mapped through `dosdevices/`, so a game on `D:` outside the prefix is found.
2. **Executables named by launcher configs**: Lutris's `exe:` lines, Heroic's sideloaded apps, and the `Exe` of each non-Steam shortcut in Steam's `userdata/*/config/shortcuts.vdf`.
3. **A walk of the `drive_c` of each launcher-named prefix and each running process's prefix**, skipping `windows`, `users` and `ProgramData`.
4. **`extra_roots`** from `config.toml`, and the roots of manually added targets.
5. **A walk of the `drive_c` of each `~/Games` prefix and of `~/.wine`**, with the same skips.
6. **A plain walk of `~/Games`** for clients outside any prefix.
7. **The roots of registered targets** (`found_by: registered`) that no route above reached, while they are still WoW roots.
8. **Roots found earlier while a client or the Battle.net app ran** (`found_by: remembered`), kept in `~/.cache/omaddons/discovery.json` while they are still WoW roots under your home. A client found once that way is still found after the process exits, registered or not. Deleting the cache file forgets them.

A directory with `.build.info` is a **Battle.net root**: each `_<flavor>_` folder in it becomes a target, identified by its `.flavor.info`, product.db or the folder table. Battle.net installs other Blizzard games the same way, so a folder whose product is not WoW's (Warcraft III: Reforged's `_retail_` is product `w3`) is skipped without a warning. A product counts as WoW when the rules table lists it or its code starts with `wow`, and a folder whose product cannot be read stays a target (a registered one whose product turns out not to be WoW's shows as `target_missing`). An unknown flavor folder `_xyz_` gets the id `xyz`, and counts as a test client when its folder or product name contains `ptr` or `beta`.

A directory with `Interface/` and a `wow*.exe` is a **custom client**: its version comes from the executable's PE version resource. Its label comes from `Data/<locale>/realmlist.wtf`, then the root's `realmlist.wtf`, then the `realmList` setting in `WTF/Config.wtf`, then the directory name. A custom client of family `unknown` takes a slug of its directory name as its id (`My Server_3.3.5` becomes `my-server-3-3-5`).

Parsed results are cached in `~/.cache/omaddons/discovery.json`. A root's parse is reused while its entries (a hash of its directory listing), the route that reached it and its stamped files (`.build.info`, `.flavor.info`, exes, `Config.wtf`, `realmlist.wtf`, `product.db`) are unchanged, and while the rules and the omaddons version are the ones the cache was built with. `targets --rescan` re-parses every root and reuses no cached parse (remembered roots are still looked at), and `status --cached` skips the scan entirely.

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

The product code does not decide the family; the version does. `wow_classic_era_ptr` runs TBC 2.5.x, and `wow_classic_beta` runs Forever 1.60.x. An unknown flavor folder still becomes a target. If its version cannot be read, it can receive installs only after you set its family (see [Configuration](#per-target-overrides)); the override then gives it that family's current load rules.

### Private-server (legacy) clients

| Client version | Family | Default id | Load rules |
|---|---|---|---|
| 1.12.1 (Vanilla) | `vanilla-legacy` | `vanilla` | plain `<Folder>.toc` only, single interface number |
| 2.4.3 (TBC) | `tbc-legacy` | `tbc` | same |
| 3.3.5a (WotLK) | `wrath-legacy` | `wotlk` | same |
| 4.3.4 (Cataclysm) | `cata-legacy` | `cata` | same |
| 5.4.8 (MoP) | `mists-legacy` | `mists` | same |
| 1.16–1.49 (builds of the 1.12 client that stamp their own version, such as Turtle WoW 1.17 and 1.18) | `vanilla-legacy` | `vanilla` | same, interface 11200 |

Legacy clients are found by their executable (`wow.exe`, then `wow-64.exe`, then any `wow*.exe`) next to an `Interface/` folder. Turtle WoW and other 1.12.1 servers are plain `vanilla-legacy` clients, whether their executable reports 1.12 or a 1.16–1.49 version of its own; name them with `targets add … --name turtle` or `targets rename`. Other custom versions follow the modern rows: 1.13–1.15 is `vanilla`, 1.50 and up `forever`, 3.4 `wrath`. A custom client whose executable has no readable version, or whose version no family covers (2.6, say), gets family `unknown` and the `fallback-custom` rules: the plain TOC only, read as a legacy client would. A version below its family's rows (a 7.x server is `mainline`, a 1.11 client `vanilla`) keeps that family but gets `fallback-custom` too. While a legacy client runs, the default running policy refuses every change, except on 3.3.5a, where an update that keeps the addon's loaded TOC lands with `/reload` (see [Running clients](#running-clients)).

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

A client's interface number is `major×10000 + minor×100 (+ patch on modern clients)`, so 12.1.5 is 120105 and 3.3.5a is 30300. A version rule can pin it instead: a custom 1.16–1.49 client is 11200, the interface its addons declare. On legacy clients, `lastAddonVersion` in `WTF/Config.wtf` overrides either.

### Choosing a target with `-t`

`-t` (alias `--flavor`) accepts any of these. The first kind that matches wins:

1. a target **key** (ULID);
2. a target **id**: `retail`, `era`, `retail@steam`, `turtle`;
3. a **product code**: `wow_classic_era`;
4. a **flavor folder**: `_classic_era_`;
5. a **game mode** or alias: `sod`, `hardcore`, `mop`;
6. a **family**: `mainline`, `vanilla`, `wrath-legacy`;
7. a **path**: the install root, the client directory or its AddOns folder.

Every target that the first matching kind matches is selected (a client reached twice is listed once). `status`, `list`, `info`, `outdated`, `update`, `export`, `sync`, `adopt`, `pin`, `unpin`, `search` and `backup` cover them all: `-t vanilla` reads every vanilla client, `-t mainline` every retail, PTR, XPTR and beta client. A command that must land on one client (`install`, `remove`, each side of `copy`, `adopt --ref`, and `history` and `undo`, which name one) refuses a selector that matches two with `target_ambiguous` (exit 3): the first candidate is the `fix`, and each other one is an `alternatives` entry. So does config `default_target` when it matches two.

An id beats a family, and the legacy default ids `vanilla`, `tbc`, `cata` and `mists` are also family names. With a 1.12.1 server present, `-t vanilla` selects it, not Classic Era. With a 5.4.8 server, `-t mists` selects it, not Mists Classic. Use `-t era` and `-t classic` (or `-t mop`) for the Battle.net clients, or rename the server's target.

`install` is the only command that picks a target when `-t` is absent. It uses config `default_target`, then the registry default (`targets default`, if that client is present), then the only live client. With several live clients and no default, it dry-runs the refs against every live client (test clients excluded) and installs only if exactly one client loads every ref that loads anywhere (a client is ruled out only by `no_compatible_release` or `wrong_flavor`). `status`, `list` and `history` cover every target unless `-t` narrows them. Bare `undo` reverts the most recent run, every transaction of that invocation on every target; `undo -t <id>` reverts that target's newest transaction. `remove` finds the target that manages the named addons; the names must all agree on one, else `target_ambiguous` asks for `-t`.

### How omaddons decides what loads

For each top-level folder in the archive:

1. **Pick the TOC the client would load.** Candidates are `<Folder>.toc` and, on clients that support several TOCs, `<Folder>_<Suffix>.toc` or `<Folder>-<Suffix>.toc` (case-insensitive). Precedence follows the client's row (table below): its game suffixes in order, then its family suffix, then the plain TOC. A suffix the row does not list never loads. Legacy clients read the plain TOC only. When two TOC names differ only in case, the byte-wise smallest wins.
2. **Read its lines as the client does.** On retail 12.0.7 and later, on every Forever, Classic Era, Anniversary, Titan and Mists client (their current builds carry 12.0.7's TOC changes), and on a Battle.net client of unknown family, a `##` line may carry inline directives such as `[AllowLoadGameType classic]`, anywhere in the line. The Wrath and Cataclysm Classic rows, retail builds before 12.0.7 and every legacy row read lines as written. Such a line applies only on the clients they admit. A line whose directive cannot be known offline, such as a text locale, counts for nothing, and a dependency named only there is reported, never installed. For one key the last line that applies wins. The exceptions add up instead: dependencies, `LoadWith`, `LoadManagers`, the saved variables and the game-type lists. Older clients read every line as written. The title, the dependencies and the steps below all use this reading.
3. **Game types.** On modern clients, an `AllowLoadGameType` that excludes this client, or an `ExcludeLoadGameType` that names it, means the folder does not load. A game type must name the client: one of its row's game types (Titan answers to both `wrath` and `titan`, Forever to `forever` and `camelot`) or its family suffix, lowercased, so `classic` admits every Classic client and `mainline` admits retail and Forever. An `AllowLoadGameType` that names no game type omaddons knows is treated as excluding the client.
4. **Interface.** Comma-separated `## Interface:` lists are read where the client supports them; otherwise only the leading number counts. The folder loads if a value falls inside the client's family range. A newer interface loads. An older one gets an `out_of_date` warning, because the client loads it only with "Load out of date AddOns" enabled.

The rows, as built in (`[client_rules]` can patch them; see [Rules overrides](#rules-overrides-advanced)):

| Row | Clients | Versions | Game suffixes, in order | Family suffix | Game types |
|---|---|---|---|---|---|
| `mainline` | retail, PTR, XPTR, beta | 12.1.5 and up | `_Standard` | `_Mainline` | `standard` |
| `mainline-12.0.7`, `mainline-10.2.7`, `mainline-9.1` | older retail builds | 9.1.0–12.1.4 | | `_Mainline` | `standard` |
| `forever` | WoW: Forever and its beta | 1.50 and up | `_Forever`, `_Camelot` | `_Mainline` | `forever`, `camelot` |
| `vanilla` | Classic Era (Hardcore, Season of Discovery) | 1.13 and up | `_Vanilla` | `_Classic` | `vanilla` |
| `tbc` | Anniversary, the Classic Era PTR | 2.5 and up | `_TBC` | `_Classic` | `tbc` |
| `wrath` | Wrath Classic | 3.4 and up | `_Wrath` | `_Classic` | `wrath` |
| `titan` | Titan (China) | 3.80 and up | `_Wrath` | `_Classic` | `titan`, `wrath` |
| `cata` | Cataclysm Classic | 4.4 and up | `_Cata` | `_Classic` | `cata` |
| `mists` | Mists Classic | 5.5 and up | `_Mists` | `_Classic` | `mists` |
| the five `*-legacy` rows | 1.12.1, 2.4.3, 3.3.5a, 4.3.4 and 5.4.8 servers | | plain TOC only | | |
| `fallback-battlenet` | a Battle.net client no other row covers (usually family `unknown`) | | | `_Mainline`, `_Classic` | |
| `fallback-custom` | a custom client no other row covers (family `unknown`, or below its family's rows) | | plain TOC only | | |

The real Forever client was observed loading `_Camelot` TOCs (2026-10-21); a TOC with the `_Forever` suffix alone, and the client's order when an addon ships both, are not yet observed.

Then the archive as a whole:

- At least one folder must load.
- If the publisher's `release.json` declares this client for this very file, the folders the client would not load are skipped and listed in `folders.skipped`. That is how DBM's retail-only modules stay out of a Classic install.
- Otherwise the addon's own folder must load, and every skipped folder must rule itself out: a game-type gate, TOCs only for other clients, or a dependency on such a folder. A folder that merely lists another client's interface marks a build for another client, and the whole archive is refused.
- When no candidate loads, the error is `no_compatible_release`, with every asset tried and why in `error.details.tried` (JSON output). For a `url:`, `file:` or `--file` archive, it is `wrong_flavor`.

Release metadata also orders the candidates. First come assets whose metadata names this flavor, then a file-name hint for it, then unannotated assets, then file-name hints for other flavors, then metadata for other flavors only (on a legacy or unknown client, every asset with metadata lands here). Within a tier, full builds come before `-nolib` builds; within the metadata tier, each of those runs exact interface, then same major.minor, then any. Assets are downloaded and verified in that order until one is accepted. omaddons never falls back to an older release, and an update of an addon adopted at its TOC's version never installs a release built for an older interface (`blocked`, `older_than_installed`; see [`adopt`](#adopt)).

---

## Sources and refs

A **ref** names where an addon comes from. Everything that works today is keyless.

| You type | It becomes | Where the release comes from |
|---|---|---|
| `weakauras`, `dbm`, `bw` | a catalog entry | [the built-in catalog](#the-built-in-catalog), by name or alias (case-insensitive); an installed addon's name also works. Any other name is `not_found`, with `omaddons search <name>` as the fix (no request is sent) |
| `gh:Owner/Repo` or `Owner/Repo` | GitHub | the stable path reads `releases/latest` and its `release.json` from github.com, with **no REST API calls**. The REST API (60 requests an hour without a token) is used only for `--channel beta`/`alpha`, for releases without a usable `release.json` (their `.zip` assets become the candidates), and for pinned tags that lack one |
| `https://github.com/o/r` | `gh:o/r` | as above. `/releases/tag/T` pins tag `T`, `/releases/download/T/F` pins tag `T` and installs exactly the asset `F` (or nothing: `not_found` naming the release's assets, or refused when it does not load), and `/tree/branch` becomes a `git:` ref. A tag may hold `/` and `+`, written as GitHub writes them or percent-encoded. A link that names no repository (`https://github.com/o`) is `ref_invalid` |
| `git:https://github.com/o/r#branch` | git on GitHub | the commits Atom feed resolves the branch or tag to a commit, and codeload serves a zip of that exact commit (a rev the feed does not serve is fetched as a tag by name). **No git binary.** A 40-hex sha is used as is. No `#rev` means the default branch |
| `git:https://codeberg.org/o/r#tag` | git elsewhere | the `git` binary (2.32+), only for hosts listed in `git_hosts`: argv only, a cleared environment, an empty home and no user or system git config, https only, no redirects and no credentials (see [Network and privacy](#network-and-privacy)). Branches win over tags with the same name (on GitHub, the commits feed decides) |
| `url:https://…/addon.zip` | a direct download | only from the always-allowed hosts: `github.com`, `codeload.github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`, `api.github.com`, `cdn.wowinterface.com`, `api.mmoui.com` and `api.tukui.org`. Any other host, `git_hosts` included, is `host_blocked` |
| `./addon.zip`, `./AddonFolder`, `file:path` | local | a zip (a typed path must end in `.zip`, and the magic bytes are checked) or a folder, read in place |
| `wowi:<id>`; WoWInterface links: `…/downloads/info<id>-Name.html`, `…/downloads/download<id>-Name`, `…/downloads/fileinfo.php?id=<id>`, and the download links `cdn.wowinterface.com/downloads/getfile.php?id=<id>&…` and `www.wowinterface.com/downloads/getfile.php?id=<id>` | WoWInterface | keyless: one `filedetails` request to `api.mmoui.com` per 50 addons, then the file from `cdn.wowinterface.com`, checked against the MD5 the API publishes; when the API publishes none, the file installs with a `download_unverified` warning and is identified by its date (`wowi:<id>@d<date>`, else `@v<version>`). WoWInterface serves only a project's current file, so `--version` matches only its current version. A release is its file: the same bytes uploaded again are no update, and any new file is one. Its compatibility tags lag behind patches, so they never rule a file out; the TOCs decide |
| `tukui:<slug>` (`tukui:elvui`, any case); Tukui links: `tukui.org/addon/<slug>`, `tukui.org/addons/<slug>`, `tukui.org/<slug>`, `download.php?ui=<name>` | Tukui | keyless: one list request to `api.tukui.org` per command, and right before each download a fresh download link, which expires within seconds and is never stored or logged (`net.log` shows `/v1/download/<slug>/…`). The download is checked against the MD5 in its `ETag`; a download without a usable one installs with a `download_unverified` warning. The list's supported patches only rank the file; the TOCs decide, so ElvUI also installs where its TOCs load but the list says nothing (Forever) |
| `cf:<id>`, `wago:<id>` | CurseForge, Wago | parsed and refused with `source_disabled` ("arrives in M5"), unless `--file` supplies the archive |
| CurseForge and Wago links (`curseforge.com/…`, `addons.wago.io/…`) | | refused with `source_disabled` while the ref is parsed, even with `--file`: use the `cf:` or `wago:` form with `--file` |
| an `https://` link to a `git_hosts` host, or any other non-GitHub link ending in `.git` | `git:` | as `git:` above (`#fragment` becomes the rev). A github.com link ending in `.git` stays `gh:` |
| any other `https://` link | `url:` | refused at plan time with `host_blocked` unless its host is one of the always-allowed hosts above |
| `http://…` (also `url:http://…`, `git:http://…`) | | refused (`ref_invalid`), suggesting the https form when omaddons can fetch it; for a host it does not download from (`details.host`), the fix is to download the file and install the local path, and for a git host not in `git_hosts`, to add it. When the https form is itself refused, its error stands (an `http://` CurseForge link is `source_disabled`) |

**Channels.** `--channel stable|beta|alpha`. The default is the addon's recorded channel, then `default_channel`, then `stable`. On GitHub, `alpha` behaves like `beta`: the newest non-draft release, stable or prerelease, among the 20 most recent (through the REST API). WoWInterface, Tukui, `git:`, `url:` and local sources publish one stream, so there the channel is only recorded. When `releases/latest` finds no stable release (only prereleases, or none published), the error is `no_compatible_release`, with `--channel beta` as the fix and the repository's `git:` ref as the alternative. When it answers 404 (no releases, or no such repository), the error is `not_found`, with the `git:` ref as the fix. A renamed repository is followed.

**Pinning.** `--version V` installs exactly that release and pins it: a tag for `gh:` refs, or a rev for `git:` refs. On the `release.json` path, `1.2.3` also tries `v1.2.3`. It is refused for `url:` and `file:` refs, and as a usage error when it disagrees with the rev of a `git:` ref, or with the tag of a pasted release link (a leading `v` aside). WoWInterface and Tukui serve only a project's current file, so there `--version` matches only the current version. A pasted release link pins its tag the same way (GitHub's own links work as pasted, `+` and `/` in tags included). Installing a pinned addon again without `--version` changes nothing and warns `addon_pinned`, without fetching its releases, even when a newer release exists. `--version W` moves it to `W`, still pinned. `--version` naming the release already installed pins it without reinstalling anything: a transaction of kind `pin` that records each folder and that `undo` unpins. A pin that another omaddons process commits while an install is planning also holds: the install re-checks it under the lock.

**What an update keeps.** Re-installing an installed addon keeps its pin and its recorded channel unless `--channel` names another. An addon installed only because another one needs it (`reason: dependency`, from milestone 2) becomes `explicit` when you install it by name, except with `--reinstall`, which keeps the recorded reason. When the release to install is the one already there, these record changes still happen, as a transaction that moves no folder: `--channel beta` whose newest release is installed records the channel (item `edited`, `fields: ["channel"]`), and a repeat changes nothing.

**A browser download.** `--file ZIP` installs an archive you downloaded as the release of one ref, so it is recorded against that addon.

**Same name, another ref.** An addon is installed from one ref at a time. Installing an addon whose name is already managed on the client from another ref is:

- **another local archive of a local install** (`./Bar-2.2.0.zip` over `./Bar-2.1.0.zip`): the same addon, `updated` from its version, now recorded from the new archive;
- **any other ref** (`gh:demo/bar`, or `--file` under a ref, over a local install, or a second repository shipping the same addon): a takeover, refused with `ownership_conflict`. The fix adds `--force`, and the alternative removes the installed one. With `--force` the item is `updated` from the replaced version, an `ownership_transferred` warning says where it now comes from, and `undo` restores the original ref and folders.

`gh:` and `git:` refs of one repository are different refs. To move an addon to another source on purpose, `omaddons adopt <folder> --ref <ref>` re-points it in place.

**Ambiguity.** `owner/repo` that also exists as a local path, or a bare name that names more than one ref, gives `ref_ambiguous` with every candidate as a runnable `omaddons install …` command. A bare name collects its candidates from installed addons on any client (by name or folder), the catalog, and a file or folder of that name in the current directory: a folder `elvui/` beside you and the catalog's ElvUI are ambiguous.

---

## The built-in catalog

62 entries are embedded in the binary. Each was checked against its publisher on 2026-10-14 (BugSack, ElvUI and Tukui on 2026-10-16; TellMeWhen through BattlePetBreedID, and the WoWInterface entries, on 2026-10-20): the GitHub entries against their `releases/latest` tag, `release.json` and the addon roots inside the zip; the Tukui entries against the Tukui API's list and the folders and TOCs of the zip it hands out; the WoWInterface entries against the API's file details (the project's current file and its MD5) and the folders and TOCs of that file, MD5-checked (their families are where those TOCs load, since the site's compatibility tags lag); and the legacy entries against their TOC's `## Interface` line. The last column is the families the publisher declares (in `release.json`, or the TOCs' interfaces). It is shown by `info`, gives `search`'s `yes` or `no`, and orders lookups. A dependency or an adopted folder is matched first to entries declaring the client's family. For a dependency, other entries of the same generation are tried only after Tukui and WoWInterface. For an adopted folder, they claim it at catalog strength, but only when no entry declares the client's family. A built-in entry is never matched across generations, legacy on a modern client or the reverse. It never refuses an install: TOC verification decides. The live catalog probe records where publishers and their own TOCs disagree. As of 2026-10-15, Plater declares titan but ships no TOC Titan loads, and RaiderIO declares only mainline but its TOCs also load on vanilla and mists.

| Name | Aliases | Ref | Declared families |
|---|---|---|---|
| WeakAuras | `wa`, `weakauras2` | `gh:WeakAuras/WeakAuras2` | vanilla, tbc, cata, mists, titan (**not retail**: 5.22.1 ships no Mainline TOC) |
| DBM | `deadlybossmods`, `dbm-core` | `gh:DeadlyBossMods/DeadlyBossMods` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| BigWigs | `bw` | `gh:BigWigsMods/BigWigs` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| LittleWigs | | `gh:BigWigsMods/LittleWigs` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| Plater | `plater-nameplates` | `gh:Tercioo/Plater-Nameplates` | mainline, vanilla, tbc, titan, mists, forever |
| Bartender4 | `bartender`, `bt4` | `gh:Nevcairiel/Bartender4` | mainline, vanilla, tbc, mists, forever |
| Questie | | `gh:Questie/Questie` | vanilla, tbc, wrath, mists, forever |
| OmniCC | | `gh:tullamods/OmniCC` | mainline, vanilla, tbc, titan, mists (11.2.8's mainline interface is 11.2: out of date on 12.x) |
| Dominos | | `gh:tullamods/Dominos` | mainline, vanilla, tbc, mists, forever |
| Masque | | `gh:SFX-WoW/Masque` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| RaiderIO | `raider.io`, `rio` | `gh:RaiderIO/raiderio-addon` | mainline |
| MythicDungeonTools | `mdt` | `gh:Nnoggie/MythicDungeonTools` | mainline |
| AllTheThings | `att` | `gh:ATTWoWAddon/AllTheThings` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| BugSack | | `gh:funkydude/BugSack` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever (its optional `!BugGrabber` has no official release and is never installed for it) |
| TellMeWhen | `tmw` | `gh:ascott18/TellMeWhen` | mainline, vanilla, tbc, mists, forever |
| ThreatPlates | | `gh:Backupiseasy/ThreatPlates` | mainline, vanilla, tbc, mists, forever |
| SilverDragon | | `gh:kemayo/wow-silverdragon` | mainline, vanilla, tbc, wrath, mists, forever |
| Capping | | `gh:BigWigsMods/Capping` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| IceHUD | | `gh:parnic/ice-hud` | mainline, vanilla, tbc, titan, mists |
| Quartz | | `gh:Nevcairiel/Quartz` | mainline, vanilla, tbc, mists (3.7.17's mainline interface is 11.2, so 12.x loads it only as out of date) |
| Mapster | | `gh:Nevcairiel/Mapster` | mainline, forever |
| BadBoy | | `gh:funkydude/BadBoy` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| NPCScan | | `gh:Torhal/NPCScan` | mainline |
| ClassicCastbars | | `gh:wardz/ClassicCastbars` | vanilla, tbc, wrath, cata, mists |
| AtlasQuest | | `gh:nanderson11/AtlasQuest` | mainline, vanilla, tbc, forever (it needs Atlas, which installs from WoWInterface) |
| VendorPrice | | `gh:ketho-wow/VendorPrice` | vanilla, tbc, mists |
| DBM-Vanilla | | `gh:DeadlyBossMods/DBM-Vanilla` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever (DBM's Vanilla and Season of Discovery mods) |
| Decursive | | `gh:2072/Decursive` | mainline, titan, forever (2.9.0-RC3 ships no other zips) |
| PremadeGroupsFilter | `pgf` | `gh:0xbs/premade-groups-filter` | mainline, mists |
| Rarity | | `gh:WowRarity/Rarity` | mainline, vanilla, mists (r751's mainline interface is 11.2: out of date on 12.x) |
| SavedInstances | | `gh:SavedInstances/SavedInstances` | mainline |
| Simulationcraft | `simc` | `gh:simulationcraft/simc-addon` | mainline, forever |
| WarpDeplete | | `gh:happenslol/WarpDeplete` | mainline |
| RCLootCouncil | `rclc` | `gh:evil-morfar/RCLootCouncil2` | mainline |
| BigDebuffs | | `gh:jordonwow/bigdebuffs` | mainline, vanilla, tbc, titan, mists (v68's mainline interface is 12.0: out of date on 12.1) |
| OmniBar | | `gh:jordonwow/omnibar` | mainline, vanilla, tbc, wrath, cata, mists (v34's mainline interface is 11.2: out of date on 12.x) |
| Cell | | `gh:enderneko/Cell` | mainline, vanilla, tbc, titan, cata, mists (r274's mainline interface is 11.2: out of date on 12.x) |
| LiteMount | | `gh:xod-wow/LiteMount` | mainline, mists, forever |
| Immersion | | `gh:seblindfors/Immersion` | mainline, vanilla, tbc, titan, mists, forever |
| SexyMap | | `gh:funkydude/SexyMap` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| GatherMate2 | | `gh:Nevcairiel/GatherMate2` | mainline |
| ThreatClassic2 | | `gh:dfherr/ThreatClassic2` | mainline, vanilla, tbc, mists, forever (one zip per client) |
| ClassicSpellActivations | | `gh:rgd87/ClassicSpellActivations` | vanilla, tbc, wrath |
| AdvancedInterfaceOptions | `aio` | `gh:Stanzilla/AdvancedInterfaceOptions` | mainline, vanilla, tbc, wrath, cata, mists, forever |
| MountJournalEnhanced | | `gh:exochron/MountJournalEnhanced` | mainline, mists |
| BattlePetBreedID | `bpbid` | `gh:MMOSimca/BattlePetBreedID` | mainline, mists |
| TomTom | | `wowi:7032` | mainline, vanilla, tbc, titan, mists, forever |
| Atlas | | `wowi:3896` | mainline, vanilla, tbc, mists, forever |
| HealBot | | `wowi:6096` | mainline, vanilla, tbc, mists, forever (HealBot Continued) |
| TitanPanel | | `wowi:8092` | mainline, vanilla, mists, forever (9.3.5's TitanAmmo lists 205056 among its interfaces, so Anniversary refuses the archive) |
| Clique | | `wowi:5108` | mainline, vanilla, tbc, titan, mists |
| Prat | `prat-3.0` | `wowi:10783` | mainline, vanilla, tbc, mists, forever |
| WIM | | `wowi:5342` | mainline, vanilla, tbc, titan, mists, forever |
| GTFO | | `wowi:17996` | mainline, vanilla, tbc, titan, mists, forever |
| HandyNotes | | `wowi:15716` | mainline, vanilla, tbc, mists |
| ElvUI | `elv` | `tukui:elvui` | mainline, vanilla, tbc, titan, mists, forever (one zip for every client) |
| Tukui | | `tukui:tukui` | mainline, vanilla, mists |
| pfUI | | `git:https://github.com/shagu/pfUI#master` | vanilla-legacy (1.12.1) |
| PlateBuffer | | `git:https://github.com/nullfoxh/PlateBuffer#master` | tbc-legacy (2.4.3) |
| GroupBulletinBoard | `gbb` | `git:https://github.com/fondlez/GroupBulletinBoard#main` | wrath-legacy (3.3.5a) |
| MikScrollingBattleText | `msbt` | `git:https://github.com/fondlez/MikScrollingBattleText-Cata#main` | cata-legacy (4.3.4) |
| InFlight | | `git:https://github.com/fondlez/InFlight-Taxi-Timer#main` | mists-legacy (5.4.8) |

**Your own entries.** Files in `~/.config/omaddons/catalog.d/*.toml` are applied in file-name order, in the same format. They can add an entry, replace one by name (the whole entry: fields are not merged), or drop one with `remove = true`. An entry's keys are `name`, `aliases`, `ref`, `families`, `folders` and `notes`. Only `folders` makes an entry findable as a dependency, or lets it claim a folder for `adopt` (`adopt` can still guess it for a folder named like it). An entry with no `families` counts as declaring every family, so it can match on any client. A file is skipped whole, with a `catalog_override_invalid` warning, when it does not parse, its `schema_version` is not 1.x, it has an unknown key, an entry has no name, no valid `ref`, an unknown family or an invalid folder name, it lists a `Blizzard_*` folder, or it repeats a name or alias.

```toml
# ~/.config/omaddons/catalog.d/mine.toml
schema_version = "1.0"

[[addon]]
name = "MyBars"
aliases = ["mb"]
ref = "gh:example/MyBars"
families = ["mainline"]
folders = ["MyBars", "MyBars_Config"]

[[addon]]
name = "Dominos"
remove = true
```

---

## Command reference

`omaddons help` lists the commands. `omaddons help <command>` or `omaddons <command> --help` shows every flag. For subcommands, use `omaddons targets add --help` or `omaddons help targets add`.

A mistyped command, flag or value is a usage error. Its fix is the corrected command when that command only reads, and `omaddons help <command>` when the closest command changes something: a typo never hands you a different command that changes addons. A mistyped flag, or a mistyped value of a flag with fixed choices, of the command you typed is corrected in place (`--channel betaa` → `--channel beta`). A bad `--actor` value's fix currently drops the flag, and a bad `undo --last-by` value breaks the rule (see [Known issues](#known-issues)). Every suggested command parses.

### Global options

Every global option works before or after the command, except `-V`/`--version`, which works only before the command (`omaddons --json -V` works; after a command, `--version` is that command's flag or a usage error).

| Option | Meaning |
|---|---|
| `--json`, `--robot` | One JSON envelope on stdout (see [The JSON contract](#the-json-contract)) |
| `--format json\|human` | Overrides `--json`; default `human`, or `OMADDONS_FORMAT=json` |
| `-t, --target <TARGET>` (alias `--flavor`) | The client to act on: [any selector](#choosing-a-target-with--t) |
| `--dry-run` | Show the plan without changing anything. Every command accepts it; one that changes nothing ignores it |
| `--actor human\|agent\|widget` | Who is acting, recorded in history by every command that changes something. Default `OMADDONS_ACTOR`, else `human` when stdin is a terminal, else `agent`. In `history` it filters |
| `--reason <TEXT>` | Why, recorded verbatim in history by every command that changes something (at most 200 characters); others accept and ignore it |
| `--offline` | Fail any network access (`OMADDONS_OFFLINE=1`) |
| `--while-running` | Allow folder changes while the client runs (never SavedVariables) |
| `--if-match <TOKEN>` | Change nothing unless the plan is still the one the `--dry-run` that returned this token showed (`data.precondition`); see [Previews and `--if-match`](#previews-and---if-match). Only `remove`, `undo` and `errors dismiss` take it today (later `backup restore` and `backup prune`); on any other command it is a usage error |
| `--no-color` | No ANSI color (also `NO_COLOR`). Color is also off with `--json`, in CI, with `TERM=dumb` and when stderr is not a terminal |
| `--quiet` | Only errors on stderr |
| `-v, --verbose` | More detail: paths, and debug `log` events on stderr (repeatable) |
| `--progress json\|human\|none`, `--no-progress` | Progress of long commands on stderr (`install`, `update`, `outdated`, `adopt`, `sync`, `copy`, `search` and `backup create`, and the stage and commit steps of `remove`, `pin` and `unpin`). Off with `--json`, in CI or when stderr is not a terminal, unless `--progress` names a mode; never with `--quiet` |
| `-h, --help` | Help |
| `-V, --version` | Version (before the command only; after a command, `--version` is that command's flag or a usage error) |

### `status`

Triage: every client, its addon and folder counts, whether it is running, its new Lua errors, and what needs attention (a missing client, a pending journal, a degraded addon, a newer state version, managed files changed on disk or managed folders gone).

```bash
omaddons status               # scan, then report (managed files checked stat-first)
omaddons status --cached      # no scan: the cached clients, re-checked on disk (fast; the widget's view)
omaddons status --deep        # re-hash every managed file, not only those whose size or time changed
omaddons status --check       # exit 6 when anything needs attention
omaddons status -v --json     # adds client_dir, install_root and exe_name, and changed paths
```

`--deep` with `--cached` is a usage error. `--cached` before any discovery cache exists also suggests `omaddons status` (action `status`). The human view shows at most 5 changed paths per addon; `-v` shows every path the check listed (at most 50 per kind and folder).

Each client also shows:

- **`running`.** Whether its game runs now, from `/proc`; `null` when `/proc` could not be read (with `running_unknown`; the human view shows `running?`).
- **`updates`.** Its pending update counts (`available`, `pinned`, `blocked`, `failed`, `unchecked`) and `checked_at`, the oldest check. They come from what earlier `outdated`, `update` and `install` runs remembered, never from a request, so `status` stays offline. `stale` is true when that check is older than `check_interval_hours` (default 6).
- **`drift`.** Each managed addon whose files changed on disk since omaddons installed them (`modified`), whose folder is gone or replaced by a symlink (`missing`), or that has no file index to check against (`unindexed`), with its changed-file count (`files`; the paths with `-v`). A file whose size or time changed is re-hashed, so one merely touched is not reported; `--deep` re-hashes every file. `--cached` checks only that the folders are there. Drift is computed on every run and never stored. `modified` raises the attention `addon_modified`, `missing` raises `addon_folder_missing`.
- **`adoptable`.** How many unmanaged folders a catalog entry names that is not installed on that client.
- **`orphans`.** How many addons installed as dependencies nothing requires any more (from the record, `--cached` too).
- **`errors`.** Its Lua errors, from BugGrabber (see [`errors`](#errors)), in `errors`' units: `capture` (`buggrabber`, or `none` with every count 0), `state`, `coverage` (`complete`, or `truncated` when a bound cut the walk), `new`, `total` (local errors, `errors`' `summary.entries`), `addons`, `received` (errors other players sent: never new and never in the other counts), `top` (at most 5 `{addon, folder, new, count}`, the addons with the most new errors, then occurrences), `last_session_at` (when the client last wrote a BugGrabber file, at logout or `/reload`) and `dismissed_at` (the newest dismissal in effect). `new` is decided at every call by the rule `errors` uses, so errors stop being new as their file passes 7 days old, or once dismissed, with nothing read again. Plain `status` reads BugGrabber's files as `errors` does and writes nothing; its `state` is `ok`, `partial` (a file the game was writing, one over its size bound, or one that could not be read: the counts are what was read) or `unreadable` (no file could be read). `--cached` opens none: it recounts what the last `errors` run recorded, checking only each file's stamp and each `SavedVariables/` folder's file names (within `errors`' bounds), and says `stale` when they changed since (the counts are that run's) or `unchecked` when no run has read the client (the counts are 0, and a dismissal still holds).

The human view opens with one summary line (`3 updates on 2 clients · 1 issue (P2) · 4 new Lua errors on 1 client · retail running`, or `Nothing needs doing`), prints each client's issues under its row (`  P2 Plater's files changed on disk (3 files) → omaddons install Plater -t retail --reinstall`) and, when they say something, its Lua errors (`  errors: 4 new (DBM 3, WeakAuras 1) · omaddons errors -t retail`, `errors: incomplete read (file was being written) · 2 new so far`, `errors: not read yet · omaddons errors -t retail`, `errors: changed since last read · omaddons errors -t retail`; addon names and counts only, never an error's text), and ends with the last change (`last change: update on classic, retail by human (2026-10-21 18:04) · undo: omaddons undo --run <run>`).

The JSON adds three keys to `data`:

- **`summary`.** The machine at a glance, for the widget and agents: `targets`, `running` (the ids of clients running now), `updates` (`available`, summed, and the `targets` that have some), `issues` (counts by severity: `p0` to `p3`), `check_due` (a check for updates is recommended), `errors` (`new`, the clients' new local Lua errors summed, and the `targets` that have some), and `widget_outdated`, which stays `false` until the bar widget ships. `state` is the most pressing of `attention` (a P0 or P1 issue), `errors` (new local errors; received ones never count), `updates` and `idle`.
- **`issues`, on each client.** Its `attention` items as findings: `id` (a failure-mode id from the vision's D20 table, stable across releases), `severity` (`P0` to `P3`), a `title`, the `addon` when it is about one, and a `remediation` (`command`, `argv`, `destructive`; `argv` is `null` when the fix is advice, not a command). Ranked by severity, then id, then addon. `attention` stays as it was.

  | Issue `id` | Severity | Raised by | Remediation |
  |---|---|---|---|
  | `fm-concurrency_primitives-journal-pending` | P0 | `journal_pending`; titled "needs manual repair" when recovery cannot settle it | advice: quit the game and run any changing command, or, for a manual repair, [`journal_pending` that does not clear](#journal_pending-that-does-not-clear) |
  | `fm-schemas-state-version-newer` | P0 | `state_version_newer` | advice: upgrade omaddons |
  | `fm-state_files-record-folder-missing` | P1 | `addon_folder_missing`, per addon | `omaddons install <addon> -t <id> --reinstall` |
  | `fm-external_artifacts-managed-files-modified` | P2 | `addon_modified`, per addon | `omaddons install <addon> -t <id> --reinstall` |
  | `fm-external_artifacts-ownership-transferred` | P2 | `addon_degraded` | `omaddons list -t <id> -v` |
  | `fm-userland_state-target-vanished` | P2 | `target_missing` | `omaddons targets rm <id>` (destructive) |

- **`last_change` and `last_change_state`.** What bare `omaddons undo` would revert now: its `run`, `kind`, `at`, `actor`, `reason`, the `targets` it touched, the folders it changed (`changes`: `added`, `replaced`, `removed`, `recorded`, `restored`, summed over the run) and the `undo` command (`command`, `argv`). Backups and settings changes are never the last change, since undo never reverts them. Plain `status` reads the whole history; `--cached` asks the history projection (`history_tail.json`) and writes nothing. `last_change_state` is `ok` (with `last_change` `null` when there is nothing to undo) or `unknown`, which `--cached` reports when the projection is missing, stale or cannot tell, and which never means there is nothing to undo; plain `status` then answers. The next command that changes something rebuilds the projection.

Pending updates never count as attention: `status --check` stays a health check, and `outdated --check` is the update check. The recommendations:

- `omaddons install <addon> -t <id> --reinstall`, for each modified or missing addon (at most 5);
- `omaddons update`, or `-t <id>` when only one client has updates;
- `omaddons adopt -t <id>`, for each client with `adoptable` folders;
- `omaddons outdated`, when a client was never checked or its check is stale;
- `omaddons errors` (`check_errors`), when a client's Lua errors are `stale` or `unchecked` (so only under `--cached`), never when a read was `partial` or `unreadable`, which reading again would not complete;
- `omaddons remove <orphans…> -t <id>` (`remove_orphans`, destructive), for each client with orphans;
- the attention fixes, and `omaddons list -t <id> -v` (action `inspect`) for a client with degraded addons;
- a look at unmanaged folders, on a client with unmanaged folders none of which is `adoptable`.

`outdated` and `update` also recommend `omaddons adopt -t <id>` for a client that manages nothing yet but has `adoptable` folders.

### `install <REF>...`

Install or update addons in one target (`add` is an alias). Each ref is fetched once and verified against the target's TOC rules before anything is written, and all addons for a target go into one transaction. Re-running `install` on an installed addon updates it to the newest release its channel allows. `url:` refs are never re-checked, and pinned addons stay where they are (`addon_pinned`; see [Pinning](#sources-and-refs)).

| Flag | Meaning |
|---|---|
| `--channel stable\|beta\|alpha` | Release channel (default: recorded, then `default_channel`, then stable) |
| `--version <V>` | Exactly this release, pinned: a tag for `gh:`, a rev for `git:` |
| `--file <ZIP>` | Use this downloaded archive as the release of the one given ref |
| `--reinstall` | Install the recorded release again, replacing its folders: repairs modified or missing files (undoable) |
| `--force` | Take over a folder another addon owns, an addon of the same name installed from another ref, or replace an unmanaged folder (all undoable until retention prunes the snapshot; `snapshot_only_copy` says when it holds the only copy) |
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
omaddons install Plater -t retail --reinstall   # put back modified or deleted files
```

Each item's `status` is `installed`, `updated`, `unchanged`, `skipped` (the same addon requested twice: `skip_reason: "duplicate"`, with `duplicate_of`) or `failed`, or, when only its record changed, `pinned`, `unpinned` or `edited` (with `fields`). A partial result (some addons failed) is still exit 0, with `partial: true` and each failure in `data.items[].error`. With `--strict` it is exit 6. When every addon fails, the command fails with the first item's error, and `error.details.items` lists them all. `-v` adds each item's `asset_url`. Never-launched clients have no `Interface/AddOns` yet: the first install creates it inside its transaction and reports `creates_addons_dir: true`.

**Reinstalling.** `--reinstall` installs the recorded release again, even when it is current, and replaces the addon's folders: files modified by hand come back as released, and deleted folders come back. The archive comes from the download cache when it is still there, with no request (offline too). Otherwise that exact release is fetched again, never a newer one. A pinned addon stays pinned, and a dependency stays a dependency. The item is `updated` from and to the same version, in an `update` transaction, so `undo` brings the replaced files back. Only an installed addon can be reinstalled (`not_managed`, fix `omaddons install <addon> -t <id>`). `omaddons reinstall X` is a usage error whose fix is `omaddons install X --reinstall`.

**Dependencies.** What an addon's TOC requires (`## Dependencies`, `## RequiredDeps`) installs in the same transaction when a keyless source ships it, from `default_channel` (never the requesting addon's channel). The folder is looked up locally, never sent to a service, in this order: the catalog's entries declaring this client's family (including your `catalog.d` entries); the Tukui list, when exactly one project ships it; the WoWInterface index, whose best project wins only when it alone ships just that folder or is named like it, and changed in the last two years (otherwise its candidates are listed); then the catalog's other entries of the client's generation, whose TOCs decide (one that does not load here is reported `not_shipped`, and the run is not partial). Each index is read at most once per command, and only when the catalog misses: the Tukui list from the release memo inside 15 minutes, the WoWInterface index from its stored copy while it is fresh (an hour, or the server's longer `max-age`). `--refresh` fetches both again. `--offline` reads what is stored, whatever its age, with one `stale_check`, and never downloads an index. A source turned off in `config.toml` is skipped, and the `missing_dependency` message names each source it could not consult and why. It is recorded as `reason: dependency` with `required_by`, and `undo` removes it with its requester. `## OptionalDeps` never installs anything. Some required folders install nothing:

- `Blizzard_*` folders and folders of the same archive.
- A folder that is already present. A managed owner gains the requester in `required_by`. For unmanaged ones, `commands` offers `omaddons adopt <folders…> -t <id>` (action `adopt`).

`missing_dependency` warns, and the run stays a success, when a required folder:

- cannot be resolved (no keyless source ships it, or several do);
- comes only from a WoWInterface project unchanged for two years (`wowi_abandoned`);
- is not shipped for this client by the project that resolves (`not_shipped`);
- has only a build that is out of date for the client;
- is required only under a condition omaddons cannot decide offline, such as a text locale (`unknown_condition`; the fix searches for it);
- is listed in an installed addon's record, but that folder is gone (`managed_folder_missing`; the fix is `omaddons install <owner> --reinstall -t <id>`);
- is past a dependency limit (4 rounds, 32 dependencies per client);
- was not installed because of `--no-deps`.

A dependency that resolves but fails to install makes the run `partial`, with a warning naming it and what requires it. The requesting addon still installs.

**In the output.** Each dependency installed (or failed) is an item of its own, right after the item that required it, with its `request` set to the folder that was required (a dependency's own dependencies follow it). The text says `(dependency of LittleWigs)`:

```text
retail: installed LittleWigs v1 (1 folder)
retail: installed BigWigs v1 (1 folder) (dependency of LittleWigs)
```

Every item carries:

- `reason`: `explicit` or `dependency` (`null` when nothing is known, such as a ref that does not parse);
- `required_by`;
- `bridge_dependencies` (`[{folder, requires}]`): what its other folders (bridges, plugins such as `Masque_Dominos`) work with and nothing installs;
- `unresolved` (`[{folder, reason, candidates}]`): its required folders that were not installed, with up to three candidate refs, best first.

With `-v` it also carries `optional_dependencies` (its `## OptionalDeps`, never installed). A bridge requirement warns `bridge_dependency` once, when the addon is first installed, with a fix that installs it (by catalog name, else by the ref the resolver finds); later installs and updates keep the field and say nothing more. After the `undo` hint (and any `adopt` hint for required folders already present), `commands` offers at most three `omaddons install <candidate> -t <id>` hints (action `install`): unresolved candidates in item order (never one that is `not_shipped`), then bridge requirements. `update`, `sync` and `pin` items that change an addon carry the same fields, each dependency right after its requester; their skipped, unchanged and failed items leave them out, except a failed dependency.

`required_by` stays true. An update whose new release no longer requires any folder of a dependency drops the addon from that dependency's `required_by`, in the same transaction and under `--no-deps` too. A dependency left required by nothing warns `orphaned_dependency` (see [`remove`](#remove-addon)). Installing a dependency by name makes it `explicit`, and an explicit addon is never an orphan.

### `update [ADDON]...`

Every managed addon on every client, or only the named ones, to its newest compatible release. `upgrade` is an alias, and bare `update` is the same as `update --all`.

```text
$ omaddons update
classic: updated 1 addon (tx 01M5Q2VA0JX3N8K2…) — /reload in game
  DBM  12.1.14 → 12.1.15
era: nothing to update (Questie is pinned)
  Questie  skipped: is pinned
retail: updated 2 addons (tx 01M5Q2VA1KR4P9D6…)
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
omaddons update --no-deps           # not what the new releases' TOCs start requiring
```

When an addon's new release starts requiring another project, `update` installs that project in the same transaction, as `install` does: recorded as a dependency, from `default_channel`, undone with the update. A dependency that resolves but fails to install is a failed item named by the folder required (never `blocked`), and the addon itself still updates.

What to update comes from the same statuses `outdated` shows, fetched once per ref (free inside 15 minutes; `--refresh` asks again). `--all` together with addon names, and `--channel` without names, are usage errors. A named addon installed nowhere is a failed `not_managed` item. Each client with updates commits its own transaction, all under one run, so `omaddons undo` reverts every client and `omaddons undo <tx>` one of them. A failure on one client does not roll back another.

Some addons are left alone and say why (`skipped`, with `skip_reason`): `pinned` (by `pin` or `install --version`; only naming it adds an `addon_pinned` warning, whose fix is `omaddons unpin <addon> -t <target>`), `ahead`, `static` (a local file, a URL or an archive given with `--file`), `unchecked`, and `blocked`. A newer release that turns out not to load on a client is `blocked`, not a failure; the verdict is remembered, so `outdated` shows it too. A fetch that fails fails its addons (`update` never installs from stale metadata) and makes the run `partial`. `unchanged` addons are counted in `summary` and listed with `-v`.

Files you changed in an addon's folders do not stop its update: they are replaced like the rest, and a `local_changes_replaced` warning names them, with `omaddons undo <tx>` as its fix, because the transaction snapshot keeps them (at least 24 hours). The same goes for `install` and `sync`. `-v` names the snapshot folder.

There is no `--version` and no `--force`: a given version is an install (`omaddons install <addon> --version V` or `omaddons pin <addon> --version V`, which pin it), and taking over another addon's folder is `omaddons install <addon> --force -t <target>`, which an `ownership_conflict` names as its fix.

### `pin <ADDON>` and `unpin <ADDON>`

Hold an addon at its release through `update`, and let it go again.

```text
$ omaddons pin questie
forever: pinned 1 addon (tx 01M5Q2VB2MZ0N4E6…)
  Questie  pinned at bundle/v12.1.0+v1.0.6
undo: omaddons undo 01M5Q2VB2MZ0N4E6V7A8C9D1EF
$ omaddons unpin questie
forever: unpinned 1 addon (tx 01M5Q2VB3NA1P5F7…)
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

A pin without `--version` changes only the record, never a game file, so it works while the game runs. Like every change it is a transaction (kind `pin`), with one per client under one run, so `omaddons undo` reverts it. `pin --version` installs that release first, as `install --version` does: it replaces folders, and its transaction is an `update` (or an `install`). An addon installed nowhere is `not_managed` (fix `omaddons install <addon>`). An addon already pinned (or unpinned) is left as it is, with no transaction. A pin keeps the addon's channel and its reason (a pinned dependency stays a dependency). `--version` takes a tag (`gh:` refs) or a rev (`git:` refs). `url:` and local installs have no other release to choose, so `--version` is refused for them. After `unpin`, the last line says what `update` would install, from the remembered metadata and without a request.

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
| `--locked` | Install the version each entry records, from the download cache when it is still there |
| `--prune` | Also remove managed addons the file does not list (see below) |

**The file.**

- A family section applies to every live client of that family, and never to a test client (a PTR or beta).
- A `[target."<id>"]` section applies to that one client, and its entries replace family entries for the same addon.
- An entry is a name or a ref, read the way `install` reads its arguments. Installed names are looked up on that client only, and relative paths are read against the file's own directory.
- An entry in table form has these keys:
  - `name`, unless `ref` is given;
  - `ref`;
  - `channel`: absent means `stable`, whatever `default_channel` says;
  - `version`;
  - `sha256`: the archive's digest, which `export` writes; with `--locked` it lets the archive come from the download cache;
  - `pinned` (default `false`);
  - `dependency`: a version lock for a dependency, not a request.
- Unknown keys are warned about (`desired_unknown_key`), and so are sections naming no known client (`desired_unknown_target`).
- A file that does not parse, or holds a bad value, an unknown `[family.<name>]` section (`reason: "unknown_family"`) or a newer `schema_version` (`newer_schema`), changes nothing: `desired_state_invalid`, exit 2, with the line. A missing file is `not_found` (exit 3).
- `-t` limits which clients are synced.

**What sync does.**

- It installs what is missing.
- A pinned entry at another version installs that version, pinned.
- An entry on another channel switches the addon to it: a records-only change when that channel's newest release is the installed one.
- A pin that differs from the file is set or cleared without touching a folder.
- It never moves an installed addon to a newer release; that is `omaddons update`'s job.

So a second `sync` changes nothing and sends no request; its human output says "nothing to do", and reminds you that `sync` does not update (`omaddons update` does). Each client with changes gets one transaction of kind `sync`, all under one run, and `omaddons undo` reverts the whole sync. An entry that cannot be resolved fails on its own (`partial: true`), and so do two entries that name the same addon. What an addon's TOC requires installs with it, as for `install`.

**`--locked`** reproduces versions, not only refs: `omaddons export` on one machine, then `omaddons sync <file> --locked` on another.

- Every entry with a `version` installs that version, and an installed addon at another version moves to it. A lock never pins: `pinned` comes only from the entry.
- With the entry's `sha256`, the archive comes from the download cache while the cache still holds those exact bytes. On the machine that exported, a restore then needs no network, `--offline` included.
- Otherwise the exact release is fetched (and downloaded unless already cached).
- A version its source no longer serves fails its entry (`not_found`), with `sync` without `--locked` as the fix.
- Entries without a version install the newest release, with one `not_locked` warning per client.
- `dependency = true` entries fix the version of a dependency that `sync --locked` installs; one that matches nothing is ignored.

**`--prune`** also removes the managed addons the file does not list, in the same transaction, so `omaddons undo` brings them back. It keeps:

- every addon an entry names, or may name: entries that fail as duplicates or ambiguous names keep every addon they could mean;
- every addon a `dependency = true` lock names;
- every addon a kept one requires, through any chain, whatever its `reason`.

It touches only clients the file has a section for (others report `no_section`), and never an unmanaged folder: it reads records, not folders. A `[target."<id>"]` section naming no known client may be a typo whose client would be emptied, so `--prune` refuses the whole file (`desired_state_invalid`, `reason: "unknown_target_section"`, fix `omaddons targets`); `sync` without it proceeds. An empty section (`addons = []`) removes every managed addon there: try `--dry-run` first.

### `export`

Write every client's addon set as one `addons.toml`, the file `sync` reads: to stdout by default, so it can go anywhere, or with `--write` to the config directory.

```bash
omaddons export > dotfiles/wow/addons.toml   # then, on another machine: omaddons sync dotfiles/wow/addons.toml
omaddons export -t vanilla                   # only these clients
omaddons export --write                      # ~/.config/omaddons/addons.toml, the previous file kept as addons.toml.bak
```

It reads only omaddons' records: no request, and without `--write` it writes nothing.

- A family section holds what every live client of that family has alike (same ref, channel, pin and version), when all of them are exported. The rest goes in each client's own `[target."<id>"]` section. Test clients (PTR, beta) only ever get their own section.
- Every entry is a table sorted by name, so the file diffs cleanly. `version` is written only for a release (not for an addon adopted at its TOC's version), `sha256` only while the archive is in the download cache, and `channel` and `pinned` only when they are not the defaults.
- A dependency is written as a lock (`dependency = true`, with its version when that is a release version): in the family section when every live client of the family records the same one and something on each of them requires it, else in its client's section.
- An addon installed from a local file or folder is left out, with `export_local_source`.
- A registered client whose directory is gone (an unplugged drive) is exported from its records, in its own section, with `export_missing_target`.

`--write` refuses, with `desired_state_invalid` (exit 2), a file changed since omaddons last wrote it, or one it did not write (`details.reason: "edited"`): `--overwrite` replaces it (without `--write` it is a usage error). An export with no entries is never written (`reason: "empty"`). The previous file is kept as `addons.toml.bak`, and an unchanged export writes nothing. Under `--json`, `data` is `{path, backup, targets, entries, content}`, with `path` and `backup` `null` when nothing was written.

### `copy`

Install one client's addons on another, where builds exist: every addon installed explicitly on `--from`, or the ones named, goes to `--to` in one transaction.

```text
$ omaddons copy --from era --to anniversary
anniversary: installed 4 addons (tx 01M5…)
  Foo  v1 (installed)
  Pin  v2 (installed)
  Req  v1 (installed)
  Lib  v1 (installed) (dependency of Req)
  skipped: Bar (no build for tbc), Loc (local source), Qux (already installed)
undo: omaddons undo 01M5…
```

```bash
omaddons copy --from era --to anniversary Questie   # only the addons named (any record of --from, dependencies too)
omaddons copy --from era --to anniversary --dry-run # what would install, and what would be skipped
omaddons copy --from era --to anniversary --strict  # exit 6 when an addon fails (skips never count)
omaddons copy --from era --to anniversary --no-deps # only the addons copied, not what their TOCs require there
```

Each side is exactly one client: a selector that matches two is `target_ambiguous`, with one fix per candidate, and `-t` is a usage error, as are `--from` and `--to` naming the same client. A named addon missing on `--from` is a failed `not_managed` item (fix `omaddons list -t <from>`). Each addon installs from its ref on its recorded channel, the way `install` would: the newest release that loads on `--to`, unpinned, because versions differ between flavors. Dependency records are not copied; what `--to`'s own TOCs require installs with it, as for `install`. Three kinds of addon are skipped, never failed: one already on `--to` (by ref, `already_installed`), one installed from a local file or folder (`local_source`), and one with no build that loads on `--to` (`no_build_for_target`). Any other failure, such as a source that cannot be reached, makes the run `partial`. `omaddons undo <tx>` from `commands` reverts the copy.

### `remove <ADDON>...`

Remove managed addons from one target, as a transaction undoable until retention prunes its snapshot (`uninstall`, `rm` and `delete` are aliases). Removing what no source gives again (an addon adopted as it was, or installed from a local archive or directory) warns `snapshot_only_copy`. An addon is named by its name, its ref or a folder it owns.

```bash
omaddons remove DBM -t retail
omaddons remove gh:BigWigsMods/BigWigs -t era
```

Unmanaged, linked (symlinked) and `Blizzard_*` folders, and names that match no managed addon, are refused with `not_managed`. A folder or ref that several managed addons answer to is `ref_ambiguous`. Without `-t`, the client is the one that manages the named addons. `--dry-run` returns `data.precondition`, and `--if-match <it>` removes exactly what that preview showed, or nothing (see [Previews and `--if-match`](#previews-and---if-match)).

An addon whose folders were all deleted by hand is forgotten: its record and index entries go, in a transaction that records each folder (`record` ops) and that `undo` reverts. Such a remove changes no folder, so it also goes through while the game runs.

**Dependencies.** Removing an addon others still require goes ahead, with `still_required` naming them: they will not load without it, and its fix is `omaddons undo <tx>`. Every addon that listed the removed one in `required_by` drops it in the same transaction (a `record` op with `edit: {"required_by_remove": "<name>"}`). A dependency that nothing requires any more is an orphan, and `orphaned_dependency` names them all with one `omaddons remove <names…> -t <id>` as its fix. Orphans are never removed by themselves. `undo` restores `required_by` exactly, even if the dependency was updated in between, and keeps requesters added after the transaction it reverts. Undoing an older install of a dependency that a later addon requires warns `still_required` too (see [`undo`](#undo-tx)). Removing an addon together with what it requires (`remove LittleWigs BigWigs`) raises neither warning.

### `undo [TX]`

Revert what a command did, as new transactions, SavedVariables included (`revert` and `rollback` are aliases).

A command that changes several clients commits one transaction per client, and all of them share a run id. Bare `undo` reverts the most recent run, every one of its transactions, newest first. After a single-client command that is one transaction, as before. Those undo transactions share a run of their own, so a second `undo` reverts them all again.

| Flag / argument | Meaning |
|---|---|
| `TX` | One transaction: an id or a unique prefix of 8+ characters |
| `--run RUN` | Every transaction of one run (`history` shows run ids; a unique prefix of 8+ characters works); with `-t`, only its transaction on that client |
| `-t TARGET` | The most recent transaction on that client only |
| `--last-by human\|agent\|widget` | This actor's most recent run (an undo counts too); with `-t`, that actor's newest transaction on that client |
| `--saved-variables auto\|restore\|keep` | `auto` (default) restores what changed and defers that restore while the game runs. `restore` refuses while the game runs. `keep` never touches SavedVariables |

```bash
omaddons undo
omaddons undo 01M51F4QY5KB
omaddons undo --run 01M51F4R2ZQX
omaddons undo -t retail
omaddons undo --last-by agent
omaddons undo 01M51F4QY5KB --saved-variables restore   # finish a deferred restore after quitting
```

`TX`, `--run` and `--last-by` cannot be combined. A prefix that matches several transactions or runs is `not_found`, with the candidates in `details.candidates`.

`--dry-run` returns `data.precondition`, and `--if-match <it>` reverts exactly what that preview showed, or nothing: after a newer change, bare `undo` would pick another run, and the bound run refuses with `plan_stale` instead (see [Previews and `--if-match`](#previews-and---if-match)).

When one transaction of a run cannot be undone (`undo_conflict`, say), the others still are. The result is then `partial`, and `transactions[]` in the JSON data holds one result per transaction. The fields before it describe the first transaction undone, as they always did. When every transaction fails, `undo` fails with the first error.

Undoing the newest change never conflicts with a later transaction. Undoing an older one is refused with `undo_conflict` if a later transaction touched the same folders, and the fix names the newer one to undo first. A later transaction that was itself undone no longer counts, so undoing your own transactions newest first, by id, always works. `undo_conflict` also covers a transaction already undone, and a folder it removed that was put back by hand. Undoing again a transaction whose undo deferred its SavedVariables restore is `undo_conflict` too, with `omaddons undo <tx> --saved-variables restore` as the fix.

Settings a `WTF/` restore put back are never overwritten in silence. Undoing an older transaction whose SavedVariables it would restore, after a later restore, is `undo_conflict` naming that restore to undo first, with the same undo and `--saved-variables keep` (folders only) as the alternative; the deferred `--saved-variables restore` follow-up is refused the same way. Undoing a restore is refused while a later transaction wrote settings: another restore, a restore of one addon's SavedVariables, or an undo that restored SavedVariables (an undo recorded before milestone 3 does not say whether it did, so it counts as one). A restore followed by its own undo no longer counts as a later restore.

Undoing a whole `WTF/` restore puts the tree from before it back with one `RENAME_EXCHANGE` (after a restore onto a client that had no `WTF/`, it moves the restored tree out), and keeps the tree it replaces in its own snapshot, so the undo can be undone in turn. It needs the same filesystem support as the restore (`filesystem_unsupported` otherwise), never runs while the game runs (`game_running`, whatever the running policy or `--while-running`), and refuses `--saved-variables keep`, which would leave nothing to undo. It reverts every play since the restore: `data.changed_since` counts the files that play changed, which stay in the undo's snapshot (`null` when the restored backup is gone and they cannot be counted). Its `--dry-run` token covers the live `WTF/`, so a play session between the preview and `--if-match` refuses the run.

A newer history line this omaddons cannot read (one a newer version wrote, or one damaged) is assumed to have touched whatever it may have: every SavedVariables file of its target, and the folders it names (every folder when it names none, names `WTF`, or is not JSON at all). Only a line naming another target is ruled out. Undoing an older transaction whose folders it may have touched is then `undo_conflict` with `details.reason: "history_unreadable"` (and `details.lines`), whose fix is to undo with the newer version; when only SavedVariables are in question, `omaddons undo <tx> --saved-variables keep` is the alternative. A line older than the transaction undone never counts.

Settings changes in the history (`config`, `target`, `auth` and `export` lines) are never undone: bare `undo`, `--last-by` and `--run` step over them, and `undo <tx>` of one is `not_found` ("transaction <tx> is a config change, which undo does not revert"). The same goes for a `backup` line: a backup is no change to undo ("transaction <tx> made a backup, which undo does not revert", fix `omaddons backup list`).

A later transaction that only added or dropped a requester in a dependency's `required_by` never conflicts. An addon the undo brings back keeps those later requesters. Undoing a transaction that installed a dependency an addon installed later still requires goes ahead with `still_required`, as `remove` does, and its fix undoes the undo. A dependency the undo leaves required by nothing warns `orphaned_dependency`.

### `backup`

Each client's `WTF/` backups: copies of its settings, the data players actually lose (milestone 3). `backup create` makes them and `backup list` shows them; checking and restoring them are the next parts of milestone 3. Bare `backup` is `backup list`.

```bash
omaddons backup create                     # every live client that has a WTF/, one backup each
omaddons backup create -t retail --reason "before the raid"
omaddons backup create -t era --verify     # also hash every file it copied
omaddons backup create --dry-run           # what it would copy, copy-on-write or not, and the space free
omaddons backup list                       # every client with backups, newest capture first
omaddons backup list -t retail --limit 5
omaddons backup list 01M6Q2VA -v           # one backup by an id prefix (8+ characters), with its addon versions
```

#### `backup create`

Copies each selected client's `WTF/` into a new backup. Without `-t` it backs up every live client that has a `WTF/`, each into a backup of its own, all under one run; a test client (PTR, beta) only when `-t` names it. A client `-t` selects that has no `WTF/` yet (it was never launched) fails on its own with `not_found`, and the run is `partial` when another client's backup was made.

- **No lock while it copies.** The copy holds no lock, so other changes go on meanwhile. The global lock is taken only to register a client the first time and to append the backup's history line. A running client is backed up too; its backup records `client_running: true`.
- **Copy-on-write.** On btrfs and xfs every file is cloned: the copy shares its data with `WTF/` until either changes, so even a large `WTF/` is copied in a moment and takes no space at first. Elsewhere files are byte-copied, and `backup_copied` says how many bytes that took.
- **Stable or best effort.** Each file is stamped before and after its copy and copied again if it changed; then `WTF/` is walked again and compared with what was copied. A capture that changed is made once more. `stable` means `WTF/` was observed not to change while it was copied: it is not a filesystem snapshot. When it changes again on the second try (the game writing SavedVariables at logout, a sync tool), that copy is kept as `best_effort`, and `backup_unsettled` counts what changed: make another after the game exits. A best-effort backup never counts as a checkpoint (`qualifying: false`).
- **Metadata only, or verified.** The manifest records each entry's path, kind, size, exact mode and mtime (and a symlink's target) as the live tree had them. By default no content is hashed (`integrity: metadata_only`), so a file later damaged at the same size goes unnoticed. `--verify` hashes every copied file before the backup is published and records the hashes (`integrity: verified`, `baseline: capture`).
- **What produced it.** `addons.json` records each managed addon's name, folders, version and source, as the client's record had them.
- **Owner only.** `backups/` and every backup in it are mode `0700`, their metadata files `0600`. Stored directories stay owner-writable, so a backup can always be pruned; their exact modes are in the manifest.
- **Free space.** When less than `limits.backup_min_free_mb` is free beside the backups (default 2,048 MiB; 0 turns the check off), `backup_space_low` warns and the backup is made anyway: it was asked for.
- **History.** Each backup appends a line of kind `backup` with `backup: {id, kind, files, bytes}` (`history` shows it as `WTF/ as <id> (<files>)`). `undo` never reverts it and steps over it.
- **`--dry-run`** walks `WTF/` without writing anything and reports `would: {files, bytes, copy, free_bytes}`, where `copy` (`copy_on_write` or `byte_copy`) comes from the filesystem's type, a hint until a clone is tried.

`data` is `{dry_run, run, targets: [{target, backup, tx, skipped, error}]}`. `backup` is the backup as `backup list` shows it, and `tx` its history line (`null` when the line could not be written, with `history_not_recorded`). `skipped` is `null` for a backup you asked for. A `WTF/` past 200,000 entries or 16 levels is `archive_too_large`. One whose files vanished while they were copied, on the second try too, is a retryable `io` error; the path it names keeps no account, realm or character name. Progress is `copy` events labelled `WTF/ files`, and with `--verify` `verify` events labelled `verifying`: counts under a fixed label, never a path.

```text
$ omaddons backup create
era: backed up WTF/ as 01M6Q2VB00000000… (57 files, 81.2 kB, copy-on-write, stable)
retail: backed up WTF/ as 01M6Q2VA00000000… (412 files, 15.9 MB, copy-on-write, stable)
```

#### `backup list`

A backup is `<client>/Interface/.omaddons/backups/<id>/`, holding:

- `backup.json`: what it is and what it proves;
- `manifest.json`: every entry's stats. It stays on this machine, because its paths name the account, realm and character;
- `addons.json`: the addon versions that produced it;
- the `WTF/` copy itself.

Listing reads the `backup.json` files and checks each manifest exists. It never walks or hashes a backup's tree (rebuilding a stale `backups.json`, below, stat-walks undo's `WTF.previous` trees, bounded, to size them). Each backup says:

- `kind`: `manual`, `auto` (with `trigger` `pre_change` or `post_session`), `before_restore`, `imported` or `recovered`.
- `created_at` (when it was published here), `captured_at` (when its content was captured; an import keeps the original's), `actor`, `reason`, `files` and `bytes` (apparent size); `client_running` (the client ran while it was copied) and `unsettled` (how many files were still changing after a second copy).
- `reflinked`: `all`, `some` or `none` of its files share their data with the live `WTF/` (copy-on-write clones).
- `consistency`: `stable` when the tree was observed quiescent while it was copied (not a filesystem snapshot), else `best_effort`.
- `integrity`: `metadata_only` (no content hash was ever compared), `verified` (its files matched their hashes when last checked: `verified_at`, `baseline`) or `damaged`. The human view never says "verified" for `metadata_only`.
- `restorable`: its `backup.json` reads and names its own directory, its `manifest.json` exists, it is not damaged, and its client's `WTF/` was inside the client dir.
- `qualifying`: whether it counts as a checkpoint: a whole, stable capture made on this machine (imported, recovered and best-effort backups never do).
- `busy`: a restore, export or verify held its directory lock when it was listed.
- with `-v`, `addons [{name, folders, version}]`: the addon versions that produced it.

A backup whose `backup.json` cannot be read is still listed, with `null` for what it would say, and is never restorable. Ids match ignoring case. An id prefix of fewer than 8 characters is a usage error, one that matches nothing is `not_found` (`details: {prefix, target, newest}`, naming the newest three), and one that matches several is `ref_ambiguous` (`details: {prefix, candidates}`) with one `backup list <id> -t <client>` per candidate.

Per client, `bytes` is the backups' apparent size. The human view says "apparent, initially shared copy-on-write" when a listed backup's `reflinked` is `all` or `some` (on btrfs and xfs a fresh backup shares its data with the live `WTF/`), and always shows the free space (`free_bytes`). omaddons never claims a backup's exclusive size, which only a walk of every extent could tell. `snapshot_bytes` is the apparent size of undo's `WTF.previous` trees (0 for an unregistered client). `auto` is `{mode, capability, last}`: the `auto_backup` setting, whether a clone-only copy worked there when last tried (`capability`: `unknown`, `supported` or `unsupported`), and the last one (`last: {at, result}`), with `unknown` and `null` until they run. `-t` lists a selected client even with no backups (`total: 0`). `--limit N` shows the newest N per client, and `shown` and `total` are always reported. Listing rebuilds `~/.cache/omaddons/backups.json`, the summary `status --cached` will read, when it no longer matches the directories (and rings the doorbell when that changed it). For each listed client whose `WTF/` holds SavedVariables and that has no qualifying backup captured in the last 7 days, `commands` offers `omaddons backup create -t <id>` (action `backup_create`).

```text
$ omaddons backup list
retail (12.1.5.70391, mainline) · 2 backups · 82.4 kB apparent, initially shared copy-on-write · 212 GB free
  01M6Q2VA00000000…  today 18:02       auto (after play)  412 files  41.2 kB  stable
  01M5Z1KB00000000…  2026-10-18 21:15  manual             412 files  41.2 kB  stable · verified  "before the raid"
```

### `history`

What changed, when, by whom and why, newest first (`log` is an alias).

Settings changes appear too: `config set`, `targets add`, `rm`, `rename` and `default`, `auth github set` and `clear`, and `export --write` each record a line of kind `config`, `target`, `auth` or `export`, with zero folder counts (human output shows what was run instead). `errors dismiss` records one line of kind `dismiss` per client it acknowledged, with `dismissed: {databases, errors}` (the table shows `2 errors in 1 database`). A dry run, or a change to nothing, records none (`auth github set` and `clear` currently record a line every time; see [Known issues](#known-issues)). Their `target` is the client for a `target` or `dismiss` line (`null` for a client not registered yet) and `null` otherwise, so `-t` shows only those lines. `undo` never reverts them and steps over them; `undo <tx>` of one is `not_found`. A line that cannot be written after the change was made warns `history_not_recorded`. Each backup `backup create` makes is a line of kind `backup` on its client, with no folder counts and the backup it made (`backup: {id, kind, files, bytes}` in JSON, `WTF/ as <id> (<files>)` in the table); `undo` steps over it too.

```bash
omaddons history
omaddons history --limit 50 --since 2w
omaddons history --by agent --since 12h
omaddons history --run 01M51F4R2ZQX        # one run's transactions (a prefix of 8+ characters works)
omaddons history -t retail -v --json       # each folder operation (ops) instead of counts
```

`--limit` defaults to 20, and `truncated` says more lines matched. `--since` takes `30m`, `12h`, `1d`, `2w` or an RFC 3339 time. An explicit `--actor` filters too (`OMADDONS_ACTOR` never does); given with a different `--by`, it is a usage error. In JSON every line has its `run` (`null` for transactions from before milestone 2, each of which is a run by itself), and an undo has `reverts`; the human table names a run only on rows whose run holds more than one line.

Each line counts the folders added (`+`), replaced (`~`) and removed (`-`). A transaction that changed records without moving a folder, such as forgetting an addon whose folders were gone, also shows `=N` for the folders it recorded (`changes.recorded` in JSON). `changes.restored` counts the settings files a restore put back (`· N files restored`); it stays 0 until restores ship later in milestone 3, whose lines carry an op `wtf` (`{"op":"wtf","folder":"WTF","addon":"","backup":…,"from":"backup","files":N}`) or `saved_variables`.

### `list`

The addons in each target's AddOns folder (`ls` is an alias).

```bash
omaddons list                              # every target: managed addons with version, ref, channel
omaddons list -t forever --unmanaged       # also unmanaged, shared, linked and invalid folders, with title, version and load verdict
omaddons list -t wotlk --protected         # also Blizzard_* folders
omaddons list -v                           # also each addon's drift (files changed on disk), with paths
omaddons list --deep                       # drift by content: every managed file re-hashed
```

Each addon also carries `latest {version, status}`: the newest release remembered from earlier checks and its status as `outdated` would judge it (`current`, `update_available`, `pinned`, `ahead`, `blocked`, `failed`, `unchecked` or `static`). `list` never asks a source. Its column shows `→ <version>` when a newer release is known (`update_available`, `pinned`; `blocked` adds `(blocked)`), `check failed` after a failed check, and is blank otherwise. With `-v` or `--deep` each addon carries `drift {state, files}` too (as in [`status`](#status); the paths with `-v`); a plain `list` checks no file. Each addon carries `reason` (`explicit` or `dependency`), `required_by` and `orphan` (a dependency nothing requires any more): the table marks a dependency `dep` (`dependency of LittleWigs` with `-v`) and an orphan `orphan`.

Each client carries `counts` per folder class and `addons_dir_missing`, and each addon its `missing_folders` (managed folders gone from disk). Under `--unmanaged`, each folder's load verdict is `compatible`, `out_of_date`, `wrong_flavor` or `no_toc`, and folders whose names differ only in case are flagged `case_duplicate`. `-v` adds each addon's folder names, `folder_tocs` and the `client_dir`.

On a client whose AddOns directory another client shares, a folder only the other client records is class `shared` (counted in `counts.shared`, and under `--unmanaged` with `owned_on [{target, addon}]`, shown as `shared (managed on era)`): it is not this client's to adopt, install over or remove.

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
omaddons outdated --changelog     # each update's release notes since the installed release
```

Each distinct ref and channel is fetched at most once, through the [release memo](#files-and-state). A check from the last 15 minutes is reused without a request, so a second run is free. After that, a GitHub release on the stable channel is first probed: one `releases/latest` request (no GitHub API), and when its tag has not moved the remembered release stands (counted as `probed`); a moved tag costs its `release.json` too. `git:` refs on GitHub revalidate their commits feed, so an unchanged branch comes back as a 304. `--offline` answers from the memo whatever its age, with a `stale_check` warning. Addons installed from a local file, a URL or `--file` have nothing upstream to compare and make no request (`static`).

The listing shows only what needs a decision; `-v` lists everything, and the summary always counts every status:

| Status | Meaning |
|---|---|
| `update_available` | A newer release exists. `confidence` says how likely it is to load: `verified` (a download proved it), `declared` (the publisher's metadata names this client), `likely`, `unlikely` |
| `pinned` | Pinned (by `pin` or `install --version`), and a newer release exists (`latest`) |
| `blocked` | The newest release is known not to load on this client (`blocked_reason: verified_not_loading`), or targets an older interface than the adopted build installed (`older_than_installed`) |
| `ahead` | The installed release is newer than the newest one on its channel |
| `failed` | The source cannot answer for it (a deleted repository: fix `omaddons remove <name> -t <id>`, and if it moved, the alternative `omaddons adopt <folder> --ref <new ref> -t <id>` re-points it; or a fetch error) |
| `current`, `static`, `unchecked` | Shown with `-v` only (`unchecked_reason`: `never_checked`, `offline`, `source_disabled`, `no_eligible_release`) |

When a fetch fails, a remembered answer is used and the warning says so; with nothing remembered, the item is `failed`. Either way the run is `partial`. `--verify` downloads only the updates not yet verified, in a dry run that takes no global lock, and remembers the verdicts: a candidate that does not load becomes `blocked` from then on. `outdated` writes only the release memo, the download cache and `net.log`.

The data also has `checked_at`, `requests {fetched, probed, reused}` and a `summary` counting every status (the human summary line leaves out `current` and `static`). `commands` holds `omaddons update` (or `update -t <id>` when only one client has updates), and `omaddons adopt -t <id>` for a client that manages nothing yet but has adoptable folders.

`--changelog` puts the release notes under each update, and in the JSON as `latest.changelog` of each `update_available` item (no other status gets one):

```text
$ omaddons outdated --changelog
retail (12.1.5, mainline)
  Foo  v1  → v3  gh:o/foo
      v3 (2026-10-28)
        Third: faster
      v2 (2026-10-27)
        - Second
```

`changelog` is `{source, entries [{version, published_at, text}], truncated, more_url}`, newest first: the releases newer than the installed one, at most 10, each as plain text of at most 4,000 characters. One changelog is fetched per ref, from the oldest release installed on any client, and each item keeps the part newer than its own. Where the notes come from:

| Source | Notes | Cost |
|---|---|---|
| GitHub | The release notes in `releases.atom` | One request, usually a 304; never the GitHub API |
| `git:` on GitHub | Commit titles from the branch's commits feed | One request, usually a 304 |
| WoWInterface | The current file's changelog, already in the release | None, offline too |
| Tukui | The latest version's changelog page | One request |
| `git:` elsewhere, `url:`, `file:` | None | None |

GitHub's feed holds the 10 newest releases. When the installed one is older, `truncated` is `true` and the listing ends with `older releases on GitHub: <more_url>`. An addon adopted without an exact release gets the latest release's notes only. With no changelog, `changelog` is `null` and `changelog_unavailable` says why: `not_supported`, `offline`, `fetch_failed` or `no_releases`. A changelog that cannot be fetched never makes the run `partial`.

### `adopt`

Bring folders installed by hand or by another manager under management, exactly as they are: only records change, never a folder. Alone, `adopt` reports where each unmanaged folder comes from and whether it is exactly a release, and changes nothing.

```bash
omaddons adopt                    # every client: identify, then check against the newest releases
omaddons adopt -t retail --no-verify   # identify only: no download
omaddons adopt --refresh          # ask every source (and the WoWInterface index) now
omaddons adopt --apply            # adopt every exact and identified addon (never a guess)
omaddons adopt Bagnon -t retail   # adopt the addon this folder belongs to, a guess included
omaddons adopt Bagnon --ref wowi:25068   # say where it comes from (anything install accepts)
```

Each client's unmanaged folders are grouped into addons. The evidence, strongest first:

1. a [catalog](#the-built-in-catalog) entry that ships the folder for the client's family (the source `install` would use);
2. the TOC's `X-Tukui-ProjectID`, then `X-Website` (a GitHub repository, a WoWInterface page or a Tukui addon; any other site is ignored), then `X-WoWI-ID`;
3. a WoWInterface project listing the folder, when its name or author matches the TOC's;
4. a name alone.

A folder that only requires another (`DBM-Extra` beside `DBM-Core`) joins its group. Each group gets a tier:

| Tier | Meaning |
|---|---|
| `exact` | The folders are, file for file, what the newest release installs here (`version_confidence: release`) |
| `identified` | Confident identification, not (or not yet) exact; `reason` says why: `files_differ`, `folder_set_differs`, `not_verified`, `not_verified_offline` or `source_error` |
| `guess` | A name-only, contested or borrowed match: other candidates are in `alternatives` |
| `cf_only` | Only a CurseForge or Wago id (those sources arrive in M5) |
| `unknown` | No evidence |

A TOC id that disagrees with stronger evidence is listed in `evidence` and `alternatives` (at most 3) and never chosen. Folders the release ships but this client does not load are listed in `unloaded_folders` and do not prevent `exact`. A folder whose addon is already managed (`already_managed`), that holds a symlink, device, FIFO or socket or cannot be read (`unindexable`), or that exceeds the archive limits (`limit_exceeded`), is listed in `failed`. Sources the report could not consult are listed in `sources_skipped`.

Checking downloads each identified group's newest release once, through the download cache (announced first as `verify: {groups, downloads, bytes}`), so a second run downloads nothing. `--offline` checks only what the release memo and the cache already hold. The report writes only the release memo, the WoWInterface index, the download cache and `net.log` (plus an in-flight lock while it downloads). Its `commands` are runnable: `adopt` (`adopt --apply`, scoped to the one client that has something to apply, else to your own `-t`), `adopt_folder` for each uncontested guess, and `adopt_with_ref` for each candidate of a contested one.

**Adopting.** `--apply` adopts every `exact` and `identified` group of the selected clients (`--dry-run` shows what it would adopt). Folder names adopt the groups holding them, from `guess` up (`--apply` with folder names is a usage error). `--ref R` (exactly one folder, on one client: `-t`, else the only client that has it; several clients holding it is `target_ambiguous`) decides where the folder comes from, overriding identification; on a folder already managed it re-points its addon to `R` instead, so the next `update` installs from there. Unless `--no-verify`, a group being adopted is checked as in the report. Each client gets one `adopt` transaction (one run: a bare `undo` reverts them all), with `reload_required: none`, allowed while the client runs. An `exact` group is recorded with its release (`version_confidence: release`); any other with its TOC's version (`toc`), so `outdated` and `update` treat it as an install of that version, and never replace it with a release built for an older interface (`blocked`, `older_than_installed`). Folders the client does not load stay unmanaged. A name another addon on the client already has is recorded as `<name> (<primary folder>)`, with an `adopt_name_changed` warning. Undo forgets the records and leaves every folder as it is.

Each item is `adopted`, `edited` (re-pointed), `unchanged` or `failed`. A folder fails when it:

| Is | Code | Fix |
|---|---|---|
| not in AddOns | `not_found` | `omaddons list -t <id> --unmanaged` |
| identified by nothing | `not_found` | `omaddons adopt --ref <ref> <folder> -t <id>` (fill in the ref), or `omaddons search <folder>` |
| known only to CurseForge or Wago | `source_disabled` | the same |
| a guess with rivals, without `--ref` | `ref_ambiguous` | one `adopt <folder> --ref <r>` per candidate |
| a leftover of a managed addon, or `--ref` names a ref another addon records | `ownership_conflict` (`details.owner`) | remove the leftover yourself, or pass the ref it belongs to |
| `shared`: another client sharing this AddOns directory records it | `ownership_conflict` (`details.reason: owned_on_shared_target`, `owner_target`) | the same command on that client; the alternative removes it there |
| a `Blizzard_*` folder | `protected_folder` | `omaddons list -t <id> --protected` |
| a symlink or not an addon folder | `unmanaged_folder` (`details.class`) | `omaddons list -t <id> --unmanaged` |
| unreadable, or larger than the archive limits | the indexer's error | `omaddons list -t <id> --unmanaged` |

Folders are hashed before the lock and checked again under it: one that changed in between makes its client's transaction `plan_stale` (retryable), and nothing of it is recorded.

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

Each client's `found_by` names the discovery route that reached it first (see [Where discovery looks](#where-discovery-looks)): `product_db`, `launcher:<name>`, `process`, `extra_root`, `scan`, `manual` (added with `targets add`), `registered`, `remembered`, or `registry` for a registered client whose directory is gone.

An id you choose (`--name`, `rename`) must match `^[a-z0-9][a-z0-9._-]*$`, be at most 32 characters, contain no `@`, and not be a key, family name or product code. Built-in default ids such as `vanilla` or `mists` are exempt, which is why they can shadow a family (see [Choosing a target](#choosing-a-target-with--t)).

### `search <QUERY>...`

Find an addon by name, folder, author or description, across the catalog, the WoWInterface index, the Tukui list and GitHub's repository search, at once. `find` is an alias.

```text
$ omaddons search damage meter -t retail
  Recount Damage Meter   wowi:7000                    WoWInterface · 2.1M downloads · updated 2026 · retail: likely
  Details! Damage Meter  wowi:23056                   WoWInterface · 515k downloads · updated 2020 · retail: abandoned
  Skada                  gh:Skada-Damage-Meter/Skada  GitHub · ★412 · updated 2026 · retail: unknown
Install: omaddons install wowi:7000 -t retail        (a hint: install verifies each addon's TOC)
```

| Flag | Meaning |
|---|---|
| `--source S` | Only this source (repeatable): `catalog`, `wowi`, `tukui` or `github` |
| `--limit N` | Most results (default 20, at most 50) |
| `--refresh` | Fetch the WoWInterface index and the Tukui list now, even inside their windows |

Results are ranked: an exact catalog name or alias, or an exact WoWInterface or Tukui project name, first; then names that start with the query; then those with every word in the name or folders; then those with every word anywhere, author and description included. Within a rank, the catalog comes first, then popularity (WoWInterface downloads and GitHub stars, each scaled to its source's best), then the newest. A WoWInterface, Tukui or GitHub hit for a catalog addon merges into the catalog's: the same ref, or the entry's name or one of its aliases (ignoring case, spaces and punctuation) with at least one of its folders. GitHub results are kept only when the repository's name, description or topics mention WoW (`wow` at the start of a word, `warcraft`, `azeroth` or `addon`); `-v` counts the rest.

Each result says, for every selected client, whether it is likely to load. This is a hint; `install` verifies every TOC.

- `yes` or `no`: the catalog's families (`unknown` for an entry that lists none).
- `likely` or `unlikely`: Tukui's patches (`unknown` for a project that lists none).
- `likely` or `unknown`: WoWInterface's declared versions, which lag behind, so never `no`.
- `abandoned`: a WoWInterface project not updated in two years.
- `unknown`: GitHub.

Names, authors and versions from WoWInterface are shown with their HTML entities decoded (`&amp;` is `&`). Text from any source loses its control characters (so it cannot send terminal escapes) and the characters that reorder or hide text, and is cut at 200 characters.

`commands` holds `omaddons install <ref> -t <client>` for up to three results, each on its best client: the strongest hint (`yes`, then `likely`, then `unknown`), and among equals the default target, then a live client before a test one, then the client with the most managed addons, then retail, then by id. Never for `no`, `unlikely` or `abandoned`. A source not consulted is listed in `sources_skipped` (`offline`, `rate_limited` with `reset_at`, `disabled`, `no_index`). A spent GitHub search is not a failure; any other GitHub error lists `github` as `no_index`. Any source that could not be read, other than a spent GitHub search, makes the run `partial`, with a warning naming the failure: its fetch failed, or it is rate limited. When every result is `no`, `unlikely` or `abandoned` on the selected clients, the human output says "No install suggested". A stored WoWInterface index that answers because a refresh failed is no failure: its `stale_check` warning says the refresh failed and why. `--offline` searches the catalog, the stored index and the remembered Tukui list, and lists the other sources `offline`.

### `errors`

Which addon most likely throws Lua errors, how often, since when, and whether an update may fix it, read from [BugGrabber](https://www.wowace.com/projects/bug-grabber)'s SavedVariables on every client, 3.3.5a included. It reads, and writes only its memo in the cache, from which `status --cached` counts each client's new errors without opening BugGrabber's files (see [`status`](#status)).

```text
$ omaddons errors
retail (12.1.5.70391, mainline) · BugGrabber · 4 errors from 4 addons · 2 new
  WeakAuras   57×  1 new  last 2026-10-15 17:30  likely source (custom code)
    [custom code]:4: attempt to perform arithmetic on a nil value
      [custom code]
      … (1 C frame)
      WeakAuras/WeakAuras.lua:4512: in function 'RunTrigger'
  Foo         3×  last 2026-10-13 18:30  likely source  1.2 → 1.3 available (may fix it)
    Foo/core.lua:12: attempt to index a nil value (field 'db')
      Foo/core.lua:12: in function 'Refresh'
era (1.15.8.64907, vanilla) · no Lua errors are captured: BugGrabber is not installed
Update them: omaddons update Foo -t retail
```

| Flag | Meaning |
|---|---|
| `--since D` | Only errors seen since then: `30m`, `12h`, `1d`, `2w`, or an RFC 3339 time |
| `--addon A` | Only one addon's errors, by addon or folder name |
| `--limit N` | Addons shown per client (default 10) |
| `--received` | Also list the errors other players sent through BugSack, apart |
| `--raw-details` | Also show messages, locals and code as written (see Privacy below) |
| `--check` | Exit 6 when a client has new errors, or could not be read completely |
| `-v` | Every frame (up to 40), and each database's file with its account as `account N` |

**What it reads.** Each account's `!BugGrabber.lua`, and the file of every installed addon whose loaded TOC lists `BugGrabberDB` under `## SavedVariables` (packs such as bdUI embed BugGrabber). The walk examines at most 32 account folders and 64 files per client, and reads at most 256 MiB per run and 64 MiB per file. A file over those, one the game is still writing (it ends early), or one that cannot be read is reported with `errors_partial` and counted from what was read; a bound hit makes `coverage` `truncated`. `SavedVariables/` folders that `undo` leaves alone (outside the client folder, or spelled two ways) are skipped and counted. While a client runs, its errors since the last `/reload` are not on disk yet, and the output says so.

**Attribution.** A stack shows where an error surfaced, not its cause, so the output says "likely source", never "caused by", and an available update "may fix it". The first frame of an addon's own code wins over a library it embeds (an addon whose callback another addon's copy of CallbackHandler dispatched is still blamed, not the copy's owner); a blocked or forbidden call names the addon that made it ("blocked call from"); WeakAuras custom code is "likely source (custom code)". Each error carries `attribution.method` (`protected_call`, `first_addon_frame`, `library_frame`, `blizzard_only` or `none`) and `attribution.confidence` (`high`, `medium` or `low`), and each addon a `kind` (`addon`, `library` when only an embedded library was found, `blizzard` or `unknown`). A folder no record manages is listed under its folder name with `managed: false`.

**New errors.** "New" is decided per database (one file of one account), never across them. The first read marks the errors of the latest session (UI load) new; after that, errors from every later session stay new, however many `/reload`s pass. A database whose session restarts below that mark (`/bugsack reset`, a restored `WTF/`) counts everything since as new. A file the game last wrote more than 7 days ago has no new errors. `errors dismiss` (below) moves the mark to the current session.

**Privacy.** By default and with `-v`, an error's text shows only when it is one of Lua's or the client's own messages, whose variable parts are identifiers from source code (`attempt to index a nil value (field 'x')`, `bad argument #1 to 'SetText' (string expected, got nil)`). Any other message, written by an addon, may hold a party member's name, so only its length shows (`custom error message (28 characters; --raw-details shows it)`, `message_kind: custom`, `message_chars`). Code chunks show as `[custom code]`, paths relative to `AddOns/`, and accounts as `account N`. `--raw-details` adds each error's message, stack, locals and chunk contents, bounded and stripped of UI codes and control characters, with player GUIDs and links replaced by `<player>` and this client's account, realm and character names by `<account>`, `<realm>` and `<character>`. It cannot recognize every name, so it says "raw details may contain other players' names and your own code; share with care". Errors other players sent through BugSack (a `source` key) are counted apart (`received`), never new, never behind an update hint; `--received` lists them in groups marked `origin: received`. The sender's name is never kept.

**`--since`.** A unix time (BugGrabber 12.0.11 and later) is the last occurrence. A string time is the first capture or the last occurrence, depending on the version that wrote it, so an error whose string time is before the cutoff may have recurred since: it is counted as `uncertain` and named in the footer ("1 older first-seen error may have recurred since then (string times cannot tell)"), never silently dropped. `no_time` counts errors without a usable time.

**The observation comes first.** Each client's counts, `new`, the memo and `--check` come from every error read; `--since`, `--addon` and `--limit` only shape what is listed (`summary.matched` and `summary.shown` say what they kept), so a narrow read never makes a client look clean. `errors` exits 0 whether or not there are errors. `--check` exits 6 (`check_failed`, the data kept) when a selected client has new local errors among those `--addon` and `--since` keep (an `uncertain` one counts), with `details.reason` `new_errors`, or when it could not read everything, with `details.reason` `incomplete`; it never looks at `--limit`.

`commands` holds `omaddons update <addons> -t <id>` for managed addons with new errors and a newer release in the release memo (no request is made), `omaddons errors dismiss -t <id>` (action `dismiss_errors`) for a client with new errors a dismissal would clear, and `omaddons search buggrabber` (action `install_buggrabber`) for a client without BugGrabber. `--addon` on clients none of which capture errors is `not_found`.

The JSON lists per client `capture` (`buggrabber` or `none`), `coverage` (`complete` or `truncated`), `files` (`read`, `accounts`, `partial`, `skipped`), `databases` (`id`, `account`, `provider`, `shape`, `session`, `state`, `baseline`, `acknowledged`: the session a dismissal covers, or `null`), `summary` (`entries`, `occurrences`, `addons`, `new`, `matched`, `shown`, `uncertain`, `no_time`, `received`), `addons`, each with `count`, `new`, `entries`, `last_seen` and its `errors`, `dismiss` (`precondition`, the token that binds a dismissal to this read, and `databases`, how many it would acknowledge) and `dismissed` (when the newest dismissal in effect was made, or `null`). An error has its `fingerprint` (`e:` and 16 hex digits, the same in both file formats and across reformatting), `database`, `origin`, `message_kind`, `message` (or `null` and `message_chars`), `location`, `attribution`, strict `stack`, `count`, `sessions`, `new`, `last_seen` and `type` (`error`, `event` or `warning`). A `last_seen` is `{at, meaning, tz}`: `meaning` is `last`, `first_or_last` or `unknown`, `tz` is `explicit` (a unix time) or `system_assumed` (a string read in this machine's zone, which may not be the one that wrote it); a combination of times keeps the weakest.

#### `errors dismiss`

Marks the errors read as seen, on every client or those `-t` selects: they stay listed, but stop being new, in `errors`, `--check` and `status`.

```text
$ omaddons errors dismiss -t retail
retail: dismissed 2 new errors in 1 database
```

Each database read whole is acknowledged at its current session, with the time of the dismissal. Errors from a later session are new again, and so is an error whose time shows it happened after the dismissal, which keeps a `/bugsack reset` from hiding what came after it: the acknowledged session is ignored while the database's session is below it. A database read in part, over budget or not at all is never acknowledged: it is listed under `skipped` with its `reason`, and the run is `partial`.

Dismissals live in the state directory (`errors_ack.json`), so emptying the cache keeps them. Each one is a history line of kind `dismiss` with counts only (`dismissed: {databases, errors}`); `undo` steps over it, and it cannot be undone. A file `errors_ack.json` that does not read stops `errors dismiss` with `state_corrupt` (`errors` still reads, as if nothing was dismissed); its fix is advice, since moving the file away brings the dismissed errors back as new.

`--if-match <token>` takes an `errors` read's `dismiss.precondition` (or a `--dry-run`'s `precondition`): the dismissal then acknowledges exactly what that read showed, or nothing (`plan_stale`, `details.changed: ["observed"]`) when a session ended since or a database appeared, went or was read differently, and `targets` or `selection` when the clients differ. An update or a remove between the read and the dismissal does not make it stale. `--dry-run` writes nothing.

The JSON lists per client `target`, `acknowledged` (`database`, `session`), `skipped` (`database`, `reason`: `partial`, `over_budget` or `unreadable`) and `errors`, the new errors that stop being new.

### `info <REF>`

What an addon is: installed records on every target, the catalog entry, and the newest release (`show` is an alias).

```text
$ omaddons info weakauras --offline
WeakAuras  gh:WeakAuras/WeakAuras2
  catalog: folders WeakAuras, WeakAurasArchive, WeakAurasModelPaths, WeakAurasOptions, WeakAurasTemplates · families vanilla, tbc, cata, mists, titan
  latest: not checked (offline)
```

`latest` is `{version, release_id, channel, flavors, targets, compat}`. `compat` says, for each selected client, how likely the release is to load: `verified` (a plan proved it) or `blocked` (a plan proved it does not), `declared` (its release metadata names the client), `unlikely` (the metadata names other clients only, the catalog lists none of the client's families, or no asset of the release is for the client), else `likely`. `targets` lists the clients that are `verified`, `declared` or `likely`, and the human line reads `likely to load on … · not on …`, or "not likely to load on any client here" (`-v` lists each client, and adds `asset_urls` and `record_path`). A hint only: `install` verifies every TOC.

The newest release comes through the [release memo](#files-and-state): a check made in the last 15 minutes is reused without a request, and `--refresh` checks again anyway. With `--offline`, `latest` is the remembered answer whatever its age, with a `stale_check` warning, or `null` (as above) when nothing is remembered. A missing repository is remembered too, so asking again within the window costs nothing; a network or server error never is. Each installed record carries its `drift {state, files}` (as in [`status`](#status), with the paths under `-v`), checked locally, so `--offline` shows it too, and its `reason`, `required_by` and `orphan` on that target.

`--changelog` adds the release notes of every release newer than the oldest one installed (the latest release's alone when no installed record names a release), as `changelog` and `changelog_unavailable` after `offline`, shaped and sourced as in [`outdated --changelog`](#outdated):

```text
$ omaddons info weakauras --changelog
WeakAuras  gh:WeakAuras/WeakAuras2
  …
  changelog:
    5.19.3 (2026-10-28)
      Fixes
      - Faster loading
    5.19.2 (2026-10-27)
      Mists support & more
```

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

### `auth github`

The opt-in GitHub token. The main GitHub path (`releases/latest`, `release.json`, the feeds) needs none; a token raises GitHub's REST budget from 60 to 5,000 requests an hour (search: 10 to 30 a minute) for the beta and alpha channels, repositories without a `release.json`, and `search`.

```bash
omaddons auth github                          # which token is used; online, the hour's REST budget left
echo "$TOKEN" | omaddons auth github set      # store a token: standard input only, never argv
omaddons auth github --from-gh                # store what `gh auth token` prints
omaddons auth github clear                    # forget the stored token
```

`set` writes `~/.config/omaddons/credentials/github` with mode 0600, in a 0700 directory, and refuses a terminal at once rather than wait for input. `--from-gh` is the only time omaddons asks `gh` for anything, and only when you pass it. `clear` empties the file: an empty file is no token. `status` makes one request, to `/rate_limit` (which costs none of the budget), and none with `--offline`. With `--offline` it also runs no credential command, so `configured` then says a credential is configured, not that it works; `set` and `clear` never run one. With `--dry-run`, `set` and `clear` check the token and write nothing ("would store", "would clear"), and `data` describes the token as the write would leave it. `data` is `{configured, source, rate_limit, dry_run}`, `source` one of `env`, `config`, `file` or `none` (see **Tokens** under [Network and privacy](#network-and-privacy) for the order). The token is never shown, logged or sent anywhere but `api.github.com`. A rejected token is `auth_rejected`, whose fix undoes the token's source: `omaddons auth github clear` (alternative: `set` a new one) for the stored file, unsetting `OMADDONS_GITHUB_TOKEN`, or correcting config.toml's `[sources.github] credential`, each with `clear` as an alternative. `--from-gh` goes with `set` (or bare `auth github --from-gh`); with `status` or `clear` it is a usage error.

`omaddons help auth` explains `auth github`; `omaddons auth github --help` and `omaddons help auth github` currently fail (see [Known issues](#known-issues)).

### `net-log`

Every network request and git subprocess omaddons made, with a summary by host and by `via` (`http` or `git`). A request or git run whose line cannot be written fails, so nothing is contacted unlogged. By default it shows the 50 most recent, oldest first (`--limit N`; `--since` filters first). It reads `net.log` and its one rotated predecessor. The summaries count every request that matched, `truncated` says more matched than are shown, and `skipped` counts lines it could not read.

```bash
omaddons net-log
omaddons net-log --limit 200
omaddons net-log --since 1d --json
```

### `help [COMMAND]`

```bash
omaddons help
omaddons help install
omaddons help targets add      # a subcommand by its path; the same as omaddons targets add --help
```

---

## The JSON contract

`--json` (or `--robot`, `--format json`, `OMADDONS_FORMAT=json`) makes every command print **exactly one JSON object on one line on stdout**, on success and on failure, including usage errors and panics. Keys always come in the same order.

### Success

```json
{"ok":true,"schema_version":"1.0","tool_version":"0.1.0","command":"install","exit_code":0,"partial":false,
 "data":{"dry_run":false,"target":{"id":"retail","key":"01M51EX04HHWM46XE9F2B9JXED"},"tx":"01M51EX645RCVFN6Z4ZV5GRWVS","kind":"install",
   "items":[{"request":"./Bar-2.1.0.zip","ref":"file:/home/me/src/Bar-2.1.0.zip","name":"Bar","status":"installed",
     "from_version":null,"to_version":"2.1.0","version_confidence":"toc","channel":"stable",
     "folders":{"added":["Bar"],"replaced":[],"removed":[],"skipped":[],"recorded":[]},"error":null,
     "reason":"explicit","required_by":[],"bridge_dependencies":[],"unresolved":[]}],
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
| `error` | object | `code`, `message`, `fix` (display string), `fix_argv` (argv array, or `null` when the fix is prose or needs a value only you can supply; such a fix shows the command with each missing value as a `<placeholder>`, as in `omaddons targets add <path>`), `alternatives` (`[{display, argv}]`), `retryable`, `details` (always an object) |
| `warnings` | array | `{code, message, fix, fix_argv}`. On a partial run, each failed item is mirrored here. On a failure, it holds what the run found before it failed (a `journal_pending`, a `config_unknown_key`), and stderr carries each as a `warning` event before the `error` event |
| `commands` | array | suggested next steps: `{action, command, argv, destructive}`. The actions: `undo` (`undo <tx>` after a change), `undo_run` (`undo --run <run>` after a change on several clients), `update`, `check_updates`, `adopt`, `adopt_folder` and `adopt_with_ref` (`adopt`'s guesses), `install` (search results, missing or bridge dependencies), `reinstall`, `inspect` (`list -v` for degraded addons), `status` (after `status --cached` with no cache), `remove_orphans` (destructive), `list_unmanaged`, `forget_target` (destructive), `backup_create` (`backup list`, for a client with no recent qualifying backup), `check_errors` (`omaddons errors`, when `status --cached` finds a client's Lua errors stale or unchecked), `dismiss_errors` and `install_buggrabber` (`errors`) |
| `meta` | object | `elapsed_ms`, `offline` (`--offline` or `OMADDONS_OFFLINE=1`, usage errors included) |

`check_failed` (exit 6) is the one failure that also carries `data` and `partial`, so `status --check --json` still returns the full status. On every other failure `commands` is empty, except a panic's, which can offer `undo` (below). Long commands also stream NDJSON `progress` events on stderr with `--progress json` (see [stderr events](#stderr-events)); stdout stays one envelope.

### Exit codes

| Exit | Name | Meaning |
|---|---|---|
| 0 | `ok` | Success, including partial success (`partial: true`, each failed item in `data`) |
| 1 | `failed` | Runtime failure: network, source, archive integrity, I/O, state, internal. `error.retryable` says whether to retry |
| 2 | `usage` | Unknown command or flag, bad argument, bad ref syntax, an unreadable `addons.toml` (`desired_state_invalid`) |
| 3 | `not_found` | Target, ref, addon, backup, run, plan or transaction not found, or ambiguous |
| 4 | `refused` | Safety refusal: running client, protected folder, ownership or case conflict, unsafe archive, wrong flavor, undo conflict, blocked host |
| 5 | `locked` | Another omaddons process held the lock past the 5-second timeout; retryable |
| 6 | `check_failed` | An opt-in assertion failed: `status --check` (drift or attention), `outdated --check` (an update is available), or `--strict` on `install`, `update`, `sync` and `copy` (a partial success). `data` is kept |

A panic still produces one `internal` envelope and exit 1, never Rust's 101. Its `commands` offer `omaddons undo <tx>` for a transaction that committed before it, and its warnings are the ones the run found. A panic after the result was written leaves that result and its exit status, with one stderr `log` line saying so (dropped by `--quiet`); a panic the program recovers from prints nothing.

<details>
<summary><strong>Every error code (40)</strong></summary>

| Code | Exit | Retryable | When |
|---|---|---|---|
| `usage` | 2 | no | bad flag, value, `--since`, `--limit`, `--actor`, `--reason`, config key or id; a missing or relative `HOME` |
| `ref_invalid` | 2 | no | malformed ref: `gh:owner`, `git:` without https, `http://` |
| `desired_state_invalid` | 2 | no | `sync`'s `addons.toml` cannot be read: broken TOML, a bad value, an unknown family, an entry listed twice, or a newer schema (`details: {path, line, key, reason}`); also a failed `sync` item when two entries name the same addon, and `export --write` refusing a file edited since it was written (`reason: "edited"`) or an empty export (`reason: "empty"`), and `sync --prune` refusing a file with a section for no known client (`reason: "unknown_target_section"`) |
| `target_not_found` | 3 | no | no target matches the selector or id; a known game mode whose client is missing; no client found at all (fix `omaddons targets add <path>`), or only test clients (the first as the fix, each other one an alternative, all with `-t`), or, for bare `backup create`, only test clients with a `WTF/`; a `targets add` path holding no client; undo of a transaction whose target is gone |
| `target_ambiguous` | 3 | no | several targets fit; one alternative per candidate |
| `ref_ambiguous` | 3 | no | a name matches several addons or a local path; a folder `adopt` could take from several refs (one `--ref` alternative each); a backup id prefix matching several backups (`details: {prefix, candidates}`, one `backup list <id> -t <client>` each) |
| `not_found` | 3 | no | unknown name (fix `omaddons search <name>`, sending no request), path, tag, asset, transaction, run or backup id (`details: {prefix, target, newest}` for a backup); a transaction or run id prefix matching several (`details.candidates`); a settings change or a backup given to `undo`; a pruned snapshot (`details.reason: "snapshot_pruned"`); nothing to undo; an addon moved to another release between plan and commit; `backup create` of a client with no `WTF/` yet (`details.target`), or, without `-t`, of none |
| `not_managed` | 3 | no | `remove` of an unmanaged, linked or `Blizzard_*` folder, or a name no managed addon answers to. When the name is an unmanaged folder (`update`, `pin`, `unpin`, `remove`), the fix adopts it: `omaddons adopt <folder> [--ref <catalog ref>] -t <id>`, other clients holding it as alternatives |
| `plan_not_found` | 3 | no | reserved for stored plans (M4) |
| `no_compatible_release` | 3 | no | no candidate loads on the client (`details.tried` lists them), or the ref has no release to choose: none published, none on the channel (only prereleases: fix `--channel beta`), no zip asset or `release.json`, or a channel that needs GitHub's release API |
| `game_running` | 4 | no | the running policy refuses (fix appends `--while-running`), or `undo --saved-variables restore` while the client runs; `details.reason` says why (`loaded_toc`, `legacy_client`, `policy_refuse` or `saved_variables`), and `details.running: "unknown"` appears when `/proc` could not be read and the client may be running. A failed item's error in `update`, `sync` or `copy` carries both too. Also a change to a client whose interrupted `WTF/` restore waits for the game to exit (`details.reason: "saved_variables"`, `details.journal_pending`) |
| `protected_folder` | 4 | no | a change would touch a `Blizzard_*` folder: an archive containing one, `adopt` of one, an undo whose history names one, or the filesystem writer's own refusal |
| `ownership_conflict` | 4 | no | another addon owns the folder; fix appends `--force`. A folder another client sharing this AddOns directory records (`details.reason: "owned_on_shared_target"`) is never overridable: the fix is the same command on that client |
| `unmanaged_folder` | 4 | no | an unmanaged folder exists; for an addon new to the client the fix adopts it (`omaddons adopt <folder> --ref <ref> -t <id>`) with the `--force` retry as alternative, otherwise the fix appends `--force`. A symlinked folder's fix is prose (remove or rename the symlink yourself) |
| `case_collision` | 4 | no | a folder name differs only in case from an existing one; AddOns already holds such a pair; `Interface` or `Interface/AddOns` exists in two spellings (`details.paths`) |
| `wrong_flavor` | 4 | no | a `url:`, `file:` or `--file` archive does not load on the client |
| `unsafe_archive` | 4 | no | traversal, absolute paths, symlinks, devices, duplicate or case-colliding entries, the compression-ratio limit |
| `archive_too_large` | 4 | no | over the entry, uncompressed-size or download limit (also a download past it while streaming, a `git:` checkout growing past it, an API response over 64 MiB, a live folder `adopt` indexes, or a `WTF/` a backup copies, past 200,000 entries or 16 levels); `details.limit`, and `details.config_key` when `config.toml` can raise it |
| `undo_conflict` | 4 | no | a later transaction touched the same folders, already undone, or a removed folder put back by hand; a newer history line this omaddons cannot read may have touched the folders or SavedVariables the undo writes back (`details.reason: "history_unreadable"`, `details.lines`) |
| `host_blocked` | 4 | no | an HTTP request or redirect to a host not on the allowlist, or a `git:` ref to a host not in `git_hosts` |
| `scope_denied` | 4 | no | the filesystem writer refused a path outside its scope |
| `state_too_new` | 4 | no | a state file written by a newer omaddons: a change is refused before anything is staged when its client's record or files index, or (to register a client) the registry, is newer; a journal a newer omaddons started (`details.reason: "journal_too_new"`, `details.journal_pending`) |
| `filesystem_unsupported` | 4 | no | a `WTF/` restore (later in milestone 3) cannot run on this client: `WTF/` is a mount point (`details.reason: "wtf_is_mount_point"`), the client's `Interface/.omaddons/` is on another mount (`stage_cross_mount`), or the filesystem cannot exchange two directories atomically (`no_rename_exchange`); `details.backup_dir` is the backup's folder to copy from by hand, with the game closed. Checked before anything is copied |
| `locked` | 5 | yes | lock held past 5 s; `details.holder` |
| `check_failed` | 6 | no | `--check` or `--strict` |
| `network` | 1 | yes (no for too many redirects or an undecodable encoding) | connection failure, also mid-body; more than 5 redirects; a `Content-Encoding` omaddons does not decode; a source response it cannot read; a git run that times out, or fails outside clone and fetch |
| `rate_limited` | 1 | yes | GitHub REST budget exhausted, or any host's rate-limit answer (403/429 with no requests remaining, `Retry-After` over 60 s, a 429 that outlasts the retries); `details.reset_at` |
| `download_failed` | 1 | no (a refused commit currently says yes; see Known issues) | the git binary could not clone or fetch a `git:` ref on another host: it needs a login (private repositories are not supported), the server refused to send the commit (fix: name a branch or tag), or another git failure |
| `http_status` | 1 | 5xx only | unexpected status; `details.status` |
| `hash_mismatch` | 1 | first attempt | downloaded bytes did not match the expected hash |
| `plan_stale` | 1 | yes (no under `--if-match`) | another run changed the target between plan and commit, a folder `adopt` hashed changed before the lock, or a `git:` branch moved between resolve and checkout; under `--if-match`, the run is no longer what its preview showed (`details.changed`, fix the `--dry-run` form; see [Previews and `--if-match`](#previews-and---if-match)) |
| `offline` | 1 | no | `--offline` blocked a request |
| `bad_archive` | 1 | no | malformed, encrypted, or not Stored/Deflate |
| `distribution_disabled` | 1 | no | reserved for CurseForge (M5) |
| `source_disabled` | 1 | no | a source turned off in `config.toml` (the fix turns it back on); CurseForge or Wago refs and links (M5); a `git:` ref on another host when `git` is missing or older than 2.32, or in a command that does not run git |
| `auth_required` | 1 | no | a GitHub token is configured or stored but cannot be used: an unset `{ env = … }` variable, an unreadable `{ file = … }`, an empty or malformed token, or `auth github --from-gh` without a working `gh` |
| `auth_rejected` | 1 | no | GitHub answered 401 to the token (the fix clears or replaces it where it came from), or a `{ command = … }` credential failed |
| `state_corrupt` | 1 | no | an unreadable state or config file (with line and column); a SavedVariables snapshot copy that no longer matches its manifest, refused before an undo moves anything (`details.reason: "snapshot_mismatch"`); a journal holding lines this omaddons cannot read, left for manual repair (`details.reason: "journal_unreadable"`, `details.journal_pending`); the recovery of a `WTF/` restore that could not finish preserving what it displaced (an I/O error, a full disk, or an identity it did not expect inside the protocol), keeping everything for the next one to resume with the same backup id (`details.reason: "preservation_incomplete"`, `details.journal_pending`) |
| `io` | 1 | no (yes for a `WTF/` that changed while a backup copied it) | filesystem error; `details.path` (below `WTF/`, without account, realm or character names) |
| `internal` | 1 | no | a bug or panic; fix is `omaddons status` (`omaddons status --cached` when `status` itself failed, and a report when that failed too). Also the recovery of a `WTF/` restore that found something it did not record at a step's paths, left for manual repair (`details.reason: "unexpected_state"`, `details.journal_pending`) |

</details>

<details>
<summary><strong>Every warning code (46)</strong></summary>

| Code | Meaning |
|---|---|
| `journal_recovered` | an interrupted transaction was finished or rolled back |
| `journal_pending` | an interrupted transaction waits for the next change command; or, saying `journal_too_new` or `journal_unreadable`, one this omaddons cannot recover, left as it is (the fix says what to do); or, saying `saved_variables`, `preservation_incomplete` or `unexpected_state`, another client's `WTF/` restore that waits for its game to exit or whose recovery stopped |
| `tx_in_progress` | another omaddons process is committing |
| `atomic_swap_unavailable` | the AddOns filesystem has no `renameat2` exchange; two journaled renames were used |
| `reload_required` | `/reload` in game to pick up the change |
| `saved_variables_deferred` | the game is running, or recovery could not tell or could not write them; restore SavedVariables later with the given command |
| `saved_variables_out_of_scope` | SavedVariables of the changed addons are in a `WTF/` reached through a symlink outside the client directory (a sync folder), or under a `WTF`, `Account` or `SavedVariables` folder spelled two ways (`WTF` beside `wtf`): they are never snapshotted or written, so undo leaves them as they are. One warning per reason; it counts the files and never names them |
| `running_unknown` | `/proc` could not be read, so every client is treated as running: changes follow the running policy, SavedVariables restores wait, and `status` reports `running: null`. Once per command |
| `snapshot_cap_exceeded` | protected snapshots alone exceed the size cap |
| `retention_failed` | pruning failed after commit; the transaction stands |
| `out_of_date` | the TOC's interface is older than the client's; it loads only with "Load out of date AddOns" enabled |
| `missing_dependency` | a folder an installed addon's TOC requires was not installed: no keyless source ships it, several projects do, its only build is out of date, a dependency limit was reached, or `--no-deps`; the fix names a candidate when there is one, and searches for the folder (`omaddons search <folder>`) when no keyless source ships it |
| `toc_comma_on_legacy` | a comma interface list on a client that reads only the first number |
| `client_not_launched_since_patch` | `Config.wtf` predates the client's current version |
| `widget_outdated` | reserved for the widget (M3) |
| `state_version_older` | state migrates forward on the next write |
| `state_version_newer` | writes refused until omaddons is upgraded |
| `config_unknown_key` | unknown key in `config.toml`, with the closest valid key |
| `unknown_flavor` | an unrecognized flavor folder, or a custom client whose version maps to no family, became a target, and no `[targets.<id>] family` override sets its family |
| `discovery_truncated` | discovery hit its directory budget; add the client with `targets add` |
| `target_key_duplicate` | two clients carry the same marker (a copied install) |
| `catalog_override_invalid` | a `catalog.d` file was skipped |
| `ownership_transferred` | `--force` moved a folder from one addon to another |
| `credential_file_mode` | a credential file (config.toml's `{ file = … }` credential, or the token `auth github set` stores) is readable by other users; the fix is `chmod 600 <file>` |
| `addon_pinned` | a pinned addon was left at its release; the fix shows it with `info`, and `--version` moves it |
| `stale_check` | an answer came from stored data, not from the source now: offline from the release memo, or from the stored WoWInterface index (offline, or after a failed refresh); it names how old. When `--offline` alone made the run offline, its `fix_argv` reruns the command online |
| `desired_unknown_key` | `addons.toml` has a key omaddons does not know (named, with its line); it is ignored |
| `desired_unknown_target` | an `addons.toml` `[target.<id>]` section names no known client; its entries are skipped |
| `adopt_name_changed` | an adopted addon's name was taken on its client; it is recorded as `<name> (<primary folder>)` |
| `export_missing_target` | `export`: a registered client whose directory is gone was exported from its records |
| `export_local_source` | `export`: an addon installed from a local file or folder was left out |
| `not_locked` | `sync --locked`: entries without a version installed their newest release (one warning per client, naming them) |
| `local_changes_replaced` | an install or update replaced folders whose files had changed on disk (it names up to 10 and counts them all, and says how long retention keeps the snapshot); the fix is `omaddons undo <tx>`, or under `--dry-run` a reminder that the snapshot keeps them |
| `snapshot_only_copy` | a transaction moved folders that exist nowhere else into its snapshot (an unmanaged folder replaced under `--force`, an addon adopted as it was or installed from a local archive or directory, removed or replaced); undo can restore them only until retention prunes it, and the message states the configured bounds. It names up to 10 folders; `-v` says where the snapshot keeps each. The fix is `omaddons undo <tx>`, or under `--dry-run` a reminder |
| `still_required` | an addon removed (by `remove` or `undo`) is required by addons that stay, which will not load without it; the fix is `omaddons undo <tx>`, or under `--dry-run` a reminder that undo restores it |
| `orphaned_dependency` | a remove, update or undo left dependencies required by nothing; the fix is one `omaddons remove <names…> -t <id>` naming them all |
| `bridge_dependency` | a newly installed addon's other folder (a bridge or plugin) works with an addon that is not installed; nothing is installed for it, and the fix installs it (`omaddons install <name or ref> -t <id>`), or searches for it (`omaddons search <folder>`) when no keyless source ships it. Only on the addon's first install |
| `target_addons_shared` | `status`: two or more clients share one AddOns directory (an `Interface/AddOns` symlinked between them); a folder one client records is `shared` on the others, which may not record it too; the fix is advice: manage those addons from one client, or stop sharing the directory. Also on `install`, `update` and `remove` of a folder both clients recorded before that was refused: the change shows on the other client too (fix `omaddons status -t <other>`) |
| `history_lines_skipped` | `history` and `undo`: lines of `history.jsonl` could not be read (a hand edit or other damage, or a line a newer omaddons wrote; not a write a crash cut short) and were skipped, so the transactions on them are not listed and cannot be undone (undo assumes a newer one touched what it may have; see [`undo`](#undo-tx)); it counts them, and the fix is advice, since omaddons never rewrites the file |
| `download_unverified` | a WoWInterface file whose details published no usable MD5, or a Tukui download without a usable `ETag` MD5 (none, weak, unquoted or not an MD5), installed unchecked: TLS protected the transfer and the TOCs were verified. A later reuse of its cached archive (another client, a reinstall) says it again; no fix |
| `history_not_recorded` | a settings change (`config set`, a `targets` change, `auth github set` or `clear`, `export --write`, `errors dismiss`) was made, or a backup, but its history line could not be written; the fix is advice to check that the state directory is writable |
| `backup_copied` | `backup create`: the filesystem could not clone some or all of a backup's files, so they were byte-copied and take their full size; it says how many bytes. No fix |
| `backup_unsettled` | `backup create`: `WTF/` changed while it was copied, and again on a second try, so the backup is `best_effort`; it counts the files still changing and those changed, added and removed after their copy. The fix is advice: make another backup after the game exits |
| `backup_space_low` | `backup create`: less than `limits.backup_min_free_mb` is free beside the client's backups; the backup is made anyway. The fix is advice: free space, or lower the setting |
| `errors_partial` | `errors`: a BugGrabber file could not be read completely (the game was writing it, it is over a size cap, or it could not be read); counts are from what was read. Its fix is `omaddons errors -t <id>`, after the game exits |
| `restore_rolled_back` | an interrupted `WTF/` restore was rolled back by the next change, and what it had displaced, which no other copy held (a tree the game played on, the last copy of a deleted backup, the tree from before it), is kept as a `recovered` backup; the message names only the backup and the client, and the fix restores it (`omaddons backup restore <id> -t <target>`) |

</details>

### Previews and `--if-match`

A confirm dialog, or an agent that previews before it acts, must not run something other than what it showed. A `--dry-run` of a destructive command returns `data.precondition`, a token describing exactly what the run would do, and the real run with `--if-match <token>` does that or nothing. `remove`, `undo` and `errors dismiss` bind today; `backup restore` and `backup prune` will. An `errors` read carries its own token for each client (`dismiss.precondition`), so a dismissal can be bound to the read the user saw, not to a later preview.

```bash
omaddons remove Foo -t retail --dry-run --json     # data.precondition: "pc1:…"
omaddons remove Foo -t retail --if-match pc1:…     # exactly that removal, or plan_stale
```

- The real run plans again and computes the token from its own plan before it stages anything, and compares it again under the lock (a commit that slips in between counts too).
- A difference is `plan_stale`, not retryable (the same token can only fail again), and nothing is changed. `details.changed` names the parts that differ: `command`, `targets` (a target's records changed, with their generation), `selection` (what it would change) and `observed` (what it saw, such as the SavedVariables an undo would restore, or whether a transaction can be undone). `fix_argv` is the same command with `--dry-run` in place of `--if-match <token>`: preview again.
- `remove`'s token covers the folders it moves out and the records it changes. `undo`'s covers the transactions it reverts, their folder operations, the SavedVariables files each restores and those files' state now.
- The token is `pc1:` and 64 hex digits. Nothing is stored: it holds as long as what it describes is unchanged, and it is no credential.
- `--if-match` is opt-in: commands without it behave as before. On a command that does not bind it is a usage error, and so is a malformed token. A `--dry-run` that is given one previews afresh and returns the current token.
- Real runs carry `precondition: null`.

### stderr events

When stderr is not a terminal, or `CI` is set, or `TERM=dumb`, stderr carries NDJSON events. Otherwise it carries human text.

```json
{"event":"error","code":"source_disabled","message":"cf:61284: CurseForge arrives in M5","fix":"omaddons help install","fix_argv":["omaddons","help","install"],"retryable":false,"exit_code":1}
{"event":"warning","code":"reload_required","message":"…","fix":"/reload in game","fix_argv":null}
{"event":"log","level":"debug","message":"discovery: scanned yes, roots reused 2, parsed 0, cache written no"}
```

Exactly one `error` event is emitted per failure, even with `--quiet`. `debug` log events need `-v`. `progress` events come from long commands (`install`, `update`, `outdated`, `adopt`, `sync`, `copy`, `search` and `backup create`, and the stage and commit steps of `remove`, `pin` and `unpin`) when progress is on: `{"event":"progress","phase":"download","message":"gh:o/foo@v1 on retail","current":1,"total":2}`. The phases are `fetch`, `download`, `verify`, `stage`, `commit` and `copy`. `backup create` counts files under a fixed label, never a path: `copy` as `WTF/ files`, and `verify` as `verifying` when `--verify` hashes them. Within a phase, `current` only grows and never passes `total`; `total` grows when a dependency round or a new fetch adds items. Each item appears once, after an optional announcement at `current: 0` (`adopt` announces its downloads that way). On a terminal in human mode, progress is one line that is rewritten in place.

### Stability

The envelope is `schema_version` 1.0. Until the first tag (milestone 4) the contract can still change. From that tag on, it is frozen. The contract suite (`tests/contract/`) runs a matrix of success and failure cases across the commands. It checks key order, `ok`/`exit_code` agreement and registered codes. Every `fix_argv`, `alternatives[].argv` and `commands[].argv` must parse as an omaddons command line and round-trip through shell quoting to its display string. A prose fix may not name a runnable omaddons command: a scan of the source and the matrix both fail one that does. A corpus of mistyped commands, and every command name with one letter dropped or two neighbours swapped, check that a fix never names a different command that changes something. The suite also checks that every failure emits exactly one stderr `error` event, that suggestions carry no presentation flags, that read-only commands accept and ignore `--dry-run` and `--reason`, that repeated runs print identical bytes, and that `status`'s recommendations run on a fresh home. Snapshots pin representative envelopes.

---

## Safety model

### Transactions

```text
 plan (no lock)              stage (no lock)                  commit window (global flock, milliseconds)
 ──────────────────────      ─────────────────────────────    ─────────────────────────────────────────────
 fetch each ref once         extract to                       recover leftover journals
 download → blob cache       <client>/Interface/.omaddons/    register a new target
 inspect the archive           stage/<tx>/<Folder>/           re-check the target's generation:
 judge every TOC against     verify each file's size            changed and conflicting → plan_stale
   the client's rules          and SHA-256                      already done by another run → no-op
 ownership + running-        one syncfs per archive           re-check the running state
   policy checks             wait (≤ 20 ms) for timestamps    journal: prepared
                               to settle                      create Interface/AddOns if never launched
                             digest folders the files         SavedVariables: keep each copy whose file's
                               index does not cover             stat is unchanged, copy the rest
                             copy touched SavedVariables      snapshot state files
                               to .omaddons/snapshots/<tx>/
                               WTF/, hashing each once
                                                              swap each folder (journal step per rename):
                                                                new      → RENAME_NOREPLACE
                                                                replaced → RENAME_EXCHANGE
                                                                old/removed → .omaddons/snapshots/<tx>/AddOns/
                                                              journal: swapped
                                                              write record, files index, history line
                                                              journal: committed → release the lock
                                                              then: status.json, cleanup, doorbell, retention
```

- **Interrupted runs.** The next command that changes something recovers under the global lock first, every client's journals whichever client the command changes. That is install, update, sync, pin, unpin, `adopt` when it adopts (`--apply`, folder names or `--ref`), copy, remove and undo (also when they turn out to change nothing), `config set`, `targets add`, `rm`, `rename` and `default`, `auth github set` and `clear`, `export --write`, and `backup create` (before its history line). Dry runs and read commands, the `adopt` report included, never recover. A journal is written back only into its own client, once that client's identity marker names the journal's target. A journal that reached `swapped` rolls forward, and anything earlier rolls back. A rollback also removes the transaction's snapshots, except one that still holds an addon folder, which it keeps and names in the `journal_recovered` warning. Each step records the inode (and, where the filesystem keeps one, the birth time) of both paths, so rollback reverses a rename only if it actually happened, also after a reboot that renumbered the device (btrfs subvolumes, FUSE and device-mapper volumes get new device numbers on every mount). An interrupted undo whose SavedVariables restore had not run yet restores them during recovery only when the recovering command found that client and it is not running; `config set`, `targets`, `auth github` and `export --write` never restore them. Otherwise, or when the restore fails, recovery leaves them as they are and warns `saved_variables_deferred` with the command to run after quitting. Read commands never recover anything: they report `journal_pending` (or `tx_in_progress` while another process holds the lock), and `status` lists it under attention. A command's journal warnings describe the state when it finishes: a journal it recovered appears only as `journal_recovered`, never also as `journal_pending`.
- **No atomic rename.** On filesystems without `renameat2` exchange (NTFS through ntfs-3g, some FUSE mounts), the first flagged rename the filesystem refuses switches the rest of the transaction to plain journaled renames (two per replaced folder, one per new or removed folder), and omaddons reports `atomic_swap: false` with an `atomic_swap_unavailable` warning. A cross-device rename is an error, never a fallback, and the staging area must be on the same filesystem as AddOns.
- **Concurrency.** Planning and staging take no lock, so a terminal and an agent (and later the widget) can work at once. The global lock (`~/.local/state/omaddons/lock`, an flock that is never renamed or deleted) is held for the commit window, briefly for retention's prunes, and for `config set`, `targets` changes, `auth github` and `export --write`, which each recover a journal left by a crash first. Backups copy without the lock: `backup create` takes it only to register a client and to append each backup's history line, recovering first too. A change command that changes nothing takes it only while a journal waits for that recovery, and leaves the journal for later if the lock is busy. SavedVariables are copied and hashed before it is taken, and under it each file is only checked with `stat` (one changed in between is copied again there). An undo also makes and checks the copies it restores before the lock, and puts each back with one `RENAME_EXCHANGE` (a plain rename when the file is gone or the filesystem cannot exchange). Folder digests come from the files index, or are taken before the lock for a folder it has no entry for. The checkpoint budget is under 50 ms per commit and undo, also with 16 MiB of SavedVariables and a 64 MiB folder (`tests/perf.rs`, release build); the M1 and M2 checkpoints measured at most 2 ms. The gate measures the same commits, SavedVariables copied byte by byte, and prints them, failing only past a 2-second sanity ceiling. A second run waits up to 5 seconds, then fails with `locked` (exit 5, retryable, `details.holder`).
- **Whole-`WTF/` restores** (the engine behind `backup restore`, which arrives later in milestone 3). A restore copies the backup's `WTF/` to `.omaddons/stage/<tx>/WTF/` without the lock, with the modes its manifest recorded (a `0500` directory included), and checks the copy against the manifest: entries and sizes, and content too when the backup holds hashes (`hash_mismatch` names a damaged backup, nothing changed). Under the lock it exchanges that copy with the live `WTF/` in one `RENAME_EXCHANGE` (a `RENAME_NOREPLACE` into place when the client has none) and moves the old tree to `.omaddons/snapshots/<tx>/WTF.previous`, each a journaled step, so `WTF/` is wholly the old tree or wholly the backup at every instant. There is no fallback to two renames, which would leave a moment with no `WTF/` in which a game launched writes a fresh default one: a restore needs `WTF/` to be a plain directory on the client's own mount, its stage on that mount too, and a filesystem that can exchange (btrfs, ext4, xfs, tmpfs; probed in the stage before anything is copied). Otherwise it is refused with `filesystem_unsupported`, naming the backup folder to copy from by hand. Before the exchange the record generation, the mounts and the running state are checked again under the lock. The digests `actions.jsonl` logs for these renames are `stamp:sha256:` (each entry's path, kind, size, mode and time), so `WTF/` is never content-hashed under the lock.
- **Recovering a restore.** A restore's journal follows its phase like any other: rolled back before `swapped`, forward from it; the renames having run never decide that. It moves `WTF/` only while its client is not running: with the game running (or possibly running, or the client not found by the recovering command), a journal with a step recorded waits, and that client's changes fail with `game_running` naming it (`details.journal_pending`); a commit window that fails checks again before recovering in process, rather than trusting the check made a moment before. A rollback that a crash interrupted is safe to repeat: a step already undone is skipped, also when a later undo has since put another tree at its path.
- **Preservation after a crash.** Between a crash and its recovery the game may play on the restored tree, a sync tool may replace `WTF/`, or the backup may be deleted, so recovery never deletes a version of your settings before it has a durable home. A step whose paths no longer hold what it recorded proves only that something changed them, never who: its live path is left exactly as found. When what the step moved out of `WTF/` is still where it recorded it (the tree from before the restore, with another `WTF/` live now), that tree is kept; when our restored copy alone was replaced, nothing of ours is displaced and recovery goes on. Then every version that differs from every surviving copy (that tree, a restored tree the game played on, the last copy of a backup deleted since) becomes one `recovered` backup (`metadata_only`, `best_effort`), through a journaled intent: a backup id reserved in the journal, the trees moved into an assembly under the transaction's stage, described, and published into `backups/` with one rename, so no listing ever shows a half-made one. Only then does recovery clean up, warning `restore_rolled_back` with the backup's id. A recovery that dies anywhere in that protocol, or meets an I/O error, a full disk or an identity it did not expect, stops with everything in place (`preservation_incomplete`), and the next one resumes with the same id. What recovery cannot place at all stops it the same way (`unexpected_state`), keeping the journal, the stage, both snapshot halves and the in-flight lock; every later change on that client then fails until it is resolved, see [`journal_pending` that does not clear](#journal_pending-that-does-not-clear).
- **A failed recovery.** If a commit fails and recovering it in the same process fails too (the disk turned read-only, say), the journal and the staged folders are kept. Those staged folders can be the only copy of the old version. The command fails with its own error, naming the transaction in `error.details.journal_pending` and the recovery's failure in `error.details.recovery_error` (`{code, message}`). The next command that changes something (see Interrupted runs) finishes it.
- **Crash testing.** All 33 engine failpoints are armed as a real `abort` in a child process for first installs, installs, updates, removes and undos (restoring, keeping, and with the client running, deferring SavedVariables), for the M2 transaction shapes, for the records-only pin and forget, each scene with SavedVariables, and for whole-`WTF/` restores and their undos (over a played-on tree with a `0500` directory, and onto a client with none). Restores and their undos are also crashed before each of their renames and during their recovery, which then runs three times; they fail on an I/O error injected at each of their writes in turn (the staged copy, the journal, the renames, the record); the game starts while one waits to be recovered, while a restore stages and before one recovers in process; and their recovery preserves what a play session, a sync tool or a deleted backup left displaced. `WTF/` must stay wholly old or wholly new, recovery must follow the journal's phase, the replaced tree must be kept, and every displaced version must end up in exactly one `recovered` backup, also when that recovery is itself crashed or fails at each step of the preservation protocol. Every journaled rename of a multi-folder swap is crashed too, with atomic renames and in the two-rename fallback. The state afterwards must be wholly old or wholly new, SavedVariables included, and at least ten failpoints must actually crash a scene. A failpoint off a scene's path has to let it succeed. After recovery no snapshot dir is left of a transaction that never committed, except the SavedVariables copies a crash before `prepared` leaves, which the orphan sweep must remove once they are an hour old. Recovery is crashed too: after each step a rollback undoes, as a roll-forward starts, and before an undo's SavedVariables restore. The command after it must finish the job. A journal, `history.jsonl` or `actions.jsonl` whose last line a crash cut in half must not stop recovery either. An undo crashed after its swap is also recovered while the client runs, and its SavedVariables must never be written then. A commit whose recovery fails too is finished by the next command, local edits and a replaced unmanaged folder included.
- **What this omaddons cannot read, it never acts on.** Journals carry their format's version (1.1 since milestone 3). One started by a newer major version (`journal_too_new`), or holding a complete line this omaddons cannot decode (`journal_unreadable`: an unknown line or step, a `prepared` line that does not decode, or a damaged line before the last), is never taken for "nothing ran": recovery leaves it as it is, with its in-flight lock, stage and both snapshot halves, its own client's changes fail with its `journal_pending` details, and other clients go on. A newer minor version and unknown fields are recovered as usual, and a torn last line is still a crash mid-append, ignored (a journal that is nothing but a torn `prepared` line is pruned: nothing ran). Before anything is staged, a change is refused with `state_too_new` when its client's record or files index, or (to register a client) the registry, was written by a newer omaddons, and checked again under the lock; the save that once caught it only after the swap stays as a backstop. History lines it cannot read stay skipped for `history`, and undo assumes a newer one touched whatever it may have (see [`undo`](#undo-tx)).
- **Records-only changes.** `adopt`, `pin` (without `--version`, which installs that release) and `unpin`, removing an addon whose folders are all gone, and a dependency's `required_by` bookkeeping are transactions that change records only: no folder is touched, they commit while the client runs with no reload needed, and each is undoable like any other.
- **Your local changes.** Files you edited in a managed addon are replaced by an update or a reinstall, with a `local_changes_replaced` warning naming them; the transaction snapshot keeps them, so `omaddons undo` brings them back.
- **Your `addons.toml`.** `export --write` keeps the previous file as `addons.toml.bak`, refuses to replace a file edited by hand since it last wrote it (without `--overwrite`), and never writes an empty export.
- **Backups.** A backup says what it proves. `stable` means `WTF/` was observed not to change while it was copied (each file stamped before and after its copy, the tree walked again afterwards), not that a filesystem snapshot was taken; a copy that kept changing is kept as `best_effort` and never counts as a checkpoint. `metadata_only` means no content was ever hashed, so a file damaged at the same size goes unnoticed; `verified` means every file was hashed (`--verify`). A backup is staged under `.omaddons/stage/` and renamed into `backups/` only once described, so a crash leaves no half backup, only a stage the orphan sweep removes. Backups are owner-only (`0700`), and their manifests, which name the account, realm and character, never leave the machine.

### Undo

- An undo is its own transaction (kind `undo`, `reverts: <tx>`), so it can be undone too.
- Every history line carries the run (command invocation) it belongs to. Bare `undo` reverts a whole run, one undo transaction per client in its own lock window. Runs are attribution, not atomicity: each client's transaction commits and undoes on its own.
- Folders come back as they were from `<client>/Interface/.omaddons/snapshots/<tx>/AddOns/`: every file's bytes and mode, directory modes, symlinks and empty directories (restored files get new timestamps, and a directory always keeps owner access); tests compare all of these. Each addon's record and files index are restored, while other addons' later changes stand.
- **SavedVariables.** Only the touched addons' own files (`WTF/…/SavedVariables/<Folder>.lua` and `.lua.bak`, account-wide and per character) are snapshotted, with a SHA-256 manifest. On undo, a file that is unchanged since is left alone, a changed one is restored, and one the new version created is moved aside. A snapshot copy that no longer matches its manifest fails the undo with `state_corrupt` before anything moves; `omaddons undo <tx> --saved-variables keep`, its fix, reverts the folders without it. With `--saved-variables auto` and the game running, folders revert (if the running policy allows), and the SavedVariables restore is deferred. You get a `saved_variables_deferred` warning carrying the command to run after quitting: `omaddons undo <tx> --saved-variables restore`. SavedVariables directories that lead outside the client directory through a symlink (a `WTF/` kept in a sync folder) are never snapshotted or written; `saved_variables_out_of_scope` counts the files undo cannot restore. Folder names under `WTF/` are matched ignoring case, as Wine does (`wtf/account/…/savedvariables/` works); a folder spelled two ways (`WTF` and `wtf`) is left alone and counted in `saved_variables_out_of_scope`, because the game reads only one of them.
- **Conflicts.** `undo_conflict` (exit 4) when a later transaction touched the same folders (undo the newest first), when the transaction was already undone, or when a folder it removed has been put back by hand. A snapshot pruned by retention gives `not_found`; for folders with no other copy, the commit said so with `snapshot_only_copy`.
- Registry changes and `target.json` are not reverted.

### Ownership and protected folders

| Folder | What happens |
|---|---|
| `Blizzard_*` (any case) | Never written, even with `--force`. Two independent layers refuse it: the planner and the filesystem writer's scope table |
| A symlink (your development checkout) | Never touched, even with `--force` (class `linked`) |
| A name that differs only in case from an existing folder | `case_collision`, even with `--force`. Wine folds case, so the game would see only one. An addon's own folder may change case on update |
| Two `Interface` or two `AddOns` folders that differ only in case (`AddOns` and `addons`) | `case_collision` (`details.paths`). An existing lowercase `interface/addons` alone is used as the AddOns folder |
| A folder two addons of the same transaction both claim | `ownership_conflict`, even with `--force`. A folder handed from one addon to another in the same transaction degrades neither |
| Another managed addon's folder | `ownership_conflict`; `--force` transfers it and marks the old owner degraded |
| A folder another client's records own, in an AddOns directory the two share (class `shared`) | `ownership_conflict` naming that client (`details.owner_target`), even with `--force`; the fix runs the same command on that client. `adopt` skips such folders and refuses them by name |
| An unmanaged folder | `unmanaged_folder`, whose fix adopts it under the ref being installed (`omaddons adopt <folder> --ref <ref> -t <id>`); `--force`, the alternative, replaces it, and the snapshot keeps the original for undo until retention prunes it (`snapshot_only_copy` says so). An update of an addon already managed has only the `--force` fix |

Ownership comes only from omaddons' own records. Other managers' metadata is not read; [`adopt`](#adopt) identifies their folders and records them as they are.

### Hostile archives

Refused as `unsafe_archive` before anything reaches the client: path traversal (`..`), absolute paths, drive letters, control characters, symlinks, devices, FIFOs and sockets, duplicate entries (also caught through the central directory's count), entries that collide when case is ignored, names that are both a file and a directory, entries that expand past their declared size, and a compression ratio over 200:1 once past 32 MiB (checked per entry and for the whole archive). Archives with more than 20,000 entries or more than 1 GiB uncompressed, and downloads over 512 MiB, are refused as `archive_too_large`. Encrypted entries and compression other than Stored or Deflate are `bad_archive`. A local folder install (and a `git:` checkout) follows the same rules, also refusing a name that is not UTF-8, and its `.git` is skipped. Staged files are written with fixed modes (0644 and 0755).

### Running clients

Running clients are detected live from `/proc` (your own processes only). The executable name is matched case-insensitively, with or without `.exe` (the kernel's `comm` keeps only 15 bytes), and the Wine path is mapped through the prefix's drive letters. A process matched by its `WINEPREFIX` alone marks every client with that executable name in that prefix as running. If omaddons cannot read `/proc` (it cannot be listed, or does not list omaddons itself: not mounted, or another PID namespace), it treats every client as running and says so with `running_unknown`; `status` then reports `running: null`. Detection sees only processes in omaddons' own PID namespace, so a sandboxed omaddons with its own `/proc` cannot see a game running outside it.

| `running_policy` | While the client runs |
|---|---|
| `safe` (default) | Adds and replacements that keep the loaded TOC go through, with `reload_required: "reload"` (`/reload` in game). Removing a folder the client does not load goes through with `reload_required: "none"`. Removing a folder the client loads, or an update that deletes or renames the TOC it loads, is refused with `game_running` until it exits. On legacy and unrecognized custom clients, every change is refused, except on 3.3.5a, which re-reads a loaded addon's TOC on `/reload`: there a replacement that keeps the loaded TOC goes through with `reload_required: "reload"`, and a new or removed folder waits for a restart |
| `refuse` | Every change is refused |
| `allow` | Nothing is refused; the result says whether `/reload` or a restart is needed |

`--while-running` lifts these folder rules for one command, even under `refuse`. **SavedVariables are never written while the client runs**, under any policy or flag, because the game rewrites them on logout.

A whole `WTF/` restore is refused while its client runs or may run, whatever the policy or flag, and the running state is checked again under the lock as the last step before the exchange. That check refuses; it cannot exclude. The game takes none of omaddons' locks, so it can still launch in the milliseconds between that check and the exchange. It then reads one consistent tree (the exchange is atomic), and may write its settings in memory over the restored files when it logs out. The restore's `WTF.previous` keeps the pre-restore tree either way.

### Retention

Retention runs after every commit, for the target just changed:

- Snapshots younger than 24 hours and the target's most recent transaction are always kept (protected).
- A snapshot that a deferred SavedVariables restore still needs (`saved_variables_deferred`) is protected until that restore runs, the undo is undone, or a later transaction changes those addons.
- Beyond those, a snapshot is kept while it is within 7 days and fewer than 10 newer snapshots are kept. Protected snapshots count toward the 10 but are never pruned for it, so 12 snapshots from the last day all stay. Everything else is pruned.
- If the kept set exceeds 2 GiB for the target (the client-side and state-side halves of each snapshot together), its oldest unprotected snapshots are pruned until it fits; when protected snapshots alone exceed it, `snapshot_cap_exceeded` says so.
- A failed prune is only a `retention_failed` warning: the transaction stands.
- History lines are never pruned.
- Undo of a pruned transaction fails `not_found`. For folders with no other copy (an unmanaged folder replaced under `--force`, an addon adopted as it was or installed from a local archive or directory), the commit says so with `snapshot_only_copy`, stating these bounds as configured.
- The download cache is an LRU capped at 1 GiB. Archives of releases installed on any target are pinned, so the cache can exceed the cap.
- Orphaned stage directories, in-flight locks and the snapshots of transactions that never committed are swept: a rolled-back or failed transaction's snapshots at once, a dead planner's temporary downloads as soon as its in-flight lock is free, and anything else a crash left once it is an hour old. A snapshot that still holds an addon folder or a `WTF.previous` is never swept.
- A whole restore's `WTF.previous` (the tree it replaced) is pruned with its transaction by the count and age rules above, and never counts toward the 2 GiB cap, so a few large restores never evict the snapshots undo needs for addons.
- `WTF/` backups (`.omaddons/backups/`) are never touched by transaction retention.

The retention and archive limits live in `src/limits.rs`. All but the 24-hour minimum age, the 1-hour orphan age, the 32 MiB ratio floor and the 5-second lock timeout can be changed in `[limits]`. The time budgets (50 ms, 2 s) live in the tests, and the failpoint list in `src/engine/tx.rs`.

---

## Network and privacy

omaddons contacts **only** these hosts. The list is checked on every redirect hop and matched exactly (subdomains are never implied), and HTTP requests must be `https` on port 443.

| Host | Used for | State |
|---|---|---|
| `github.com` | `releases/latest`, `release.json`, release asset URLs (which redirect), `releases.atom` (release notes), commits feeds | used |
| `api.github.com` | REST fallback (beta and alpha channels, releases without `release.json`); `search`'s repository search; `auth github`'s `/rate_limit` | used |
| `codeload.github.com` | zips of exact commits for `git:` refs | used |
| `objects.githubusercontent.com`, `release-assets.githubusercontent.com` | release asset downloads | used |
| `api.mmoui.com`, `cdn.wowinterface.com` | WoWInterface: the file list, project details, downloads | used |
| `api.tukui.org` | Tukui: the project list, download links, downloads, changelog pages | used; responses are never stored by the HTTP cache (`no-store`); the project list, without its download links, is kept in the release memo (15 minutes, and for `--offline`) |
| `api.curseforge.com`, `edge.forgecdn.net`, `mediafilez.forgecdn.net` | CurseForge | off until enabled (M5) |
| `addons.wago.io` | Wago | off until enabled (M5) |
| every host in `git_hosts` | `git:` refs through the git binary, never contacted over HTTP, not even by a redirect | only when you list them |

`url:` refs may download from any of the always-allowed hosts above (every row but CurseForge, Wago and `git_hosts`).

**What a stable GitHub install costs:** zero REST calls. omaddons reads `releases/latest` without following its redirect, then fetches `release.json` and the asset. Both of those redirect to `release-assets.githubusercontent.com`, so `net.log` shows five lines per repository, plus one more download for each candidate that does not load. Requests are limited to 6 at a time overall and 3 per host. Retries cover 429, 5xx and network errors, up to 3 times with full-jitter backoff (0.5 s base, 8 s cap), and honor `Retry-After` up to 60 s. That includes a connection that fails while a response or download is being read: the request is made again from the first byte, and each attempt is its own `net.log` line. Timeouts are 10 s to connect, 30 s for an API call and 300 s for a download; at most 5 redirects are followed (then `network`); an API response is capped at 64 MiB (`archive_too_large`). API requests ask for gzip, and every request says `User-Agent: omaddons/<version>`.

**Tokens.** A GitHub token is opt-in and sent only to `api.github.com`, where it raises the REST budget from 60 to 5,000 requests an hour (search: 10 to 30 a minute). It is, in order: `OMADDONS_GITHUB_TOKEN`; config.toml's `[sources.github] credential` (`{ env = "…" }`, `{ file = "…" }` or `{ command = [...] }`, a command run only when a REST request first needs it, never under `--offline`); or the file `omaddons auth github set` writes. `GITHUB_TOKEN` and `gh`'s credentials are never read, except `gh auth token` once, on `auth github --from-gh`, and `Authorization` is dropped on any redirect to another host. With or without a token, omaddons stops sending REST requests once the response headers say 5 or fewer remain, and reports `rate_limited` with the reset time. The budget is tracked per run, so the first REST request of a run is always sent.

**Search.** Only GitHub receives what you search for, in one `search/repositories` request per run (10 a minute without a token, 30 with one, counted apart from the REST budget). The catalog is built in, and WoWInterface's index and Tukui's list are whole lists, fetched (or reused) and searched on this machine, so neither learns the query.

**The git subprocess** runs with a cleared environment. Only `PATH` is kept. `HOME` and `XDG_CONFIG_HOME` point at an empty directory omaddons owns (`~/.cache/omaddons/git-home/`, mode 0700; a run refuses with `io` if anything is put in it), so no `.netrc`, git attribute file or other per-user setting reaches git or libcurl. Then `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_ALLOW_PROTOCOL=https`. It also gets no credential helper, no askpass, no redirects and https as the only protocol, and it may contact only hosts listed in `git_hosts`, never the hosts above. Arguments go as argv (never through a shell), and the run is in its own process group, which is killed on timeout (30 s for `ls-remote`, `init`, `checkout` and `rev-parse`, 300 s for `clone` and `fetch`). A checkout is held to the download limit (`limits.download_max_bytes`, `.git` included): git is stopped, with its process group, once the checkout grows past it (`archive_too_large`). What git prints is kept up to 64 MiB per stream. URLs with embedded credentials are refused.

**`net.log`.** Every request that reaches the network (each redirect hop and each retry) and every git run against a repository is one JSON line in `~/.local/state/omaddons/net.log`, rotated once at 10 MiB. Requests refused before sending (offline, `host_blocked`) and the `git --version` probe are not logged. Query strings and headers are never written. An illustrative line:

```json
{"time":"2026-10-14T21:00:00Z","method":"GET","host":"github.com","path":"/WeakAuras/WeakAuras2/releases/latest","status":302,"bytes":0,"duration_ms":34,"purpose":"release-latest","via":"http","error":null}
```

Production purposes for HTTP are `release-latest`, `release-json`, `releases-atom` (release notes), `rest-releases`, `commits-atom`, `asset` (every download, WoWInterface's and Tukui's included), `wowi-filelist`, `wowi-details`, `tukui-list`, `tukui-addon`, `tukui-changelog`, `github-search` and `rate-limit`. `wire_bytes` is what crossed the network when it differs from `bytes` (WoWInterface's file list arrives gzipped). A Tukui download link carries a token: its path is logged as `/v1/download/<slug>/…`, never with the token. git lines have `"method":"GIT"`, `"via":"git"`, the exit code as `status`, and a purpose of `ls-remote`, `clone`, `init`, `fetch`, `checkout` or `rev-parse`.

**Offline.** `--offline` or `OMADDONS_OFFLINE=1` refuses all network access, git included, and runs no credential command. Local installs, `remove`, `undo` and every read command work offline. Release metadata a command fetched is remembered in the release memo; offline, `info` answers from it with a `stale_check` warning. When `--offline` alone made the run offline, an `offline` error's and a `stale_check` warning's `fix_argv` is the same command without it; under `OMADDONS_OFFLINE=1` the fix stays prose.

**Fetch once.** Within one command each ref is fetched once, however many targets and engine runs need it: `install` without `-t` probes every live client and then installs with the same answer. `install`, `copy` and `pin --version` always ask the source (a cheap probe may confirm the remembered release). `outdated`, `update`, `sync`, `adopt`, `search` and `info` reuse an answer up to 15 minutes old (`SOURCE_RECHECK_MIN_INTERVAL`); `--refresh`, where a command has it, asks anyway.

**Account, realm and character names.** `WTF/` paths name them, so omaddons keeps them out of what it prints: SavedVariables warnings count files and never name them, and a backup's `manifest.json`, which lists every path, never leaves the machine (`src/privacy.rs` holds the one redactor, used by backups today).

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

auto_backup = "off"          # "off" or "daily"; backup list shows it, automatic backups arrive later in milestone 3
# How old an update check may be before status calls it stale (updates.stale):
# 1–48 hours; 0 keeps the default of 6.
check_interval_hours = 6

[limits]                     # sizes are integers or "KiB" / "MiB" / "GiB" strings
archive_max_entries = 20000
archive_max_uncompressed = "1GiB"
archive_max_ratio = 200
download_max_bytes = "512MiB"    # the largest download accepted
tx_retention_count = 10
tx_retention_days = 7
snapshot_max_bytes = "2GiB"      # per target, beyond the 24-hour window
blob_cache_max_bytes = "1GiB"
backup_min_free_mb = 2048        # MiB free beside backups below which they warn; 0 turns it off
backup_auto_keep_days = 7        # local days of automatic backups kept per target

# Turn a source off: its refs fail with source_disabled and make no request.
[sources.wowi]
enabled = false

# Per-target overrides, keyed by id or key.
[targets.wotlk]
label = "my 3.3.5a server"
```

| Key | Default | Meaning |
|---|---|---|
| `extra_roots` | `[]` | Extra discovery roots |
| `default_target` | none | `install`'s target when `-t` is absent; wins over `targets default`. It also leads `search`'s install hints among equals |
| `default_channel` | `stable` | Channel for new installs |
| `running_policy` | `safe` | `safe`, `refuse` or `allow` |
| `git_hosts` | `[]` | Hosts allowed for `git:` refs through the git binary |
| `auto_backup` | `off` | `off` or `daily`. `backup list` shows it (`auto.mode`); nothing takes automatic backups yet (milestone 3) |
| `check_interval_hours` | `6` | `status` reports `updates.stale` when the oldest check is older than this (1–48; 0 keeps the default) |
| `limits.archive_max_entries` | `20000` | Entries per archive |
| `limits.archive_max_uncompressed` | `1GiB` | Expanded size per archive |
| `limits.archive_max_ratio` | `200` | Compression ratio, checked past 32 MiB |
| `limits.download_max_bytes` | `512MiB` | Largest download, and git checkouts |
| `limits.tx_retention_count` | `10` | Snapshots kept per target within the window |
| `limits.tx_retention_days` | `7` | The retention window |
| `limits.snapshot_max_bytes` | `2GiB` | Snapshot total per target beyond 24 hours |
| `limits.blob_cache_max_bytes` | `1GiB` | Download cache (installed releases are pinned) |
| `limits.backup_min_free_mb` | `2048` | MiB free beside a client's backups below which `backup create` warns `backup_space_low` (and makes the backup anyway); 0 to 1,048,576, where 0 turns the check off |
| `limits.backup_auto_keep_days` | `7` | Local days of automatic backups that backup retention keeps per target: for each of the newest days holding any, its first pre-change and last post-session backup, and every before-restore one (1 to 100) |
| `sources.<id>.enabled` | `true` | `false` turns a source off (`github`, `git`, `url`, `local`, `wowi`, `tukui`): its refs fail with `source_disabled` and make no request, and `outdated` reports its addons `unchecked`. An id outside that list is accepted and ignored, without a warning |
| `sources.github.credential` | none | Where the GitHub token comes from: `{ env = "…" }`, `{ file = "…" }` or `{ command = [...] }` (see **Tokens** under [Network and privacy](#network-and-privacy)) |
| `targets.<id or key>.label`, `.family` | none | [Per-target overrides](#per-target-overrides) |
| `client_rules.<row>.<field>`, `rules.*` | built in | [Rules overrides](#rules-overrides-advanced) |

### Per-target overrides

```toml
[targets.retail]
label = "main"               # shown in status and targets

[targets.mystery]
family = "wrath-legacy"      # a misdetected or unreadable client: re-picks its load rules
```

A `family` override re-picks the client's load-rules row and its interface, as detection would have for that family. A client whose version cannot be read gets the family's current row: on Battle.net, `vanilla` loads `_Vanilla.toc` and honours `AllowLoadGameType`. Its interface then comes from the client's own `WTF/Config.wtf` (`lastAddonVersion`), when that lies in the family's range. Otherwise installs are judged by the family's interface range alone, and `out_of_date` is not reported. On a `*-legacy` row, the client's `lastAddonVersion` always wins. A family with no row for the install kind, such as a `*-legacy` family on a Battle.net folder, keeps the fallback rules. `unknown_flavor` is not reported for a client whose family an override sets.

### Rules overrides (advanced)

The detection tables can be patched without a new release (unknown keys there warn `config_unknown_key` too, array elements named by index, such as `rules.products[0].nmae`):

- `[client_rules.<row>]` (or `[rules.client_rules.<row>]`) patches one load-rules row; an unknown row name adds a new row, which then needs every required field. Rows are `mainline`, `mainline-12.0.7`, `mainline-10.2.7`, `mainline-9.1`, `forever`, `vanilla`, `tbc`, `wrath`, `titan`, `cata`, `mists`, the five `*-legacy` rows, `fallback-battlenet` and `fallback-custom`. The fields are `family`, `install_kind`, `version_min`, `version_max`, `game_types`, `multi_toc`, `comma_interface`, `game_suffixes`, `family_suffixes`, `legacy_suffixes`, `directive_conditionals` (whether the client reads inline directives such as `## Dependencies: X [AllowLoadGameType vanilla]`, or reads every line as written; true for every current client), `interface_includes_patch`, `legacy` and `reload_rereads_toc` (whether a running client re-reads a loaded addon's TOC on `/reload`, so the `safe` running policy lets a replacement that keeps it land; true for every modern row and `wrath-legacy`). A new row needs `family`, `version_min`, `multi_toc`, `comma_interface`, `interface_includes_patch` and `legacy`; its `directive_conditionals` and `reload_rereads_toc` default to false, so set both for a modern client.
- `[rules]` can add or replace `products`, prepend `version_families` rules, and replace `family_ranges`. A `version_families` rule may set `interface`, the client's addon interface when its version does not give it; it must lie in the family's range.

```toml
[[rules.version_families]]          # a 1.12-based client that reports 1.70
install_kind = "custom"
major = { min = 1, max = 1 }
minor = { min = 70, max = 70 }
family = "vanilla-legacy"
interface = 11200
```

Every rules edit applies on the next command. The discovery cache records the rules and the omaddons version it was built with, and re-parses every client once when either changes.

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

Debug builds also read `OMADDONS_FAILPOINTS`, `OMADDONS_FAILPOINT_DIR`, `OMADDONS_HTTP_FIXTURES`, `OMADDONS_RECORD`, `OMADDONS_PROC_ROOT`, `OMADDONS_NOW` (an RFC 3339 time that freezes the clock), `OMADDONS_FS_CLONE` (`yes` or `no`: whether the filesystem under a client clones files) and `OMADDONS_FS_FREE_MB` (the free space backups see) for testing. Release builds ignore them. `OMADDONS_ALLOW_SKIP`, `OMADDONS_REAL_MACHINE`, `OMADDONS_LIVE`, `OMADDONS_REAL_CUSTOM_ROOT`, `OMADDONS_WRITE_SEEDS`, `BENCH_HOME` and `LIVE_HOME` are read only by the test harness, never by the binary (see [Milestone checkpoints](#milestone-checkpoints-not-part-of-the-gate)).

---

## Files and state

| Path | What | Safe to delete? |
|---|---|---|
| `~/.config/omaddons/config.toml` | Settings | Yes (defaults apply) |
| `~/.config/omaddons/catalog.d/*.toml` | Your catalog overrides | Yes |
| `~/.config/omaddons/addons.toml`, `addons.toml.bak` | The desired state `sync` reads and `export --write` writes, and the previous file | Yes (yours) |
| `~/.config/omaddons/credentials/github` | The GitHub token `auth github set` stores (mode 0600, in a 0700 directory) | Yes (forgets the token) |
| `~/.local/state/omaddons/registry.json` | Registered targets and the default | Loses ids and registrations |
| `~/.local/state/omaddons/targets/<key>.json`, `<key>.files.json` | Per-target addon records, and every managed file's size and hash | Loses management |
| `~/.local/state/omaddons/errors_ack.json` | Dismissed Lua errors: per client and database (named by a hash of its path, never the path), the session acknowledged and when. No error text | Yes; but every dismissed error is new again |
| `~/.local/state/omaddons/history.jsonl` | One line per committed transaction, and per settings change (`config`, `target`, `auth`, `export`, `dismiss`, never undone; `target` is `null` when it names no client). A line a crash cut short is closed off by the next write, and recovery appends that transaction's line again; other unreadable lines are skipped and counted in a `history_lines_skipped` warning on `history` and `undo` | Loses history |
| `~/.local/state/omaddons/snapshots/<tx>/`, `journal/`, `inflight/`, `trash/` | Undo data for state files, journals, in-flight locks, staged deletions | No |
| `~/.local/state/omaddons/lock` | The global lock (never delete while running) | No |
| `~/.local/state/omaddons/status.json` | A derived summary, rewritten after each commit and undo | Yes |
| `~/.local/state/omaddons/net.log`, `net.log.1` | The request log | Yes |
| `~/.cache/omaddons/blobs/`, `http/`, `discovery.json`, `tmp/`, `inflight/`, `trash/`, `git-home/` | Downloads, HTTP validators, the discovery cache, temporary downloads and git checkouts, the in-flight locks of planning and dry runs, staged deletions, git's empty home directory | Yes when no command is running; reinstalls then download again |
| `~/.cache/omaddons/releases.json` | The release memo: the newest release each fetch saw (at most 4,096 entries), failures retrying cannot change, and what downloading candidates taught (each archive's hash, and whether it loads on a client) | Yes; the next commands fetch and download again |
| `~/.cache/omaddons/wowi/index.json` | WoWInterface's file list, slim, its text decoded and made safe (format 1.1; a 1.0 index is decoded once on load), refreshed after its hour | Yes; the next search or dependency lookup fetches it again |
| `~/.cache/omaddons/doorbell.json` | A signal for the bar widget: rewritten (`{at, command, actor}`) by every change that can alter what `status --cached` reports, never by a read | Yes; the next change writes it again |
| `~/.cache/omaddons/errors.json` | The errors memo: what `errors` last observed per client and database (file stamps, sessions, first-read baselines, counts per session and addon), so `status` can tell new errors without reading BugGrabber's files. No message, stack or locals text | Yes; but deleting it forgets which older errors were new, except what `errors dismiss` acknowledged (kept in `errors_ack.json`) |
| `~/.cache/omaddons/history_tail.json` | The history projection: the end of `history.jsonl` that holds its newest 16 runs with something to undo (at most 1 MiB), so `status --cached` reports the last change without reading a history of any size. Believed only while the history is exactly as long as it says and ends with its last line | Yes; `--cached` says `unknown` until the next change rebuilds it |
| `~/.cache/omaddons/export.json` | The hash of the `addons.toml` `export --write` last wrote, so a hand-edited file is not overwritten | Yes; the next `--write` then needs `--overwrite` |
| `<client>/Interface/.omaddons/target.json` | The client's identity, so a moved install keeps its id | Loses identity |
| `<client>/Interface/.omaddons/snapshots/<tx>/` | `AddOns/` (replaced and removed folders) and `WTF/` (SavedVariables copies with a manifest): most of the undo data | Loses undo |
| `<client>/Interface/.omaddons/stage/`, `trash/` | Staging (same filesystem as AddOns) and staged deletions | No |
| `<client>/Interface/.omaddons/backups/<id>/` | One `WTF/` backup, owner-only (`0700`): `backup.json` (what it is and proves), `manifest.json` (every entry's stats, and hashes after `--verify`; it names the account, realm and character, so it stays on this machine), `addons.json` (the addon versions that produced it) and the `WTF/` copy (see [`backup`](#backup)) | Loses that backup |
| `~/.cache/omaddons/backups.json` | The backup inventory: what each client's backups add up to, checked against the directories before it is believed | Yes; the next `backup list` rebuilds it |

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
        │ realmlist, /proc,    │ │ WoWInterface,    │ │ ownership, running        │
        │ registry, cache      │ │ Tukui, url, local│ │ policy, undo, retention,  │
        │                      │ │ resolve/: select,│ │ blob cache                │
        │                      │ │ catalog, deps,   │ │                           │
        │                      │ │ adopt, search    │ │ backup/: WTF/ backups     │
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
                        │ fsx/: the single filesystem  │  write-scope table, flock and
                        │ writer                       │  directory locks, renameat2,
                        └─────────────┬────────────────┘  reflink clones, stamps, prune,
                                      │                   failpoints
                                      │
          ┌───────────────────────────┼──────────────────────────────┐
          ▼                           ▼                              ▼
 <client>/Interface/         ~/.local/state/omaddons/       ~/.cache/omaddons/
 AddOns/, .omaddons/         registry, records, history,    blobs, http validators,
 (stage, snapshots,          journals, snapshots, net.log   discovery cache, release
 backups)                                                   memo, WoWInterface index,
                                                            doorbell, backups.json
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
| `src/inventory.rs` | Folder classification: protected, managed, shared, linked, unmanaged, invalid |
| `src/drift.rs` | Drift: managed files changed on disk since their commit, checked against the files index |
| `src/text.rs` | Release notes and source text made plain and terminal-safe |
| `src/source_scan.rs` | Test-only: the crate's own source with comments and test items blanked, for the unit tests that scan it |
| `src/sources/` | Ref grammar and every source: GitHub (release.json, REST, Atom feeds, search, token), git (GitHub without the binary, other hosts with it), WoWInterface, Tukui, url, local, changelogs |
| `src/resolve/` | Candidate selection, the catalog, dependency resolution, adoption evidence, search, update statuses, and `ReleaseCache`, the one way a command gets release metadata (each ref fetched once per run, the release memo across runs) |
| `src/net/` | HTTP client, allowlist, guard, retries, concurrency limits, validators, net.log, record/replay |
| `src/engine/` | The transaction engine |
| `src/fsx/` | The only code allowed to write to the filesystem: the write-scope table, the global flock and per-directory locks, `renameat2`, the reflink clone policy, stamp digests, pruning, failpoints |
| `src/state/` | Config, paths, registry, records and the files index, history, status, the release memo, `addons.toml`, the export memo, the doorbell |
| `src/backup/` | `WTF/` backups: the on-disk model, the one tree copy, the inventory projection, filesystem hints |
| `src/privacy.rs` | The one redactor for account, realm and character names in `WTF/` paths (backups use it today) |
| `src/clock.rs` | The clock time rules read (`OMADDONS_NOW` freezes it in debug builds) |
| `src/politeness.rs`, `src/limits.rs` | Every network-politeness constant and every safety limit, each in one place |
| `catalog/catalog.toml` | The built-in catalog, embedded with `include_str!` |
| `tests/`, `fuzz/` | Integration, contract, crash, concurrency and live tests; cargo-fuzz targets |

**One doorbell.** Every write that can change what `status --cached` reports rings `~/.cache/omaddons/doorbell.json` once, after the write: each commit window (undo's too, after its lock is released), a recovery that rolled a journal back or forward, a release memo write that changed the memo, a backup inventory write that changed it (a backup made, or a stale summary rebuilt), each backup's history line, and every settings command. Reads never ring it, so `status` stays write-free and the bar widget can watch the cache directory instead of polling. The file is a signal, never a source of state: the widget reads `status --cached` again when it rings.

**Invariants the build enforces:**

- Only `src/fsx/` writes to the filesystem (`tests/single_writer.rs` scans every production item of the source, test-only items aside). Only two modules start processes, each named with its reason and held to that by the same test: git checkouts (`src/sources/git_hosts.rs`) and the credential command (`src/state/config.rs`, which reads its output).
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

Wine prefixes are found as direct children of `~/Games`, as `~/.wine`, where a launcher config names them, and, while a WoW or Battle.net process runs, as that process's `WINEPREFIX` under your home; once registered, such a client stays found after it exits. A nested prefix such as Heroic's `~/Games/Heroic/Prefixes/…` is found through Heroic: its configs, or its per-game prefix folder when Heroic is installed. Outside prefixes, `~/Games` is walked 4 levels deep, and `extra_roots`, manually added roots and `product.db` paths are used as well. A custom client needs an `Interface/` folder next to its `wow*.exe`. A warning `discovery_truncated` means the 50,000-directory budget ran out; add the client by path.

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
- it is a legacy client, where the `safe` policy refuses every change (on 3.3.5a, every change but a replacement that keeps the loaded TOC);
- `running_policy` is `refuse`;
- you ran `undo --saved-variables restore`.

Quit the game and run the command again. For folder changes you can also add `--while-running`, but SavedVariables are never written while the game runs, whatever the flags.

### `unmanaged_folder` or `ownership_conflict`

A folder with that name already exists and omaddons does not own it, or another managed addon does. A folder you put there yourself can be adopted as it is, under the ref you were installing (the fix says so), and the next `update` updates it:

```bash
omaddons adopt <folder> --ref <ref> -t retail
```

Replacing it instead is undoable until retention prunes the transaction's snapshot, which then holds the folder's only copy (`snapshot_only_copy` says so, and `-v` says where):

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

Usually the GitHub REST budget (60 an hour without a token) ran out; only the beta and alpha channels, releases without a usable `release.json` and some pinned tags use REST. Any host can also cause it, keyless github.com included: a 403 or 429 saying no requests remain, a `Retry-After` longer than 60 seconds, or a 429 that outlasts three retries. Wait for `details.reset_at`. For the REST budget, a token raises it to 5,000 an hour (search: 30 a minute):

```bash
echo "$TOKEN" | omaddons auth github set   # or: omaddons auth github --from-gh
```

### `host_blocked` for a git host

`config set` replaces the whole list, so include the hosts already in it:

```bash
omaddons config get git_hosts
omaddons config set git_hosts '["codeberg.org", "gitlab.com"]'
```

### `source_disabled`

A source turned off in `config.toml` (`[sources.wowi] enabled = false`) refuses its refs with this error, and its `fix_argv` turns it back on (`omaddons config set sources.wowi.enabled true`). CurseForge and Wago refs arrive in milestone 5. Many popular addons also publish GitHub releases, so try `gh:Owner/Repo`. Otherwise, download the zip in a browser and install it as a local path (`./Addon.zip`). The same error for a `git:` ref on another host means `git` is missing or older than 2.32.

### `journal_pending` that does not clear

Journals are recovered by the next command that changes something: install, update, sync, pin, unpin, `adopt` when it adopts, copy, remove and undo (also when they change nothing), `config set`, `targets add`, `rm`, `rename` and `default`, `auth github set` and `clear`, and `export --write`. Dry runs and read commands, the `adopt` report included, never recover. A command whose own recovery failed (its error carries `details.journal_pending`) keeps the journal and its staged folders for exactly that. If recovery finds a step that matches neither its before nor its after state, or the journal's client no longer carries its target's marker, it stops and leaves the journal for manual repair. So it does with a journal it cannot read: one a newer omaddons started (`journal_too_new`: finish it with that version) or one holding lines this omaddons cannot decode (`journal_unreadable`). It leaves such a journal whole, with its in-flight lock, stage and snapshots, and `status` titles the issue "needs manual repair". Every later change on that client then fails until it is fixed (`error.details.journal_pending` names the transaction, `error.details.reason` why); other clients are not affected and keep reporting it as `journal_pending`. `doctor`, which will automate this repair, is planned for milestone 4.

An interrupted `WTF/` restore adds three cases. While its game runs, it waits (`game_running`): quit the game and run any change. With `preservation_incomplete`, recovery could not finish giving what the restore displaced a home as a `recovered` backup (an I/O error or a full disk, say): the trees are under `Interface/.omaddons/stage/<tx>/` (in `recovered/<id>/` once moved there), and the next change resumes with the same backup id once the cause is gone. With `unexpected_state`, something the restore moved is no longer where it recorded it; everything was left exactly as found.

---

## Limitations and known issues

### What omaddons does not do (yet)

| Capability | State | Planned |
|---|---|---|
| Batched GitHub checks (GraphQL, one request for many repositories) | ❌ one `releases/latest` per repository instead | M5 |
| The Omarchy bar widget | ❌ the help text mentions it, and `--actor widget`, `status --cached` and the doorbell are ready for it; the widget itself does not exist yet | M3 |
| Installing BugGrabber from a keyless `wowace:` source | ❌ (`errors` reads BugGrabber installed by hand or embedded in another addon) | M3 |
| Checking and restoring `WTF/` backups, automatic backups | ❌ (`backup create` and `backup list` work) | M3 |
| `capabilities`, `doctor`, `robot-docs`, stored plans, agent skill | ❌ | M4 |
| CurseForge, Wago | ❌ | M5, optional, bring your own key |
| Shell completions | ❌ | M4 |
| Packages, man page | ❌ build from source | M5 |
| Windows, macOS | ❌ | Never (non-goal) |
| A GUI, TUI, daemon or MCP server | ❌ | Not in v1 (non-goal) |
| Editing WeakAuras strings, ElvUI profiles or SavedVariables | ❌ | Never (non-goal) |

### Known limitations

- Discovery scans only the direct children of `~/Games` as prefixes (a nested prefix is found through a launcher config or while its client runs). A relative Lutris `exe` is resolved against the game's `working_dir` or `prefix`, where Lutris itself uses the game directory in its `pga.db`, which omaddons does not read.
- Drive-letter mapping applies to clients that Battle.net's `product.db` names. A custom client on another drive letter needs `targets add` or `extra_roots`.
- Custom clients between the five legacy versions (2.0–2.3, 3.0–3.2, 4.0–4.2, 5.0–5.3) get family `unknown`, which matches only their exact major.minor.
- On unknown custom clients, the `safe` running policy behaves as for legacy clients and refuses every change while they run.
- Do not run a pre-M3 omaddons build to change state that M3 has written. Reading with it (`status`, `list`, `history`) is safe. A pre-M3 `undo` cannot see M3 restores, and it is refused a newer record only at its save, after its folders moved; a pre-M3 binary is stopped only at the `WTF/` boundary of an M3 restore journal. From M3 on, every build refuses what it cannot read (see [Transactions](#transactions)).
- The download cache reuses an asset whose SHA-256 is known: from the source (GitHub REST assets carry a digest) or remembered from an earlier download of that release's asset in the release memo. A release's assets are taken to be immutable, so a publisher re-uploading different bytes under the same tag and file name is not noticed while the old blob is cached (unless the source declares a different size or hash). Within one command, the same asset is still downloaded once per target that tries it. An unchanged release is never downloaded.
- A `url:` ref's release is its URL, so new content published at the same URL is never picked up.
- Private GitHub repositories and HTTP proxies are not supported.
- Two clients sharing one AddOns directory (an AddOns symlinked between Classic clients) keep separate records: a folder one records is `shared` on the other, which cannot record it too, so manage those addons from one client. Records made on both before this check stay doubled: `status` warns `target_addons_shared`, a change through either warns the same, and M4's doctor will report them.
- An archive whose side folder lists only another client's interface is refused whole, even when its main addon loads and the source offers no other file. WoWInterface's ArkInventory 3.12.17 (on Mists Classic, Classic Era and Forever) and Titan Panel 9.3.5 (on Anniversary) are refused this way. Whether to skip such a folder instead is an open decision (deferred bead `omad-mixed-archive-single-file-skk1`).
- The legacy running behavior was observed only on 3.3.5a: a new addon needs a restart, and a loaded addon's TOC is re-read on `/reload`. So only 3.3.5a takes replacements while it runs; the other legacy clients refuse every change until they are observed. Running detection was observed under umu and GE-Proton for retail, Classic Era, Anniversary and 3.3.5a (Mists Classic's process has the same shape). The Battle.net app itself runs as `CrBrowserMain`, so with no client open the process route relies on Battle.net's agent (`Agent.exe`, run from `ProgramData/Battle.net/Agent/`). Under umu the agent was observed (2026-10-21, the app alone open) carrying the prefix's `WINEPREFIX` (its `pfx` path, a link to the prefix root), so the route holds.
- Loading was observed in game, not only judged from TOCs: retail, Mists Classic, Classic Era, Anniversary, WoW: Forever (the 2026-10-21 stress test) and 3.3.5a each wrote the SavedVariables of the addons omaddons installed. An addon out of date for its client (OmniCC 11.2.8 on retail 12.1.5) did not load, as the rules predict. A modern client adds newly installed addons to `AddOns.txt` only when it creates the file, so `AddOns.txt` is no load evidence. Where a modern client stores "Load out of date AddOns" is not located. The Lutris, Heroic and Steam readers follow their upstream source (Lutris v0.5.22, Heroic v2.22.4, and for closed-source Steam the readers of Lutris, protontricks and ValvePython `vdf`), cited in each reader; the Bottles layout was verified against Bottles 67.4, and `product.db` against a real Agent database and two upstream readers.

### Known issues

The README describes the intended behaviour; these are the places where the code at HEAD does not match it yet. Each one has a bead (`br show <id>`), and the README changes only if a fix decides otherwise.

| Bead | Priority | Where the code differs |
|---|---|---|
| `omad-rnk8` | P1 | A mistyped `undo --last-by` value (`--last-by humna -t retail`) gets the fix `omaddons undo --last-by agent`, dropping every other argument (`-t`, `--dry-run`) instead of correcting the value in place, so the fix is a different command that changes addons. A bad `--actor` value's fix drops the flag instead of correcting it |
| `omad-ck0h` | P2 | `git_hosts` accepts the always-allowed HTTP hosts (`codeload.github.com`, `api.mmoui.com`, …), so the git binary can contact them, and a `git:` ref on such a host suggests adding it. The two allowlists are meant to share no host |
| `omad-oxxv` | P2 | `omaddons help auth github` and `omaddons auth github --help` fail with a cut-off usage error (use `omaddons help auth`). `install`'s REF help leaves out `wowi:` and `tukui:` |
| `omad-zwl3` | P2 | When nothing resolves a bridge requirement, `install`'s `commands` can hold an `omaddons search <folder>` hint labelled action `install` |
| `omad-npec` | P3 | `auth github set` and `clear` record a history line even when the stored token does not change |
| `omad-o3gd` | P3 | With no update but a client that has adoptable folders, `outdated`'s human footer reads "Run `omaddons adopt -t <id>` to install them" |
| `omad-peud` | P3 | A `git:` commit the server refuses to send fails `download_failed` marked retryable, although only naming a branch or tag can help |
| `omad-hp8r` | P3 | A failed command's envelope leaves out the `catalog_override_invalid` warning the run found before failing (a successful one carries it) |

---

## Roadmap

| Milestone | Scope | State |
|---|---|---|
| **M1: working slice** | Discovery, ClientRules and the TOC engine, fsx and the transaction engine with undo, keyless GitHub/git/url/local sources, the core CLI contract | ✅ Done (2026-10-15) |
| Follow-up: Classic and Forever gaps | Multi-flavor archives, never-launched clients, game modes, real-machine tests for every live client, the catalog × flavor probe, and the in-game load check on the real Forever client | ✅ Done (2026-10-21) |
| **M2: updates and adoption** | `outdated`, `update`, `pin`/`unpin`; WoWInterface and Tukui; changelogs; dependencies; drift detection; tiered `adopt`; `addons.toml` with `export`, `sync` and `copy`; `search`; `auth github` | ✅ Done (2026-10-16) |
| Post-M2 audit | An independent audit of M1 and M2 (engine failure paths, contract holes, integrity and privacy statements, discovery and rules fields, test blind spots, this README) and real-world checks on real clients: running detection under Proton, in-game loading on every client, legacy `/reload` behaviour | ✅ Done (2026-10-21) |
| **M3: the widget** | The Omarchy bar widget (QML, embedded in the binary, managed by `omaddons omarchy install`, `status` and `remove`), `errors` (BugGrabber, installed keyless from a new `wowace:` source), `backup` of `WTF/` (list, create, verify, restore, export and import, automatic backups) | 🔄 In progress: the doorbell, the privacy redactor, `errors` and `errors dismiss`, errors in `status`, `backup list` and `backup create` are in |
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

Other tasks: `argc fmt`, `argc lint`, `argc test [filter]`, `argc fuzz <target>`, `argc fuzz-all` and `argc bench` (the last three are checkpoint tasks, below).

### Tests

At the close of the post-M2 audit (2026-10-21) there are 1,645 tests: 883 unit tests in `src/`, 761 integration tests in 75 test binaries and one doctest. The gate runs 1,592 of them. The other 53 are `#[ignore]`d and run only at checkpoints or by hand: live probes, real-machine and real-client checks, time budgets and the bench homes (`tests/perf.rs`), fixture recorders, the fuzz-seed writer and child-process helpers. (At the M2 checkpoint, 2026-10-16, there were 1,333.) `cargo test -- --list` counts today's.

| Suite | What it covers |
|---|---|
| `tests/contract/` | A matrix of success and failure envelopes across the commands: key order, exit codes, registered codes, suggested argvs that parse and round-trip, usage fixes that never name another changing command; snapshots |
| `tests/engine_crash.rs` | All 33 engine failpoints, every journaled rename (atomic and two-rename), and crashes during recovery, armed as real `abort`s in a child process over SavedVariables; undo included; torn journal, history and actions lines; whole-`WTF/` restores and their undos run by the engine in a child of the test binary, with injected I/O failures, a game started mid-restore, the preservation of displaced trees (crashed and failed at each of its steps) and bind-mounted topologies |
| `tests/engine_concurrency.rs` | Parallel runs, duplicate installs, races between plan and commit (each run stopped at a named step), no lock while planning and staging, and the lock hold of commits and undo, measured (16 MiB of SavedVariables and a 64 MiB folder included) |
| `tests/perf.rs` | Time budgets (release build, checkpoint only) and the request-economy checks that run in the gate |
| `tests/discover_*.rs` | The discovery matrix, launchers, scanning, custom clients, the cache |
| `tests/catalog.rs` | The 46 GitHub and 5 `git:` catalog entries replayed offline from recorded HTTP fixtures with no REST calls, and the 9 WoWInterface entries in one `filedetails` request with their MD5s (the two Tukui entries: `tests/sources_tukui.rs`) |
| `tests/single_writer.rs` | Only `src/fsx/` writes to the filesystem, and only the two named modules start processes |
| `tests/backup_list.rs`, `tests/backup_create.rs`, `tests/doorbell.rs` | Milestone 3 so far: `backup list` over fixture backups; `backup create` over a fixture `WTF/` (exact copies, owner-only modes, capture consistency with the tree changed mid-copy through `pause` failpoints, no lock while copying, dry runs writing nothing, privacy on both streams); the doorbell's ring sites (directory locks: unit tests in `src/fsx/lock.rs`) |
| `tests/fsx_lock.rs` | The global lock and the in-flight locks |

Recorded HTTP fixtures live in `tests/fixtures/http/<scenario>/`. Debug builds replay them with `OMADDONS_HTTP_FIXTURES=<dir>` and re-record them with `OMADDONS_RECORD=1`. Recording never stores request headers, drops `set-cookie`, and strips secret query parameters from URLs and `location` headers.

### Milestone checkpoints (not part of the gate)

```bash
argc fuzz-all --time 600                                   # every fuzz target, 10 minutes each
argc bench                                                 # hyperfine medians on a synthetic home (needs hyperfine, jq)
cargo test --release --test perf -- --ignored --skip bench_home --skip live_home   # the time budgets
OMADDONS_LIVE=1 cargo test --test live_probes -- --ignored  # the real endpoints
OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1 OMADDONS_ALLOW_SKIP=keyless_installs_forever \
  cargo test --test real_machine -- --ignored --test-threads=1   # this machine's clients; every addon change is undone
```

A checkpoint test that cannot run (a variable unset, its client running, the Forever stress-test set still installed) prints `SKIP <test>: <reason>` on stderr and fails, so a skip never counts as a pass. `OMADDONS_ALLOW_SKIP=<test>[,<test>…]` accepts the named skips, which still print their line; the real-machine command above accepts `keyless_installs_forever` for as long as the stress-test set stays installed. The checkpoint's close reason quotes every `SKIP` line.

- **Benchmarks.** `argc bench` (`scripts/bench.sh`) builds a home of three synthetic clients with 30 addons and 5,040 managed files, one release behind, and times `status --cached`, `status` (the drift check), `outdated` cold and warm, `update --all --dry-run` warm and `update --all` cold. Each line names its binary: the release binary for what needs no network, and for cold runs a `bench-fixtures` build (release with debug assertions), because release builds never replay fixtures. Warm runs must add no `net.log` line, and a missed target exits 1. The budgets in `tests/perf.rs` check the same targets in-process.
- **Live probes.** They include `catalog_matches_declared_flavors`, which builds a synthetic client for every live product and every legacy version and dry-runs each catalog addon on each. An addon must install exactly where its publisher declares it and nowhere else, with zero REST calls. The probe prints the addon × client matrix, and publishers whose `release.json` disagrees with their own TOCs are listed in it with a date.
- **Real-machine tests.** They run against the real home and undo every addon change they make. Target registrations and `target.json` markers written on the first change stay, by design.
- **Fuzzing.** There are 17 cargo-fuzz targets: `archive`, `atom_feed`, `bbcode_text`, `bottles`, `bpsv`, `desired_toml`, `heroic`, `html_text`, `lutris`, `product_db`, `ref_parse`, `steam_shortcuts`, `steam_vdf`, `toc`, `tukui_addons`, `wowi_details` and `wowi_filelist`. Each has committed seeds under `fuzz/seeds/`. They need `cargo-fuzz` and a nightly toolchain at least as new as the crate's `rust-version` (pick one with `--toolchain`). A crash input becomes a regression unit test in the owning parser.

### Work tracking

Work is tracked with beads (`br`), and agents follow `AGENTS.md`. `br ready` shows unblocked work, and `br show <id>` shows one bead.

---

## FAQ

### Why "omaddons"?

It is built alongside [Omarchy](https://omarchy.org), the Arch-based desktop that installs Battle.net through umu and Proton into `~/Games/battlenet`, and an Omarchy bar widget is planned for milestone 3. The CLI does not depend on Omarchy and works on any Linux desktop.

### Will it touch the addons I already have?

No. Folders it did not install are reported as unmanaged and left alone. Replacing one requires `--force`, and even that is undoable for as long as the snapshot is kept (at least 24 hours; `snapshot_only_copy` says when a snapshot holds the only copy). `adopt` brings existing addons under management as they are, without touching their files.

### Is it safe to use while WoW is running?

Yes, by default. Updates that keep the loaded TOC land and tell you to `/reload`. Removing a loaded addon is refused until the game exits, and you run it again then. SavedVariables are never written while the game runs. Legacy private-server clients get no changes while running, except 3.3.5a updates that keep the addon's loaded TOC, which need a `/reload`. See [Running clients](#running-clients).

### How do I update my addons?

Run `omaddons outdated` to see what has updates on every client, then `omaddons update` to install them (or `omaddons update <addon>` for some). Each client gets its own transaction, and `omaddons undo` reverts the whole update. `url:` refs are one exception: their release is the URL, so install a new URL instead. Pinned addons (`omaddons pin <addon>`, or installed with `--version`) are the other: they stay put until `omaddons unpin <addon>`.

### How do I find which addon throws Lua errors?

Install [BugGrabber](https://www.wowace.com/projects/bug-grabber) (BugSack shows them in game), play, and run `omaddons errors` after the game exits. It lists each addon's errors on every client, the likely source with how sure it is, which are new, and whether an update may fix them. `omaddons errors --check` exits 6 when something is new.

### Why is an error's message hidden?

An addon can write anything into its own error messages, a party member's name included, and nothing can reliably tell a name from other text. So `errors` shows a message only when it is one of Lua's or the game's own, whose variable parts are identifiers from code; any other shows its length. `omaddons errors --raw-details` shows them all, with your own account, realm and character names and every player link replaced, but it cannot catch every name: share its output with care.

### Does it work with Turtle WoW, Warmane or other private servers?

Any client from 1.12.1, 2.4.3, 3.3.5a, 4.3.4 or 5.4.8 with an `Interface/` folder is a first-class target, identified by its executable. Its label comes from `realmlist.wtf`. Server-specific patches are not special-cased: Turtle WoW is a `vanilla-legacy` client, whether its executable reports 1.12.1 or its own 1.17 or 1.18. Give it a name with `targets add <path> --name turtle` or `targets rename`.

### Can I use CurseForge addons?

Not through CurseForge yet; optional CurseForge support with your own key is planned for milestone 5. CurseForge's terms make keys non-transferable and forbid caching, so omaddons will never bundle a key or proxy. Many CurseForge addons also publish on GitHub (`gh:Owner/Repo`). You can also download a zip in the browser and install it as a local path.

### Does it phone home?

No. There is no telemetry. Only the hosts on the allowlist are contacted, every request that reaches the network is in `omaddons net-log`, and `--offline` makes network access impossible.

### Where does undo data live, and how long is it kept?

Old folders and SavedVariables copies go to `<client>/Interface/.omaddons/snapshots/`, on the same filesystem as the addons, and state-file copies go to `~/.local/state/omaddons/snapshots/`. Per target, everything younger than 24 hours, the latest change, and whatever a deferred SavedVariables restore still needs are always kept. Beyond those, at most the newest 10 changes within 7 days are kept, capped at 2 GiB.

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
  omaddons outdated --json                       # what has updates, on every client
  omaddons update --all --dry-run --json         # then without --dry-run
  omaddons undo --last-by agent --json           # the escape hatch
  omaddons search <words> --json                 # find a ref; install verifies it
  omaddons backup create --json --actor agent --reason "<why>"   # copy every live client's WTF/
  omaddons backup list --json                    # WTF/ backups and what each proves

Refs     catalog name | gh:Owner/Repo | Owner/Repo | wowi:<id> | tukui:<slug> | git:https://…#rev
         | url:https://… | a pasted GitHub, WoWInterface or Tukui link | ./zip-or-folder
Targets  -t retail | era | sod | hardcore | classic | anniversary | forever | wotlk | <key> | <path>

Read the envelope
  ok, exit_code, partial, data, error{code, message, fix, fix_argv, alternatives[{display, argv}], retryable, details}
  warnings[{code, message, fix, fix_argv}], commands[{action, command, argv, destructive}]
  Run fix_argv / alternatives[].argv / commands[].argv as argv arrays; never parse the display strings.
  fix_argv is null when the fix is prose or needs a value only you can supply (read error.fix:
  the command shows each missing value as a <placeholder>; alternatives may hold a read-only step).
  A locked or retryable plan_stale error's fix_argv reruns the same command.

Exit codes  0 ok (check partial) · 1 failed (see retryable) · 2 usage · 3 not found/ambiguous
            4 refused (safety) · 5 locked (retry) · 6 --check/--strict failed

Rules
  - --dry-run first for anything destructive; --force only after reading the refusal.
  - Bind the real remove or undo to that preview: --if-match <data.precondition>. A plan_stale
    answer (not retryable) means the world changed since: preview again.
  - Never write into Interface/AddOns yourself; every change must go through omaddons to stay undoable.
  - --offline for read-only work; net-log shows the requests that reached the network (the 50 most recent; --limit N).
  - update installs the newest releases on every client in one run; one undo reverts it. Pinned addons stay
    (skipped, skip_reason pinned; an addon_pinned warning only when you named them).
  - Only install falls back to a default target; pass -t everywhere else when more than one client matches.
  - README "Known issues" lists where the code currently differs from this contract.

State   ~/.config/omaddons/config.toml · ~/.local/state/omaddons/ · ~/.cache/omaddons/
```

---

## License

There is no license yet. The repository is private, and all rights are reserved by its owner until a license is chosen when the project is opened. The dependency policy (`deny.toml`) admits only Apache-2.0, BSD-3-Clause, CDLA-Permissive-2.0, ISC, MIT, Unicode-3.0 and Zlib crates, so no dependency constrains that choice.
