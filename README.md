# omaddons

<div align="center">

**One binary that finds every World of Warcraft client on a Linux machine, installs only the addons each client will actually load, and can undo every change it makes.**

![status: pre-alpha](https://img.shields.io/badge/status-pre--alpha-orange)
![milestone: M4 done](https://img.shields.io/badge/milestone-M4%20done-brightgreen)
![platform: Linux](https://img.shields.io/badge/platform-Linux-blue)
![rust: 1.97.1](https://img.shields.io/badge/rust-1.97.1-dea584)
![accounts: none needed](https://img.shields.io/badge/accounts-none%20needed-brightgreen)
![telemetry: none](https://img.shields.io/badge/telemetry-none-brightgreen)

</div>

```text
$ omaddons status
Nothing needs doing
anniversary  tbc                                           2.5.6.70006   live  anniversary (battlenet)  0 addons · 0 unmanaged · 0 protected folders
classic      mists                                         5.5.4.70268   live  classic (battlenet)      0 addons · 0 unmanaged · 0 protected folders
era          Classic Era · Hardcore · Season of Discovery  1.15.9.70003  live  era (battlenet)          0 addons · 0 unmanaged · 0 protected folders
forever      forever                                       1.60.1.70245  live  forever (battlenet)      0 addons · 3 unmanaged · 0 protected folders · 3 adoptable
retail       mainline                                      12.1.5.70391  live  retail (battlenet)       2 addons in 2 folders · 0 unmanaged · 0 protected folders
wotlk        wrath-legacy                                  3.3.5.12340   live  localhost                0 addons · 0 unmanaged · 23 protected folders
Next: omaddons adopt -t forever
```

<sub>Output from a test home laid out like the development machine: five Battle.net clients in one Proton prefix and a 3.3.5a private-server client in another, found with no configuration. Trimmed: today's `status` also prints a `backups:` line under each client, and may recommend more than one next step (see [`status`](#status)).</sub>

<div align="center">
<h3>Build and install</h3>

```bash
git clone https://github.com/a44-io/omaddons.git && cd omaddons
cargo install --locked --path .
```

</div>

There is no packaged release, installer or prebuilt binary yet; see [Installation](#installation).

> [!NOTE]
> **Pre-alpha: milestones 1 to 4 are done, and the JSON contract is frozen at `v0.1.0`.** omaddons runs daily on the development machine, where every client it has managed (retail, Mists Classic, Classic Era, Anniversary, WoW: Forever and a 3.3.5a server) has been seen in game loading the addons it installed.
>
> - **Done (M1 to M3):** discovery and the load rules, transactions and undo, keyless GitHub, git, WoWInterface, Tukui and WowAce sources, `outdated`, `update`, `pin`, `adopt`, dependencies, `sync`, `export`, `copy`, `search`, `errors` (Lua errors from BugGrabber), `backup` (`WTF/` settings, with automatic checkpoints and undoable restores) and the Omarchy bar widget.
> - **Done (M4):** `capabilities`, `robot-docs guide`, `schema`, `errors` and `install-skill`, shell completions, intent inference (a client named in the wrong place or mistyped is corrected in the fix), stored plans with `apply`, `plans show` and `--if-match`, the doctor (`doctor`, `explain`, `fix`, `diff`, `undo`, `gc` and `--online`), `status`'s issues taken from the doctor's cheap checks, bare help's line naming the commands agents start from, the pre-freeze contract audit, verbosity levels (`-vv` traces the release memo, discovery and the global lock on stderr), and the contract's golden (the semantic manifest, compared with the code in every gate).
> - **The first tag, `v0.1.0` (2026-11-08):** contract 1.0 is frozen; a contract change now carries its `schema_version` bump ([Stability](#stability)).
>
> M5 is next. The repository is private. See [What works today](#what-works-today), [Known issues](#known-issues) and the [Roadmap](#roadmap).

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
| **The game's rules decide** | TOC suffix precedence (`_Mainline`, `_Vanilla`, `_Forever`, …), `## Interface:` lists, `AllowLoadGameType`, interface ranges per family, retail 12's floor of 120000 and the legacy single-TOC rules, applied to what is actually inside the archive. A build for another flavor is never installed, nor one the client never loads. |
| **Multi-flavor archives** | DBM and BigWigs ship retail-only boss modules in the same zip as their Classic modules. On Classic or Forever, the folders that client would not load are skipped and listed in `folders.skipped`. A stray side folder built only for other clients (ArkInventory's retail-only example module) is left out and said so, and a file its publisher labels for other clients only is refused. |
| **Atomic, journaled changes** | Folders are staged on the same filesystem, then swapped in with `renameat2(RENAME_EXCHANGE)` under a journal. A crash leaves each folder wholly old or wholly new, and the next change command finishes the work or rolls it back. |
| **Undo any change** | Each install, remove and undo snapshots what it replaces. `undo` restores folders as they were (file bytes and modes, directory modes, symlinks, empty directories) and restores SavedVariables only where they changed, until retention prunes its snapshot. An undo is itself undoable, and `undo --last-by agent` reverts the last thing an agent did. |
| **Keyless** | Stable GitHub releases are read from `release.json` without the REST API, and git branches through GitHub's commits feed and codeload, without the git binary. No account, token or API key is needed for anything that works today. |
| **Safe around a running game** | With the default policy, updates that keep the loaded TOC can land while you play (`/reload` to pick them up), and a new addon lands too, then asks for a restart. Removing a loaded addon is refused until the client exits, and SavedVariables are never written while it runs. |
| **Verifiable privacy** | A hard host allowlist is checked on every redirect hop. Every request that reaches the network and every git run is logged locally (`omaddons net-log`), `--offline` refuses all network access, and there is no telemetry. |
| **Built for agents** | `--json` gives exactly one envelope on stdout, success or failure. Error codes are typed, fixes come as `fix_argv` and `alternatives` argv arrays whenever they are commands, exit codes 0 to 6 are documented, and every change records its actor and argv. `capabilities --json` describes the build in one offline call, `robot-docs` prints the agent guide, every JSON Schema and the error catalog, and a `--dry-run` stores a plan that `apply` runs exactly. |
| **A doctor that repairs** | `omaddons doctor` checks 31 failure modes and names the evidence and the fix for each finding. `doctor fix` repairs the ones its fixers cover as one undoable transaction: it quarantines a damaged cached archive, finishes an interrupted change through omaddons' own recovery, sets a stuck journal aside (only with `--force --yes`), and makes a credential file private again. |
| **Settings backups** | Each client's `WTF/` is backed up before the day's first change and after a play session, cloned on btrfs and xfs so a backup costs no space at first. `backup restore` puts back the whole `WTF/` or one addon's settings; it is refused while the game runs and can be undone. |
| **Lua errors, attributed** | `omaddons errors` reads BugGrabber's captures on every client, names the likely source and how sure it is, marks which errors are new, and says whether an update may fix them. It never shows text an addon wrote unless asked. |
| **A bar widget** | On [Omarchy](https://omarchy.org), `omaddons omarchy install` puts a button on the bar with updates, issues, Lua errors and backups in a keyboard panel, and every action runs an omaddons command. |

### Quick example

```bash
# Every client on the machine, no setup
omaddons status

# Preview, then install exactly what the preview showed (its plan id)
omaddons install dbm bigwigs -t retail --dry-run    # Plan 01M8Q2VA… stored for 24 hours
omaddons apply 01M8Q2VA

# Season of Discovery runs in the Classic Era client: -t sod selects it
omaddons install questie -t sod

# A 3.3.5a private server: a git branch, fetched without the git binary
omaddons install gbb -t wotlk

# Anything on GitHub: a pasted link works
omaddons install https://github.com/tullamods/OmniCC -t retail

# What changed, by whom, and take it back
omaddons history
omaddons undo

# Something wrong? The evidence and the fix, then the repair, previewed first
omaddons doctor
omaddons doctor fix --dry-run

# The same thing, for an agent
omaddons install plater -t forever --json --actor agent --reason "user asked for nameplates"
```

---

## What works today

Milestones 1 (the working slice), 2 (updates, dependencies, adoption and desired state), 3 (the Omarchy widget, Lua errors and `WTF/` backups) and 4 (the agent surface) are complete. Milestone 4 ended with the first tag, `v0.1.0`, which froze the JSON contract at 1.0. Everything in this README describes code that exists and is tested, unless it is marked as planned. Bugs the code has today, where it differs from what this README says, are listed under [Known issues](#known-issues).

| Area | State |
|---|---|
| Discovery of Battle.net, Steam, Lutris, Heroic, Bottles and custom clients | ✅ Done |
| The TOC load-rule engine (`ClientRules`) for all 12 Battle.net products and five legacy private-server versions | ✅ Done |
| The transaction engine: plan, stage, journal, atomic swap, crash recovery, undo, retention | ✅ Done |
| Keyless sources: GitHub releases, git (GitHub without the binary, other hosts with it), direct URLs, local zips and folders | ✅ Done |
| A catalog of 63 verified addons in 66 entries (46 GitHub release entries, 9 WoWInterface entries, ElvUI and Tukui from Tukui, 5 legacy private-server addons from git, and BugGrabber's 4 builds from WowAce) | ✅ Done (BugGrabber in milestone 3) |
| The CLI contract: one JSON envelope, typed errors with fixes (an argv whenever the fix is a command), exit codes 0 to 6, contract snapshot tests | ✅ Done (frozen at the first tag, `v0.1.0`: contract 1.0) |
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
| `backup create`: a copy of each client's `WTF/`, made without the global lock (copy-on-write on btrfs and xfs), that says whether `WTF/` held still while it was copied and which addon versions produced it; `--verify` hashes it. `backup list`: each client's backups, what each proves, and the space they take. `backup restore`: the whole `WTF/` back in one atomic exchange, or with `--addon` one addon's settings only, file by file, refused while the game runs, undoable. `backup prune`: automatic backups thinned by day after every backup, and your own deleted only when you ask, never one being read. `backup export`: one backup as a reproducible `.tar.zst` off this disk, checked against its hashes, never replacing a file. `backup import`: such a file back as a verified backup, accepting only an exact export. `backup verify`: a backup's files against its recorded hashes (a byte changed at the same size is caught), or a first baseline recorded, said to prove nothing of the capture. `status` shows each client's backups, whether automatic ones work there and the space free, and recommends a backup when none is recent | ✅ Done (milestone 3) |
| `errors`: which addon most likely throws Lua errors, from BugGrabber's data on every client (3.3.5a included), with the attribution's method and confidence, new errors per database, and nothing an addon wrote shown unless asked for; `errors dismiss` marks what was read as seen, bound to that read; `status` shows each client's new errors, and `status --cached` counts them without opening BugGrabber's files | ✅ Done (milestone 3) |
| Automatic `WTF/` checkpoints: before the day's first change and after a play session (`backup create --auto`, which the widget runs when a client stops and for every stopped client when it starts), skipped when nothing changed, clone-only by default, within the free-space floor | ✅ Done (milestone 3) |
| WowAce: `wowace:` refs and links, keyless, every tree fetched at one pinned revision; projects that need nothing from the packager but `package-as` and `@project-version@` (BugGrabber). `omaddons install buggrabber` installs each client's own build (catalog variants), and `errors` offers exactly that command where BugGrabber is missing | ✅ Done (milestone 3) |
| `status` as triage for the widget and agents: a summary of every client, issues with a failure-mode id, a severity, the doctor's remediation, whether the doctor repairs it unforced and the command that explains it, each client's Lua errors and backups, the last change and its undo, and the bar widget; `--cached` answers in a few milliseconds, without a scan | ✅ Done (milestone 3) |
| Previews that bind: a `--dry-run` of `install`, `update`, `sync`, `copy`, `remove`, `undo`, `backup restore`, `backup prune` or `errors dismiss` returns a token, and the real run with `--if-match` does exactly what it showed, client by client, or nothing (`plan_stale`). Stored plans: the dry run keeps its preview, and `apply <plan_id>` runs exactly it | ✅ Done (milestone 3; `install`, `update`, `sync` and `copy`, stored plans and `apply` in milestone 4) |
| The Omarchy bar widget: `omarchy install` renders it for this binary, validates it, swaps it in atomically and puts it on the bar; `omarchy status` says whether it is current and on the bar, and `status` reports it on every call (an outdated one is warned about and turns read-only); `omarchy remove` takes it away. The button shows a badge for updates (urgent for problems and new Lua errors) and a summary on hover, and right-click checks for updates; its keyboard panel has its frame (summary, Check now, Undo last, the client filter, the view switcher, progress and results, the confirm dialog) and its five views (Updates, Installed, Issues, Backups, Search, with the restore chooser); on its own it follows a game starting and exiting, takes the after-play checkpoint and reads the session's Lua errors, checks for updates when due, and notifies about new errors (and, when asked, new updates); its safety rules are scanned on every gate | ✅ Done (milestone 3) |
| `completions`: a static completion script for bash, zsh, fish, elvish and PowerShell (commands, subcommands, aliases, flags and fixed values), generated from the command tree; `--json` gives `{shell, script}` | ✅ Done (milestone 4) |
| `robot-docs guide`, `robot-docs schema` and `robot-docs errors`: the agent guide, every output's JSON Schema and the error and warning catalog, from the binary | ✅ Done (milestone 4) |
| `robot-docs install-skill`: the agent guide written byte for byte as `SKILL.md` where coding agents load skills (`$HOME/.claude/skills/omaddons`, or `--dir`), never through a symlink or into a client, a Wine prefix or omaddons' own folders, keeping a hand-edited copy as `SKILL.md.bak`, recorded in history | ✅ Done (milestone 4) |
| `apply <plan_id>`: a stored preview run exactly, by one applier at a time, every source checked again, each client reported (`committed`, `unchanged`, `stale`, `failed`) | ✅ Done (milestone 4) |
| `doctor` and `doctor diagnose`: what is broken among 31 failure modes, with evidence and the exact remediation, each check under a budget and isolated from the others, in a run directory of its own, with the doctor's own exit codes; `--since` compares with an earlier run; `doctor explain` shows one failure mode in full with its live instances; `--online` also checks GitHub's REST budget, whether GitHub takes the configured token and whether the sources your addons come from answer, one logged request each | ✅ Done (milestone 4) |
| `doctor fix`: the findings 10 of those failure modes' fixers repair (a damaged cached archive quarantined, an interrupted change finished by omaddons' own recovery, a stuck journal set aside, a credential file made private), as one journaled transaction, previewed and bound with `--if-match`; a forced repair runs only when named, with `--force --yes`. `doctor diff` shows what a fix would change, or what changed since a run. `doctor undo` (or `undo <tx>`) reverts a repair, strictly. `doctor gc` deletes old runs and what they kept by a retention plan that keeps whatever an unfinished change needs, previewed and bound | ✅ Done (milestone 4) |
| `capabilities`: every command, flag, value, alias, exit code, error and warning code, action, limit, source, family, client-rules row and `-t` form, generated from the code, offline, in milliseconds | ✅ Done (milestone 4) |
| `status` on the doctor's registry: its issues are the doctor's findings of every `cached` check (`--cached`) or every `cached` and `quick` check, from the doctor's own detector loop on what `status` already read, so the two never disagree | ✅ Done (milestone 4) |
| Intent inference: a client named where a command or argument belongs (`omaddons retail update`), a `-t` within two edits of a client here (`-t retial`), or a mistyped flag or value gets a fix that keeps your command and corrects only that word, and a mistyped command never gets a fix that runs a different command that changes addons (see [Choosing a target](#choosing-a-target-with--t) and [Command reference](#command-reference)) | ✅ Done (milestone 4) |
| The pre-freeze contract audit: every key and value held to the [naming rules](#naming-rules), the rule test's allowlist emptied and closed, every `details` key given one shape, and every declared `details` key shown by a real envelope of the contract suite | ✅ Done (milestone 4) |
| Verbosity levels: `-v` adds detail fields and `debug` events, as it always did; `-vv` also adds `trace` events on stderr (the release memo's decisions with their probe and fetch times, discovery's routes, waits for the global lock) and changes no stdout | ✅ Done (milestone 4) |
| The contract's golden: the semantic manifest (`tests/contract/manifest.json`) and its readable projection (`wire_names.txt`), generated from the code, committed, and compared with it in every gate; since the first tag, also with the released baseline (`tests/contract/released/1.0.json`), so a change that does not carry its `schema_version` bump fails (see [Stability](#stability)) | ✅ Done (milestone 4) |
| The first tag, `v0.1.0`: contract 1.0 frozen and its released baseline committed, after a checkpoint run on the real machine (WoW: Forever and the 3.3.5a client; the four clients uninstalled before it are checked again once they are back) | ✅ Done (milestone 4, 2026-11-08) |
| A user-first overhaul of the Omarchy bar widget | 🗓️ Planned |
| Launch-week fixes: the [Known issues](#known-issues), a `--help` overhaul to the standard of `br` and `bv`, and dynamic shell completion of client ids and addon names | 🗓️ Planned |
| Optional CurseForge and Wago (bring your own key), release packaging | 🗓️ Milestone 5 |

`omaddons outdated` lists what has updates, and `omaddons update` installs them on every client, one transaction per client, all undone by one `omaddons undo`. Two kinds of addon are left alone. A `url:` ref's release is the URL itself, so there is nothing newer to fetch. A pinned addon (by `pin`, or installed with `--version`) stays where it is (`skipped`, reason `pinned`) until `unpin`.

---

## Design philosophy

**1. The game's rules decide.** A host's flavor tags, a `release.json` and a file name order the candidates; they never make a folder install that the client would not load. What decides is the TOC file the client would load from each folder in the downloaded archive, judged by that client's rules row (suffixes, interface range, game types). So omaddons refuses the retail build of an addon for Classic Era even when the release metadata claims otherwise, and it installs a Classic module on Forever only if Forever would load it.

**2. Every change is a transaction.** There is one engine and one filesystem writer (`src/fsx/`). Every install, update, remove and undo stages its folders on the client's own filesystem and swaps them in under a journal and a lock held only for the commit window. Then it records the actor, the argv and the reason. A test in the gate fails if any `std::fs` write call appears outside `src/fsx/`.

**3. Keyless and private by default.** Everything that works today needs no account. The network is a closed allowlist of hosts, every request is logged locally, and `--offline` is absolute. CurseForge and Wago will be strictly opt-in, bring your own credential, and their data will never be cached.

**4. The CLI is the product.** Humans read the terminal output, agents read the JSON envelope, and the Omarchy widget renders the same envelope without logic of its own. Commands never print: they return typed outcomes that one output layer renders, and clippy denies `print!`. Every error names its fix: an argv an agent can run, or a sentence when no command would help.

**5. Detection is data.** Products, the version-to-family table, interface ranges and client-rules rows are tables in `src/rules.rs`. Each can be patched from `config.toml`: a new product code, a changed version range or load rule for a known family, or a misdetected private server is a config change, not a release. A new family (an expansion line with its own `release.json` flavor) still needs a release, because families are a closed set in code.

---

## How omaddons compares

| | **omaddons** | WowUp | CurseForge app | instawow | Unzip by hand |
|---|---|---|---|---|---|
| Interface | CLI + JSON | GUI (Electron) | GUI | CLI, optional GUI | File manager |
| Native on Linux | ✅ Linux only | ✅ | ✅ AppImage, .deb | ✅ | ✅ |
| Finds clients inside Wine/Proton, Steam, Lutris, Heroic and Bottles prefixes with no setup | ✅ | ⚠️ ¹ | ⚠️ ¹ | ⚠️ ¹ | ❌ |
| Supports private-server clients 1.12.1 to 5.4.8 | ✅ | ❌ | ❌ | ❌ | ⚠️ by hand |
| Checks the archive's TOCs against the client's load rules before writing | ✅ | ⚠️ host metadata | ⚠️ host metadata | ⚠️ host metadata | ❌ |
| Undo installs and removals byte for byte, SavedVariables included | ✅ | ❌ | ❌ | ⚠️ `rollback` of updates | ❌ |
| Settings (`WTF/`) backups and restore | ✅ automatic, whole or one addon's, undoable | ✅ by hand, whole | ❌ ² | ❌ | ⚠️ copy `WTF/` |
| Lua error triage: which addon, how often, whether an update may fix it | ✅ `errors`, every client | ❌ | ❌ | ❌ | ⚠️ BugSack in game |
| Update everything in one command | ✅ `update`, every client in one run | ✅ | ✅ | ✅ | ❌ |
| GitHub releases | ✅ keyless | ✅ | ❌ | ✅ | ⚠️ |
| WoWInterface, Tukui | ✅ keyless | ✅ | ❌ | ✅ | ⚠️ |
| CurseForge | 🗓️ M5, opt-in | ✅ WowUp-CF build | ✅ | ✅ | ⚠️ |
| Wago Addons | 🗓️ M5, opt-in | ✅ | ❌ | ✅ | ⚠️ |
| Documented machine contract for agents | ✅ | ❌ | ❌ | ❌ | ❌ |

¹ They detect standard install locations; other clients are added by pointing the app at their folder.

² The CurseForge app documents no settings backup: its support FAQ suggests a sync profile, or copying the `AddOns` folders.

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

The release profile uses thin LTO, a single codegen unit and stripped symbols; the binary is about 23 MB, with the catalog, the agent guide and the bar widget embedded.

For Claude Code: `omaddons robot-docs install-skill` installs the agent guide as a skill (see [`robot-docs install-skill`](#robot-docs-install-skill)).

### Shell completions

`omaddons completions <SHELL>` prints a completion script for bash, zsh, fish, elvish or PowerShell, generated from this build's own command tree. Save it where your shell looks for completions. Generate it again after upgrading: a script from an older build does not know newer commands.

**bash** (bash-completion loads it on the first Tab):

```bash
mkdir -p "${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
omaddons completions bash > "${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions/omaddons"
```

Without bash-completion, add `source <(omaddons completions bash)` to `.bashrc`.

**zsh** (a directory on `$fpath`, read by `compinit`): `omaddons completions zsh > "${fpath[1]}/_omaddons"` works when that directory is yours. Often it is a system directory; then use one of your own:

```zsh
mkdir -p "$HOME/.zfunc"
# in .zshrc, before compinit:
fpath=("$HOME/.zfunc" $fpath)
# then:
omaddons completions zsh > "$HOME/.zfunc/_omaddons"
```

Start a new shell. If the new script is not picked up, delete `.zcompdump` in your home directory.

**fish** (loaded on first use):

```fish
omaddons completions fish > "${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions/omaddons.fish"
```

**elvish:** add `eval (omaddons completions elvish | slurp)` to `rc.elv`.

**PowerShell:** add `omaddons completions powershell | Out-String | Invoke-Expression` to `$PROFILE`.

What completes: commands, subcommands, aliases, flags, and fixed values such as `--format json`, `--progress`, `search --source`, `omarchy install --section`, `auth github`'s actions and the shells. What does not: client ids and addon names, because the scripts are static (see [Known limitations](#known-limitations)); completing them is planned among the launch-week fixes. Packaged completions are milestone 5.

### Why there is no installer or package

The repository is private, and the project deliberately has no CI, public distribution or public installer until it fully works on its owner's setup (vision decision D16). Shell completion scripts come from `omaddons completions` (see [Shell completions](#shell-completions)); packaged completions, a local `PKGBUILD`, a man page and a version scheme are planned for milestone 5. The public installer is reconsidered after that.

### Uninstall

```bash
omaddons omarchy remove                # if you installed the bar widget
cargo uninstall omaddons
rm -rf ~/.config/omaddons ~/.local/state/omaddons ~/.cache/omaddons
rm -rf ~/.claude/skills/omaddons       # if you installed the agent skill (or the --dir folder)
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

**6. Optionally:** read the last session's Lua errors (BugGrabber captures them; `omaddons install buggrabber` adds it), back up a client's settings by hand, and, on Omarchy, put the widget on your bar:

```bash
omaddons errors
omaddons backup create -t retail
omaddons omarchy install
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
  2 addons in 2 folders · 1 unmanaged · 1 protected · 0 linked folders

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

Discovery runs whenever a command needs the clients: every command except `auth`, `config`, `net-log`, `help`, `completions`, `robot-docs`, `capabilities`, `plans show` and `targets rm` (but `targets rm --purge`, which checks that the client is no longer installed). The doctor's checks scan only when one of them needs the clients, and never write the discovery cache. Each walk goes at most 4 levels deep, and all the walks in one run share a budget of 50,000 directories.

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

**An uninstalled client is no target.** Battle.net uninstalls a client by dropping its row from `.build.info` and deleting its files except `Interface/`, so the flavor folder stays, holding omaddons' `Interface/.omaddons/` and the AddOns. Such a folder is no client and is skipped without a warning, but only when all of these hold:

- it has no `.flavor.info`;
- `.build.info` has a `Product` column, and no row names the folder's product;
- the folder holds no `wow*.exe`;
- `product.db` names neither the folder nor its product.

Anything less certain keeps the folder a target: a client being patched keeps its `.flavor.info` and executable, and one being installed has its `.build.info` row before its executable appears. A registered client that Battle.net uninstalled is `missing`, as a client whose directory is gone is (attention `target_missing`, and `fm-userland_state-target-vanished` in [`doctor`](#doctor)). `-t` naming it is refused with `target_not_found`, and nothing writes to its folder. Installing it again through Battle.net brings it back under its old id and key. `omaddons targets rm <id>` forgets it for good. The folder stays as Battle.net left it, with what omaddons kept there (its `WTF/` backups and undo snapshots), until you delete it, or until `omaddons targets rm <id> --purge` deletes what omaddons kept, after a preview (see [`targets`](#targets)).

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
| `mainline` | 60000 and up (120000 and up on 12.x clients: the [floor](#how-omaddons-decides-what-loads)) | retail, PTR, XPTR, beta |
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

Every target that the first matching kind matches is selected (a client reached twice is listed once). `status`, `list`, `info`, `outdated`, `update`, `export`, `sync`, `adopt`, `pin`, `unpin`, `search`, `backup` and `errors` cover them all: `-t vanilla` reads every vanilla client, `-t mainline` every retail, PTR, XPTR and beta client. A command that must land on one client (`install`, `remove`, each side of `copy`, `adopt --ref`, and `history` and `undo`, which name one) refuses a selector that matches two with `target_ambiguous` (exit 3): the first candidate is the `fix`, and each other one is an `alternatives` entry. So does config `default_target` when it matches two.

**A mistyped selector.** When `-t` (or `copy`'s `--from` or `--to`) matches nothing, the error is `target_not_found` (exit 3). If a client id, game mode or family that selects a client on this machine is within two edits of what you typed (`retial`, `wtolk`), the fix is your own command with that word in its place, the message names it, further close words follow as `alternatives`, and `omaddons targets` comes last. The fix keeps your command even when it changes something, because only the client word changed. Nothing is corrected on its own: the command you typed did nothing. With no close word, or when the selector came from config `default_target`, the fix stays `omaddons targets`. Product codes, flavor folders, keys and paths are never guessed.

**`-t all`** (any case) selects every client, test clients included, in id order. For most commands that is the same as no `-t`; where a command's default leaves clients out (`backup create` backs up live clients, `backup list` lists clients that have backups), `-t all` includes them. On a command that lands on one client (`install`, `remove`, `adopt --ref`, `history`, `undo`, `backup restore`, `backup import`) it is `target_ambiguous`, one fix per client, unless there is only one client. A target named `all` (registered by an older build, or found in a flavor folder `_all_`) wins over the keyword, with the warning `target_id_reserved`; rename it with `omaddons targets rename all <new id>`.

An id beats a family, and the legacy default ids `vanilla`, `tbc`, `cata` and `mists` are also family names. With a 1.12.1 server present, `-t vanilla` selects it, not Classic Era. With a 5.4.8 server, `-t mists` selects it, not Mists Classic. Use `-t era` and `-t classic` (or `-t mop`) for the Battle.net clients, or rename the server's target.

`install` is the only command that picks a target when `-t` is absent. It uses config `default_target`, then the registry default (`targets default`, if that client is present), then the only live client. With several live clients and no default, it dry-runs the refs against every live client (test clients excluded) and installs only if exactly one client loads every ref that loads anywhere (a client is ruled out only by `no_compatible_release` or `wrong_flavor`). `status`, `list` and `history` cover every target unless `-t` narrows them. Bare `undo` reverts the most recent run, every transaction of that invocation on every target; `undo -t <id>` reverts that target's newest transaction. `remove` finds the target that manages the named addons; the names must all agree on one, else `target_ambiguous` asks for `-t`.

**A client in the wrong place.** A client named where a command or an argument belongs is a usage error whose fix moves it into `-t` and keeps your command: `omaddons retail update` → `omaddons update -t retail`, `omaddons sod install questie` → `omaddons install questie -t sod`, `omaddons status era` → `omaddons status -t era`, `omaddons info dbm era` → `omaddons info dbm -t era`. A client word is a built-in id, product code, flavor folder, game mode or family, or the id of a registered target. A catalog addon of the same name is never moved, and nothing is moved when `-t` is already given. `error.details` is `{target, command}`. `config`, `targets`, `net-log`, `auth`, `omarchy`, `copy`, `completions` and `help` do not take `-t` this way and keep their usual fix. `install`, `update` and `remove` check their names before anything else: one that is a client and no addon (`omaddons remove dbm retail`) makes the whole command a usage error whose fix moves it into `-t` (`omaddons remove dbm -t retail`), so nothing is removed from another client first. A name that is an installed addon, a catalog name or alias, a ref, or (for `install`) a path is never moved; a name that is no client fails alone, as before; with `-t` given, nothing is moved.

### How omaddons decides what loads

For each top-level folder in the archive:

1. **Pick the TOC the client would load.** Candidates are `<Folder>.toc` and, on clients that support several TOCs, `<Folder>_<Suffix>.toc` or `<Folder>-<Suffix>.toc` (case-insensitive). Precedence follows the client's row (table below): its game suffixes in order, then its family suffix, then the plain TOC. A suffix the row does not list never loads. Legacy clients read the plain TOC only. When two TOC names differ only in case, the byte-wise smallest wins.
2. **Read its lines as the client does.** On retail 12.0.7 and later, on every Forever, Classic Era, Anniversary, Titan and Mists client (their current builds carry 12.0.7's TOC changes), and on a Battle.net client of unknown family, a `##` line may carry inline directives such as `[AllowLoadGameType classic]`, anywhere in the line. The Wrath and Cataclysm Classic rows, retail builds before 12.0.7 and every legacy row read lines as written. Such a line applies only on the clients they admit. A line whose directive cannot be known offline, such as a text locale, counts for nothing, and a dependency named only there is reported, never installed. For one key the last line that applies wins. The exceptions add up instead: dependencies, `LoadWith`, `LoadManagers`, the saved variables and the game-type lists. Older clients read every line as written. The title, the dependencies and the steps below all use this reading.
3. **Game types.** On modern clients, an `AllowLoadGameType` that excludes this client, or an `ExcludeLoadGameType` that names it, means the folder does not load. A game type must name the client: one of its row's game types (Titan answers to both `wrath` and `titan`, Forever to `forever` and `camelot`) or its family suffix, lowercased, so `classic` admits every Classic client and `mainline` admits retail and Forever. An `AllowLoadGameType` that names no game type omaddons knows is treated as excluding the client.
4. **Interface.** Comma-separated `## Interface:` lists are read where the client supports them; otherwise only the leading number counts. The folder loads if a value falls inside the client's family range. A newer interface loads. An older one gets an `out_of_date` warning, because the client loads it only with "Load out of date AddOns" enabled. A folder with no value in range is another client's, unless its `AllowLoadGameType` names one of the client's own game types (its row's, below: `tbc` for Anniversary, `camelot` or `forever` for Forever; never the family-level `classic` or `mainline`). That is the author building it for this client, so it is `out_of_date` too, and the warning lists its interfaces. Titan Panel 9.3.5's TitanAmmo declares `## Interface: 11509, 16001, 205056` (205056 a typo for 2.5.x) with `AllowLoadGameType: camelot, tbc, vanilla`, and installs on Anniversary this way. Legacy clients ignore game types, and an `ExcludeLoadGameType` naming the client still wins.

   **Unverified: a newer-only interface.** Whether a folder whose in-range values are all newer than the client's (`120200` on a 120105 client, a TOC bumped ahead of its patch) is out of date is not known. omaddons reports it current. The client makes the comparison in compiled code: Blizzard's addon list only reads its verdict (`reason == "INTERFACE_VERSION"`). The community sources all say the interface must *match*, but none states or observes the newer case:
   - [warcraft.wiki.gg, TOC format](https://warcraft.wiki.gg/wiki/TOC_format): "If an addon doesn't set an interface version that matches the client's, then it's classified as out-of-date" (uncited).
   - [WoWUIBugs #531](https://github.com/Stanzilla/WoWUIBugs/issues/531), the comma-list proposal, labelled "Acknowledged by Blizzard" and reported implemented in 4.4.0.53750 by its author: "flagged as out of date if no values match".
   - Two forum posts disagree, and neither shows a test. [One from 2007](https://wowinterface.com/forums/showthread.php?p=71226) says a TOC bumped to the 2.3 PTR's `20300` "would" be out of date on 2.2 ("20300 =/= 20200"). [One from 2019](https://www.wowinterface.com/forums/showthread.php?t=57155) says only major.minor counts.

   On 2026-10-24 no addon installed on this project's test clients had a newer-only TOC, so their SavedVariables could not settle it. An in-game check would: install a copy of an addon with a newer-only interface, leave "Load out of date AddOns" off, and see whether it writes its SavedVariables.
5. **The floor.** Retail 12.0 and later load no Mainline TOC below interface 120000, "at all (with no player override)" (warcraft.wiki.gg, Patch 12.0.0 API change notes, "Forced interface matching for 12.0.0"). On such a client a folder whose in-family values are all below 120000 is `wrong_flavor` with reason `below_client_floor` and the `floor`, never `out_of_date`, so nothing suggests enabling out-of-date addons for it. The floor binds by the client's own interface, not by its row: 12.0.0 to 12.0.6 builds share the `mainline-10.2.7` row with 11.x builds, and only they are held to it. It is the row field `interface_floor` (see [Rules overrides](#rules-overrides-advanced)).

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

The `mainline`, `mainline-12.0.7` and `mainline-10.2.7` rows carry the interface floor 120000, binding their 12.x clients only; no other row has one. The real Forever client was observed loading `_Camelot` TOCs (2026-10-21); a TOC with the `_Forever` suffix alone, and the client's order when an addon ships both, are not yet observed.

The principle is to install what the author built for this client. Each download is then graded as a whole:

- At least one folder must load.
- **Clean.** The publisher's `release.json` declares this client for this very file, and the folders the client would not load are skipped and listed in `folders.skipped` (that is how DBM's retail-only modules stay out of a Classic install). Or the addon's own folder loads and every skipped folder rules itself out: a game-type gate, TOCs only for other clients, or a dependency on such a folder.
- **Leaves folders out.** The addon's own folder loads, and the other skipped folders are built for other clients only: they list only other clients' interfaces, have no Interface the client reads, or lie below the client's floor (or require such a folder). Those folders are left out, listed in `folders.skipped` with their reason, and each gets a `folder_left_out` warning saying what it lists ("ArkInventoryRules_Example lists only interface 120105 (mainline), built for other clients, so it is left out (classic would list it as out of date)"). The game itself would list such a folder as out of date rather than skip it.
- **Refused** otherwise: nothing loads, or the addon's own folder does not.
- **The publisher's word against it.** A file whose `release.json` entries name no client of this family ("release.json lists Foo.zip for mainline only"), or whose file name names only other clients ("its file name Foo-1.0-retail.zip names mainline only"), is refused even when its TOCs load. An entry names a client by its interface's range, else its flavor name. A file-name word that is part of the addon's own folder or repository names (`Foo_Classic-1.0.zip` shipping `Foo_Classic`) says nothing, and a bare `classic` counts only against retail and Forever. On a legacy or unknown client a `release.json` cannot name the client, so it says nothing. Nor does either when the addon's own folder carries a TOC named for this client (one of its row's game or family suffixes, never the plain `Folder.toc`) at an interface in the client's range: that is the author building for the client, and the metadata is incomplete. Raider.IO's `release.json` names mainline only, while `RaiderIO/` ships `RaiderIO_Classic.toc` at 50504 and `RaiderIO_Vanilla.toc` at 11509, so it installs on Mists Classic and Classic Era. Tukui's patch list and WoWInterface's compatibility tags are never evidence, and only GitHub's asset names carry file-name hints.

Candidates are tried in rank order (below). The first clean one with no evidence against it installs; when there is none, the first one that leaves folders out does; otherwise the ref is refused. A reinstall's cached archive carries no rank, so it wins only clean; for a build that leaves folders out the source is asked for the release again.

When no candidate could be downloaded at all and none was judged, the download is the problem, not the client: the error is the download's own (`hash_mismatch`, with `tried`; or `network`, for a connection that failed after its retries or a web page served instead of the file). Otherwise, when no candidate installs, the error is `no_compatible_release`, with every asset tried in `error.details.tried` (JSON output), each `{asset, code, message}`: `code` is the refusal (`wrong_flavor`, `older_than_installed`, `packaging_unsupported`), `look_back` for a release a look-back passed over, or the error code of an asset that could not be read, and `message` says why. The error's own message says what decided the best-ranked candidate: the folder verdict ("Foo.zip: Foo lists only interface 120105 (mainline)", or, below the floor, "retail never loads an addon whose interface is below 120000, whatever the player enables") or the evidence. For a `url:`, `file:` or `--file` archive, it is `wrong_flavor`, saying the same.

Release metadata also orders the candidates. First come assets whose metadata names this flavor, then a file-name hint for it, then unannotated assets, then file-name hints for other flavors, then metadata for other flavors only (on a legacy or unknown client, every asset with metadata lands here). Within a tier, full builds come before `-nolib` builds; within the metadata tier, each of those runs exact interface, then same major.minor, then any. Assets are downloaded and verified in that order, until a clean one is found or none is left. omaddons falls back to an older release only on GitHub, when the newest release has no build for the client at all ([look-back](#a-release-per-client-github-look-back), below), and an update of an addon adopted at its TOC's version never installs a release built for an older interface (`blocked`, `older_than_installed`; see [`adopt`](#adopt)).

#### A release per client (GitHub look-back)

Some authors publish one GitHub release per client: VuhDo tags `v3.218-titan`, then `v3.217-mists`, then `v3.216-bcc`, so the newest release is only ever one client's. When no candidate of a `gh:` addon's newest release on its channel installs on a client, and that release's own `release.json` declares no build for it, omaddons looks back. On the stable channel it reads `releases.atom` (the 10 newest releases, `LOOK_BACK_MAX_RELEASES`), newest first. It reads each older release's `release.json` from github.com and stops at the first one that declares the client: an entry whose interface is in the client's range, or, failing that, whose flavor names it. It also stops after three releases in a row that declare the same other clients (`LOOK_BACK_SAME_TRACK`): such a repository publishes one track, a retail-only addon for one, and reading the rest of the feed would cost two requests a release for nothing. Releases named like prereleases and releases without `release.json` neither count toward the three nor break the run. That release is then installed as any release would be (its TOCs still decide), with an `older_release_for_client` warning naming both releases. The feed does not mark prereleases, so the release found is confirmed through GitHub's API, one request; none when the newest release fits. A release GitHub marks a prerelease is passed over (another request for each such release), and so are a tag named like one (`-alpha`, `-beta`, `-rc`, `-pre`, not asked about) and a release without a usable `release.json`. When the feed or the API cannot be asked (offline, or the API's budget spent), the look-back stops and the refusal's `tried[]` says why (`{asset: "look_back", code: "rate_limited", message: "…"}`). The newest release is refused all the same, so `update` still reports the addon blocked, and nothing is remembered: the next run looks back again. A refusal that is only the [adopted build's floor](#adopt) (`older_than_installed`) never looks back: an older release is older still. Nor does a newest release that declares the client but does not load there (OmniCC 11.2.8 below retail 12's floor): its build for the client exists, and an older one would fail the same way, so the refusal names the newest. When nothing declares the client, the refusal (`no_compatible_release`) lists each release looked at in `tried[]` (`{asset: "<tag>/release.json", code: "look_back", message: "…"}`).

- **Remembered against the newest tag.** The answer is kept in the release memo for each client family. While the newest release is unchanged, `outdated`, `update`, `status` and `list` reuse it without a request. A new newest release looks back again.
- **Never a downgrade.** An installed release that is the one found or newer than it stays. Newer means by place in the feed, or, when the feed does not hold both, by dotted version (`v3.217-mopc` before `v3.218-mopc`). A release neither places (deleted, a tag like `mists-hotfix`) stays too. An older one is an update (`update_available`, `declared`) to the release found.
- **An alpha series.** When every older release in the feed is named like a prerelease, the feed cannot reach the stable releases. The look-back then probes the newest stable tag's siblings: its version with each suffix the feed's tags use, a prerelease mark taken off. VuhDo's `v3.218-catac`, with a feed of `v3.219-mopc-beta-3`, `v3.219-tbcc-alpha-18`, `v3.219-beta-3` and so on, gives `v3.218-mopc`, `v3.218-tbcc`, `v3.218-vanilla` and `v3.218`. Each probe is one `release.json` request, a 404 when no such release exists, and the found one is confirmed as any other is. A newest tag that does not start with a version (`Plater-v661`) probes nothing.
- **Beta and alpha.** These channels list the 20 newest releases through GitHub's API, and the look-back walks that same listing, newest first, with no further API request. Prereleases count there, and the listing says which releases are; the same three-in-a-row stop applies. VuhDo's `--channel beta` on Mists Classic installs `v3.219-mopc-beta-3` when the newest prerelease is `v3.219-catac-beta-3`.
- **Never for a named release.** `--version`, a pasted asset link and `--file` name their release themselves and never look back.

---

## Sources and refs

A **ref** names where an addon comes from. Everything that works today is keyless.

| You type | It becomes | Where the release comes from |
|---|---|---|
| `weakauras`, `dbm`, `bw` | a catalog entry | [the built-in catalog](#the-built-in-catalog), by name or alias (case-insensitive); an installed addon's name also works. Any other name is `not_found`, with `omaddons search <name>` as the fix (no request is sent), except a client's name (`retail`, `era`, `sod`), which is a usage error that moves it into `-t` |
| `gh:Owner/Repo` or `Owner/Repo` | GitHub | the stable path reads `releases/latest` and its `release.json` from github.com, with **no REST API calls**. The REST API (60 requests an hour without a token) is used only for `--channel beta`/`alpha`, for releases without a usable `release.json` (their `.zip` assets become the candidates), for pinned tags that lack one, and once to confirm a [look-back](#a-release-per-client-github-look-back)'s release is no prerelease |
| `https://github.com/o/r` | `gh:o/r` | as above. `/releases/tag/T` pins tag `T`, `/releases/download/T/F` pins tag `T` and installs exactly the asset `F` (or nothing: `not_found` naming the release's assets, or refused when it does not load), and `/tree/branch` becomes a `git:` ref. A tag may hold `/` and `+`, written as GitHub writes them or percent-encoded. A link that names no repository (`https://github.com/o`) is `ref_invalid` |
| `git:https://github.com/o/r#branch` | git on GitHub | the commits Atom feed resolves the branch or tag to a commit, and codeload serves a zip of that exact commit (a rev the feed does not serve is fetched as a tag by name). **No git binary.** A 40-hex sha is used as is. No `#rev` means the default branch |
| `git:https://codeberg.org/o/r#tag` | git elsewhere | the `git` binary (2.32+), only for hosts listed in `git_hosts`: argv only, a cleared environment, an empty home and no user or system git config, https only, no redirects and no credentials (see [Network and privacy](#network-and-privacy)). Branches win over tags with the same name (on GitHub, the commits feed decides) |
| `url:https://…/addon.zip` | a direct download | only from the always-allowed hosts: `github.com`, `codeload.github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`, `api.github.com`, `cdn.wowinterface.com`, `api.mmoui.com`, `api.tukui.org` and `repos.wowace.com`. Any other host, `git_hosts` included, is `host_blocked` |
| `./addon.zip`, `./AddonFolder`, `file:path` | local | a zip (a typed path must end in `.zip`, and the magic bytes are checked) or a folder, read in place |
| `wowi:<id>`; WoWInterface links: `…/downloads/info<id>-Name.html`, `…/downloads/download<id>-Name`, `…/downloads/fileinfo.php?id=<id>`, and the download links `cdn.wowinterface.com/downloads/getfile.php?id=<id>&…` and `www.wowinterface.com/downloads/getfile.php?id=<id>` | WoWInterface | keyless: one `filedetails` request to `api.mmoui.com` per 50 addons, then the file from `cdn.wowinterface.com`, checked against the MD5 the API publishes; when the API publishes none, the file installs with a `download_unverified` warning and is identified by its date (`wowi:<id>@d<date>`, else `@v<version>`). WoWInterface serves only a project's current file, so `--version` matches only its current version. A release is its file: the same bytes uploaded again are no update, and any new file is one. Its compatibility tags lag behind patches, so they never rule a file out; the TOCs decide |
| `tukui:<slug>` (`tukui:elvui`, any case); Tukui links: `tukui.org/addon/<slug>`, `tukui.org/addons/<slug>`, `tukui.org/<slug>`, `download.php?ui=<name>` | Tukui | keyless: one list request to `api.tukui.org` per command, and right before each download a fresh download link, which expires within seconds and is never stored or logged (`net.log` shows `/v1/download/<slug>/…`). The download is checked against the MD5 in its `ETag`; a download without a usable one installs with a `download_unverified` warning. The list's supported patches only rank the file; the TOCs decide, so ElvUI also installs where its TOCs load but the list says nothing (Forever) |
| `wowace:<slug>` (`wowace:bug-grabber`), `wowace:<slug>#<tag>`, `wowace:<slug>#r<rev>`, `wowace:<slug>#r<rev>:<directory>`; WowAce links: `repos.wowace.com/wow/<slug>/…` (`…/tags/<tag>/`, `…/!svn/bc/<rev>/trunk/`, `…/!svn/bc/<rev>/<directory>/`) and `wowace.com/projects/<slug>` | WowAce | keyless, from WowAce's public Subversion repository at `repos.wowace.com`. One `PROPFIND` of the project's `tags/` gives every tag's revision (the revision at which that tag last changed); the newest stable tag (`v12.1.1`, not `-beta`, `-alpha`, `-rc` or `-pre`) installs, and `#<tag>` names another. `#r<rev>` is trunk as of that revision, with no tag list asked for, and never updates. `#r<rev>:<directory>` is another directory of the project as of that revision (not under `trunk/` or `tags/`; up to 8 segments of letters, digits, `.`, `_`, `+`, `-` or `!`), which is the addon's folder itself and is named after its last segment, versioned `r<rev>` and never updating: BugGrabber's 1.12.1 build is `#r43:tmp/trunk/!BugGrabber`. Every directory listing and file is fetched under `!svn/bc/<rev>/…`, the tree as of that one revision, so a tag edited while it downloads cannot mix revisions. The tree is built as the WowAce packager would build a project that needs only `package-as` and `@project-version@` (every file under the `package-as` folder, dotfiles left out, `@project-version@` in each TOC replaced with the tag), packed into a reproducible zip (the same revision always gives the same bytes) and kept in the download cache. Anything else the packager would do (`.pkgmeta` `externals`, `ignore`, `move-folders`, …, or a keyword such as `--@debug@`) refuses the release with `packaging_unsupported`, naming each, in `no_compatible_release`'s `tried[]`. The server sends no hash, so TLS and the TOCs are the check, as on GitHub's path, and no `download_unverified` warning is given. No changelog (`not_supported`) |
| `cf:<id>`, `wago:<id>` | CurseForge, Wago | parsed and refused with `source_disabled` ("arrives in M5"), unless `--file` supplies the archive |
| CurseForge and Wago links (`curseforge.com/…`, `addons.wago.io/…`) | | refused with `source_disabled` while the ref is parsed, even with `--file`: use the `cf:` or `wago:` form with `--file` |
| an `https://` link to a `git_hosts` host, or any other non-GitHub link ending in `.git` | `git:` | as `git:` above (`#fragment` becomes the rev). A github.com link ending in `.git` stays `gh:` |
| any other `https://` link | `url:` | refused at plan time with `host_blocked` unless its host is one of the always-allowed hosts above |
| `http://…` (also `url:http://…`, `git:http://…`) | | refused (`ref_invalid`), suggesting the https form when omaddons can fetch it; for a host it does not download from (`details.host`), the fix is to download the file and install the local path, and for a git host not in `git_hosts`, to add it (never for an HTTP host, which `git_hosts` cannot list: there the fix says to use a `gh:`, `url:`, `wowi:` or `tukui:` ref). When the https form is itself refused, its error stands (an `http://` CurseForge link is `source_disabled`) |

**Channels.** `--channel stable|beta|alpha`. The default is the addon's recorded channel, then `default_channel`, then `stable`. On GitHub, `alpha` behaves like `beta`: the newest non-draft release, stable or prerelease, among the 20 most recent (through the REST API), or, when it has no build for the client, the newest of them that declares one ([look-back](#a-release-per-client-github-look-back)). WoWInterface, Tukui, WowAce (its stable tags), `git:`, `url:` and local sources publish one stream, so there the channel is only recorded. When `releases/latest` finds no stable release (only prereleases, or none published), the error is `no_compatible_release`, with `--channel beta` as the fix and the repository's `git:` ref as the alternative. When it answers 404 (no releases, or no such repository), the error is `not_found`, with the `git:` ref as the fix. A renamed repository is followed.

**Pinning.** `--version V` installs exactly that release and pins it: a tag for `gh:` refs, a rev for `git:` refs, or a tag, `r<rev>` (a trunk revision) or `r<rev>:<directory>` for `wowace:` refs. On the `release.json` path, `1.2.3` also tries `v1.2.3`. It is refused for `url:` and `file:` refs, and as a usage error when it disagrees with the rev of a `git:` ref, or with the tag of a pasted release link (a leading `v` aside). WoWInterface and Tukui serve only a project's current file, so there `--version` matches only the current version. A pasted release link pins its tag the same way (GitHub's own links work as pasted, `+` and `/` in tags included). Installing a pinned addon again without `--version` changes nothing and warns `addon_pinned`, without fetching its releases, even when a newer release exists. `--version W` moves it to `W`, still pinned. `--version` naming the release already installed pins it without reinstalling anything: a transaction of kind `pin` that records each folder and that `undo` unpins. A pin that another omaddons process commits while an install is planning also holds: the install re-checks it under the lock.

**What an update keeps.** Re-installing an installed addon keeps its pin and its recorded channel unless `--channel` names another. An addon installed only because another one needs it (`reason: dependency`, from milestone 2) becomes `explicit` when you install it by name, except with `--reinstall`, which keeps the recorded reason. When the release to install is the one already there, these record changes still happen, as a transaction that moves no folder: `--channel beta` whose newest release is installed records the channel (item `edited`, `fields: ["channel"]`), and a repeat changes nothing.

**A browser download.** `--file ZIP` installs an archive you downloaded as the release of one ref, so it is recorded against that addon. `--file` also takes an unpacked folder, such as one you develop in. Its identity is a hash of its listing (every file's path, size and modification time, `.git` aside), as for a `file:` folder. Installing an unchanged folder again changes nothing (the item is `unchanged`), an edited one updates, and `--reinstall` installs it again regardless. `update` and `outdated` never replace it from upstream (it is `static`).

**Same name, another ref.** An addon is installed from one ref at a time. Installing an addon whose name is already managed on the client from another ref is:

- **another local archive of a local install** (`./Bar-2.2.0.zip` over `./Bar-2.1.0.zip`): the same addon, `updated` from its version, now recorded from the new archive;
- **any other ref** (`gh:demo/bar`, or `--file` under a ref, over a local install, or a second repository shipping the same addon): a takeover, refused with `ownership_conflict`. The fix adds `--force`, and the alternative removes the installed one. With `--force` the item is `updated` from the replaced version, an `ownership_transferred` warning says where it now comes from, and `undo` restores the original ref and folders.

`gh:` and `git:` refs of one repository are different refs. To move an addon to another source on purpose, `omaddons adopt <folder> --ref <ref>` re-points it in place.

**Ambiguity.** `owner/repo` that also exists as a local path, or a bare name that names more than one ref, gives `ref_ambiguous` with every candidate as a runnable `omaddons install …` command; `details.candidates` lists the refs and `details.matches` where each came from (`[{ref, from}]`). A bare name collects its candidates from installed addons on any client (by name or folder), the catalog, and a file or folder of that name in the current directory: a folder `elvui/` beside you and the catalog's ElvUI are ambiguous.

---

## The built-in catalog

66 entries are embedded in the binary: 62 addons with one entry each, and BugGrabber's four builds. Each was checked against its publisher on 2026-10-14 (BugSack, ElvUI and Tukui on 2026-10-16; TellMeWhen through BattlePetBreedID, and the WoWInterface entries, on 2026-10-20; BugGrabber on 2026-10-22; Clique's and HandyNotes' families again on 2026-11-07, when their TOCs had added WoW: Forever): the GitHub entries against their `releases/latest` tag, `release.json` and the addon roots inside the zip; the Tukui entries against the Tukui API's list and the folders and TOCs of the zip it hands out; the WoWInterface entries against the API's file details (the project's current file and its MD5) and the folders and TOCs of that file, MD5-checked (their families are where those TOCs load, since the site's compatibility tags lag); the legacy entries against their TOC's `## Interface` line; and BugGrabber against WowAce: the newest tag's TOC for the modern build, and trunk's TOC revision by revision for the legacy ones. The last column is the families the publisher declares (in `release.json`, or the TOCs' interfaces), less a family whose live client never loads the declared build: a mainline interface below retail 12's floor of 120000 declares no live retail client. It is shown by `info`, gives `search`'s `yes` or `no`, and orders lookups. A dependency or an adopted folder is matched first to entries declaring the client's family. For a dependency, other entries of the same generation are tried only after Tukui and WoWInterface. For an adopted folder, they claim it at catalog strength, but only when no entry declares the client's family. A built-in entry is never matched across generations, legacy on a modern client or the reverse. For a name without variants it never refuses an install: TOC verification decides. The live catalog probe records where publishers and their own TOCs disagree. As of 2026-10-15, Plater declares titan but ships no TOC Titan loads, and RaiderIO declares only mainline but also ships client-specific TOCs for vanilla and mists. Those in-range suffixed TOCs prove support, so RaiderIO installs on Classic Era and Mists Classic despite the incomplete declaration (see [load rules](#how-omaddons-decides-what-loads)).

**Variants.** One name can have a build per set of client families. Each is a `variant`, and their families never overlap, so `omaddons install buggrabber` installs the build that client's family has: the newest WowAce tag on every modern client, trunk at r152 on 3.3.5a and at r130 on 2.4.3, and the 1.12-era tree outside trunk at r43 on 1.12.1. A client no variant covers gets what a name the catalog does not know gets (`not_found`, with `search` as the fix), never another family's build. BugGrabber has no 4.3.4 or 5.4.8 build: every 4.3 and 5.4-era revision needs the WowAce packager (`@localization` keywords, filled in from CurseForge's localization service), so none stays keyless unless a CurseForge source (milestone 5) offers packaged files for them. Without `-t`, a name with variants resolves for the default target's family; with no default target, `install` probes every live client with its own build and skips one no build is known for. `info` shows the variant of the selected client (else the default target's, else the modern one) and lists the other variants' families. `update`, `pin` and `unpin` find an installed variant by the name or any alias. `search` lists one hit per name, judging each client by the build it would get.

| Name | Aliases | Ref | Declared families |
|---|---|---|---|
| WeakAuras | `wa`, `weakauras2` | `gh:WeakAuras/WeakAuras2` | vanilla, tbc, cata, mists, titan (**not retail**: 5.22.1 ships no Mainline TOC) |
| DBM | `deadlybossmods`, `dbm-core` | `gh:DeadlyBossMods/DeadlyBossMods` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| BigWigs | `bw` | `gh:BigWigsMods/BigWigs` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| LittleWigs | | `gh:BigWigsMods/LittleWigs` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| Plater | `plater-nameplates` | `gh:Tercioo/Plater-Nameplates` | mainline, vanilla, tbc, titan, mists, forever |
| Bartender4 | `bartender`, `bt4` | `gh:Nevcairiel/Bartender4` | mainline, vanilla, tbc, mists, forever |
| Questie | | `gh:Questie/Questie` | vanilla, tbc, wrath, mists, forever |
| OmniCC | | `gh:tullamods/OmniCC` | vanilla, tbc, titan, mists (11.2.8 declares mainline at 110207, which retail 12.x never loads) |
| Dominos | | `gh:tullamods/Dominos` | mainline, vanilla, tbc, mists, forever |
| Masque | | `gh:SFX-WoW/Masque` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| RaiderIO | `raider.io`, `rio` | `gh:RaiderIO/raiderio-addon` | mainline |
| MythicDungeonTools | `mdt` | `gh:Nnoggie/MythicDungeonTools` | mainline |
| AllTheThings | `att` | `gh:ATTWoWAddon/AllTheThings` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| BugSack | | `gh:funkydude/BugSack` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever (its `!BugGrabber` is BugGrabber below; `install bugsack` does not pull it, since it is an optional dependency: `omaddons install buggrabber bugsack`) |
| TellMeWhen | `tmw` | `gh:ascott18/TellMeWhen` | mainline, vanilla, tbc, mists, forever |
| ThreatPlates | | `gh:Backupiseasy/ThreatPlates` | mainline, vanilla, tbc, mists, forever |
| SilverDragon | | `gh:kemayo/wow-silverdragon` | mainline, vanilla, tbc, wrath, mists, forever |
| Capping | | `gh:BigWigsMods/Capping` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever |
| IceHUD | | `gh:parnic/ice-hud` | mainline, vanilla, tbc, titan, mists |
| Quartz | | `gh:Nevcairiel/Quartz` | vanilla, tbc, mists (3.7.17 declares mainline at 110207, which retail 12.x never loads) |
| Mapster | | `gh:Nevcairiel/Mapster` | mainline, forever |
| BadBoy | | `gh:funkydude/BadBoy` | mainline, vanilla, tbc, wrath, titan, cata, mists |
| NPCScan | | `gh:Torhal/NPCScan` | mainline |
| ClassicCastbars | | `gh:wardz/ClassicCastbars` | vanilla, tbc, wrath, cata, mists |
| AtlasQuest | | `gh:nanderson11/AtlasQuest` | mainline, vanilla, tbc, forever (it needs Atlas, which installs from WoWInterface) |
| VendorPrice | | `gh:ketho-wow/VendorPrice` | vanilla, tbc, mists |
| DBM-Vanilla | | `gh:DeadlyBossMods/DBM-Vanilla` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever (DBM's Vanilla and Season of Discovery mods) |
| Decursive | | `gh:2072/Decursive` | mainline, titan, forever (2.9.0-RC3 ships no other zips) |
| PremadeGroupsFilter | `pgf` | `gh:0xbs/premade-groups-filter` | mainline, mists |
| Rarity | | `gh:WowRarity/Rarity` | vanilla, mists (r751 declares mainline at 110207, which retail 12.x never loads) |
| SavedInstances | | `gh:SavedInstances/SavedInstances` | mainline |
| Simulationcraft | `simc` | `gh:simulationcraft/simc-addon` | mainline, forever |
| WarpDeplete | | `gh:happenslol/WarpDeplete` | mainline |
| RCLootCouncil | `rclc` | `gh:evil-morfar/RCLootCouncil2` | mainline |
| BigDebuffs | | `gh:jordonwow/bigdebuffs` | mainline, vanilla, tbc, titan, mists (v68's mainline interface is 12.0: out of date on 12.1) |
| OmniBar | | `gh:jordonwow/omnibar` | vanilla, tbc, wrath, cata, mists (v34 declares mainline at 110200, which retail 12.x never loads) |
| Cell | | `gh:enderneko/Cell` | vanilla, tbc, titan, cata, mists (r274 declares mainline at 110207, which retail 12.x never loads) |
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
| TitanPanel | | `wowi:8092` | mainline, vanilla, tbc, mists, forever (9.3.5's TitanAmmo lists 205056, a typo, under `AllowLoadGameType: tbc`, so it installs on Anniversary out of date) |
| Clique | | `wowi:5108` | mainline, vanilla, tbc, titan, cata, mists, forever |
| Prat | `prat-3.0` | `wowi:10783` | mainline, vanilla, tbc, mists, forever |
| WIM | | `wowi:5342` | mainline, vanilla, tbc, titan, mists, forever |
| GTFO | | `wowi:17996` | mainline, vanilla, tbc, titan, mists, forever |
| HandyNotes | | `wowi:15716` | mainline, vanilla, tbc, mists, forever |
| ElvUI | `elv` | `tukui:elvui` | mainline, vanilla, tbc, titan, mists, forever (one zip for every client) |
| Tukui | | `tukui:tukui` | vanilla, mists (20.463's Mainline TOC is 110207, which retail 12.x never loads) |
| BugGrabber (`modern`) | `!buggrabber`, `bug-grabber` | `wowace:bug-grabber` | mainline, vanilla, tbc, wrath, titan, cata, mists, forever (the newest WowAce tag) |
| BugGrabber (`r152`) | `!buggrabber`, `bug-grabber` | `wowace:bug-grabber#r152` | wrath-legacy (3.3.5a) |
| BugGrabber (`r130`) | `!buggrabber`, `bug-grabber` | `wowace:bug-grabber#r130` | tbc-legacy (2.4.3) |
| BugGrabber (`r43`) | `!buggrabber`, `bug-grabber` | `wowace:bug-grabber#r43:tmp/trunk/!BugGrabber` | vanilla-legacy (1.12.1) |
| pfUI | | `git:https://github.com/shagu/pfUI#master` | vanilla-legacy (1.12.1) |
| PlateBuffer | | `git:https://github.com/nullfoxh/PlateBuffer#master` | tbc-legacy (2.4.3) |
| GroupBulletinBoard | `gbb` | `git:https://github.com/fondlez/GroupBulletinBoard#main` | wrath-legacy (3.3.5a) |
| MikScrollingBattleText | `msbt` | `git:https://github.com/fondlez/MikScrollingBattleText-Cata#main` | cata-legacy (4.3.4) |
| InFlight | | `git:https://github.com/fondlez/InFlight-Taxi-Timer#main` | mists-legacy (5.4.8) |

**Your own entries.** Files in `~/.config/omaddons/catalog.d/*.toml` are applied in file-name order, in the same format. They can add an entry, replace one (the whole entry: fields are not merged), or drop one with `remove = true`. An entry's keys are `name`, `variant`, `aliases`, `ref`, `families`, `folders` and `notes`. An entry that names a `variant` replaces or removes only that variant (`name = "BugGrabber"` with `variant = "r152"`); one naming only the name replaces every variant of it with itself, or with `remove = true` removes them all. To give a name that has one entry variants, remove it first (`remove = true`, earlier in the same file), then add each variant. Only `folders` makes an entry findable as a dependency, or lets it claim a folder for `adopt` (`adopt` can still guess it for a folder named like it, as the build of the client's family). An entry with no `families` counts as declaring every family, so it can match on any client; a variant must declare some. A file is skipped whole, with a `catalog_override_invalid` warning, when it does not parse, its `schema_version` is not 1.x, it has an unknown key, an entry has no name, no valid `ref`, an unknown family, an invalid folder name or variant key, it lists a `Blizzard_*` folder, it gives one name two entries (in one file, or one without a `variant` beside others), two variants of a name share a family, or an alias names two different addons (a name's variants share theirs).

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

The top level comes at two depths too. Bare `omaddons` (or `omaddons -h`) is one screen: the commands by group, where to start and the exit codes. `omaddons --help` (or `omaddons help`) is the full page: every command under its group with its aliases and a one-line summary, every global option, the environment variables omaddons reads, where to start, the exit codes and where to learn more. A command's help comes at two depths. `omaddons <command> -h` is the compact page: the command's about and usage, its aliases, its own options one line each, two or three examples, and one line naming the global options most command lines need. `omaddons <command> --help`, or `omaddons help <command>`, is the whole page: the long about, each own option's full help, the command's examples and notes, related commands (`See also:`), then every global option on one line and where to learn more. A page lists `--if-match` only on the commands that bind to a preview, and never `--robot` (`--json` by another name, still accepted everywhere). A page wraps to the terminal's width, never wider than 100 columns (bare help keeps its 80), with each wrapped line hanging under its own column. On a terminal it is styled (headings bold and underlined, commands and flags bold) under the same color policy as all human output, so `NO_COLOR`, `--no-color`, `TERM=dumb`, CI, a pipe and `--json` keep it plain. For subcommands, use `omaddons targets add --help` or `omaddons help targets add`. `omaddons help --json` gives the same as data: `commands` (each with its `about`, `aliases` and `group`), `global_flags` and `exit_codes`; `omaddons help <command> --json` names that command by its canonical path (`data.command`: `targets add`, `auth github`, and `install` for `help add`), with its `about`, `usage`, `args` and `flags`. Bare help also names the commands an agent with no prior knowledge starts from (see [`help`](#help-command)), and `help --json` lists them as `discovery`.

A mistyped command, flag or value is a usage error. Its fix is the corrected command when that command only reads, and `omaddons help <command>` when the closest command changes something: a typo never hands you a different command that changes addons. A mistyped flag, or a mistyped value of a flag with fixed choices, of the command you typed is corrected in place (`--channel betaa` → `--channel beta`). A bad value with no close match, such as `--actor robot` or `undo --last-by robot`, gets `omaddons help <command>`. A client named before or after the command is moved into `-t`; that fix keeps your command even when it changes something, because only the client moved. A `-t` value that names no client but is within two edits of one that does is corrected the same way, as the fix of `target_not_found`, keeping your command. Every suggested command parses.

### Global options

Every global option works before or after the command, except `-V`/`--version`, which works only before the command (`omaddons --json -V` works; after a command, `--version` is that command's flag or a usage error).

| Option | Meaning |
|---|---|
| `--json`, `--robot` | One JSON envelope on stdout (see [The JSON contract](#the-json-contract)) |
| `--format json\|human` | Overrides `--json`; default `human`, or `OMADDONS_FORMAT=json` |
| `-t, --target <TARGET>` (alias `--flavor`) | The client to act on: [any selector](#choosing-a-target-with--t), or `all` |
| `--dry-run` | Show the plan without changing addons or settings. Every command accepts it; one that changes nothing ignores it. When `install`, `update`, `pin`, `unpin`, `adopt` (adopting), `sync`, `copy`, `remove`, `undo`, `backup restore` or `backup prune` would change something, its dry run also stores the preview for 24 hours: `data.plan_id`, `data.plan_expires_at` and `data.approval` (see [Stored plans](#stored-plans)) |
| `--actor human\|agent\|widget` | Who is acting, recorded in history by every command that changes something. Default `OMADDONS_ACTOR`, else `human` when stdin is a terminal, else `agent`. In `history` it filters |
| `--reason <TEXT>` | Why, recorded verbatim in history by every command that changes something (at most 200 characters); others accept and ignore it |
| `--offline` | Fail any network access (`OMADDONS_OFFLINE=1`) |
| `--while-running` | Allow folder changes while the client runs (never SavedVariables) |
| `--if-match <TOKEN>` | Change nothing unless the plan is still the one the `--dry-run` that returned this token showed (`data.precondition`); see [Previews, plans and `--if-match`](#previews-plans-and---if-match). Only `remove`, `undo`, `backup restore`, `backup prune`, `errors dismiss`, `install`, `update`, `pin`, `unpin`, `adopt` (when it adopts), `sync`, `copy`, `doctor fix`, `doctor gc` and `targets rm` take it; on any other command it is a usage error |
| `--no-color` | No ANSI color (also `NO_COLOR`). Color is also off with `--json`, in CI, with `TERM=dumb` and when stderr is not a terminal |
| `--quiet` | Only errors on stderr |
| `-v, --verbose` | More detail, at two levels. `-v` adds detail fields to the output (paths, evidence, every list item) and `debug` log events on stderr. `-vv` also adds `trace` log events on stderr: each release-memo decision with its probe and fetch times, the discovery route that reached each client root and whether its cached entry was reused, and each wait for the global lock. `-vv` never changes stdout: `--json` output is the same at `-v` and `-vv`. Give the v's together (`-vv`) or side by side (`-v -v`); a `-v` before the command and another after it count as `-v`. More than two act as `-vv`. `--quiet` drops log events of every level |
| `--progress json\|human\|none`, `--no-progress` | Progress of long commands on stderr (`install`, `update`, `outdated`, `adopt`, `sync`, `copy`, `search` and `backup create`, and the stage and commit steps of `remove`, `pin` and `unpin`). Off with `--json`, in CI or when stderr is not a terminal, unless `--progress` names a mode; never with `--quiet` |
| `-h, --help` | Help: `-h` the compact page, `--help` the whole one (see above) |
| `-V, --version` | Version (before the command only; after a command, `--version` is that command's flag or a usage error) |

### `status`

Triage: every client, its addon and folder counts, whether it is running, its new Lua errors and its backups, what needs attention (a missing client, a pending journal, a degraded addon, a newer state version, managed files changed on disk or managed folders gone), its issues, and the bar widget. The issues are the doctor's cheap checks: every `cached` check of its [registry](#doctor) with `--cached`, and every `cached` and `quick` check without.

```bash
omaddons status               # scan, then report (managed files checked stat-first)
omaddons status --cached      # no scan: the cached clients, re-checked on disk (fast; the widget's view)
omaddons status --deep        # re-hash every managed file, not only those whose size or time changed
omaddons status --check       # exit 6 when anything needs attention, or a P0 or P1 issue exists
omaddons status -v --json     # adds client_dir, install_root and exe_name, and changed paths
```

`--deep` with `--cached` is a usage error. `--cached` before any discovery cache exists also suggests `omaddons status` (action `status`). Plain `status` writes one file: the discovery cache `--cached` reads (`~/.cache/omaddons/discovery.json`), when its scan finds a client changed since the cache was written; `--cached` writes nothing. The human view shows at most 5 changed paths per addon; `-v` shows every path the check listed (at most 50 per kind and folder).

Each client also shows:

- **`display_name`.** The name a player knows it by: `targets.<id>.name` from `config.toml` when set, else its game mode's name ("Retail", "Classic Era", "Burning Crusade Anniversary", "Mists of Pandaria Classic", "WoW: Forever"), its family's with what it is for a Battle.net client without modes ("Retail PTR", "Titan"), its family's and version for a custom client, with "private server" on a legacy one ("Wrath 3.3.5a private server"), else its product code or id. Two clients that would share a name are told apart by their id ("Retail (ptr)"). `-t` still takes the id; `targets` carries the same name.
- **`running`.** Whether its game runs now, from `/proc`; `null` when `/proc` could not be read (with `running_unknown`; the human view shows `running?`).
- **`updates`.** Its pending update counts (`available`, `pinned`, `blocked`, `failed`, `unchecked`) and `checked_at`, the oldest check. They come from what earlier `outdated`, `update` and `install` runs remembered, never from a request, so `status` stays offline. `stale` is true when that check is older than `check_interval_hours` (default 6).
- **`drift`.** Each managed addon whose files changed on disk since omaddons installed them (`modified`), whose folder is gone or replaced by a symlink (`missing`), or that has no file index to check against (`unindexed`), with its changed-file count (`files`; the paths with `-v`). A file whose size or time changed is re-hashed, so one merely touched is not reported; `--deep` re-hashes every file. `--cached` checks only that the folders are there. Drift is computed on every run and never stored. `modified` raises the attention `addon_modified`, `missing` raises `addon_folder_missing`.
- **`adoptable`.** How many unmanaged folders a catalog entry names that is not installed on that client.
- **`orphans`.** How many addons installed as dependencies nothing requires any more (from the record, `--cached` too).
- **`any_flavor_addons`.** The names of its addons installed under [`--any-flavor`](#a-build-not-made-for-this-client---any-flavor) (from the record, `--cached` too); the human view says `any flavor: Foo (builds not made for this client, by --any-flavor)`. A standing choice, never attention.
- **`errors`.** Its Lua errors, from BugGrabber (see [`errors`](#errors)), in `errors`' units: `capture` (`buggrabber`, or `none` with every count 0), `state`, `coverage` (`complete`, or `truncated` when a bound cut the walk), `new`, `total` (local errors, `errors`' `summary.entries`), `addons`, `received` (errors other players sent: never new and never in the other counts), `top_addons` (at most 5 `{addon, folder, new, count}`, the addons with the most new errors, then occurrences), `last_session_at` (when the client last wrote a BugGrabber file, at logout or `/reload`) and `dismissed_at` (the newest dismissal in effect). `new` is decided at every call by the rule `errors` uses, so errors stop being new as their file passes 7 days old, or once dismissed, with nothing read again. Plain `status` reads BugGrabber's files as `errors` does and writes nothing; its `state` is `ok`, `partial` (a file the game was writing, one over its size bound, or one that could not be read: the counts are what was read) or `unreadable` (no file could be read). `--cached` opens none: it recounts what the last `errors` run recorded, checking only each file's stamp and each `SavedVariables/` folder's file names (within `errors`' bounds), and says `stale` when they changed since (the counts are that run's) or `unchecked` when no run has read the client (the counts are 0, and a dismissal still holds).
- **`backups`.** How well its `WTF/` is protected (see [`backup`](#backup)), from the backup inventory (`backups.json` in the cache), checked against one listing of the backups folder and one of the undo snapshots. No backup's description is read and no tree walked, so `--cached` stays fast with hundreds of backups. `null` for a client whose folder is gone.
  - `count`: its backups, from the listing.
  - `stale`: `true` when the listing differs from what the inventory added up (a backup added or deleted around omaddons), or when backups exist with no inventory at all. The other numbers are then the inventory's, or `null` without one; `backup list` brings them up to date. `status` never does: it writes nothing.
  - `latest`: the newest backup, `{id, captured_at, kind, trigger}`.
  - `latest_qualifying`: the newest capture that counts as a checkpoint, `{id, captured_at}`: a manual, automatic or before-restore backup made here, whole and stable. An imported, recovered or best-effort backup never counts.
  - `auto`: `{mode, capability, last}`. `mode` is the configured `auto_backup` (`off`, `daily`, `daily-copy`). `capability` is what automatic backups observed on this client (`supported`, `unsupported`), or, before any attempt, `unsupported` on a filesystem that cannot clone files and `unknown` elsewhere. `last` is the last attempt's `{at, result}`: `created`, or why none was made (`unchanged`, `done_today`, `no_reflink`, `space_low`, `running`, `in_progress`, `no_wtf`, `off`, `failed`).
  - `bytes` and `snapshot_bytes`: the apparent size of its backups and of the `WTF/` trees whole restores keep for undo. On btrfs and xfs, backups first share their data with the live `WTF/`, so these overstate what they hold on disk.
  - `free_bytes`: the space free to you on the filesystem holding them, measured on every call.

The human view opens with one summary line (`3 updates on 2 clients · 1 issue (P2) · 4 new Lua errors on 1 client · retail running`, or `Nothing needs doing`), prints each client's issues under its row (`  P2 Plater's files changed on disk (3 files) → omaddons install Plater -t retail --reinstall`) and, when they say something, its Lua errors (`  errors: 4 new (DBM 3, WeakAuras 1) · omaddons errors -t retail`, `errors: incomplete read (file was being written) · 2 new so far`, `errors: not read yet · omaddons errors -t retail`, `errors: changed since last read · omaddons errors -t retail`; addon names and counts only, never an error's text) and its backups (`  backups: 5 (newest today 18:02, after play) · auto: daily, working · 212 GB free`; when automatic backups cannot work it says why, as `auto: daily, but this filesystem cannot clone; use daily-copy or back up by hand` or `but space ran low (limits.backup_min_free_mb)`, and `details out of date` when `stale`). After every client come the issues of no client (`data.issues`), in the same form (`  P2 The GitHub token file is readable by other users → omaddons doctor fix --only fm-secrets-credential-file-mode`), with no client found too; the bar widget's has a line of its own. A forced repair is read before it is run, so its line names the explain command first: `P0 An interrupted change was started by a newer omaddons; … → omaddons doctor explain fm-concurrency_primitives-journal-too-new -t retail (read it before the forced repair: omaddons doctor fix --only fm-concurrency_primitives-journal-too-new -t retail --force --yes)`. When the bar widget needs a word it says so (`bar widget: from another omaddons build → omaddons omarchy install`, or, for a widget installed under other `HOME` or XDG directories, `bar widget: serves another omaddons state (installed under other HOME or XDG directories)`, with no command). The view ends with the last change (`last change: update on classic, retail by human (2026-10-21 18:04) · undo: omaddons undo --run <run>`).

The JSON adds five keys to `data`:

- **`summary`.** The machine at a glance, for the widget and agents: `targets`, `running` (the ids of clients running now), `updates` (`available`, summed, and the `targets` that have some), `issues` (counts by severity: `p0` to `p3`), `check_due` (a check for updates is recommended), `errors` (`new`, the clients' new local Lua errors summed, and the `targets` that have some), and `widget_outdated`: the bar widget serving this omaddons state is from another build (see `widget`). `issues` counts every issue listed, each once: a client's interrupted changes in one state are one issue, and the machine's issues count too. `state` is the most pressing of `attention` (a P0 or P1 issue), `errors` (new local errors; received ones never count), `updates` and `idle`.
- **`issues`, on each client.** Its findings of the doctor's checks, computed by the doctor's own loop on what `status` read itself (nothing is discovered or checked twice): every `cached` check with `--cached`, every `cached` and `quick` check without (`--deep` too, whose re-hash reaches `managed-files-modified`). Which checks those are is the status column of the doctor's [failure-mode table](#doctor). Each issue has an `id` (a failure-mode id from the vision's D20 table; the seven `status` published before the doctor existed are stable across releases), `severity` (`P0` to `P3`), a `title`, the `addon` when it is about one, a `remediation` (`command`, `argv`, `destructive`; `argv` is `null` when the fix is advice, not a command), `auto_fixable` and `explain` (`command`, `argv`), all three as `omaddons doctor` gives them. A repair the doctor makes is `omaddons doctor fix --only <id> [-t <client>]`, and `auto_fixable` says it runs with no force. A forced repair ends `--force --yes` and is never auto-fixable: the human line shows its explain command first, and the bar widget never runs an argv holding `--force`. Any other issue keeps its failure mode's own command or advice, such as `omaddons install <addon> -t <id> --reinstall` or "upgrade omaddons". `explain` is `omaddons doctor explain <id> [-t <client>]`, which shows the issue's evidence and what its repair would do; `status` never asks a repair to plan. Ranked by severity, then id, then addon. `attention` stays as it was. An interrupted change is reported as the state recovery leaves it in, as `omaddons doctor` reports it: one issue per state and client, titled for its one journal or for `<n>` of them. A finding about no client (a credential file, the lock, the agent skill, a journal no client can carry) is in the machine's `issues`, and so is one about a client `status` does not list, without `-t` (with `-t` it is out of scope). A check whose input could not be read is never reported clean: `-v` names it on stderr (`status: <id> not checked (<reason>)`), and `omaddons doctor` lists it in `skipped`. A `config.toml` that does not read fails `status` itself, with its line and column, which is how `status` reports `fm-configs-config-parse-error`.
- **`widget` and `issues`.** The bar widget, read from its files with no subprocess, so `--cached` keeps its pace: `omarchy` (Omarchy's shell is installed, not only its config), `installed`, `enabled` (on a section of the bar), `version`, `outdated` and `context_matches`. `widget` is `null` without `~/.config/omarchy`.
  - `outdated`: the installed code is not what this binary renders for the directories the widget was installed under: another build or other files, never merely another state. A widget whose `Build.js` is missing is outdated too.
  - `context_matches`: whether it was installed under this run's `HOME` and XDG directories; `null` when none is installed, or when its `Build.js` cannot say.
  - When an outdated widget serves this run's state, `status` warns `widget_outdated`, recommends `omaddons omarchy install` (`reinstall_widget`), sets `summary.widget_outdated` and lists the issue `fm-plugins-widget-version-drift` in `data.issues`, the issues of no client. The widget itself turns read-only on it.
  - From another state it reports the facts and recommends nothing: reinstalling from there would point the widget at that state.
- **`last_change` and `last_change_state`.** What bare `omaddons undo` would revert now: its `run`, `kind`, `at`, `actor`, `reason`, the `targets` it touched, the folders it changed (`changes`: `added`, `replaced`, `removed`, `recorded`, `restored`, summed over the run) and the `undo` command (`command`, `argv`). Backups and settings changes are never the last change, since undo never reverts them. A repair (`doctor fix`) can be the last change; its `targets` are the clients whose files it touched, none for a repair outside every client. Plain `status` reads the whole history; `--cached` asks the history projection (`history_tail.json`) and writes nothing. `last_change_state` is `ok` (with `last_change` `null` when there is nothing to undo) or `unknown`, which `--cached` reports when the projection is missing, stale or cannot tell, and which never means there is nothing to undo; plain `status` then answers. The next command that changes something rebuilds the projection.

Pending updates never count as attention: `status --check` stays a health check, and `outdated --check` is the update check. A P0 or P1 issue fails it as attention does, a client's or the machine's, and `data.needs_attention` says so; P2 and P3 issues never do. Its message and fix are the first attention item's, then those of an interrupted change no client carries, then the most urgent issue's (`<client>: <title>`, or the title for the machine's), with its command, or its advice; a forced repair's fix is its `doctor explain`. The recommendations:

- `omaddons install <addon> -t <id> --reinstall`, for each modified or missing addon (at most 5);
- `omaddons update`, or `-t <id>` when only one client has updates;
- `omaddons adopt -t <id>`, for each client with `adoptable` folders;
- `omaddons outdated`, when a client was never checked or its check is stale;
- `omaddons errors` (`check_errors`), when a client's Lua errors are `stale` or `unchecked` (so only under `--cached`), never when a read was `partial` or `unreadable`, which reading again would not complete;
- `omaddons backup list -t <id>` (`refresh_backups`), for each client whose `backups` are `stale`;
- `omaddons backup create -t <id>` (`backup_create`), for each client whose `WTF/` holds SavedVariables and that has no checkpoint captured in the last 7 days, whatever `auto_backup` says: automatic backups may be off, or unable to run on this filesystem. An import, a recovered or a best-effort backup does not stop it, and a client never played (no SavedVariables) is not asked. Without an inventory, the refresh comes first;
- `omaddons omarchy install` (`reinstall_widget`), when the bar widget serving this omaddons state is from another build;
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
| `--any-flavor` | On this client, install builds the publisher did not make for it, and keep doing so: see [`--any-flavor`](#a-build-not-made-for-this-client---any-flavor) below |
| `--no-any-flavor` | Clear `--any-flavor` for the named addons on this client |

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

Each item's `status` is `installed`, `updated`, `unchanged`, `skipped` (the same addon requested twice: `skip_reason: "duplicate"`, with `duplicate_of`) or `failed`, or, when only its record changed, `pinned`, `unpinned` or `edited` (with `fields`). A partial result (some addons failed) is still exit 0, with `partial: true` and each failure in `data.items[].error`. With `--strict` it is exit 6. When every addon fails, the command fails with the first item's error, and `error.details.items` lists them all, each `{target, request, error}` as on every command (`target` is `null` when no client was chosen yet, `error` the item's own error). `-v` adds each item's `asset_url`. Never-launched clients have no `Interface/AddOns` yet: the first install creates it inside its transaction and reports `creates_addons_dir: true`.

`--dry-run` returns `data.precondition`, and `--if-match <it>` installs exactly what that preview showed, or nothing (`plan_stale`). `install` asks its sources again, so a release published since the preview makes the bound run stale; see [Previews, plans and `--if-match`](#previews-plans-and---if-match). Without `-t`, `install` chooses the client as it always does, and the token binds that choice: a run that would now choose another client is stale. With `-v`, a dry run's human output ends with the `--if-match` it takes: `Or bind a typed run to it: --if-match <token>` after the stored plan's line, else `Run it bound to this preview: --if-match <token>`.

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

#### A build not made for this client: `--any-flavor`

Some addons the rules refuse still work: many small addons run unchanged on every client while their TOC lists only retail, and players have long enabled "Load out of date AddOns" for them. `omaddons install <ref> -t <id> --any-flavor` installs such a build on that one client and records the choice on the addon there, so every later `update`, `sync` and reinstall keeps it, as the channel is kept, until `--no-any-flavor` clears it. Under it:

- the publisher's word against the file (its `release.json`, its name) is ignored;
- a folder whose TOC the client reads, but whose interfaces are off its range or missing (`interface_not_in_family`, `no_interface`), installs as out of date, with the `out_of_date` warning, the addon's own folder included;
- still refused or left out: a game-type gate (`game_type_excluded`: the author excluded this client), TOCs the client never reads (`no_loadable_toc`), retail 12's interface floor (`below_client_floor`: the client never loads it), and what requires those; the ownership, `Blizzard_*` and symlink rules are unchanged.

Each install that needed it warns `any_flavor`, naming what it allowed. A refusal it would lift offers `omaddons install <ref> -t <id> --any-flavor` among its `alternatives`, never as its fix: agents use it only when the user asks for that addon on that client. Setting or clearing it on an installed addon at its release is a records-only change (`edited`, `fields: ["any_flavor"]`) that `undo` takes back; on a pinned addon it moves nothing. `status` lists each client's addons with it (`any_flavor_addons`, never attention, so `status --check` stays a health check), and `list` and `info` mark them (`any_flavor: true`). A dependency installed for such an addon is judged without it, `copy` never carries it to another client, and `export` writes it (`any_flavor = true`) for `sync` to apply.

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

With `-v` it also carries `optional_dependencies` (its `## OptionalDeps`, never installed). A bridge requirement warns `bridge_dependency` once, when the addon is first installed, with a fix that installs it (by catalog name, else by the ref the resolver finds), or searches for it (`omaddons search <folder>`) when nothing resolves it; later installs and updates keep the field and say nothing more. After the `undo` hint (and any `adopt` hint for required folders already present), `commands` offers at most three more: `omaddons install <candidate> -t <id>` for unresolved candidates in item order (never one that is `not_shipped`), then each bridge requirement's fix. Each is labelled by the verb it runs, action `install` or `search`. `update`, `sync` and `pin` items that change an addon carry the same fields, each dependency right after its requester; their skipped, unchanged and failed items leave them out, except a failed dependency.

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
omaddons update -t all --dry-run    # every client, test clients included; the first line counts them
omaddons update --strict            # exit 6 when only some addons update
omaddons update --no-deps           # not what the new releases' TOCs start requiring
```

When an addon's new release starts requiring another project, `update` installs that project in the same transaction, as `install` does: recorded as a dependency, from `default_channel`, undone with the update. A dependency that resolves but fails to install is a failed item named by the folder required (never `blocked`), and the addon itself still updates.

What to update comes from the same statuses `outdated` shows, fetched once per ref (free inside 15 minutes; `--refresh` asks again). `--all` together with addon names, and `--channel` without names, are usage errors. A named addon installed nowhere is a failed `not_managed` item. A client named among the addons is a usage error (see [Choosing a target](#choosing-a-target-with--t)). Each client with updates commits its own transaction, all under one run, so `omaddons undo` reverts every client and `omaddons undo <tx>` one of them. A failure on one client does not roll back another.

Some addons are left alone and say why (`skipped`, with `skip_reason`): `pinned` (by `pin` or `install --version`; only naming it adds an `addon_pinned` warning, whose fix is `omaddons unpin <addon> -t <target>`), `ahead`, `static` (a local file, a URL or an archive given with `--file`), `unchecked`, and `blocked`. A newer release that turns out not to load on a client is `blocked`, not a failure; the verdict is remembered, so `outdated` shows it too. A fetch that fails fails its addons (`update` never installs from stale metadata) and makes the run `partial`. When every addon fails, the command fails with the first item's error, whole (its own `details`, such as a damaged archive's `blob` and `actual`), and `error.details.items` lists them all; so do `pin`, `unpin`, `sync`, `copy` and `adopt`. `unchanged` addons are counted in `summary` and listed with `-v`.

Files you changed in an addon's folders do not stop its update: they are replaced like the rest, and a `local_changes_replaced` warning names them, with `omaddons undo <tx>` as its fix, because the transaction snapshot keeps them (at least 24 hours). The same goes for `install` and `sync`. `-v` names the snapshot folder.

There is no `--version` and no `--force`: a given version is an install (`omaddons install <addon> --version V` or `omaddons pin <addon> --version V`, which pin it), and taking over another addon's folder is `omaddons install <addon> --force -t <target>`, which an `ownership_conflict` names as its fix.

`--dry-run` returns `data.precondition`, and `--if-match <it>` updates exactly what that preview showed, or nothing (`plan_stale`). The token also covers what the preview left alone and why (each `skipped` item's `skip_reason`), and with nothing to update it covers every selected client, so a change on any of them makes the bound run stale. Inside the 15-minute window in which `update` answers from remembered releases, a bound run installs the previewed version even if a newer one appeared since; with `--refresh` it asks again, and a newer release makes it stale. See [Previews, plans and `--if-match`](#previews-plans-and---if-match). With `-v`, a dry run's human output ends with the `--if-match` it takes: `Or bind a typed run to it: --if-match <token>` after the stored plan's line, else `Run it bound to this preview: --if-match <token>`.

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

`--dry-run` returns `data.precondition`, and `--if-match <it>` pins (or unpins) exactly what that preview showed, or nothing (`plan_stale`; see [Previews, plans and `--if-match`](#previews-plans-and---if-match)). A bound run with nothing left to do still checks its token: a pin made by someone else in between is `plan_stale` (`targets`), not `unchanged`. With `--version`, the token also covers the release and every file of each folder it replaces.

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
  - `revision`: with `version`, the release's source revision (WowAce: the tag's revision, `revision = 403`), which `export` writes; with `--locked`, or on a pinned entry, exactly that tree installs, even after the tag was moved upstream;
  - `pinned` (default `false`);
  - `dependency`: a version lock for a dependency, not a request;
  - `any_flavor` (default `false`): [`--any-flavor`](#a-build-not-made-for-this-client---any-flavor) on that client; `sync` sets or clears it to match, without touching a folder when only it differs.
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

`--dry-run` returns `data.precondition`, and `--if-match <it>` syncs exactly what that preview showed, or nothing (`plan_stale`). The token covers the file's exact bytes: editing it after the preview, even a comment, makes the bound run stale. With nothing to do it covers every selected client the file has a section for. Like `update`, a bound run inside the 15-minute window installs the previewed releases even if newer ones appeared since. See [Previews, plans and `--if-match`](#previews-plans-and---if-match). With `-v`, a dry run's human output ends with the `--if-match` it takes: `Or bind a typed run to it: --if-match <token>` after the stored plan's line, else `Run it bound to this preview: --if-match <token>`.

**`--locked`** reproduces versions, not only refs: `omaddons export` on one machine, then `omaddons sync <file> --locked` on another.

- Every entry with a `version` installs that version, and an installed addon at another version moves to it. A lock never pins: `pinned` comes only from the entry. With a `revision`, the same version at another revision counts as another version, and the recorded tree is fetched directly (no tag list asked for).
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

- A family section holds what every live client of that family has alike (same ref, channel, pin, version and revision), when all of them are exported. The rest goes in each client's own `[target."<id>"]` section. Test clients (PTR, beta), and a client whose family is unknown, only ever get their own section. While a registered client's directory is gone, its family cannot be known, so every client gets its own section.
- Every entry is a table sorted by name, so the file diffs cleanly. `version` is written only for a release (not for an addon adopted at its TOC's version), with `revision` when its source has one (WowAce), `sha256` only while the archive is in the download cache, and `channel` and `pinned` only when they are not the defaults.
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

Each side is exactly one client: a selector that matches two is `target_ambiguous`, with one fix per candidate, and `-t` is a usage error, as are `--from` and `--to` naming the same client. A named addon missing on `--from` is a failed `not_managed` item (fix `omaddons list -t <from>`). Each addon installs from its ref on its recorded channel, the way `install` would: the newest release that loads on `--to`, unpinned, because versions differ between flavors. Dependency records are not copied; what `--to`'s own TOCs require installs with it, as for `install`. Nor is `--any-flavor`: it is a decision about one client. Three kinds of addon are skipped, never failed: one already on `--to` (by ref, `already_installed`), one installed from a local file or folder (`local_source`), and one with no build that loads on `--to` (`no_build_for_target`). Any other failure, such as a source that cannot be reached, makes the run `partial`. `omaddons undo <tx>` from `commands` reverts the copy.

`--dry-run` returns `data.precondition`, and `--if-match <it>` copies exactly what that preview showed, or nothing (`plan_stale`). The token covers the source client too: an edit in a folder of an addon being copied, or another change committed on `--from`, makes the bound run stale (`details.changed` names `observed`), as does a change on `--to` (`targets`). `copy` asks its sources again, so a newer release does too. With nothing to copy, it covers `--to` and `--from`'s record. See [Previews, plans and `--if-match`](#previews-plans-and---if-match). With `-v`, a dry run's human output ends with the `--if-match` it takes: `Or bind a typed run to it: --if-match <token>` after the stored plan's line, else `Run it bound to this preview: --if-match <token>`.

### `remove <ADDON>...`

Remove managed addons from one target, as a transaction undoable until retention prunes its snapshot (`uninstall`, `rm` and `delete` are aliases). Removing what no source gives again (an addon adopted as it was, or installed from a local archive or directory) warns `snapshot_only_copy`. An addon is named by its name, its ref or a folder it owns.

```bash
omaddons remove DBM -t retail
omaddons remove gh:BigWigsMods/BigWigs -t era
```

Unmanaged, linked (symlinked) and `Blizzard_*` folders, and names that match no managed addon, are refused with `not_managed`. A folder or ref that several managed addons answer to is `ref_ambiguous`. Without `-t`, the client is the one that manages the named addons. A client named among the addons is a usage error (see [Choosing a target](#choosing-a-target-with--t)). `--dry-run` returns `data.precondition` and stores the preview as a plan (`data.plan_id`, see [Stored plans](#stored-plans)); `--if-match <it>` removes exactly what that preview showed, or nothing (see [Previews, plans and `--if-match`](#previews-plans-and---if-match)).

An addon whose folders were all deleted by hand is forgotten: its record and index entries go, in a transaction that records each folder (`record` ops) and that `undo` reverts. Such a remove changes no folder, so it also goes through while the game runs.

**Dependencies.** Removing an addon others still require goes ahead, with `still_required` naming them: they will not load without it, and its fix is `omaddons undo <tx>`. Every addon that listed the removed one in `required_by` drops it in the same transaction (a `record` op with `edit: {"required_by_remove": "<name>"}`). A dependency that nothing requires any more is an orphan, and `orphaned_dependency` names them all with one `omaddons remove <names…> -t <id>` as its fix. Orphans are never removed by themselves. `undo` restores `required_by` exactly, even if the dependency was updated in between, and keeps requesters added after the transaction it reverts. Undoing an older install of a dependency that a later addon requires warns `still_required` too (see [`undo`](#undo-tx)). Removing an addon together with what it requires (`remove LittleWigs BigWigs`) raises neither warning.

### `undo [TX]`

Revert what a command did, as new transactions, SavedVariables included (`revert` and `rollback` are aliases).

A command that changes several clients commits one transaction per client, and all of them share a run id. Bare `undo` reverts the most recent run, every one of its transactions, newest first. After a single-client command that is one transaction. Those undo transactions share a run of their own, so a second `undo` reverts them all again.

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

`TX`, `--run` and `--last-by` cannot be combined. A prefix that matches several transactions or runs is `not_found`, with the candidates in `details.candidates`. `--run` takes the runs `history` shows; given a doctor run id (`2026-10-30T18-03-40Z__01M8S…`), as `--run` or as `TX`, `undo` answers `usage` before reading anything, with the fix `omaddons doctor undo <id>`.

`--dry-run` returns `data.precondition` and stores the preview as a plan (`data.plan_id`, see [Stored plans](#stored-plans)); `--if-match <it>` reverts exactly what that preview showed, or nothing: after a newer change, bare `undo` would pick another run, and the bound run refuses with `plan_stale` instead (see [Previews, plans and `--if-match`](#previews-plans-and---if-match)). The stored intent names the full transaction or run the preview resolved, including when the request was bare `undo` or used `--last-by`.

When one transaction of a run cannot be undone (`undo_conflict`, say), the others still are. The result is then `partial`, and `transactions[]` in the JSON data holds one result per transaction. The fields before it describe the first transaction undone, as they always did. When every transaction fails, `undo` fails with the first error.

Undoing the newest change never conflicts with a later transaction. Undoing an older one is refused with `undo_conflict` if a later transaction touched the same folders, and the fix names the newer one to undo first. A later `doctor fix` repair counts the same way when it touched one of those folders, the client's record or files index, or the transaction's undo snapshot (`details.conflicting[].folders` is then empty). A later transaction that was itself undone no longer counts, so undoing your own transactions newest first, by id, always works. `undo_conflict` also covers a transaction already undone, and a folder it removed that was put back by hand. Undoing again a transaction whose undo deferred its SavedVariables restore is `undo_conflict` too, with `omaddons undo <tx> --saved-variables restore` as the fix.

Settings a `WTF/` restore put back are never overwritten in silence. Undoing an older transaction whose SavedVariables it would restore, after a later restore, is `undo_conflict` naming that restore to undo first, with the same undo and `--saved-variables keep` (folders only) as the alternative; the deferred `--saved-variables restore` follow-up is refused the same way. Undoing a restore is refused while a later transaction wrote settings: another restore, a restore of one addon's SavedVariables, or an undo that restored SavedVariables (an undo recorded before milestone 3 does not say whether it did, so it counts as one). A restore followed by its own undo no longer counts as a later restore.

Undoing a whole `WTF/` restore puts the tree from before it back with one `RENAME_EXCHANGE` (after a restore onto a client that had no `WTF/`, it moves the restored tree out), and keeps the tree it replaces in its own snapshot, so the undo can be undone in turn. It needs the same filesystem support as the restore (`filesystem_unsupported` otherwise), never runs while the game runs (`game_running`, whatever the running policy or `--while-running`), and refuses `--saved-variables keep`, which would leave nothing to undo. It reverts every play since the restore: `data.changed_since` counts the files that play changed, which stay in the undo's snapshot (`null` when the restored backup is gone and they cannot be counted). Its `--dry-run` token covers the live `WTF/`, so a play session between the preview and `--if-match` refuses the run.

A newer history line this omaddons cannot read (one a newer version wrote, or one damaged) is assumed to have touched whatever it may have: every SavedVariables file of its target, and the folders it names (every folder when it names none, names `WTF`, or is not JSON at all). Only a line naming another target is ruled out. Undoing an older transaction whose folders it may have touched is then `undo_conflict` with `details.reason: "history_unreadable"` (and `details.lines`), whose fix is to undo with the newer version; when only SavedVariables are in question, `omaddons undo <tx> --saved-variables keep` is the alternative. A line older than the transaction undone never counts.

Settings changes in the history (`config`, `target`, `auth` and `export` lines) are never undone: bare `undo`, `--last-by` and `--run` step over them, and `undo <tx>` of one is `not_found` ("transaction <tx> is a config change, which undo does not revert"). The same goes for a `backup` line: a backup is no change to undo ("transaction <tx> made a backup, which undo does not revert", fix `omaddons backup list`).

A later transaction that only added or dropped a requester in a dependency's `required_by` never conflicts. An addon the undo brings back keeps those later requesters. Undoing a transaction that installed a dependency an addon installed later still requires goes ahead with `still_required`, as `remove` does, and its fix undoes the undo. A dependency the undo leaves required by nothing warns `orphaned_dependency`.

#### Undoing a repair

A `doctor fix` repair (`kind: repair` in `history`) is undone like any change. `omaddons undo <tx>` names it. Bare `undo` takes it when it is the newest change, which it usually is. `--last-by`, `--run` and `-t <client>` (when the repair touched that client) take it too, and [`omaddons doctor undo <run>`](#doctor-undo) names it by the doctor run that made it.

- **Strict, in reverse.** The undo inverts the repair's operations newest first. Before anything moves, it checks two things: every path is still exactly as the repair left it, and every backup it restores from still holds the bytes it kept. Anything else is `undo_conflict` with `details.reason: "changed_since_repair"` and the paths in `details.paths`. The repaired state and the repair's backups then stay as they are.
- **A set-aside journal.** The undo of a set-aside puts the journal back only while nothing that journal depends on has changed: the client's record generation, its identity marker, and the folders and `WTF/` the journal would recover. Otherwise it is `undo_conflict` with `details.reason: "changed_since_suspension"`. The journal is restored under the provisional name `<tx>.reactivating.jsonl` first and renamed to its own name last. A set-aside of several journals comes back whole, the activations last, one per journal.
- **Its own record.** The undo is a repair-family transaction of its own:
  - a history line of kind `undo`, with `reverts` and `target: null`;
  - its own doctor run directory, of kind `undo` (see [Files and state](#files-and-state));
  - it can be undone in turn.
- **Clients.** A client the repair touched must not be running, whatever the running policy (`game_running`, `details.reason: "repair"`). A client the repair names that is not on this machine is `target_not_found`. `--saved-variables` has no effect on a repair.
- **Previews.** `--dry-run` returns a token and stores a plan, like any undo. The token covers the clients' record generations, the inverse operations, and what the checks read: every path's state, every backup's digest, every guard's value. A change between the preview and `--if-match` is `plan_stale`.
- **Data.** For a repair:
  - `data.kind` is `repair` (`folders` for every other undo, whose `repairs` is `null`);
  - `data.repairs` lists the inverse operations in the order they run. Each has `seq`, `fix`, `fm`, `op`, `path`, `to_rel`, `octal_mode` and `activation`. `to_rel` names a destination in the undo's own quarantine by its side (`state/…`, `client/…`). A preview and the run show the same list; the undo's history line records the operations as committed;
  - `data.doctor_run_id` is the undo's own run (`null` under `--dry-run`, which makes none);
  - `data.reverted.targets` lists the clients the repair touched;
  - `data.reverted.target` and `data.transactions[].target` are `null`, since a repair names no single client.

### `apply <PLAN_ID>`

Run exactly what a `--dry-run` stored: the plan its `data.plan_id` names (the `Plan … stored` line of its human output). `apply` executes the plan's typed intent through its command's own code, bound to the plan's token, one transaction per client, as the preview showed or not at all.

```bash
omaddons install weakauras -t era --dry-run    # Plan 01M8Q2VA… stored for 24 hours
omaddons apply 01M8Q2VA                        # exactly that install, or plan_stale
omaddons apply 01M8Q2VA --dry-run              # check it against the world now; changes nothing
```

- **Which plan.** The full id or a unique prefix of 8+ characters, in any case. A prefix several plans share is `ref_ambiguous` (one `omaddons apply <id>` per candidate); an unknown one is `plan_not_found` (`details.prefix`), as is a plan file that does not read (`details.reason: "unreadable"`). Plans last 24 hours, and emptying the cache removes them: run the command again with `--dry-run`. Transactions already committed stay in history.
- **What it checks first**, each refusal a `plan_stale` that is not retryable, whose fix is the plan's own command with `--dry-run`, with `details.reason`: `already_applied` (history records a transaction of this plan: `details.txs`), `expired` (past its 24 hours), `binary_changed` (another omaddons build made it), `input_missing` (a local archive, folder or `sync` file it installs from is gone: `details.path`). Then the run itself is bound: anything it finds changed since the preview is `plan_stale` with `details.changed`, and nothing changes.
- **Every source again.** `apply` checks every source the plan installs from, whatever its command's 15-minute window: a release published since the preview makes it `plan_stale`, never a different install. What that check fetched and downloaded is remembered, so the preview you run next shows the new release without asking its source or downloading it again.
- **One applier per plan.** A real `apply` holds the plan's lease (`<plan_id>.lease`) from before its checks until every client's outcome is known. A second `apply` of the same plan waits up to 30 seconds, then fails `locked` (exit 5, retryable, `details.lease` and its holder); once it gets the lease it finds `already_applied`, or runs when the first committed nothing.
- **Per client.** `data.outcomes[]` reports each client, sorted by id: `committed` (with its `tx`), `unchanged`, `stale` or `failed` (with its `error_code`). A run over several clients is never called atomic; a client that failed is reported beside the ones that committed.
- **`--dry-run`** checks the plan against the world now and shows what it would do: `data.approved` (the approval stored with the plan) beside `data.result` (the fresh preview), every outcome `unchanged`, and the `omaddons apply <id>` hint. It takes no lease and no lock, and writes nothing, not even a plan of its own.
- **Offline.** A plan that installs releases from their sources cannot be checked again offline: `offline`, before anything runs. A plan of local files, or a `remove`, `undo` or `backup` plan, applies offline.
- **History.** Each transaction records the plan's command line as its argv and the plan's id as `plan`. `--actor` is the applier's, and `--reason` the applier's, else the preview's.
- **Refused flags.** `--if-match` (the plan carries its own token), `-t` (the plan names its clients) and `--while-running` (the policy was part of what was approved: preview again with it) are usage errors.
- `data` is `{plan_id, applied: {command, display_argv}, approved, result, outcomes}`: `result` is the executed command's own data, and its schema is one variant per storable command, `applied.command` naming which. The envelope's `command` is `apply`; its exit code is the executed command's.

To adopt addons installed by hand, the command is `omaddons adopt --apply`, and `apply` points there when it is mistaken for it. `omaddons apply` alone is `usage`, with `omaddons help apply` as the fix and `omaddons adopt --apply` as the alternative (it changes something, so it is never the fix). `omaddons apply <word>`, when no plan matches and the word is an addon folder omaddons did not install, is `not_found`, with the help as the fix and one `omaddons adopt <folder> -t <client>` alternative per client that has it.

### `backup`

Each client's `WTF/` backups: copies of its settings, the data players actually lose (milestone 3). `backup create` makes them, `backup list` shows them, `backup restore` puts one back (whole, or one addon's settings with `--addon`), `backup prune` deletes old ones, `backup export` writes one to a file off this disk and `backup import` brings such a file back; `backup verify` checks one against its hashes. Bare `backup` is `backup list`.

```bash
omaddons backup create                     # every live client that has a WTF/, one backup each
omaddons backup create -t retail --reason "before the raid"
omaddons backup create -t era --verify     # also hash every file it copied
omaddons backup create --dry-run           # what it would copy, copy-on-write or not, and the space free
omaddons backup list                       # every client with backups, newest capture first
omaddons backup list -t retail --limit 5
omaddons backup list 01M6Q2VA -v           # one backup by an id prefix (8+ characters), with its addon versions
omaddons backup restore 01M6Q2VA --dry-run # what it would put back, and the token that binds it
omaddons backup restore 01M6Q2VA -t retail # the whole WTF/ back as that backup holds it (undoable)
omaddons backup prune --dry-run            # what retention would delete now, and the token that binds it
omaddons backup prune -t retail --keep 3   # also your own backups beyond the newest 3 of each kind
omaddons backup prune -t retail -- 01M6Q2VA   # exactly that backup
omaddons backup export 01M6Q2VA /run/media/me/usb/retail-wtf.tar.zst   # off this disk
omaddons backup import /run/media/me/usb/retail-wtf.tar.zst -t retail   # back, on any machine
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
- **Retention.** After each backup, the client's backups get the retention pass [`backup prune`](#backup-prune) runs, outside the lock. It only ever deletes automatic backups, never the one just made. A failure is only `retention_failed` (fix `omaddons backup prune -t <id>`): the backup stands. Once a day, a run that made a backup also runs undo-snapshot [retention](#retention) for every registered client discovery finds, so a client that is played but never changed is pruned too.
- **`--auto`** makes the after-play checkpoint of [Automatic backups](#automatic-backups) instead: only when one is due, following `auto_backup`, never hashed. Without `-t` it considers every live client. A skip is a success, exit 0, with `skipped: {reason}`: `off`, `no_wtf`, `running`, `unchanged`, `no_reflink`, `space_low` or `in_progress`. A backup it makes gets its history line, the doorbell and retention like any other. With `--dry-run` it reports the decision, and what a due one would copy, and writes nothing.
- **`--dry-run`** walks `WTF/` without writing anything and reports `would: {files, bytes, copy, free_bytes}`, where `copy` (`copy_on_write` or `byte_copy`) comes from the filesystem's type, a hint until a clone is tried.

`data` is `{dry_run, run, targets: [{target, backup, tx, skipped, error}]}`. `backup` is the backup as `backup list` shows it, and `tx` its history line (`null` when the line could not be written, with `history_not_recorded`). `skipped` is `null` for a backup you asked for. A `WTF/` past 200,000 entries or 16 levels is `archive_too_large`. One whose files vanished while they were copied, on the second try too, is a retryable `io` error; the path it names keeps no account, realm or character name. Progress is `copy` events labelled `WTF/ files`, and with `--verify` `verify` events labelled `verifying`: counts under a fixed label, never a path. A run over several clients counts them as one, with a total of their files taken before anything is copied, each client continuing where the one before it ended.

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
- `integrity`: `metadata_only` (no content hash was ever compared: paths, kinds and sizes were checked, so a file damaged at the same size goes unnoticed), `verified` (its files matched their hashes when last checked: `verified_at`, `baseline`) or `damaged` (a check failed: `restorable: false`, and `backup restore` refuses it). `baseline` says when the hashes were recorded: `capture` (`backup create --verify`), `import` (checked by `backup import`) or `later` (the first [`backup verify`](#backup-verify) of a backup that had none, which proves nothing changed since then, not that the capture was intact). The human view says `verified (capture)`, `verified (import)` or `verified (later)`, never "verified" alone, nothing for `metadata_only`, and `damaged`.
- `restorable`: its `backup.json` reads and names its own directory, its `manifest.json` exists, it is not damaged, and its client's `WTF/` was inside the client dir.
- `qualifying`: whether it counts as a checkpoint: a whole, stable capture made on this machine (imported, recovered and best-effort backups never do).
- `busy`: a restore, export or verify held its directory lock when it was listed.
- with `-v`, `addons [{name, folders, version}]`: the addon versions that produced it.

A backup whose `backup.json` cannot be read is still listed, with `null` for what it would say, and is never restorable. Ids match ignoring case. An id prefix of fewer than 8 characters is a usage error, one that matches nothing is `not_found` (`details: {prefix, target, newest_ids}`, naming the newest three), and one that matches several is `ref_ambiguous` (`details: {prefix, candidates}`) with one `backup list <id> -t <client>` per candidate.

Per client, `bytes` is the backups' apparent size. The human view says "apparent, initially shared copy-on-write" when a listed backup's `reflinked` is `all` or `some` (on btrfs and xfs a fresh backup shares its data with the live `WTF/`), and always shows the free space (`free_bytes`). omaddons never claims a backup's exclusive size, which only a walk of every extent could tell. `snapshot_bytes` is the apparent size of undo's `WTF.previous` trees (0 for an unregistered client). `auto` is `{mode, capability, last}`: the `auto_backup` setting, whether a clone-only copy worked there when last tried (`capability`: `unknown`, `supported` or `unsupported`), and the last one (`last: {at, result}`), with `unknown` and `null` until they run. `-t` lists a selected client even with no backups (`total: 0`). `--limit N` shows the newest N per client, and `shown` and `total` are always reported. Listing rebuilds `~/.cache/omaddons/backups.json`, the summary `status --cached` will read, when it no longer matches the directories (and rings the doorbell when that changed it). For each listed client whose `WTF/` holds SavedVariables and that has no qualifying backup captured in the last 7 days, `commands` offers `omaddons backup create -t <id>` (action `backup_create`).

```text
$ omaddons backup list
retail (12.1.5.70391, mainline) · 2 backups · 82.4 kB apparent, initially shared copy-on-write · 212 GB free
  01M6Q2VA00000000…  today 18:02       auto (after play)  412 files  41.2 kB  stable
  01M5Z1KB00000000…  2026-10-18 21:15  manual             412 files  41.2 kB  stable · verified (capture)  "before the raid"
```

#### `backup restore`

Puts a client's whole `WTF/` back as a backup holds it: keybindings, macros, chat and every addon's settings, files and modes exactly. It is one transaction, undone by `omaddons undo <tx>`. The id is an 8+ character prefix, after `--` too. Without `-t`, the client is the one the backup belongs to. With `-t`, it must be that client: a backup of another client is `not_found`, and the fix names its own (`omaddons backup restore <id> -t <client>`). `-t` selecting several clients is `target_ambiguous`.

Every refusal comes before anything is copied, in this order:

- **The backup is reserved.** A restore holds a shared lock on the backup's directory until it commits, so a `backup prune` from another process finds it `busy` and leaves it. One deleted meanwhile is `not_found`.
- **Not a partial backup.** A partial backup (the settings a crashed restore of one addon displaced) cannot replace the whole `WTF/` (`usage`); it restores with `--addon`.
- **Newer state is refused** (`state_too_new`) when a newer omaddons wrote this client's record, files index or registry.
- **Never while the game runs.** A running client, or one that may be running because the process table cannot be read, is `game_running` (`details.reason: "saved_variables"`), whatever `running_policy` or `--while-running` say. Dry runs included.
- **It must be restorable.** A backup whose `backup.json` or manifest does not read, that is `damaged`, or whose tree differs from its manifest is `state_corrupt`, with `omaddons backup list` as the fix.
- **The filesystem must exchange.** `WTF/` must be a plain directory on the client's own mount, and the exchange is probed where the restore will run; otherwise `filesystem_unsupported`. There is no fallback (see [Safety model](#transactions)).

Then, on a real run:

- **The current `WTF/` is kept as a backup** (kind `before_restore`), but only where the filesystem clones files (btrfs, xfs), so it costs no space at first. Elsewhere, or when there is no `WTF/`, `before_restore_skipped` says so, and the restore goes on: the transaction's own snapshot keeps the replaced tree until retention. Making it runs no backup retention, and it keeps its history line even when the restore then fails.
- **The copy is checked.** The backup's `WTF/` is copied into the stage with the modes its manifest recorded, then compared with the manifest: entries and sizes, and every file's content when the backup holds hashes. A mismatch is `hash_mismatch` ("backup … is damaged"), and nothing changed. A restore or export that finds the damage records it as [`backup verify`](#backup-verify) does (`integrity: damaged`, no longer restorable), and its fix is `omaddons backup verify <id> -t <target>`, which checks the backup again.
- **One exchange.** Under the lock, the copy replaces `WTF/` in one atomic exchange, and the old tree goes to the transaction's snapshot for `undo`.
- **Retention.** Backup retention then runs once with the restored backup kept whatever its age, so a restore never prunes its own source.

`data` is `{dry_run, target, tx, kind: "restore", mode: "whole", backup: {id, captured_at, kind, integrity}, files: {restored, differed, created_since, unchanged}, before_restore: {id} | null, reload_required: "restart", precondition, plan_id, plan_expires_at, approval, exchange, addons: null}`:

- `files` compares the backup's manifest with the live `WTF/` by size and time, never content. `differed` counts the backup's files the live tree lacks or holds differently. `created_since` counts live files the backup lacks; they stay in the undo snapshot.
- `backup.integrity` is what was proved. `verified` only when the copy matched the backup's hashes; the human view then says "verified against capture-time hashes" (or import-time, or hashes recorded later, which prove only that nothing changed since then). It says "metadata checked; content not verified" for a `metadata_only` backup, and never "verified".
- `exchange` is `verified` when the exchange was probed, and `unverified` on a dry run, which does not probe it.
- `reload_required` is `restart`: the client reads `WTF/` when it starts.
- `commands` offers `omaddons undo <tx>`.

`--dry-run` stops after the plan and stores that preview (`data.plan_id`, see [Stored plans](#stored-plans)), for both whole and `--addon` restores. It reports the counts, whether the current `WTF/` would be kept as a backup, and `precondition`, which covers the client's records, the backup and its `backup.json`, and the live `WTF/`. A play session, a re-verified backup or a commit on the client between the preview and `--if-match <token>` gives `plan_stale`, and nothing changes. A commit that slips in between the copy and the lock is caught there too.

```text
$ omaddons backup restore 01M6K2VA -t retail
retail: restored WTF/ from backup 01M6K2VA00000000… (2026-10-20 19:40, auto (after play)): 410 files, 37 differed
  kept the current WTF/ as backup 01M6S7QB00000000… (before restore)
  metadata checked; content not verified
tx 01M6T0XC00000000… · undo: omaddons undo 01M6T0XC000000000000000000
```

**One addon's settings: `--addon`.** "My WeakAuras settings are gone" needs only WeakAuras's settings back, not yesterday's keybindings and macros too. `omaddons backup restore <id> -t <id> --addon WeakAuras` puts back only the SavedVariables files of the folders that addon had when the backup was taken (`<Folder>.lua` and `<Folder>.lua.bak`), account-wide and for every character, and changes nothing else in `WTF/`. `--addon` repeats.

- **Which folders.** An addon of the backup's own addon record, by name, ref or one of its folders: the folders it owned then, so it works after the addon was removed or renamed a folder. A folder it owns now that it did not then is listed in `new_folders` and left as it is. Otherwise an addon managed on the client now (its folders now, said so in the preview), otherwise a folder whose settings the backup holds. `folder:<F>` names one folder alone. A folder the addon owned then that another addon owns now is `ref_ambiguous`, never broadened: each folder alone (`--addon folder:<F>`) is an alternative. An unknown name is `not_found`, with up to 3 of the backup's folders as alternatives.
- **Each file.** One the backup has and the client too is replaced, one only the backup has is put back (`restored`), and one written since that the backup does not have is moved aside (`created_since`), so the addon's settings are exactly the backup's. Names match ignoring case, as Wine's do; a file keeps the client's spelling. Files under a folder reached through a symlink outside the client, or spelled two ways, are left alone (`saved_variables_out_of_scope`, counts only).
- **The same refusals, before anything is copied:** the game running, a filesystem that cannot exchange a file atomically, or a SavedVariables folder on another mount than `Interface/.omaddons/` (`filesystem_unsupported`, `stage_cross_mount`). Each file is staged, checked against the backup's hashes when it has them (`hash_mismatch`), then exchanged in place under the lock, a file the game rewrote meanwhile being `plan_stale`. There is no fallback to deleting and writing a file: a file that is briefly absent reads as "reset this addon" to the game.
- **No `before_restore` backup:** the transaction's own snapshot keeps the files it replaced and moved aside, and `omaddons undo <tx>` puts every one back as it was (its undo is undoable too). Undoing it conflicts like a write of those addons' settings: an older install of WeakAuras cannot be undone over a later `--addon WeakAuras` restore (undo the restore first, or keep SavedVariables with `--saved-variables keep`), and a later write of them refuses undoing the restore. `--saved-variables keep` is refused for the restore's own undo: it would leave nothing to undo.
- **The preview binds** to the resolved folders and files, and to every SavedVariables file of the client, so any play session after it refuses the bound run (`details.changed: ["observed"]`), and other addons are another selection (`["selection"]`).

`data` is the same with `mode: "addons"`, `files: null`, `before_restore: null`, `reload_required: "none"` (the next login reads the files) and `addons`: each `{addon, folders, folders_from: "backup" | "installed" | "folder", captured_version, installed_version, new_folders, files: {replaced, restored, created_since}}`. A folder the addon owned then that another addon owns now is refused as `ref_ambiguous`, with `details.current_owners` (`[{folder, owner}]`).

```text
$ omaddons backup restore 01M6K2VA --addon WeakAuras -t retail --dry-run
retail: would restore WeakAuras's SavedVariables from backup 01M6K2VA00000000… (2026-10-20 19:40, manual)
  settings written by WeakAuras 5.19.2; installed now: 5.20.0
  6 files: 4 replaced, 1 restored (absent now), 1 created since (moved aside)
  metadata checked; content not verified
Run it bound to this preview: --if-match pc1:9f3c…
Plan 01M6K2WB… stored for 24 hours. Run exactly this: omaddons apply 01M6K2WB…
$ omaddons backup restore 01M6K2VA --addon WeakAuras -t retail
retail: restored WeakAuras's SavedVariables from backup 01M6K2VA00000000… (2026-10-20 19:40, manual): 6 files
  settings written by WeakAuras 5.19.2; installed now: 5.20.0
  metadata checked; content not verified
tx 01M6R0QA00000000… · undo: omaddons undo 01M6R0QA000000000000000000
```

#### `backup prune`

Deletes old backups, for each client `-t` selects or every client with backups. It cannot be undone.

- **Without ids**, it runs the retention pass described under [Retention](#retention) ("Backups"): automatic backups are thinned by day, and your own backups are kept.
- **`--keep N`** also deletes every backup beyond the newest N of its kind (`manual`, `auto`, `before_restore`, `imported` and `recovered` each count on their own). **`--older-than D`** also deletes manual, imported and recovered backups captured before then; it takes `history --since`'s forms (`30m`, `12h`, `1d`, `2w` or an RFC 3339 time). Neither ever deletes a backup younger than a day, nor the client's newest checkpoint.
- **With ids** (8+ character prefixes, after `--` too), it deletes exactly those, whatever their age. It refuses the client's newest checkpoint with `usage` (fix `omaddons backup create -t <id>`: make a newer one first), and deletes nothing. Ids cannot go with `--keep` or `--older-than`.
- **Busy backups.** A backup that a restore, an export or a verify is reading holds a shared lock on its directory. Each deletion takes that lock exclusively without waiting, once more after 50 ms (so `backup list`'s own instant probe never counts). A backup still held is skipped and listed under `busy_backups`; that never fails the run, and a later prune deletes it. A reader killed outright frees it at once.
- **How it deletes.** Each backup is moved to `<client>/Interface/.omaddons/trash/` under the global lock, then deleted there, read-only directories included; nothing is left behind. A prune that deleted something rings the doorbell and updates `backups.json`. It appends no history line: the backups themselves are the record.
- **Preview binding.** `--dry-run` deletes nothing. It lists what would go under `pruned` (and what is held now under `busy_backups`), returns `precondition`, and stores a changing preview as a plan (`data.plan_id`, see [Stored plans](#stored-plans)). A prune with no victims stores no plan. Its stored intent resolves id prefixes to full backup ids and `--older-than` to an absolute cutoff, so the selection does not shift as time passes. With `--if-match <token>`, the real run deletes exactly those backups or nothing: if the set changed (a new backup moved `--keep`'s line) or one of them was rewritten since, it fails with `plan_stale`. See [Previews, plans and `--if-match`](#previews-plans-and---if-match).

`data` is `{dry_run, targets: [{target, pruned: [{id, kind, bytes}], busy_backups: [{id}], kept, reclaimed_for_space, error}], precondition, plan_id, plan_expires_at, approval}` (`backup list`'s `busy` is one backup's state; here the backups held are a list):

- `bytes` is apparent: on btrfs and xfs a backup may still share most of its data with `WTF/`, so deleting it frees less.
- `kept` counts the backups left.
- `reclaimed_for_space` is `true` only for deletions made to free space (by [automatic backups](#automatic-backups)), so always `false` here.
- A client whose deletions failed carries `error`, and the run is `partial` when another client's prune worked.

```text
$ omaddons backup prune --dry-run
retail: would prune 3 backups (45.1 MB apparent), kept 5
Run it bound to this preview: --if-match pc1:…
Plan 01M6K2WC… stored for 24 hours. Run exactly this: omaddons apply 01M6K2WC…
```

#### `backup export`

Writes one backup to a file, so a disk failure does not take the backups with the settings: `omaddons backup export <ID> <FILE>`. The backup is named as for `backup restore` (an id prefix of 8+ characters, on the client `-t` names or whichever has it).

- **What the file is.** A `tar` compressed with zstd (level 3; `tar --zstd -tvf` lists it): `backup.json` first (the backup's description with `verified_against`), then `addons.json` when the backup has one, then every `WTF/` entry in path order with the mode and modification time its manifest recorded, uid and gid 0 and no owner names (symlinks stay symlinks), and `manifest.json` last, with the SHA-256 of every file as it was written. It holds no export time, so one backup always exports to the same bytes. The file is mode 0600: like `WTF/` itself, it holds account, realm and character names, so keep it private.
- **Checked as it streams.** When the backup's manifest has hashes (a `--verify` capture, an import or a `backup verify`), every file is checked against them, and `verified_against` says which (`capture`, `import`, `later`); a file that differs is `hash_mismatch` (a backup damaged on disk; not retryable), its path shown without account names. Without them, `verified_against` is `computed_at_export`. A file gone or resized since the backup was made is `io`.
- **Never replaces a file.** An existing `<FILE>` is `file_exists`, and so is one another program creates while the export runs: the archive is written to a hidden temporary file beside `<FILE>` and published only if nothing is there yet. An export that fails (a damaged backup, a full disk, an interruption) leaves no `<FILE>` and removes its temporary file; a crash can leave only that hidden `.omaddons-…` file.
- **Where it may write.** Only `<FILE>`, in a folder that already exists (`io` otherwise): never inside a game client, a Wine prefix or omaddons' own config, state or cache folders (`scope_denied`). `-` (standard output) is `usage`: binary data would break `--json`'s one envelope. Nothing is copied anywhere first: the compressed bytes are written once, straight to `<FILE>`'s folder.
- **Meanwhile.** The export holds a reader's lock on the backup for its whole stream, so a `backup prune` lists it under `busy_backups` instead of deleting it. It is not a transaction: no history line and nothing to undo; delete the file to be rid of it.
- **`--dry-run`** checks the same (the backup, the destination, whether it exists) and writes nothing.

`data` is `{dry_run, target, backup, file, files, bytes, compressed_bytes, sha256, verified_against}`: `backup` as `backup list` shows it, `files` and `bytes` the backup's files and their apparent size, `compressed_bytes` and `sha256` the archive's size and SHA-256 as written (`null` in a dry run).

```text
$ omaddons backup export 01M6Q8PN /run/media/me/usb/retail-wtf.tar.zst
exported backup 01M6Q8PN (retail, today 18:02) to /run/media/me/usb/retail-wtf.tar.zst: 412 files, 15.2 MB → 1.9 MB
```

#### `backup import`

Brings an exported backup back as a backup of the client `-t` names (required): `omaddons backup import <FILE> -t <id>`. Then `backup restore` puts it in place, with its undo, its running-game refusal and its atomic exchange. With `omaddons export` and `sync --locked` for the addons, that moves or rebuilds a whole machine.

- **What you get.** A new backup of kind `imported`, `integrity: verified` with `baseline: import` (every hash in the archive was just checked, and its `manifest.json` keeps them), its original capture time, and `source` naming the backup it was exported from. It never counts as a checkpoint (`qualifying: false`): an archive says nothing about whether today's settings are backed up, so the day's [automatic backup](#automatic-backups) still runs. [Retention](#retention) never deletes it; `backup prune <id>` does when you ask. It has a history line of kind `backup` (`kind: imported`) and rings the doorbell.
- **Only an exact export.** The archive becomes files inside a client folder, so anything but an unmodified `backup export` file is refused, before it is kept and mostly before a byte is written: the archive is read through once, checked whole, and only then decompressed again into a staging folder, with each file hashed again as it is written and symlinks created last. Refused, with `details.reason`:
  - an entry of a kind an export never holds (a hard link, a device, a FIFO, a sparse file, an unknown record): `unsafe_archive`, `unsupported_entry`;
  - a name that is not a plain path under `WTF/` (`..`, an absolute path, a drive letter, a backslash, a control character): `bad_name`; two entries of one name, ignoring case: `duplicate_name`; a second `backup.json` or `manifest.json`: `duplicate_metadata`; neither: `missing_metadata`;
  - entries that differ from the archive's manifest (a file it does not list, one it lists that is missing, another size, mode, link target or SHA-256), or an entry whose folder is a file or missing: `manifest_mismatch`;
  - a symlink that leaves `WTF/`, passes through another link, loops, or has entries placed below it: `symlink_escape`;
  - more than 200,000 entries, 8 GiB expanded, an expansion past 200 times its size (beyond 32 MiB), a path over 4,096 bytes, a PAX or long-name record over 64 KiB, a `manifest.json` over 128 MB (640 bytes for each entry it may list) or a `backup.json` over 1 MiB: `archive_too_large`, naming the bound;
  - a zstd frame asking for a decoder window over 16 MiB (refused before it is allocated), a corrupt or truncated file, description files that do not read: `bad_archive` (`decoder_window`, `corrupt`, `truncated`, `metadata_unreadable`).

  After any refusal no staging folder is left and nothing new is listed. A path a refusal names is shown only below `Account/`, with every name that could be an account, realm or character replaced (as `account 1`, or `…`).
- **Another kind of client.** A backup from a client of another family (a Classic Era backup into retail) is `wrong_flavor`, whose fix is the same import into a client of the backup's family when you have one, and whose alternative adds `--force`, which imports it anyway.
- **`--dry-run`** reads and checks the whole archive, and the family, and writes nothing.

`data` is `{dry_run, target, backup, source, files, bytes}`: `backup` as `backup list` shows it (`null` in a dry run), `source` the exported backup (`{id, target_id, family, created_at, captured_at, tool_version}`).

```text
$ omaddons backup import /run/media/me/usb/retail-wtf.tar.zst -t retail
imported backup 01M6Q8PN (retail, 2026-10-21 18:02) as backup 01M7A3XC on retail: 412 files, 15.2 MB
restore with: omaddons backup restore 01M7A3XC -t retail
```

#### `backup verify`

Checks what a backup's content is proved to be, or establishes it: `omaddons backup verify <ID>… [-t <id>]`. Each id is an 8+ character prefix; an unknown one refuses the run (`not_found`) before anything is hashed.

- **With hashes** (a backup made with `--verify`, an imported one, or one verified before): every file is hashed and compared. All matching, the backup stays `verified` with the baseline it had, and a new `verified_at`. One differing, even at the same size, makes it `damaged`: `backup list` shows it, `backup restore` refuses it (`state_corrupt`), and the run is `partial`, still exit 0, since the check itself succeeded.
- **Without hashes** (automatic and plain `backup create` backups do not hash, to stay fast): the hashes are recorded now, as a `later` baseline, and the result says "no hashes existed; recorded a baseline (this cannot prove the capture was intact)". Every later verify compares against them, so it catches any change from then on.
- **First the structure:** a file missing, extra or of another size or kind is `damaged` without hashing.
- **`--dry-run`** hashes and compares, and records nothing: a backup without hashes then establishes no baseline (`integrity: metadata_only`), and says so.
- It reads the backup under its reader reservation, so a `backup prune` meanwhile finds it `busy`; it takes no global lock, so it runs beside any other command, and it appends no history line (a check, not a change). It writes the manifest first and `backup.json` second, so a crash between the two leaves hashes the next verify compares against. A recorded result rings the doorbell.

`data` is `{dry_run, targets: [{target, backups: [{id, integrity, baseline, files, checked, mismatched, note}]}]}`. `note` says what the result proves, never "verified" without what against; a mismatch names the first differing file with account, realm and character replaced.

```text
$ omaddons backup verify 01M6Q2VA 01M6Q8PN -t retail
retail: backup 01M6Q2VA00000000… verified (later): 412 files, no hashes existed; recorded a baseline (this cannot prove the capture was intact)
retail: backup 01M6Q8PN00000000… damaged: 410 files, 1 files differ from their recorded hashes; the first: Account/account 1/SavedVariables/WeakAuras.lua
```

### `history`

What changed, when, by whom and why, newest first (`log` is an alias).

Settings changes appear too: `config set`, `targets add`, `rm`, `rename` and `default`, `auth github set` and `clear`, and `export --write` each record a line of kind `config`, `target`, `auth` or `export`, with zero folder counts (human output shows what was run instead). `errors dismiss` records one line of kind `dismiss` per client it acknowledged, with `dismissed: {databases, errors}` (the table shows `2 errors in 1 database`), and `omarchy install` and `omarchy remove` one of kind `widget`. `robot-docs install-skill` records one of kind `skill` with the folder it wrote (`dir` in JSON, on `skill` lines only). A dry run, or a change to nothing (an `auth github set` of the token already stored, a `clear` with none, a skill already current), records none. Their `target` is the client for a `target` or `dismiss` line (`null` for a client not registered yet) and `null` otherwise, so `-t` shows only those lines. `undo` never reverts them and steps over them; `undo <tx>` of one is `not_found`. A line that cannot be written after the change was made warns `history_not_recorded`. Each backup `backup create` makes is a line of kind `backup` on its client, with no folder counts and the backup it made (`backup: {id, kind, files, bytes}` in JSON, `WTF/ as <id> (<files>)` in the table); `undo` steps over it too.

A repair made by `doctor fix` is a line of kind `repair` that names no single client: `target` is `null`, `targets` lists the clients whose files it touched (`{id, key}` each, empty when it touched only files outside every client, such as a credential's permissions), `doctor_run_id` names its doctor run directory, and `repairs` lists each filesystem operation in order, as the repair recorded it: `seq`, its failure mode `fm`, `op` (`write_file`, `chmod` or `rename_noreplace`), the absolute `path`, a rename's `to`, the `mode` it set, and the `pre` and `post` state it checked (`kind` `absent`, `file`, `dir` or `symlink`, with the identity, digest, mode, directory stamp or link target compared). The undo of a repair is a line of kind `undo` of the same shape. `-t` shows such a line under each client in `targets`, and under every client when it touched a client's files without naming one. The table shows its clients, its first operation and path, and the doctor run. A line that names no client but is neither a repair nor the undo of one, or one with folder operations, is skipped and counted in `history_lines_skipped`.

```bash
omaddons history
omaddons history --limit 50 --since 2w
omaddons history --by agent --since 12h
omaddons history --run 01M51F4R2ZQX        # one run's transactions (a prefix of 8+ characters works)
omaddons history -t retail -v --json       # each folder operation (ops) instead of counts
```

`--limit` defaults to 20, and `truncated` says more lines matched. `--since` takes `30m`, `12h`, `1d`, `2w` or an RFC 3339 time. An explicit `--actor` filters too (`OMADDONS_ACTOR` never does); given with a different `--by`, it is a usage error. In JSON every line has its `run` (`null` for transactions from before milestone 2, each of which is a run by itself), and an undo has `reverts`; the human table names a run only on rows whose run holds more than one line.

Every line also has `plan`: the [stored plan](#stored-plans) its transaction was committed from, `null` on every other line. A transaction [`apply`](#apply-plan_id) commits records the plan's `display_argv` as its argv and the plan's id as `plan`. With `-v`, the human table adds a column naming the plan (`plan 01M9P0000000000…`) when some row has one.

Each line counts the folders added (`+`), replaced (`~`) and removed (`-`). A transaction that changed records without moving a folder, such as forgetting an addon whose folders were gone, also shows `=N` for the folders it recorded (`changes.recorded` in JSON). A `backup restore` is a transaction of kind `restore`, undone like any other. `changes.restored` counts the settings files the restore put back (`· N files restored`), and its ops name settings, not AddOns folders: `wtf` for a whole `WTF/` (`{"op":"wtf","folder":"WTF","addon":"","backup":…,"from":"backup","files":N}`) and `saved_variables` for one addon's files.

### `plans show <PLAN_ID>`

What a stored plan approved, and whether history records it as applied: the answer for an `apply` whose output was lost (a broken pipe, a restart, an agent that crashed) before you preview again or change anything. It reads the plan in the cache and a bounded tail of `history.jsonl`, and never re-plans: no lock, no request, no write, so it is safe while anything runs, `--offline` included.

```bash
omaddons plans show 01M8Q2VA           # what plan 01M8Q2VA… approved, and whether it was applied
omaddons plans show 01M8Q2VA --json    # the same as one envelope
```

```text
$ omaddons plans show 01M8Q2VA
plan 01M8Q2VA7K3N5P8R1T4W6X9Y0Z (update): partial, 1 of 2 clients committed
approved: omaddons update
  classic: would update DBM 12.1.14 → 12.1.15
  retail: would update DBM 12.1.14 → 12.1.15
committed:
  classic: tx 01M8T4XQ0000000… at 2026-10-30 18:05 · undo: omaddons undo 01M8T4XQ00000000000000000A
not recorded: retail
apply never resumes a partial plan; preview again for the rest: omaddons update --dry-run
```

- **The id.** The full id or a prefix of 8+ characters, in any case. Stored plans are matched first; a plan whose file is gone (expired and swept, or the cache emptied) is still found through history when it committed something. A prefix two plans share is `ref_ambiguous`; one that names nothing is `plan_not_found`.
- **`state`:** `pending` (stored, unexpired, nothing committed: `apply` it), `completed` (a committed transaction on every client the approval named), `partial` (some, not all: `apply` never resumes it, so preview again for the rest), `expired` (past its 24 hours, nothing committed) or `unknown` (the file is gone or does not read, history was cut by its bound or holds lines that do not read, or an interrupted transaction on an approved client may hold its commit). Only history lines that carry the plan's id count as applied, never addon versions that happen to match.
- **`backup prune`** appends no history line, so a prune plan is always `unknown` here: preview it again to see what is left.
- **`stored` and `readable`:** whether the plan's file is in the cache, and whether it read. A file that does not read is reported (`readable: false`), never trusted.
- **`evidence`:** `targets`, one `{target, key, tx, result, committed_at}` per history line carrying the plan, oldest first, and `complete`, `false` when the scan stopped at its bound (20,000 lines) before the plan was made.
- `data` is `{plan_id, state, stored, readable, created_at, expires_at, build_id, build_matches, intent: {command, display_argv}, approval, inputs: [{path, present}], evidence}`, every key always present (`null` when unknown). `build_matches` says whether this build made the plan (`apply` refuses one another build made), and `inputs` whether each local file it installs from is still there. `commands` offers the undo of what it committed (`undo --run <run>` first when one run holds several transactions). `-t` is ignored: a plan names its own clients. With `-v` it adds the approval's reasons, the build, the inputs and how far history was read.

### `list`

The addons in each target's AddOns folder (`ls` is an alias).

```bash
omaddons list                              # every target: managed addons with version, ref, channel
omaddons list -t forever --unmanaged       # also unmanaged, shared, linked and invalid folders, with title, version and load verdict
omaddons list -t wotlk --protected         # also Blizzard_* folders
omaddons list -v                           # also each addon's drift (files changed on disk), with paths
omaddons list --deep                       # drift by content: every managed file re-hashed
```

Each addon also carries `latest {version, status, source, release_id, asset}`: the newest release remembered from earlier checks, its status as `outdated` would judge it (`current`, `update_available`, `pinned`, `ahead`, `blocked`, `failed`, `unchecked` or `static`), and which release that is: its source (`github`, `wowi`, `tukui`, `git`, `wowace`, …), its release id and the file `update` would try first, all `null` when the status names no release. They are what an `update` preview's approval names for that release. `list` never asks a source. Its column shows `→ <version>` when a newer release is known (`update_available`, `pinned`; `blocked` adds `(blocked)`), `check failed` after a failed check, and is blank otherwise. With `-v` or `--deep` each addon carries `drift {state, files}` too (as in [`status`](#status); the paths with `-v`); a plain `list` checks no file. Each addon carries `reason` (`explicit` or `dependency`), `required_by` and `orphan` (a dependency nothing requires any more): the table marks a dependency `dep` (`dependency of LittleWigs` with `-v`) and an orphan `orphan`. It also carries `any_flavor`, marked `any-flavor` in the table.

Each client carries `counts` per folder class and `addons_dir_missing`; the human view's line under each table names both units, addons and the folders they own (`1 addon in 22 folders · 0 unmanaged · 39 protected · 0 linked folders`), as `status`'s row does (`1 addon in 22 folders · 0 unmanaged · 39 protected folders`), and each addon its `missing_folders` (managed folders gone from disk). Under `--unmanaged`, each folder's load verdict is `compatible`, `out_of_date`, `wrong_flavor` or `no_toc`, and folders whose names differ only in case are flagged `case_duplicate`. `-v` adds each addon's folder names, `folder_tocs`, `left_out` (as [`info`](#info-ref) shows it; the human view lists them under the table as `DBM-Core left out: DBM-Brawlers (gated to other game types), …`) and the `client_dir`.

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
| `update_available` | A newer release exists. `confidence` says how likely it is to load: `verified` (a download proved it), `declared` (the publisher's metadata names this client), `likely`, `unlikely`. For WowAce, the installed tag moved upstream is one too: `latest.retagged: true`, with its new `latest.revision`, shown as `v12.1.1 (re-tagged upstream)`; a commit anywhere else in the repository is not |
| `pinned` | Pinned (by `pin` or `install --version`), and a newer release exists (`latest`) |
| `blocked` | The newest release is known not to load on this client (`blocked_reason: verified_not_loading`), or targets an older interface than the adopted build installed (`older_than_installed`) |
| `ahead` | The installed release is newer than the newest one on its channel |
| `failed` | The source cannot answer for it (a deleted repository: fix `omaddons remove <name> -t <id>`, and if it moved, the alternative `omaddons adopt <folder> --ref <new ref> -t <id>` re-points it; or a fetch error) |
| `current`, `static`, `unchecked` | Shown with `-v` only (`unchecked_reason`: `never_checked`, `offline`, `source_disabled`, `no_eligible_release`) |

When a fetch fails, a remembered answer is used and the warning says so; with nothing remembered, the item is `failed`. Either way the run is `partial`. `--verify` downloads only the updates not yet verified, in a dry run that takes no global lock, and remembers the verdicts: a candidate that does not load becomes `blocked` from then on. A remembered verdict holds only while the client is judged as when it was made: each records a digest of what judging reads (the client's rules row with any `[client_rules]` patch, the family ranges, and the judging revision, bumped when an omaddons release changes a verdict rule). After such a change, an old verdict counts as unverified until the next `--verify`, plan or install judges again; the archive's hash is still remembered. `outdated` writes only the release memo, the download cache and `net.log`.

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

A TOC id that disagrees with stronger evidence is listed in `evidence` and `alternatives` (at most 3) and never chosen. Folders the release ships but this client does not load are listed in `unloaded_folders` and do not prevent `exact`; a group made only of them is not adopted (`wrong_flavor`), as its record would hold no folder. A folder whose addon is already managed (`already_managed`), that holds a symlink, device, FIFO or socket or cannot be read (`unindexable`), or that exceeds the archive limits (`limit_exceeded`), is listed in `failed`. Sources the report could not consult are listed in `sources_skipped`.

Checking downloads each identified group's newest release once, through the download cache (announced first as `verify: {groups, downloads, bytes}`), so a second run downloads nothing. `--offline` checks only what the release memo and the cache already hold. The report writes only the release memo, the WoWInterface index, the download cache and `net.log` (plus an in-flight lock while it downloads). Its `commands` are runnable: `adopt` (`adopt --apply`, scoped to the one client that has something to apply, else to your own `-t`), `adopt_folder` for each uncontested guess, and `adopt_with_ref` for each candidate of a contested one.

**Adopting.** `--apply` adopts every `exact` and `identified` group of the selected clients (`--dry-run` shows what it would adopt). Folder names adopt the groups holding them, from `guess` up (`--apply` with folder names is a usage error). `omaddons adopt apply` is read as the flag: a usage error whose fix is `omaddons adopt --apply`, your other arguments kept (a folder really named `apply` is adopted with `--ref`, or beside other folders). [`omaddons apply`](#apply-plan_id) runs a stored preview, which is something else. `--ref R` (exactly one folder, on one client: `-t`, else the only client that has it; several clients holding it is `target_ambiguous`) decides where the folder comes from, overriding identification; on a folder already managed it re-points its addon to `R` instead, so the next `update` installs from there. Unless `--no-verify`, a group being adopted is checked as in the report. Each client gets one `adopt` transaction (one run: a bare `undo` reverts them all), with `reload_required: none`, allowed while the client runs. An `exact` group is recorded with its release (`version_confidence: release`); any other with its TOC's version (`toc`), so `outdated` and `update` treat it as an install of that version, and never replace it with a release built for an older interface (`blocked`, `older_than_installed`). Folders the client does not load stay unmanaged. A name another addon on the client already has is recorded as `<name> (<primary folder>)`, with an `adopt_name_changed` warning. Undo forgets the records and leaves every folder as it is.

`--dry-run` of an adopting run returns `data.precondition`, and `--if-match <it>` adopts exactly what that preview showed, or nothing: a folder whose content changed since the preview (any file, clean or not, or an added empty directory) is `plan_stale` (`observed`, not retryable). A `touch` alone does not change the preview. The report takes no `--if-match` (`usage`); its fix keeps the report and drops the token.

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

Folders are hashed before the lock and checked again under it: one that changed in between makes its client's transaction `plan_stale` (retryable), and nothing of it is recorded. Under `--if-match`, a folder that changes after the run's own check and before its lock is caught first by the approval checked again under the lock: `plan_stale` with `observed`, not retryable, naming the folder (see [Previews, plans and `--if-match`](#previews-plans-and---if-match)).

### `targets`

List the clients, or add, remove, rename and choose them.

```bash
omaddons targets                           # id, key, family or modes, version, kind, label, path
omaddons targets --rescan                  # ignore the discovery cache
omaddons targets add /mnt/games/turtle --name turtle   # a root, flavor folder, client dir, Interface or AddOns path
omaddons targets rename wotlk warmane
omaddons targets default retail            # install's default when -t is absent; config default_target still wins
omaddons targets rm warmane                # unregister; nothing in the client directory is touched
omaddons targets rm classic --purge --dry-run   # an uninstalled client: what a purge deletes
omaddons targets rm classic --purge --yes --if-match <token>   # delete exactly that preview
```

`targets rm` binds to its preview like the other changing commands: its `--dry-run` returns `data.precondition`, and `--if-match <token>` forgets the client only if the id still names the same one (`plan_stale` otherwise).

**Purging an uninstalled client.** `targets rm <id> --purge` also deletes what omaddons kept for a client that is no longer installed (`missing` in `status`): everything of its `Interface/.omaddons/` (its `WTF/` backups, undo snapshots, repair halves, leftover stages and its `target.json` marker), the state halves of the transactions that touched it alone, and its record and files index. Then it unregisters the client. It never touches `Interface/AddOns/`, what the launcher or Proton left (fonts, loader DLLs, shader caches), `history.jsonl` (the purge adds its own line, as every `targets` change does) or any other client's data. Anything under `.omaddons/` that omaddons did not write is listed in `kept_entries` and left, and `.omaddons` stays with it.

- **Only for an uninstalled client.** `--purge` on a client discovery finds is a usage error whose fix is the plain `targets rm <id>`, raised before anything is locked or written.
- **Preview first.** `--dry-run` lists each path that would go in `data.purge.deletions` (`kind`: `marker`, `backup`, `snapshot`, `repair`, `stage`, `trash`, `state_snapshot` or `record`, with its apparent `bytes`), the backups by id, kind, capture time and folder (`backups`; copy one out of its folder first to keep it), the transactions that can no longer be undone (`lost_undos`) and the total `bytes`. It writes nothing and returns `data.precondition`.
- **A real run needs `--yes`.** Without it, the run is a usage error whose fix is the preview. With `--if-match`, it deletes exactly what that preview listed, or nothing (`plan_stale`: a snapshot or backup appeared or went, or one was replaced under the same name).
- **How it deletes.** Under the global lock it recovers unfinished journals, as every `targets` change does, and refuses while a stuck journal protects anything it would delete. It plans again, then moves every victim into a trash folder (`.omaddons/trash` or the state's `trash`) and deletes them after releasing the lock. It takes the lock again to empty the client's trash and remove the emptied directories, and unregisters the client last. A crash or an I/O failure at any step leaves the client registered and missing, with every victim either in place or in trash and no backup half deleted, and the next purge finishes the job. A backup another omaddons process is reading refuses the purge with `locked`.
- **It cannot be undone.** The undo snapshots it deletes are undo's data.

Each client carries its `display_name`, as [`status`](#status) gives it. Each client's `found_by` names the discovery route that reached it first (see [Where discovery looks](#where-discovery-looks)): `product_db`, `launcher:<name>`, `process`, `extra_root`, `scan`, `manual` (added with `targets add`), `registered`, `remembered`, or `registry` for a registered client whose directory is gone or that Battle.net uninstalled.

An id you choose (`--name`, `rename`) must match `^[a-z0-9][a-z0-9._-]*$`, be at most 32 characters, contain no `@`, and not be a key, family name or product code, or `all`. Built-in default ids such as `vanilla` or `mists` are exempt, which is why they can shadow a family (see [Choosing a target](#choosing-a-target-with--t)).

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
- `unknown`: GitHub, whose search gives repository metadata only.
- `likely` or `unlikely` from the release memo, for a GitHub or WoWInterface hit that is otherwise `unknown` on a client: when an earlier `info`, `install`, `outdated` or `update` remembered the addon's newest stable release, search judges it as [`info`](#info-ref) does, without a request. Only evidence counts: the release's flavor metadata naming the client, or a plan having verified it loads, is `likely` (never `yes`, since the memo may be days old); metadata naming other clients only, or a plan having verified none loads, is `unlikely`; a release with no flavor metadata stays `unknown`.

Names, authors and versions from WoWInterface are shown with their HTML entities decoded (`&amp;` is `&`). Text from any source loses its control characters (so it cannot send terminal escapes) and the characters that reorder or hide text, and is cut at 200 characters.

`commands` holds one next step for each of up to three results, best first:

- `omaddons install <ref> -t <client>` (action `install`) when some selected client is `yes` or `likely`, on its best client: `yes` before `likely`, and among equals the default target, then a live client before a test one, then the client with the most managed addons, then retail, then by id.
- `omaddons info <ref>` (action `info`) when every selected client is `unknown`. No client is guessed from no evidence: `info` reads the newest release and judges each client. The human view prints it as `Details: omaddons info <ref>        (judges each client)`.
- Nothing for a result that is `no`, `unlikely` or `abandoned`.

A source not consulted is listed in `sources_skipped` (`offline`, `rate_limited` with `reset_at`, `disabled`, `no_index`). A spent GitHub search is not a failure; any other GitHub error lists `github` as `no_index`. Any source that could not be read, other than a spent GitHub search, makes the run `partial`, with a warning naming the failure: its fetch failed, or it is rate limited. When no result has a next step (each is `no`, `unlikely` or `abandoned` on the selected clients), the human output says "No install suggested". A stored WoWInterface index that answers because a refresh failed is no failure: its `stale_check` warning says the refresh failed and why. `--offline` searches the catalog, the stored index and the remembered Tukui list, and lists the other sources `offline`.

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
Capture Lua errors: omaddons install buggrabber -t era
```

A client without BugGrabber gets the install of the catalog's build for its family (`omaddons install buggrabber -t <id>`, which installs the newest WowAce tag on modern clients, r152 on 3.3.5a, r130 on 2.4.3 and the 1.12-era tree at r43 on 1.12.1). Where no build is known (4.3.4 and 5.4.8: every BugGrabber revision for them needs the WowAce packager's localization step, which no keyless source performs), the line says "no Lua errors are captured: no BugGrabber build for this client is known", the client's `capture_note` says the same, and no command is offered. `omaddons install buggrabber bugsack` adds the BugSack display too.

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

**Counts and filters.** Each client's counts, `new`, the memo and `--check` come from every error read; `--since`, `--addon` and `--limit` only shape what is listed (`summary.matched` and `summary.shown` say what they kept), so a narrow read never makes a client look clean. `errors` exits 0 whether or not there are errors. `--check` exits 6 (`check_failed`, the data kept) when a selected client has new local errors among those `--addon` and `--since` keep (an `uncertain` one counts), with `details.reason` `new_errors`, or when it could not read everything, with `details.reason` `incomplete`; it never looks at `--limit`.

`commands` holds `omaddons update <addons> -t <id>` for managed addons with new errors and a newer release in the release memo (no request is made), `omaddons errors dismiss -t <id>` (action `dismiss_errors`) for a client with new errors a dismissal would clear, and `omaddons install buggrabber -t <id>` (action `install_buggrabber`) for a client without BugGrabber that the catalog has a build for. `--addon` on clients none of which capture errors is `not_found`.

The JSON lists per client `capture` (`buggrabber` or `none`), `capture_note` (only when no BugGrabber build is known for the client), `coverage` (`complete` or `truncated`), `files` (`read`, `accounts`, `partial`, `skipped`), `databases` (`id`, `account`, `provider`, `shape`, `session`, `state`, `baseline`, `acknowledged`: the session a dismissal covers, or `null`), `summary` (`entries`, `occurrences`, `addons`, `new`, `matched`, `shown`, `uncertain`, `no_time`, `received`), `addons`, each with `count`, `new`, `entries`, `last_seen` and its `errors` (at most 10: the new ones first, then the most frequent; `entries` counts every one), `dismiss` (`precondition`, the token that binds a dismissal to this read, and `databases`, how many it would acknowledge) and `dismissed_at` (when the newest dismissal in effect was made, or `null`). An error has its `fingerprint` (`e:` and 16 hex digits, the same in both file formats and across reformatting), `database`, `origin`, `message_kind`, `message` (or `null` and `message_chars`), `location`, `attribution`, strict `stack`, `count`, `sessions`, `new`, `last_seen` and `type` (`error`, `event` or `warning`). A `last_seen` is `{at, meaning, tz}`: `meaning` is `last`, `first_or_last` or `unknown`, `tz` is `explicit` (a unix time) or `system_assumed` (a string read in this machine's zone, which may not be the one that wrote it); a combination of times keeps the weakest.

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

`catalog` is the entry the query names: for a name with variants, the selected client's build (one `-t`, else the default target's, else the modern one) with its `variant`, and the others' `variants` (`{variant, families}`), shown as `other build (r152): families wrath-legacy`. `latest` is `{version, release_id, channel, flavors, targets, compat}`. `compat` says, for each selected client, how likely the release is to load: `verified` (a plan proved it) or `blocked` (a plan proved it does not), `declared` (its release metadata names the client), `unlikely` (the metadata names other clients only, the catalog lists none of the client's families, or no asset of the release is for the client), else `likely`. `targets` lists the clients that are `verified`, `declared` or `likely`, and the human line reads `likely to load on … · not on …`, or "not likely to load on any client here" (`-v` lists each client, and adds `asset_urls` and `record_path`). A hint only: `install` verifies every TOC.

The newest release comes through the [release memo](#files-and-state): a check made in the last 15 minutes is reused without a request, and `--refresh` checks again anyway. With `--offline`, `latest` is the remembered answer whatever its age, with a `stale_check` warning, or `null` (as above) when nothing is remembered. A missing repository is remembered too, so asking again within the window costs nothing; a network or server error never is. Each installed record carries its `drift {state, files}` (as in [`status`](#status), with the paths under `-v`), checked locally, so `--offline` shows it too, and its `reason`, `required_by`, `orphan` and `any_flavor` (` · any flavor (--any-flavor)` in the human view) on that target. It also carries `left_out`: the folders of the installed release this client does not load, each `{name, reason, …}` exactly as the install's `folders.skipped` gave it (DBM's retail-only modules on Classic, ArkInventory's example module), so the reason a module is missing stays on record after the run's output is gone. Every install or update replaces it, `undo` restores it with the record, and `export` and `copy` never carry it (each client's install is judged there). The human view says `left out: DBM-Brawlers (gated to other game types), …` under the record's line.

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

### `doctor`

What is broken in what omaddons manages, with the evidence and the exact remediation: every check of the doctor's failure-mode registry, each within a budget and isolated from the others. Bare `doctor` is `doctor diagnose`. It changes nothing outside its own run record in `~/.local/state/omaddons/doctor/`, takes no lock, and makes no request unless `--online` asks for its network checks.

```bash
omaddons doctor                       # every check but the deep ones
omaddons doctor --quick               # the cached and quick checks only
omaddons doctor --only fm-userland_state-target-vanished,schemas
omaddons doctor --skip plugins --severity P2
omaddons doctor --budget 500ms        # the whole run's time, shared by the checks
omaddons doctor --since latest        # what changed since the last run
omaddons doctor -t retail --json
```

| Flag | Meaning |
|---|---|
| `--only IDS` | Only these checks: failure-mode ids or subsystem names, comma-separated (repeatable). Alone it runs exactly what it names, whatever the tier. An unknown one is a usage error: its fix corrects it in place to the closest id, or, when none is close, says what an item looks like, with `omaddons help doctor` as the alternative |
| `--skip IDS` | Not these (wins over `--only`) |
| `--severity P0`…`P3` | Report findings of that severity and more severe ones only. A check that could not finish, or crashed, is reported whatever it says |
| `--quick` | The cached and quick tiers only (without it, every tier but `deep`) |
| `--include-deep` | Every tier |
| `--online` | Also the checks that need the network: GitHub's REST budget, whether GitHub takes the configured token, and whether the sources your addons come from answer (see **Online checks** below). Without it they are listed in `skipped` (`online_required`). With `--offline` it is a usage error. `--only` naming only such checks without it is `online_required` (doctor exit 6), and a mixed selection lists them in `skipped` |
| `--budget 500ms`, `2s` | The run's whole time, shared by the checks' declared costs. A selection declared to cost more is refused before anything runs (`doctor_refused`, `details.reason: "over_budget"`, doctor exit 4), with the budget it needs as the fix |
| `--since RUN` | Compare with an earlier run, by its id or `latest`: `since: {run, new_instances, resolved_instances, unchanged}`, by instance key. An unknown or interrupted run (no `report.json`) is `not_found` (doctor exit 66), with the newest finished runs as fix and alternatives |

`-t` narrows the checks that look at clients; findings about the machine stay.

**What it reports.** Each finding names its failure mode (`id`); its `instance`, which is the failure mode, the client's key or `machine`, and the resource, and stays the same across renames; a `label`; its `severity`, `subsystem`, `title` and `confidence`; its client (`target`), `addon` and `folder`. It also carries the `evidence`: paths, digests, modes, counts and positions, never a file's content and never a path below `WTF/`, where the game keeps account names. Last comes its `remediation`: `command` and `argv` (`null` for advice), the `doctor explain` command (`explain_command`, `explain_argv`), `auto_fixable`, `needs_force`, `estimated_actions` (the operations a fix would take; 0 until `doctor fix` plans it) and `destructive`. A check whose input could not be read (a broken `config.toml`, a corrupt registry) is listed in `skipped` with its reason, never reported clean, and the file it could not read is a finding of its own.

**Budgets.** Each check runs under a budget: 5 ms in the cached and quick tiers (`QUICK_DETECTOR_BUDGET`, twenty times that in debug builds), 2 s in the others (`DOCTOR_DETECTOR_DEADLINE`), and at most 20,000 entries for any walk or listing (`DOCTOR_SCAN_MAX_ENTRIES`). A check a limit stops is `fm-state_files-check-incomplete` (P3), naming the check, the limit and its value. A check that panics is `fm-state_files-detector-crashed` (P1), naming the check and the place in omaddons' source, and the run goes on. Either way, the run is never `DONE_OK`.

**Online checks (`--online`).** Three checks ask the network, and only with `--online`: `fm-network-github-rate-limited` and `fm-network-credential-rejected` each send one `GET` to `api.github.com`'s `/rate_limit` (which costs none of the budget), and `fm-network-source-unreachable` sends one request to each enabled source an installed addon comes from: GitHub's `releases/latest` of the first such repository, WoWInterface's file details of one project, Tukui's list, WowAce's tag list (`PROPFIND`), in that order, so at most four. `git:` hosts, `url:` and local addons are never probed. Every probe is one wire request with its own `net.log` line, purpose `doctor-probe`: no retry, no followed redirect, and the whole call ends within its check's deadline. The GitHub token, when one is configured, resolves as for any REST request (a `command` credential runs) and goes only to `/rate_limit`. A probe that gets no answer the check can judge (a status it cannot read, or a failure of omaddons' own, such as a `net.log` line that cannot be written) is never reported as a pass or as an unreachable source: it is `fm-state_files-check-incomplete` with the limit `unanswered`, whose remediation asks again (`omaddons doctor --online --only <check>`). A source that does not answer at all is a finding of medium confidence, since the request may have failed on this machine's own network; a 404 is an answer.

**The run directory.** Every run gets `~/.local/state/omaddons/doctor/runs/<UTC time>__<ULID>/`, as in `2026-10-30T20-45-20Z__01M678YN…`. It is made under a hidden name at 0700 and published with one rename. `report.json` holds the envelope as printed, with `kind: "diagnose"` and no `meta`; `runtime.jsonl` holds one line per write the run made. When the run ends both are made read-only (0400); the directory stays 0700. `doctor/latest` links to the newest finished run and never moves back to an older one. Refusals (a usage error, `online_required`, `doctor_refused`, an unknown `-t` or `--since`) come before the run starts and create nothing. A run directory that cannot be created exits 73.

```text
$ omaddons doctor
omaddons doctor · 15 checks · 1 finding (1 P1) (7 checks skipped, 3 online checks skipped)
  P1 fm-configs-config-parse-error  machine · ~/.config/omaddons/config.toml
     config.toml does not parse (line 1, column 19)
     → fix line 1, column 19 of ~/.config/omaddons/config.toml; omaddons never rewrites a config.toml that does not parse
run 2026-10-30T20-45-20Z__01M678YN3A22K2ASPW2VZH88A8
```

With `config.toml` unreadable, the checks that need it are skipped (`-v` lists them) and no client is counted.

The JSON is the [doctor envelope](#the-doctor-envelope), and its report keys are these:

- `view` (`full`), `doctor_contract_version`, `run_id`, `run_dir`, `started_at`, `finished_at`;
- `state`: `DONE_OK` with no finding at all, else `DONE_FINDINGS`;
- `summary`: `total_findings`, `by_severity` (`P0` to `P3`), `auto_fixable`, `needs_force`, `manual`, `online_required`, `skipped`, `incomplete` and `duration_ms`;
- `findings`, by severity, failure mode, client and instance;
- `skipped`: `{id, reason, target}` each;
- `since`, with `--since`;
- `next_steps`: one `omaddons doctor explain <fm> [-t <client>]` per failure mode found (`-t` when all its instances are on one client), the same commands `commands[]` carries as `doctor_explain`.

It exits 0 when `DONE_OK` and 1 when `DONE_FINDINGS`. `run_id`, `run_dir`, `started_at`, `finished_at` and `summary.duration_ms` change with every run; the rest of two runs over one state is the same.

**The failure modes.** The doctor's catalog is code (`src/doctor/registry.rs`): 31 failure modes, 10 of them with a fixer (the blob quarantine, the delegation to recovery on two journal rows, the forced set-aside on six, and the credential chmod). [`status`](#status) reports the cheap ones as its issues, through the doctor's own loop; its column here says which: `C` with and without `--cached`, `Q` without `--cached` only, `—` never. The runtime's two rows (`C`) are about `status`'s own checks, and `status` fails outright on a `config.toml` it cannot read.

| Id | Severity | Tier | `status` | When |
|---|---|---|---|---|
| `fm-concurrency_primitives-journal-pending` | P0 | cached | C | An interrupted change recovery can settle: the next change rolls it back, rolls it forward or removes it; an interrupted doctor repair counts too. Repair: delegate, `doctor fix` runs omaddons' own recovery, disclosed in its preview; undo a rolled-forward change with `omaddons undo <tx>` |
| `fm-concurrency_primitives-journal-too-new` | P0 | cached | C | An interrupted change was started by a newer omaddons, whose journal this build does not read. Repair: set aside, with `--only <id> --force --yes`: suspended into quarantine, nothing deleted; undo reactivates it only while nothing it depends on changed (finishing it with the newer build is the safer way) |
| `fm-concurrency_primitives-journal-unreadable` | P0 | cached | C | An interrupted change's journal cannot be read. Repair: set aside, as above |
| `fm-concurrency_primitives-journal-unexpected-state` | P0 | cached | C | An interrupted change to `WTF/` stopped at a step it cannot place. Repair: set aside, as above, with the game closed |
| `fm-concurrency_primitives-journal-step-unrecognized` | P0 | cached | C | An interrupted change, or doctor repair, stopped at a step in neither its before nor its after state. Repair: set aside, as above |
| `fm-concurrency_primitives-journal-marker-mismatch` | P0 | cached | C | An interrupted change names a client directory that is no longer its client's (or a repair, a skill directory omaddons cannot attribute). Repair: set aside, as above; what the mismatched directory holds stays and is listed |
| `fm-concurrency_primitives-journal-unattributed` | P0 | cached | C | An interrupted change cannot be attributed to any client, so every change is refused. Repair: set aside, as above |
| `fm-schemas-state-corrupt` | P0 | default | when a file its checks read does not | A state file cannot be read |
| `fm-schemas-state-version-newer` | P0 | cached | C | State was written by a newer omaddons |
| `fm-configs-config-parse-error` | P1 | cached | fails: `status` stops with the line and column | `config.toml` cannot be read |
| `fm-concurrency_primitives-journal-preservation-incomplete` | P1 | cached | C | An interrupted restore has not finished keeping the settings it displaced. Repair: delegate, `doctor fix` runs omaddons' own recovery, disclosed in its preview, while that recovery would resume (it retries once a full disk or an I/O error is gone) |
| `fm-state_files-record-folder-missing` | P1 | cached | C | A managed addon's folder is gone |
| `fm-external_artifacts-ownership-conflict` | P1 | default | — | Several addons of one client's record claim one AddOns folder |
| `fm-network-credential-rejected` | P1 | default (online) | — | GitHub answers 401 to the configured token, or the configured credential cannot be read at all. Remediation by where it comes from: `omaddons auth github clear` for the stored token; unset or replace `OMADDONS_GITHUB_TOKEN`; correct `[sources.github] credential` in `config.toml` |
| `fm-state_files-detector-crashed` | P1 | runtime | C | A check stopped with an internal error |
| `fm-external_artifacts-managed-files-modified` | P2 | quick | Q | A managed addon's files changed on disk |
| `fm-external_artifacts-ownership-transferred` | P2 | cached | C | Addons lost a folder to another addon (a client whose directory is gone reports `target-vanished` instead) |
| `fm-external_artifacts-case-collision` | P2 | default | — | AddOns entries' names differ only in case (`Foo` and `foo`) |
| `fm-external_artifacts-wrong-flavor` | P2 | default | — | A managed addon's folder no longer loads on its client (its TOC's interface, game type or floor) |
| `fm-userland_state-target-vanished` | P2 | cached | C | A registered client is gone or no longer installed: its directory is gone, or Battle.net uninstalled it and its folder holds only what the uninstall left (see [Where discovery looks](#where-discovery-looks)). The manual fix forgets it, and a purge preview shows what deleting omaddons' data for it would take (see [`targets`](#targets)) |
| `fm-concurrency_primitives-journal-waits-for-game` | P2 | cached | C | An interrupted change to `WTF/` waits for the game to exit |
| `fm-concurrency_primitives-lock-held-long` | P2 | cached | C | An omaddons process has held the global lock for more than 10 minutes (`LOCK_HELD_LONG`) |
| `fm-caches-blob-hash-mismatch` | P2 | deep | — | A cached download's bytes no longer hash to its name, or it cannot be read. Checked only with `--include-deep` or `--only`, never by `status` or a plain `doctor`. Repair: `doctor fix --only fm-caches-blob-hash-mismatch` quarantines it into `<cache>/quarantine/<run id>/blobs/` (never deleted; `doctor gc` removes it with its run), and the next install downloads it again; undoing the repair puts it back only while the cache holds no good copy |
| `fm-network-github-rate-limited` | P2 | default (online) | — | GitHub's `/rate_limit` says 5 or fewer core REST requests remain this hour, for the token omaddons sends or, without one, for this address. Advice: wait for the reset it names, or store a token |
| `fm-network-source-unreachable` | P2 | default (online) | — | An enabled source an installed addon comes from (GitHub, WoWInterface, Tukui or WowAce) got no HTTP answer, a server error or a 429. One instance per source (`…@machine:source=<id>`). Advice: check the network and the host, then ask again later |
| `fm-plugins-widget-invalid` | P2 | quick | Q | The installed bar widget fails Omarchy's plugin validation, as omaddons' port of `omarchy-plugin-validate` checks it: its manifest, its entry points, or a symlink inside it. The evidence holds the broken rules' codes (`rules`, such as `default_section` or `symlink`) and their `count`, never the messages, which quote the manifest; `omaddons omarchy status` lists the messages. Remediation: `omaddons omarchy install`, from the desktop session; reported, never reinstalled by the doctor. A widget installed under other `HOME` or XDG directories (`evidence.reason: other_context`) gets advice instead, since it serves another omaddons state |
| `fm-secrets-credential-file-mode` | P2 | cached | C | A credential file other users can read: the stored GitHub token, another key file in `<config>/credentials/`, or a file a `[sources] credential = { file = … }` names, with any group or other permission bit set. Judged from a stat alone: the file is never opened, and the evidence holds its path and mode. Repair: `omaddons doctor fix --only fm-secrets-credential-file-mode` sets it to 0600 inside `<config>` (no backup; undoing the repair restores the old mode, and with it the exposure). A symlink, a file outside `<config>` or another user's file is manual, with the `chmod 600` to run |
| `fm-external_artifacts-out-of-date-toc` | P3 | default | — | A managed addon loads only as out of date |
| `fm-plugins-skill-version-drift` | P3 | cached | C | An agent skill omaddons installed (the default skill folder, or a folder `robot-docs install-skill --dir` wrote, as the history projection lists them) holds another guide than this build's, judged by the stamp on the line after its frontmatter, never by rendering the guide: `evidence.reason` `outdated` (another guide's pristine bytes), `edited` (bytes that no longer match their own stamp), `unstamped`, `too_large`, `symlink` (a linked `SKILL.md` whose target is not this build's guide, with `link_target`), `not_a_file` or `unreadable`, `edited` saying whether it was changed by hand, `observed` and `expected` the digests. A copy of this build's guide, edited or not, is current; a missing skill or a link to nothing is no finding. Remediation: `omaddons robot-docs install-skill` (`--dir <dir>` for another folder), which keeps an edited copy as `SKILL.md.bak`; prose for a link, a non-file or an unreadable file; reported, never rewritten by the doctor |
| `fm-plugins-widget-version-drift` | P3 | cached | C | The bar widget is from another omaddons build (`evidence.reason`: `code_differs`, or `context_unknown` when its `Build.js` cannot say which state it serves; `observed` and `expected` are the installed and this build's versions, which a rebuild of one version leaves equal). Remediation: `omaddons omarchy install`; reported, never reinstalled by the doctor |
| `fm-state_files-check-incomplete` | P3 | runtime | C | A check could not finish within its limit (`deadline`, `entries`), or an online check got no answer it could judge (`unanswered`) |

`runtime` rows are raised by the run itself, never selected: naming one in `--only` or `--skip` is a usage error.

The journal rows read one classifier, a read-only simulation of what recovery will do with each journal, so each state recovery stops at has its own finding. Each finding names its transaction (`tx`), its journal and format, what recovery stops with (`reason`, the same string the client's failing commands carry in `details.reason`), and how many entries its client stage and snapshots hold (`stage_folders`, `snapshot_folders`, `may_hold_only_copy`). A stuck journal's finding also says whether everything it owns can be told without trusting its lines (`attribution`: `complete`, or `partial`), and which directories may hold the rest (`unattributed_dirs`, never a path below a client's `Interface/` or `WTF/`). What it owns is found by its transaction id alone, in omaddons' own folders on the clients whose identity marker checks out; its lines are read to judge that, never followed. None is reported while another omaddons process holds the lock: it is committing, or about to recover them. The stuck rows' remediation is their forced set-aside, `doctor fix --only <id> [-t <id>] --force --yes`; `status --check` points at their `doctor explain` instead, so nothing is consented to before it is read. `lock-held-long` is advice only: omaddons never touches the lock file.

The `external_artifacts` rows over a client's full inventory (`ownership-conflict`, `case-collision`, `wrong-flavor`, `out-of-date-toc`) look only at registered clients and repair nothing. Which addon owns a folder, and which spelling to keep, are yours to decide, so the first two give advice; the other two give `omaddons update <addon> -t <id>`, since a newer build may load (or load as current). An addon installed with `--any-flavor` is never reported as the wrong flavor, and nothing is reported on a client whose own interface is unknown.

The doctor contract's `--fix` and `--explain <id>` are commands here, not flags. Typed on `doctor`, they are usage errors whose fix is the command: `omaddons doctor --explain fm-schemas-state-version-newer -t retail` → `omaddons doctor explain fm-schemas-state-version-newer -t retail`. `omaddons doctor --fix --only fm-… -t retail` → `omaddons doctor fix --dry-run --only fm-… -t retail`: the selection is kept, and never `--force` or `--yes`.

### `doctor explain`

One failure mode in full, and its instances now. It changes nothing: no run directory, no lock, no request.

```bash
omaddons doctor explain fm-userland_state-target-vanished          # every instance
omaddons doctor explain fm-userland_state-target-vanished -t era   # those on one client
omaddons doctor explain 'fm-concurrency_primitives-journal-pending@01M5ZQ7Y3KJ8V9W2X4C6B0N1RT:tx=01M7A1B2C3D4E5F6G7H8J9K0MN'
```

The id is a failure-mode id, or one instance's key as a report gives it (`<fm-id>@<target key or machine>[:<kind>=<value>]`), compared exactly; a label never matches. `-t` narrows, and an instance on another client, or one that is gone, gives `instances: []` and exit 0. An unknown id is a usage error (exit 64) whose fix corrects it in place when a registered id is within two edits (`fm-concurrency_primitives-journal-pendng` → `…-pending`), else `omaddons help doctor explain`.

It runs that one check as a diagnose would, with the full detector budget (2 s, 20,000 entries) whatever its tier. A check that panics is `internal` (exit 74): explain has no run to go on with.

```text
$ omaddons doctor explain fm-configs-config-parse-error
fm-configs-config-parse-error · P1 · configs · cached
config.toml cannot be read
  what: config.toml does not parse, holds a value out of range, is not UTF-8, is too large, or cannot be read.
  why:  Every change command fails closed with state_corrupt until it is fixed, and the doctor skips every check that depends on the configuration.
  repair: no automatic repair; edit config.toml where the finding points: the line and column, the key, or the file's encoding, size or permissions (config.toml is yours: omaddons never rewrites one it cannot read)
1 instance:
  machine · ~/.config/omaddons/config.toml
    column: 19
    line: 1
    path: ~/.config/omaddons/config.toml
    reason: parse
    → fix line 1, column 19 of ~/.config/omaddons/config.toml; omaddons never rewrites a config.toml that does not parse
```

The JSON is the [doctor envelope](#the-doctor-envelope) with these report keys:

- `failure_mode`: the registry's row: `id`, `subsystem`, `severity`, `title`, `description`, `why` and `tier`, then `fixer` (`{ops, scopes, enabled, needs_force}`, `null` while no fixer repairs it) and `manual` (`{instruction, why}`: what you do and why it is yours to do, `<placeholders>` for your values);
- `instances`: findings, the same shape as `doctor diagnose`'s;
- `skipped`: where it could not be checked, as `doctor diagnose` says it (a layer it reads could not be loaded, or the network it needs);
- `incomplete`: `null`, or `{limit, deadline_ms, max_entries}` when a limit stopped the check, so `instances` may be partial.

Its `commands` is empty: each instance carries its own remediation.

### `doctor fix`

Repair what a fixer can repair, as one transaction, after the same checks `doctor` makes. It takes `doctor`'s selection flags (`--only`, `--skip`, `--severity`, `--quick`, `--include-deep`, `--online`, `--budget`; not `--since`) and `-t`.

```bash
omaddons doctor fix --dry-run                       # what it would change, and its token
omaddons doctor fix --if-match pc1:…                # exactly that, or plan_stale
omaddons doctor fix --only fm-…                     # one failure mode's repair, even a disabled one
omaddons doctor fix --only fm-… --force --yes       # a forced repair: named, and consented to
```

**What it repairs.** Each finding falls in one group:

- **planned**: its failure mode has an enabled fixer, or `--only` names it by id: its repair runs;
- **needs `--force --yes`**: a forced repair (one that changes how an interrupted change ends, for instance). It runs only with `--only <id> --force --yes`, never in a default `doctor fix`, and the bar widget never runs it. `--only` naming one without `--force` is refused (`doctor_refused`, `details.reason: "force_required"`, exit 4);
- **disabled**: its fixer exists but is off by default. `--only <id>` runs it;
- **manual**: no fixer repairs it, or its fixer cannot repair this instance (a credential file outside `<config>`, say). `doctor explain <id>` says what to do.

A default `doctor fix` repairs every planned finding and lists the others; the human output names each group. Today's repairs: the delegation of interrupted changes to recovery, the forced set-aside of stuck journals, the quarantine of damaged cached archives, and the chmod of exposed credential files to 0600.

**Preview, then bind.** `--dry-run` runs the checks and plans the repair without taking the lock or changing anything outside its run directory. It lists every filesystem operation (`actions_planned`: `op`, `path`, a rename's `to`, the `mode` a chmod sets, whether the old content is kept as a backup, and `bytes`), every recovery of an interrupted change it would finish (`delegated_planned`: `tx`, `family` `folder` or `repair`, the `phase` its journal stands in, `unprepared`, `prepared`, `step`, `swapped`, `preserve` or `done`, and the `direction` it settles, `roll_back`, also for a journal that never ran, or `roll_forward`), `summary.bytes_to_back_up`, and `precondition`, the token `--if-match` takes. The token describes the repair, not the run (no run id, no time), so a bound run in another process, later, confirms it: `doctor fix --if-match <token>` plans again under the lock and compares before its first recovery or operation; a difference is `plan_stale` (exit 4) with the preview as its fix, and nothing changes. A bound `--dry-run` compares its token too. `doctor fix` stores no plan: its exits are the doctor's, and `apply` reports the general ones.

**The run.** Under the global lock, in this order: plan again, compare the token, finish the disclosed recoveries, then repair as one journaled transaction. A failed operation rolls back every one before it (`DONE_FAILED`, exit 3). After the lock is released, the repaired failure modes are checked again, and so is every other offline one the run found (a recovery may have settled it: a rolled-forward update leaves its files as its record says), while an online finding stands as found: `findings` holds what is left, and a repaired mode still found makes the run `DONE_PARTIAL` (exit 2). With its repairs verified, a run whose `findings` still hold anything (another failure mode, an instance its fixer cannot repair, such as a credential outside `<config>`, or a check that could not finish) is `DONE_FINDINGS` (exit 1), never `DONE_OK`. It never deletes: what it sets aside is moved into the run's `quarantine/`, and what it overwrites is kept in `backups/`. It never writes below `WTF/`, `config.toml`, `addons.toml` or a SavedVariables file. A repair is a line of kind `repair` in [`history`](#history); `--reason` and `--actor` are recorded on it. Its run directory holds `undo.sh`, which restores what the repair changed; [`doctor undo`](#doctor-undo) with the run's id does the same.

**Refusals.** Before anything changes, as `doctor_refused` (exit 4) with `details.reason`: `force_required`; `client_running` (the repair needs a client that is running to be closed); `filesystem_mismatch` (a rename across filesystems); `too_many_actions` (over 1,000 operations, `DOCTOR_FIX_MAX_ACTIONS`); `fixer_conflict` (two repairs touch the same files, or a repair touches an unfinished change's). A fixer that declines names its own reason. Another process holding the lock past the timeout is exit 5. `--force` without `--yes`, or without `--only`, is a usage error (exit 64) whose fix is the preview, `omaddons doctor fix --dry-run` with your selection: a fix never supplies consent.

```text
$ omaddons doctor fix --dry-run
omaddons doctor fix --dry-run · 1 finding · 1 action planned (7 B to back up)
  fm-…
    write_file        <state>/…  (kept as a backup)
precondition pc1:… (pass --if-match to apply exactly this)
run 2026-10-31T07-23-32Z__01M68DF7…

$ omaddons doctor fix
repaired 1 failure mode (1 action, 7 B backed up)
  fm-…  write_file        <state>/…  (kept as a backup)
run 2026-10-31T07-23-38Z__01M68DFD… · tx 01M68DFDPC…
```

Paths are shown below their root (`<state>`, `<config>`, `<cache>`, `<run>`); `-v` prints them in full. Other lines, when they apply: `recovered tx <tx> (<outcome>)`, `needs --force --yes: <fm> <label> → omaddons doctor explain <fm>`, `disabled: <fm> <label> → omaddons doctor fix --only <fm>`, `manual: N findings · omaddons doctor explain <fm>`, `still found: <fm> <label>`, and `rolled back: N actions restored; nothing was changed`. A clean home prints `omaddons doctor fix · no findings`.

The JSON is the [doctor envelope](#the-doctor-envelope) with `kind: "fix"` in `report.json`, and these report keys: `doctor_contract_version`, `run_id`, `run_dir`, `started_at`, `finished_at`, `state` (`DONE_OK`, `DONE_FINDINGS`, `DONE_PARTIAL`, `DONE_FAILED`, `REFUSING`, `LOCK_LOST`), `dry_run`, `tx`, `summary` (`doctor`'s counts, then `findings_before`, `findings_after`, `actions_taken`, `actions_failed`, `bytes_to_back_up`, `bytes_backed_up`, `backups_restored`), `findings`, `skipped`, `actions` (each operation as its journal recorded it, with `ok`), `delegated` (`{fm, tx, family, outcome, reason}`: each recovery its preview disclosed, `outcome` `rolled_back`, `rolled_forward`, `waiting` or `failed`, and `reason` only for the last two, a stop's `details.reason` or else its error code), `actions_planned`, `delegated_planned`, `precondition` (a dry run's; `null` on a real run), `actions_jsonl_path`, `backups_dir`, `undo_command`, `undo_argv` and `next_steps`. A real run's keys are `null` in a dry run, and a dry run's in a real one. After a dry run that plans something, `commands[]` holds the bound run (`doctor_fix`).

It exits 0 when nothing is left, 1 when a dry run plans something or findings remain that it does not repair, 2, 3, 4 and 5 as above, 6 for `--only` naming checks that need `--online`, 64, 66 for an unknown `-t`, 73 when the run directory cannot be created, and 74.

### `doctor diff`

What `doctor fix` would change, or what changed since a run. It changes nothing: no run directory, no lock, no request.

```bash
omaddons doctor diff                # the plan of a default doctor fix --dry-run, and its token
omaddons doctor diff latest         # findings new, resolved and unchanged since the last run
omaddons doctor diff 2026-10-31T07-23-32Z__01M68DF7…
```

Without a run, it is the plan a default `doctor fix --dry-run` with the same `-t` would make: `actions_planned`, `delegated_planned`, `bytes_to_back_up` and `precondition`, which on an unchanged home equals that dry run's, so `doctor fix --if-match <token>` runs it (`commands[]` offers it). With a run (its id or `latest`), it compares this moment's findings with that run's report, as `doctor --since` does: `since: {run, new_instances, resolved_instances, unchanged}`, with a `doctor explain` command per failure mode that has a new instance. An unknown or interrupted run is `not_found` (exit 66), with the newest finished runs as fix and alternatives. The other mode's keys are `null`. It takes no selection flag, and exits 0, 1 when anything is found, 4 when planning is refused, 64, 66 and 74.

### `doctor undo`

Undo a `doctor fix` repair, named by the doctor run that made it. It is the same undo as [`omaddons undo <tx>`](#undoing-a-repair): the same strict checks and refusals, and a run directory of kind `undo` of its own (`report.json` names `reverted_run`).

```bash
omaddons doctor undo                                 # the newest repair still in effect
omaddons doctor undo 2026-10-30T18-03-40Z__01M8S…    # the repair that run made
omaddons doctor undo --dry-run                       # what it would revert; changes nothing
```

- **Which repair.**
  - Without a run: the newest repair that committed and is still in effect, whatever diagnoses ran since (they make no history).
  - A run id, as `doctor fix` printed it: the repair history records for that run, also when the run's report is gone.
  - An undo run's id: that undo, undone in turn.
  - A run that committed nothing (a diagnose, a dry run, a rolled-back fix): "changed nothing", exit 0.
  - An unknown run: `not_found` (exit 66).
  - `latest`: `usage` (exit 64), since the newest report may be a diagnose. Its fix is bare `omaddons doctor undo`.
  - A repair's transaction id: `usage`, whose fix names its run.
- **Idempotent.** A repair already undone is no error: exit 0, `already_undone: true` and `undo_tx` the undo that undid it. Nothing is written, also when another process undid it a moment before. (`omaddons undo <tx>` of an undone repair stays `undo_conflict`, as for every undo.)
- **Previews.** `--dry-run` runs the checks and lists the inverse operations in `actions_planned` (`doctor fix`'s shape, the undo's own names written `<run>` and `<tx>`), without a run directory or a lock. `doctor undo` stores no plan and takes no `--if-match`. Its bound form is the preview its dry run offers in `commands[]`, `omaddons undo <tx> --dry-run`, which stores a plan for `apply`.
- **What it leaves.** An interrupted change the fix run completed (its `delegated[]`, rolled forward) is not reverted. The human output names it, and `commands[]` offers `omaddons undo <tx>` for it.

```text
$ omaddons doctor undo
undid repair 01M9… from run 2026-10-30T18-03-40Z__01M8S… (1 write)
run 2026-10-30T18-05-12Z__01MA… · tx 01MA… · redo: omaddons undo 01MA…
```

The JSON is the [doctor envelope](#the-doctor-envelope) with `kind: "undo"` in `report.json`, and these report keys:

- `doctor_contract_version`;
- `run_id` and `run_dir`, the undo's own run (`null` when it made none);
- `started_at` and `finished_at`;
- `reverted_run`, the fix run;
- `tx`, the repair reverted;
- `undo_tx`, the undo's own transaction;
- `state` (`DONE_OK`, `DONE_FAILED`, `REFUSING`, `LOCK_LOST`);
- `actions_taken` and `already_undone`;
- under `--dry-run`, `actions_planned`.

It exits 0 when the repair was undone, was already undone, or there is nothing to undo, and for a dry run. Otherwise:

| Exit | When |
|---|---|
| 3 | its commit rolled back |
| 4 | refused (`undo_conflict`, `game_running`, `scope_denied`) |
| 5 | the lock was not acquired |
| 64 | usage |
| 66 | `not_found`, `target_not_found` |
| 73 | its run directory could not be created |
| 74 | anything else |

### `doctor gc`

Delete old doctor runs and what they kept: run directories, the `.repair` client halves of repair transactions (`<client>/Interface/.omaddons/snapshots/<tx>.repair/`) and the cache quarantines (`~/.cache/omaddons/quarantine/<run>/`). It is the only command that deletes them, and it never deletes what an unfinished change still needs.

```bash
omaddons doctor gc --before 30d --dry-run                  # the retention plan: what would go, what stays and why
omaddons doctor gc --before 30d --yes --if-match pc1:…     # delete exactly what that preview listed
```

**What is eligible, and what is kept.** What started before `--before` (an RFC 3339 time or an age: `30m`, `12h`, `30d`, `2w`) is eligible: a run by its id's time, an orphaned half by its transaction's. The retention plan then keeps, in `exclusions`, each eligible thing something may still need, with its `pin`, strongest first:

- `unfinished_journal`: an interrupted transaction recovery has not settled may read or write it, or it is the run of a repair whose journal is still there. While a repair journal cannot be read at all (a newer omaddons wrote it, or a line does not decode), everything is kept, and a real run refuses with that journal's error.
- `active_undo`: an interrupted undo of a repair restores from it.
- `recovery_dependency`: a set-aside that can still be reactivated (its guards hold or cannot be read) keeps what it moved into quarantine, and the run of a repair it suspended, whose backups that repair's rollback reads; a history line gc cannot read keeps every run and transaction it mentions.
- `active_run`: its run is still going (an in-flight lock is held), or it has no report and started less than an hour ago.

An exclusion's `path` is the backup or quarantined file it protects when that lies inside the run or half, else the run or half itself.

**What it reports.** `deletions` lists each deletion in the order it happens (per run: its cache quarantine, its halves, its directory last), with its `kind` (`run`, `client_repair`, `cache_quarantine`), `run_id`, `target` (a half's client), `tx`, `path`, `bytes` (apparent; `null` when the size walk stopped at 20,000 entries) and `may_hold_only_copy`: a backup or a quarantined file a repair kept, which may exist nowhere else. `lost_undos` lists the repairs, and undos of repairs, whose own undo reads something deleted: they can no longer be undone. A chmod and a file a repair created read nothing, so `omaddons undo <tx>` still undoes them; after a run directory is deleted, `omaddons doctor undo <run>` no longer finds that run. `latest` moves (`{from, to}`) when it names a run being deleted or leads nowhere: to the newest run kept whose report says `DONE_OK`, `DONE_FINDINGS` or `DONE_PARTIAL`, or away (`to: null`) when there is none. At most 1,000 deletions are made per run, whole runs oldest first; `remaining` says how many eligible ones wait for the next run (`null` when a listing stopped at its bound, so more may exist).

**Preview, consent, bind.** `--dry-run` reads and plans, takes no lock and writes nothing, and returns `precondition`. A real run needs `--yes` (without it, a usage error whose fix is the preview), takes the global lock, reads everything again and plans again under it, and that plan is the one carried out. `--if-match <token>` makes it delete exactly what the preview listed, or nothing (`plan_stale`, exit 4): another eligible run, a pin that came or went, a deletion whose inode changed, or more undos lost. An age is resolved when the command starts, so a run that crosses the cutoff between the preview and the bound run makes it stale: to bind exactly, pass the preview's `before_at` as `--before`. Then `latest` moves, and every deletion is renamed into a trash folder under the lock and deleted after it is released. gc never recovers a journal, writes no history line and makes no run directory; a run that deleted something rings the widget's doorbell. `-t` is a usage error: the retention plan covers every client at once. The bar widget never runs it.

The JSON is the [doctor envelope](#the-doctor-envelope) with these report keys: `dry_run`, `before_at` (the cutoff `--before` resolved to), `deletions`, `exclusions`, `lost_undos`, `latest`, `remaining` and `precondition` (`null` on a real run). `commands[]` is empty. It exits 0 (also when nothing is eligible), 4 (`plan_stale`, a newer repair journal, or a fence's refusal), 5 (`locked`), 64 (usage) and 74 (an unreadable registry or repair journal, an I/O error).

```text
$ omaddons doctor gc --before 30d --dry-run
would delete 14 run directories, 2 client halves (.repair) and 1 cache quarantine (18.4 MiB), all started before 2026-09-30 18:03 UTC
  may hold an only copy: 3 (backups or quarantined files a repair kept)
  undo no longer possible: 01M8A2B3C4D5E6F7…; `omaddons undo <tx>` still undoes the others
kept 3 (pinned): 1 unfinished_journal, 1 active_undo, 1 active_run
latest → 2026-10-28T08-00-00Z__01M9D… (it named a run this deletes)
precondition pc1:… (delete exactly this: --yes --if-match pc1:…)
```

`-v` lists every deletion and exclusion.

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

With `--json`, bare `config`'s `data` is `{path, exists, settings: [{name, value, source}]}`, `config get`'s `{name, value, source, default}` and `config set`'s `{name, value, previous, changed, dry_run}`: `name` is the setting as `config get <KEY>` takes it (`limits.download_max_bytes`), and `source` is `file` or `default`. A `config.toml` value out of range fails with `state_corrupt`, whose `details` give its `path`, `line`, `column` and `toml_key`.

### `auth github`

The opt-in GitHub token. The main GitHub path (`releases/latest`, `release.json`, the feeds) needs none; a token raises GitHub's REST budget from 60 to 5,000 requests an hour (search: 10 to 30 a minute) for the beta and alpha channels, repositories without a `release.json`, and `search`.

```bash
omaddons auth github                          # which token is used; online, the hour's REST budget left
echo "$TOKEN" | omaddons auth github set      # store a token: standard input only, never argv
omaddons auth github --from-gh                # store what `gh auth token` prints
omaddons auth github clear                    # forget the stored token
```

`set` writes `~/.config/omaddons/credentials/github` with mode 0600, in a 0700 directory, and refuses a terminal at once rather than wait for input. `--from-gh` is the only time omaddons asks `gh` for anything, and only when you pass it. `clear` empties the file: an empty file is no token. `status` makes one request, to `/rate_limit` (which costs none of the budget), and none with `--offline`. With `--offline` it also runs no credential command, so `configured` then says a credential is configured, not that it works; `set` and `clear` never run one. With `--dry-run`, `set` and `clear` check the token and write nothing ("would store", "would clear"), and `data` describes the token as the write would leave it. `data` is `{configured, source, rate_limit, dry_run}`, `source` one of `env`, `config`, `file` or `none` (see **Tokens** under [Network and privacy](#network-and-privacy) for the order). The token is never shown, logged or sent anywhere but `api.github.com`. A rejected token is `auth_rejected`, whose fix undoes the token's source: `omaddons auth github clear` (alternative: `set` a new one) for the stored file, unsetting `OMADDONS_GITHUB_TOKEN`, or correcting config.toml's `[sources.github] credential`, each with `clear` as an alternative. `--from-gh` goes with `set` (or bare `auth github --from-gh`); with `status` or `clear` it is a usage error.

`omaddons doctor --online` checks that GitHub accepts the token (`fm-network-credential-rejected`), and reports a credential that cannot be had at all, with the same fix by its source. `omaddons doctor` reports a credential file other users can read (`fm-secrets-credential-file-mode`), and `omaddons doctor fix --only fm-secrets-credential-file-mode` sets it back to 0600.

`omaddons help auth` lists `auth`'s one subcommand, `github`; `omaddons help auth github` (or `omaddons auth github --help`) explains it: its actions and `--from-gh`.

### `net-log`

Every network request and git subprocess omaddons made, with a summary by host and by `via` (`http` or `git`). A request or git run whose line cannot be written fails, so nothing is contacted unlogged. By default it shows the 50 most recent, oldest first (`--limit N`; `--since` filters first). It reads `net.log` and its one rotated predecessor. The summaries count every request that matched, `truncated` says more matched than are shown, and `skipped` counts lines it could not read. `since_at` is the instant `--since` resolved to (`null` without it). Each of `entries[]` is a `net.log` line as written, so its instant keeps the line's own key, `time` (see [Naming rules](#naming-rules)).

```bash
omaddons net-log
omaddons net-log --limit 200
omaddons net-log --since 1d --json
```

### `omarchy`

The bar widget for [Omarchy](https://omarchy.org)'s desktop: one gamepad button on every monitor's bar, with a badge for pending updates that turns urgent for problems and new Lua errors, a summary on hover, and a check for updates on right-click. The widget reads the same envelopes the CLI prints and runs omaddons command lines for everything it does, so each change it makes is recorded with `--actor widget` and can be undone.

A left click (or the bar's keybinding) opens its keyboard panel, and a middle click opens it on Issues. The panel has a summary ("2 clients · 3 updates · 1 issue", when the last check ran, which client is running) with Check now, Undo last and Open AddOns, a filter by client, the switcher between its five views (Updates, Installed, Issues, Backups, Search, keys `1` to `5`), the progress of a running change, the last result with its Undo, and a failure with its Fix. Opening it and switching views make no network request.

- **Updates** lists what is behind, per client: updates first (`DBM  12.1.14 → 12.1.15`, "then /reload" while that client runs), then pinned addons with a newer release, blocked ones and failed checks. Its header updates everything in the filter (**Update all (3)**); with nothing to do it says "Everything is current · checked 25 min ago". Update (Enter), Update all and `u` preview first: the widget runs the update's dry run, and when the plan is exactly what the rows showed (the same client, addon, source, release, file and versions) and needs nothing more, it applies that plan (`omaddons apply <plan_id>`) with no dialog. Otherwise the confirmation opens, starting on No: "Changed since you looked" with what moved, or one line per effect the plan adds (a new dependency, a folder it removes, files you changed that it replaces, kept for undo, another source or channel, a change while the game runs). A preview with nothing to do refreshes the view. Pins run directly.
- **Installed** lists every managed addon per client with its version, source (`gh`, `wowi`, `tukui`, `git`, `wowace`, `file`) and markers: `dep`, `orphan`, `pinned`, and `modified` or `missing` when its files changed on disk. Unmanaged folders show as a hint (`omaddons adopt -t <id>`).
- **Issues** puts every problem in one place with its fix one key away: `status`'s problems by severity (Enter runs the remediation; a destructive one asks first; one the widget cannot run shows as text, and so does a forced repair, any argv holding `--force`, which is read with `doctor explain` and run from a terminal, and a purge, any argv holding `--purge`, with what it deletes; `y` copies such a command), each client's Lua errors from `omaddons errors` (the likely source with how sure it is, "update available (may fix it)", the error lines in strict text: an addon's own free-text message shows only its length; errors received from other players only as a count), a BugGrabber install where it is missing, and the last status's warnings.
- **Search** finds addons (`/`, type, Enter) on every client at once, or on the filtered one, with each client's compatibility hint. Enter installs into the filtered client when its hint is `yes` or `likely`; under All it asks which client, choosing none for you.
- **Backups** lists each client's backups, newest first: when, what kind, how big, `stable` or `best effort`, and "verified" only for a backup whose hashes were checked; one another process is reading says "in use". Each client's header says how its automatic backups are doing ("daily, working", "daily, but this filesystem cannot clone", "skipped: low disk space", "off") and how much space is free.
- **Restoring settings** goes through a chooser that puts the narrow fix first. Press `r` on an addon (Installed, or an erroring addon in Issues) to choose a backup, each showing which version of that addon wrote its settings. Or press Enter on a backup. Then choose **"<addon>'s settings"** or, second, **"Whole WTF/"**, labelled with what else it changes: keybindings, macros, chat settings and every other addon's settings. Either way the confirmation shows the CLI's own dry run: how many files change, "settings written by WeakAuras 5.19.2; installed now: 5.20.0", what is not in the backup, and that a whole restore keeps the current `WTF/` as a backup. When a folder changed owner since the backup, a button per folder restores just that one. While the client runs (or nobody can tell), restores are off: "quit the game first".

| Key | Does |
|---|---|
| `h`/`l`, ←/→, `1`–`5` | Previous or next view, or view N |
| `j`/`k`, ↓/↑ | Move the cursor |
| Enter | Updates: preview the update, then apply it. Installed: reinstall it after a confirmation when its files changed, else update it when a release waits |
| `p` | Pin or unpin the addon |
| `i` | Updates: the newest release notes (fetched only then), with a link to GitHub, WoWInterface, Tukui or WowAce |
| `x` | Installed: remove the addon, after a confirmation |
| `s` | Issues: an addon's raw error details, labelled "raw details: may contain other players' names and your own code" |
| `r` | Restore settings: on an addon (Installed, Issues) that addon's from a backup, on a backup (Backups) from that backup |
| `b` | Back up the filtered client now, or every client |
| `x` on a backup | Delete that backup, after a confirmation (never one in use) |
| `d` | Dismiss the new Lua errors the panel shows (the filtered client's, or each client's); if a session ended since, nothing is dismissed and the panel says "New errors arrived; review them" |
| `y` | Issues: copy the command a row shows instead of running (a forced repair, a purge), to run from a terminal |
| `t` | Filter by client: All, then each client in turn |
| `u` | Update everything, or the filtered client, previewed |
| `c` | Check for updates now |
| `z` | Undo (or redo) the last change, after a confirmation |
| `o` | Open the filtered client's `Interface/AddOns` folder |
| `/` | Search |
| Esc | Close the panel (or the confirmation) |
| Tab | The next panel on the bar (Omarchy's) |

Every update, install, reinstall and destructive action (an undo, a removal, a restore, a backup deletion) first runs the command's own dry run, which stores a plan. A reinstall and every destructive action always ask, showing what omaddons would do. The dialog starts on No: `y`, or moving to Yes with `h`/`l` and pressing Enter, confirms, and `n` or Esc cancels. Yes, or a routine matching update, runs `omaddons apply <plan_id>` of exactly that plan. When the plan went stale, expired or is gone, nothing runs: the widget previews again and the dialog comes back with "Changed since you looked" and the new preview. Dismissing Lua errors still binds to the read the panel showed, and forgetting a client (`targets rm`) runs bound to its preview's token. Only a fresh key press confirms it, never a key still held from before. When the widget finds a binary from another build, the panel turns read-only and keeps only Retry and Reinstall widget. When the binary is missing, it shows the two commands that bring it back.

The widget looks after a play session on its own:

- **A game starting or exiting shows within a second.** Hyprland's window events make it ask `status` again, and a window closing while a client runs is followed up 2, 5 and 15 seconds later, because the game process can outlive its window. While a client runs, its refresh behind the doorbell comes at least every 30 seconds.
- **After play**, when a client stops, it takes that client's post-session settings checkpoint (`backup create --auto`, which skips one when nothing changed) and then reads its Lua errors. When the widget starts, every stopped client gets the same checkpoint, so a session played with the bar off (or before a reboot) is still covered. Either runs quietly in the background, after any change you asked for, and says something only when it fails.
- **Lua errors are read again** when `status` says a client's are not read yet or changed since the last read. An incomplete read is not retried on every refresh.
- **Updates are checked in the background** when `status` says a check is due (a client never checked, or last checked longer ago than `check_interval_hours`), at most every 15 minutes, with `autoCheck` on (the default). Check now (`c`) runs regardless.
- **Notifications:** new Lua errors after a session ("Lua errors in your last session on retail · DBM 3 · WeakAuras 1", on by default as `notifyErrors`) and new updates (off by default, `notifyUpdates`). A notification carries addon names, client ids and counts, never an error's text, and clicking it opens the panel. A change you started in the panel that finishes after you closed it also says so in a notification. None of this fires on the widget's first refresh, or while the binary is missing or from another build.

The widget keeps a few rules, and every gate checks them:

- It runs omaddons only with an argv from its own command table, or one the CLI printed and the widget's classifier accepted, never through a shell or as a string.
- It runs `omaddons apply` only for the plan of the preview it just ran, never from a fix or a recommendation.
- Besides omaddons it runs five desktop tools: `inotifywait`, `omarchy-notification-send`, `xdg-open`, `omarchy-launch-browser`, the last only for an `https:` link to GitHub, WoWInterface, Tukui or WowAce, and `wl-copy`, only to put a command the widget shows on the clipboard, with the text as one argument. When Omarchy's shell offers no summon call to open the panel, it also runs `omarchy-shell shell summon` with the widget's id, as a fixed argv.
- It never runs a change no undo brings back: an argv holding `--purge` (`targets rm --purge`) before `--` is never a button, not even behind a confirmation. Its row shows the command, what it deletes, and `y` to copy it for a terminal.
- Every omaddons process runs under the `HOME` and XDG directories the widget was installed for.
- It writes no file.
- It shows the CLI's strings as plain text, and decides nothing from a warning's text.
- An update, an install, a reinstall or a destructive action runs only after its preview, and only as that preview's `apply`.
- It lists `status`'s issues as the CLI reports them and matches none by its id, so a failure-mode id the CLI narrows or splits changes nothing it decides.
- A notification carries names and counts only.

`omarchy/tests/security.test.js` scans the widget's source for code that goes around any of these, and each scan must also catch a violation planted in a copy of the source.

The widget also answers on the shell's IPC, under its id: `omarchy-shell a44.omaddons state` prints its state as one JSON object (`phase`, the bar's badge and urgency, `summary`, the running `action`, the last result, whether a panel is open, the binary and both versions, what each of its lanes is running, whether a confirmation is open (`confirm`: its heading and plan id) and what the last preview decided (`lastPreview`: `direct`, `dialog`, `refresh`, `refuse` or `failed`, with its plan id)), and `refresh`, `check` and `show <view>` ask for a refresh, an update check, or the panel opened at a view.

```bash
omaddons omarchy                          # status: installed, current, on the bar? (bare omarchy)
omaddons omarchy install                  # install the widget and put it on the bar (right section)
omaddons omarchy install --section left   # where a widget not on the bar yet goes
omaddons omarchy install --no-enable      # the files only; the bar stays as it is
omaddons omarchy install --dry-run        # render and validate; write nothing
omaddons omarchy remove                   # take it off the bar and delete it
```

Omarchy's tools run with your environment, so run `install` and `remove` from the desktop session.

#### `omarchy status`

Whether Omarchy is here (`omarchy.present`: `~/.config/omarchy` and its shell), whether its shell is running (`omarchy.shell_running`: `omarchy-shell shell ping` answered `ok` within 2 seconds) and whether `omarchy-plugin-validate` is on `PATH` (`omarchy.validator`). Then the widget, as `plugin` (the same shape `install` reports): where it is, which version and binary it runs, whether it is `outdated` (this binary would render other code for the directories the widget was installed under, or its `Build.js` is missing), whether it is on the bar and in which `section`, and `context_matches`. `validation` is omaddons' copy of Omarchy's plugin validator over the installed folder, `{ok, errors}`, or `null` when nothing is installed. The copy reads `manifest.json` only up to 64 KiB (`WIDGET_READBACK_MAX_BYTES`): a larger one is reported as too large and never parsed, the one rule it adds to Omarchy's.

It recommends `omaddons omarchy install` for an outdated widget (`reinstall_widget`) and for one on no section of the bar (`enable_widget`). When the widget was installed under other `HOME` or XDG directories than this run's (`context_matches: false`), it serves that other omaddons state, and the human output says so without recommending anything, even for an outdated widget: installing from here would point it at this run's state, which is your call. A widget whose `Build.js` is missing serves no state, so its reinstall is recommended from anywhere. It writes nothing and makes no request; its one subprocess is the ping.

#### `omarchy install`

It renders the widget embedded in the binary for this binary's path and this session's `HOME` and XDG directories, so the widget runs the same omaddons on the same state. It assembles the plugin in a hidden `~/.config/omarchy/plugins/.omaddons.stage-<id>/`, which Omarchy's plugin registry ignores, and checks it with omaddons' own copy of Omarchy's plugin validator and, when it is installed, `omarchy-plugin-validate`. It then swaps it into `~/.config/omarchy/plugins/omaddons/` with one atomic rename, so the shell reloads the widget once and never sees it half-written. Last, it records a `widget` line in history, asks the shell to rescan its plugins and, unless `--no-enable`, runs Omarchy's own `omarchy-bar put a44.omaddons --section <section>`. A widget already on the bar stays where you put it, and omaddons never writes Omarchy's `shell.json`.

Run again with nothing changed, it writes nothing (`installed: unchanged`). Reinstall after moving the binary or changing the XDG directories: the widget's `Build.js` records both. Two installs at once (a terminal and the widget reinstalling itself) each stage their own copy under an in-flight lock: the last to publish wins, and neither touches the other's stage. A stage left by a killed install is removed by the next install, once its lock shows its process is gone, never by age. On a filesystem without `RENAME_EXCHANGE` the swap takes two renames (warning `atomic_swap_unavailable`), and an install interrupted between them leaves the widget absent until the next install, which finishes it.

`data` is `{dry_run, installed, enabled, section, plugin}`. `installed` is `fresh`, `replaced` or `unchanged`, and `enabled` is `yes` (placed now), `already`, `skipped` (`--no-enable`) or `failed`. `plugin` is `{installed, path, id, version, binary, outdated, enabled, section, context_matches}`, where `context_matches` says whether the widget was installed under this run's `HOME` and XDG directories. Without Omarchy (no `~/.config/omarchy`, or no shell: `omarchy-shell` on `PATH` or `$OMARCHY_PATH/shell`) it fails `not_found` with `details.reason: "omarchy_missing"`. When the shell cannot be told (over SSH, or with the shell stopped), the files stay installed and the warning `omarchy_shell_unreachable` says to run it again from the desktop session. A plugin that fails validation is a bug in the build: `internal`, with `details.reason: "plugin_invalid"` and the validators' messages, and nothing is swapped.

The widget's settings live in Omarchy's `shell.json` and are set with Omarchy's own tool. `omarchy bar set a44.omaddons binary /path/to/omaddons` runs another build (empty, the default, runs the one that installed it), and `refreshIntervalSec` is the refresh behind the doorbell (30 to 3600 seconds, 300 by default). `panelWidth` is the panel's width (400 to 800 pixels, 540 by default), `autoCheck` turns the background update check on or off (`On` by default), and `notifyErrors` and `notifyUpdates` turn the two notifications on or off (`On` and `Off` by default).

#### `omarchy remove`

The supported way to take the widget away (Omarchy's own `omarchy plugin remove a44.omaddons` cannot, see the [FAQ](#why-does-omarchy-plugin-remove-a44omaddons-not-work)). First it takes the widget off the bar with Omarchy's `omarchy-plugin-disable a44.omaddons` (or, without that tool, the shell's own `setPluginEnabled`). **Omarchy then drops the widget's bar settings with its entry** (`binary`, `refreshIntervalSec` and the rest), and `remove` says so. Then, under the global lock, it moves `~/.config/omarchy/plugins/omaddons/` to a hidden name with one rename, so the shell sees the widget go at once, records a `widget` line in history, and deletes the moved folder after the lock. What it leaves in `~/.config/omarchy/plugins/` is what was there before the install.

`data` is `{dry_run, removed, disabled, settings_dropped}`. `disabled` is whether the widget is off the bar: it was on no section, or Omarchy took it off. Taking it off is best effort: when the shell cannot be told (over SSH, or with the shell stopped), the files are still removed, `disabled` is `false`, and the warning `omarchy_shell_unreachable` says to run `omarchy plugin disable a44.omaddons` from the desktop session, as the entry stays in Omarchy's bar layout until then. With no widget installed, or when another `remove` got there first, it fails `not_found` (`details.reason: "widget_not_installed"`, fix `omaddons omarchy install`). `--dry-run` says what it would do, including whether Omarchy would drop settings, and runs nothing and writes nothing.

### `completions <SHELL>`

```bash
omaddons completions bash
omaddons completions zsh --json
```

Prints a completion script for `bash`, `zsh`, `fish`, `elvish` or `powershell` on stdout, and nothing else, so `> file` saves exactly the script ([where each shell looks](#shell-completions)). The script is generated in memory from this build's command tree at a fixed width: it is the same bytes on every run and in every terminal. It is static: commands, subcommands, aliases, flags and fixed values complete; client ids and addon names do not. It reads no state, needs no home, makes no request and writes nothing.

`data` is `{shell, script}`, where `script` is byte for byte what the human form prints. A shell outside the five is a usage error whose fix is the closest one (`omaddons completions bsh` → `omaddons completions bash`); with no shell, the fix is `omaddons help completions`.

### `robot-docs`

Documentation for programs and agents, generated from the code, so it cannot drift from what the binary does. `guide`, `schema` and `errors` read no file or setting, need no `HOME`, send no request and change nothing; `install-skill` writes the guide where coding agents load skills. Bare `omaddons robot-docs` prints the agent guide, as `omaddons robot-docs guide` does.

```bash
omaddons robot-docs                           # the agent guide, as Markdown
omaddons robot-docs schema --json             # every schema: envelope, events, commands, files
omaddons robot-docs schema install --json     # the data of one command
omaddons robot-docs schema backup restore     # a nested command, by its path
omaddons robot-docs errors --json             # every error and warning code
omaddons robot-docs install-skill             # the guide as a Claude Code skill
```

#### `robot-docs guide`

The agent guide: Markdown that opens with YAML frontmatter, the same bytes as `skills/omaddons/SKILL.md` in this repository (a test holds them equal). It covers the core loop (preview, apply, reconcile a lost output, undo), reading the envelope, the exit codes, targets and refs, the safety model, what omaddons never does, eight recipes of at most two commands each, the common errors, the doctor loop, every command and the environment, in under 500 lines and 32 KiB (a test holds it there). [`omaddons robot-docs install-skill`](#robot-docs-install-skill) installs the same bytes as a skill for your coding agent. It reads no file or setting, so it prints the same bytes on every machine. With `--json`, `data` is `{"format":"markdown","content":"…"}`. The line after the frontmatter is the stamp `<!-- omaddons guide sha256:<hex> -->`, the SHA-256 of the document without that line; it names no version or build.

#### `robot-docs schema [COMMAND]`

The JSON Schema (draft 2020-12) of every command's `data`, of the envelope, of every stderr NDJSON event and of every file omaddons writes. `data` is `{envelope, events, commands, files}`: `events` is keyed by event name, `commands` by command path and `files` by file kind (a JSON Lines kind describes one line). Each schema is generated with the serialize contract, so a key a command may leave out is optional, and with a generator of its own, so its `$defs` names depend only on that type.

With a command path (`install`, or `backup restore` as two words or one quoted), `commands` holds that one command's schema and the other three keys are `null`; `.data.commands["install"]` works in both forms. An alias names its command (`add` gives `install`). The `omaddons` and `version` envelopes carry `help`'s data, which `commands["help"]` describes. A path no command has is `usage`, whose fix is the closest path (`robot-docs schema instal` → `omaddons robot-docs schema install`), else the command the path started with, else `omaddons help robot-docs schema`.

The published schemas are open: a key a newer release adds does not break a validator built from them, so such a validator must accept keys it does not know. The human form prints the same data, as pretty JSON.

#### `robot-docs errors`

Every registered error code, in registry order, as `{code, exit, doctor_exit, retryable, meaning, details_schema, fix_pattern}`, and every warning code as `{code, meaning, fix_pattern}` (`data` is `{error_codes, warning_codes}`).

- `doctor_exit` is the code's exit under `doctor`, from the [doctor's dictionary](#doctor-exit-codes).
- `retryable` is `true`, `false` or `"conditional"`. The last is for `plan_stale`, `http_status`, `hash_mismatch`, `network` and `io`, whose errors decide one by one: each error's own `retryable` says.
- `details_schema` is the code's typed `details` as a JSON Schema fragment, the same one the error's schema carries.
- `fix_pattern` says what form the fix takes: `prose: …` when no fix is a command, `none: …` when there is no fix.

The human form is two tables, `-v` adding each code's fix pattern under it.

#### `robot-docs install-skill`

Installs the agent guide as a skill: `SKILL.md` in the folder coding agents load skills from, byte for byte what `omaddons robot-docs guide` prints (its stamp line included), so a Claude Code session on this machine knows omaddons as soon as a conversation turns to WoW addons. A new session picks it up.

```bash
omaddons robot-docs install-skill                                   # $HOME/.claude/skills/omaddons/SKILL.md
omaddons robot-docs install-skill --dry-run                         # what it would write
omaddons robot-docs install-skill --dir $HOME/.codex/skills/omaddons
```

- **Where.** By default `$HOME/.claude/skills/omaddons/`, which needs `$HOME/.claude` (Claude Code's folder) and creates `skills/` and `skills/omaddons/` in it. Without `$HOME/.claude` it is `not_found` (`details.path`), with no runnable fix and one `alternatives` entry per other agent's skill folder that exists (`$HOME/.codex/skills`, `$HOME/.agents/skills`). `--dir` names the skill folder itself, relative to the working directory or absolute; it is created when its parent exists (a missing parent is `not_found`, `details.path`), and a `--dir` that names no folder, is a file or is not UTF-8 is `usage`.
- **Never there.** A folder inside a game client, a Wine prefix or omaddons' own config, state or cache folder, or one holding a client or one of those, is `scope_denied` with `details.reason: "skill_dir"` and the folder in `details.path`; a symlinked folder is judged where it points. A `SKILL.md` that is a symlink (a dotfiles setup) is never written through, whatever it points at: `scope_denied` with `details.reason: "symlink"`, `details.path` and `details.link_target`; install into the link's real folder with `--dir`, or replace the link with a file. A `SKILL.md` that is not a regular file is `io`.
- **What it writes.** `written` is `fresh` (there was no `SKILL.md`), `replaced` or `unchanged` (it already held these bytes, and nothing is written). A `SKILL.md` that is some guide's own bytes, an older guide's say, is replaced. One that is not (edited by hand, without a stamp, or larger than 1 MiB) is first kept as `SKILL.md.bak` beside it, replacing an older `SKILL.md.bak`; the human output says so. The file is written atomically, mode 0644.
- **Other agents.** For each other agent's skill folder that exists and does not already hold these bytes, `commands` offers `omaddons robot-docs install-skill --dir <root>/omaddons` (action `install_skill`); nothing is written there unasked. `status` never recommends installing the skill.
- **History.** A run that writes takes the global lock, finishes any interrupted change first as `config set` does, reads the folder again and decides again under the lock, writes, and records a settings line of kind `skill` with the folder (`dir`); `undo` steps over it. `--dry-run` and an `unchanged` run take no lock and write nothing. It neither stores a plan nor takes `--if-match`: the bytes come from the binary and the folder from the argument.
- **Drift.** `omaddons doctor` reports an installed skill that holds another guide than this build's, or a copy edited by hand (`fm-plugins-skill-version-drift`, P3), and never rewrites it: run `install-skill` again.
- **Removing it.** There is no remove command: delete the folder.

`data` is `{dry_run, dir, file, written, bytes, tool_version}`: the skill folder and `SKILL.md`, canonical, what was (or would be) written, and the guide's length in bytes.

```text
$ omaddons robot-docs install-skill
installed the omaddons skill: /home/me/.claude/skills/omaddons/SKILL.md (18,432 bytes)
other agents: omaddons robot-docs install-skill --dir /home/me/.codex/skills/omaddons
```

### `capabilities`

What this build can do, answered offline in a few milliseconds, and the first call an agent should make. Everything in it is generated from the code (the live command tree and the registries), with the values `config.toml` changes as they are in effect.

```bash
omaddons capabilities           # a summary of under 40 lines
omaddons capabilities --json    # everything, as data
```

It reads the binary and `config.toml`, and whether a credential is there: the `OMADDONS_GITHUB_TOKEN` variable, a configured reference, or the stored token file, never its content. It runs no discovery, sends no request, runs no credential command and writes nothing. `-t` and the other global flags change nothing; `--if-match` is a usage error whose fix is the same command without it. It has no volatile key, so two runs over one config print the same bytes.

`data`, in this order:

- `version`, `contract_version` (every envelope's `schema_version`, `"1.1"`), `build_id` (the build's 16 hex digits, which a stored plan records) and `guide_digest` (`sha256:` and the agent guide's 64 hex digits, as its stamp line writes them);
- `features`: one word per surface this build has (`json_envelope`, `stored_plans`, `doctor`, …);
- `commands`: every command path in help's order, each `{path, description, group, mutates, json, dictionary, exit_codes, stores_plan, binds, network, aliases, args, flags, data_schema}`. `json` is always `true`; `dictionary` names the exit dictionary its `exit_codes` come from (`general`, or `doctor`); `binds` says whether a run binds to its preview with `--if-match`; `network` is `none`, `optional` or `required`. `args` lists its positionals as `{name, value_name, required, multiple, default, possible_values}`, and `flags` its own flags, then the global ones, as `{long, short, aliases, value_name, takes_value, required, multiple, default, possible_values, conflicts_with, requires, global, description}`. `required` marks a flag the command is refused without (`doctor gc --before`). `possible_values` is the exact list a value must come from, `null` for any text. `data_schema` is the command that prints its data's JSON Schema (`omaddons robot-docs schema backup restore`);
- `exit_codes`: `{general, doctor}`, the two exit dictionaries, each code with its `name` and `meaning` (and, for `general`, `retryable`: `null` for `ok` and `failed`);
- `env_vars`: the variables a release build reads, `{name, description, mirrors}`;
- `limits`: every limit and politeness constant, `{name, value, unit, config_key, source}`. `value` is bare, its type following `unit`: an integer for `bytes`, `ms`, `count`, `ratio` and `chars`, a string for `text`, an array for `list`, `{min_age_ms, keep_last, window_ms}` for `retention`. `config_key` is the `config.toml` key that changes it (`limits.download_max_bytes`), or `null`;
- `sources`: `{id, enabled, requires_credential, credential_configured, hosts, source}`. `git`'s `hosts` include the `git_hosts` added. `credential_configured` means a credential is present, not that it works: `omaddons auth github` checks GitHub's;
- `families`: `{name, interface_min, interface_max, legacy, source}`;
- `client_rules`: every load-rules row (see [Rules overrides](#rules-overrides-advanced)), then the two fallbacks, each field as the row has it (its name as `row`), and `source`;
- `target_id_forms`: what `-t` accepts, in [selector precedence](#choosing-a-target-with--t), then `all`;
- `error_codes`: `{code, exit, doctor_exit, retryable, meaning, details_schema}`, as [`robot-docs errors`](#robot-docs-errors) gives them; `warning_codes`: `{code, meaning}`; `actions`: every `commands[].action`, `{action, description, destructive}`, where `destructive` is the action's worst case (`apply` is `true` because a plan may remove, undo, restore or prune; each hint's own `destructive` is that instance's);
- `failure_modes`: `{count, subsystems}`: how many failure modes the doctor registers, and the subsystems they belong to (`omaddons doctor explain <id>` describes one in full);
- `schemas_uri` (`omaddons robot-docs schema`) and `robot_docs_uri` (`omaddons robot-docs guide`);
- `config_error`: `null`, or why `config.toml` could not be used, `{path, line, column, message}`.

Each value `config.toml` can change carries `source`: `config` when the file sets a key that applies to it (even to the built-in value), else `builtin`. `check_interval_hours = 0` keeps the default, so it stays `builtin`, and CurseForge and Wago stay `enabled: false` whatever the file says. A pointer (`schemas_uri`, `robot_docs_uri`, `data_schema`) is a command to run, and `null` in a build that does not have that command.

A `config.toml` that does not parse, holds a value out of range or cannot be read does not fail the command: it exits 0 with the built-in values, every `source` `builtin`, and `config_error` (`line` and `column` `null` for a file that could not be read at all). With `HOME` unset or relative there is no config path: `config_error.path` is `null`. A config that loads with unknown keys warns `config_unknown_key`, as every command does.

The human form names the build, counts the commands by group, prints help's exit-code table, the doctor's exit codes, each source (`on` or `off`, a configured token, `needs a key (M5)`, added git hosts, `(config)`) and the registries' sizes, and ends with `--json for the full description`. It never prints the config's path or a credential. Here with a stored GitHub token, `git_hosts = ["codeberg.org"]` and WoWInterface turned off (the build id differs with every build):

```text
$ omaddons capabilities
omaddons 0.1.0 · contract 1.1 · build e49d5dfe18e17d93
58 commands: look 18 · change 18 · setup 22 (omaddons help <command> for one)
Exit codes:
  0  ok            Success, including partial success (partial: true)
  1  failed        Runtime failure; see error.retryable
  2  usage         Unknown command or flag, bad argument, bad ref syntax
  3  not_found     Unknown or ambiguous target, ref, addon, plan or transaction
  4  refused       Safety refusal; fix names the flag or command
  5  locked        Lock held past the acquire timeout; retryable
  6  check_failed  An opt-in assertion (--check, --strict) failed
Doctor exit codes: 0 healthy · 1 findings_present · 2 fix_partial
                   3 fix_failed_rolled_back · 4 refused_unsafe
                   5 concurrency_lost · 6 online_required · 64 usage_error
                   66 no_input · 73 cannot_create · 74 io_error
Sources:
  github      on   token configured
  git         on   + codeberg.org (config)
  url         on
  local       on
  wowi        off  (config)
  tukui       on
  curseforge  off  needs a key (M5)
  wago        off  needs a key (M5)
  wowace      on
43 error codes · 55 warning codes · 26 actions · 31 failure modes
  115 limits (0 from config)
--json for the full description
```

### `help [COMMAND]`

```bash
omaddons help
omaddons help install
omaddons help targets add      # a subcommand by its path; the same as omaddons targets add --help
```

Bare help (`omaddons` or `omaddons -h`) is one screen: what omaddons is, the usage, the commands by group, where to start, the commands for agents and the exit codes. It never runs an action, opens a pager or prompts. Its lines stay under 80 columns. It ends:

```text
Start with `omaddons status`. Run `omaddons help <command>` for details; add
--json for one JSON envelope on stdout.
Agents: omaddons capabilities --json · omaddons robot-docs guide
        omaddons doctor --json

Exit codes:
  0  ok            Success, including partial success (partial: true)
  …

omaddons --help lists every command with what it does
```

The full page (`omaddons --help` or `omaddons help`) lists every command under its group (Look: changes no addon; Change: preview any with `--dry-run`; Setup) with its aliases and one-line summary, then every global option, the environment variables omaddons reads (a debug build's own variables on one line; the test harness's are never read by the binary and are not listed), the same "Start with" and `Agents:` lines, the exit codes and where to learn more, within 100 columns.

The `Agents:` line is the discovery trio: `capabilities --json` (everything this build can do, as data), `robot-docs guide` (the agent guide) and `doctor --json` (what is broken, with each remediation). Each is a command that changes nothing, and a test holds the line to the command tree.

With `--json`, bare help's `data` is `{name, version, usage, commands, global_flags, exit_codes, discovery}`. `commands` holds each `{name, about, aliases, group}`, and `exit_codes` each `{code, name, meaning, retryable}`. `discovery` lists the trio in the line's order, each `{command, argv, purpose}`. `argv` starts with `omaddons`, and `command` is that argv shell-quoted. `help <command> --json`'s `data` is `{command, about, usage, args, flags}`, where `command` is the canonical path (`targets add`).

---

## The JSON contract

`--json` (or `--robot`, `--format json`, `OMADDONS_FORMAT=json`) makes every command print **exactly one JSON object on one line on stdout**, on success and on failure, including usage errors and panics. Keys always come in the same order.

### Success

```json
{"ok":true,"schema_version":"1.1","tool_version":"0.1.0","command":"install","exit_code":0,"partial":false,
 "data":{"dry_run":false,"target":{"id":"retail","key":"01M51EX04HHWM46XE9F2B9JXED"},"tx":"01M51EX645RCVFN6Z4ZV5GRWVS","kind":"install",
   "items":[{"request":"./Bar-2.1.0.zip","ref":"file:/home/me/src/Bar-2.1.0.zip","name":"Bar","status":"installed",
     "from_version":null,"to_version":"2.1.0","version_confidence":"toc","channel":"stable",
     "folders":{"added":["Bar"],"replaced":[],"removed":[],"skipped":[],"recorded":[]},"error":null,
     "reason":"explicit","required_by":[],"bridge_dependencies":[],"unresolved":[]}],
   "reload_required":"none","atomic_swap":true,"creates_addons_dir":false},
 "warnings":[],
 "commands":[{"action":"undo","command":"omaddons undo 01M51EX645RCVFN6Z4ZV5GRWVS","argv":["omaddons","undo","01M51EX645RCVFN6Z4ZV5GRWVS"],"destructive":true}],
 "meta":{"elapsed_ms":3,"offline":true}}
```

<sub>Wrapped for reading; the real output is one line.</sub>

### Failure

```json
{"ok":false,"schema_version":"1.1","tool_version":"0.1.0","command":"install","exit_code":3,
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
| `schema_version` | string | `"1.1"` (contract 1.0 at the first tag, `v0.1.0`; see [Stability](#stability)) |
| `tool_version` | string | the binary's version |
| `command` | string | the command's canonical path: `install`, `targets add`, `auth github`; a bare command that runs its default action names that action (`backup` is `backup list`, `omarchy` is `omarchy status`); `help` for help, `version` for `--version`, and `omaddons` when no command could be read from the arguments |
| `exit_code` | int | the process exit code |
| `partial` | bool | success with some failed items (present on success and on `check_failed`) |
| `data` | object | the command's result (success, and `check_failed`) |
| `error` | object | `code`, `message`, `fix` (display string), `fix_argv` (argv array, or `null` when the fix is prose or needs a value only you can supply; such a fix shows the command with each missing value as a `<placeholder>`, as in `omaddons targets add <path>`), `alternatives` (`[{display, argv}]`), `retryable`, `details` (always an object) |
| `warnings` | array | `{code, message, fix, fix_argv}`. On a partial run, each failed item is mirrored here. On a failure, it holds what the run found before it failed (a `journal_pending`, a `config_unknown_key`), and stderr carries each as a `warning` event before the `error` event |
| `commands` | array | suggested next steps: `{action, command, argv, destructive}`, where `destructive` says whether this step removes addons, undoes a change, restores settings or prunes backups (confirm before running one that does). The actions: `undo` (`undo <tx>` after a change, destructive), `undo_run` (`undo --run <run>` after a change on several clients, destructive), `update`, `check_updates`, `adopt`, `adopt_folder` and `adopt_with_ref` (`adopt`'s guesses), `install` (search results, missing or bridge dependencies), `search` (a bridge dependency nothing resolves), `info` (`info <ref>` for a search result no selected client has evidence for), `reinstall`, `inspect` (`list -v` for degraded addons), `status` (after `status --cached` with no cache), `remove_orphans` (destructive), `list_unmanaged`, `forget_target` (destructive), `backup_create` (`backup list` and `status`, for a client with no recent qualifying backup), `refresh_backups` (`status`, `backup list -t <id>` when a client's backup inventory is stale), `check_errors` (`omaddons errors`, when `status --cached` finds a client's Lua errors stale or unchecked), `dismiss_errors` and `install_buggrabber` (`errors`), `reinstall_widget` (`status` and `omarchy status`, for an outdated bar widget), `enable_widget` (`omarchy status`, for one that is not on the bar), `apply` (`apply <plan_id>`: first in every dry run that stored a plan, and after `apply <plan_id> --dry-run` for the plan it checked; as destructive as the plan's command), `doctor_explain` (`doctor explain <fm> [-t <client>]`, once per failure mode a `doctor` run found) and `doctor_fix` (`doctor fix --if-match <token>`: the repair a `doctor fix --dry-run` or `doctor diff` planned, bound to its token) and `install_skill` (`robot-docs install-skill --dir <root>/omaddons`, once per other agent's skill folder that exists) |
| `meta` | object | `elapsed_ms`, `offline` (`--offline` or `OMADDONS_OFFLINE=1`, usage errors included) |

`check_failed` (exit 6) is the one failure that also carries `data` and `partial`, so `status --check --json` still returns the full status. On every other failure `commands` is empty, except a panic's, which can offer `undo` (below). Long commands also stream NDJSON `progress` events on stderr with `--progress json` (see [stderr events](#stderr-events)); stdout stays one envelope.

### Exit codes

`omaddons robot-docs errors --json` lists these codes from the binary, with each one's meaning, retryability, `details` schema and fix pattern, and `omaddons robot-docs schema <command> --json` gives a command's `data` schema.

| Exit | Name | Meaning |
|---|---|---|
| 0 | `ok` | Success, including partial success (`partial: true`, each failed item in `data`) |
| 1 | `failed` | Runtime failure: network, source, archive integrity, I/O, state, internal. `error.retryable` says whether to retry |
| 2 | `usage` | Unknown command or flag, bad argument, bad ref syntax, an unreadable `addons.toml` (`desired_state_invalid`) |
| 3 | `not_found` | Target, ref, addon, backup, run, plan or transaction not found, or ambiguous |
| 4 | `refused` | Safety refusal: running client, protected folder, ownership or case conflict, unsafe archive, wrong flavor, undo conflict, blocked host |
| 5 | `locked` | Another omaddons process held the lock past the 5-second timeout; retryable |
| 6 | `check_failed` | An opt-in assertion failed: `status --check` (attention, or a P0 or P1 issue), `outdated --check` (an update is available), or `--strict` on `install`, `update`, `sync` and `copy` (a partial success). `data` is kept |

A panic still produces one `internal` envelope and exit 1, never Rust's 101 (exit 74 in the `doctor` tree). Its fix is `omaddons doctor`, never the command that failed: `status` and every `doctor` command point to `omaddons status --cached` instead, and `status --cached` to a bug report, so following the fixes ends within three steps. Its `commands` offer `omaddons undo <tx>` for a transaction that committed before it, and its warnings are the ones the run found. A panic after the result was written leaves that result and its exit status, with one stderr `log` line saying so (dropped by `--quiet`); a panic the program recovers from prints nothing.

### The doctor envelope

Every `doctor` command prints the same envelope keys in the same order, with two differences:

- **No `data` and no `partial`.** The report's own keys sit at the top level, after `exit_code`: `view`, `run_id`, `state`, `summary`, `findings` and the rest ([`doctor`](#doctor)).
- **Findings are not a failure.** `ok` is `true` only for exit 0, and `error` is there only when the run failed (exits 4, 5, 6, 64, 66, 73 and 74), beside the report when the run had one.

A refusal before the run started has no report. Its stderr `error` event carries the doctor's exit code. `omaddons robot-docs schema doctor --json` gives the report's schema.

### Doctor exit codes

Every `doctor` command, usage errors included, exits by its own dictionary, in the BSD `sysexits` range for what is not a diagnosis. A typo of `doctor` itself (`omaddons doctr`) is a root-level usage error: exit 2.

| Exit | Name | Meaning |
|---|---|---|
| 0 | `healthy` | Every selected check ran to completion and found nothing |
| 1 | `findings_present` | The run completed and reports findings, a check that could not finish among them |
| 2 | `fix_partial` | `doctor fix` ran and a failure mode it repaired is still found |
| 3 | `fix_failed_rolled_back` | `doctor fix` failed and was rolled back; nothing changed |
| 4 | `refused_unsafe` | Refused before changing anything: a safety precondition, a missing `--force`, a conflict, a stale preview or an over-budget selection |
| 5 | `concurrency_lost` | Another omaddons process held the lock past the acquire timeout |
| 6 | `online_required` | The selection needs the network: add `--online` |
| 64 | `usage_error` | Unknown command or flag, or a bad argument |
| 66 | `no_input` | A target, run, failure mode or other input was not found, or is ambiguous |
| 73 | `cannot_create` | The run directory could not be created |
| 74 | `io_error` | A runtime failure: I/O, state, network or internal |

Every error code has its doctor exit, from its general exit: 2 → 64, 3 → 66, 4 → 4, 5 → 5, 1 → 74, except `plan_stale` (4), `check_failed` (1) and `online_required` (6). `robot-docs errors` lists each code's `doctor_exit`.

<details>
<summary><strong>Every error code (43)</strong></summary>

| Code | Exit | Retryable | When |
|---|---|---|---|
| `usage` | 2 | no | bad flag, value, `--since`, `--limit`, `--actor`, `--reason`, config key or id; a missing or relative `HOME`; a client named where a command or argument belongs, or among the names of `install`, `update` or `remove` (`details: {target, command}`) |
| `ref_invalid` | 2 | no | malformed ref: `gh:owner`, `git:` without https, `http://` |
| `desired_state_invalid` | 2 | no | `sync`'s `addons.toml` cannot be read: broken TOML, a bad value, an unknown family, an entry listed twice, or a newer schema (`details: {path, line, toml_key, reason}`, `toml_key` the TOML key on that line); also a failed `sync` item when two entries name the same addon, and `export --write` refusing a file edited since it was written (`reason: "edited"`) or an empty export (`reason: "empty"`), and `sync --prune` refusing a file with a section for no known client (`reason: "unknown_target_section"`) |
| `target_not_found` | 3 | no | no target matches the selector or id (for a near miss of a client id, game mode or family that selects a client here: the closest is the fix, in your own command, the others alternatives, then `omaddons targets`); a known game mode whose client is missing; no client found at all (fix `omaddons targets add <path>`), or only test clients (the first as the fix, each other one an alternative, all with `-t`), or, for bare `backup create`, only test clients with a `WTF/`; a `targets add` path holding no client; undo of a transaction whose target is gone |
| `target_ambiguous` | 3 | no | several targets fit; one alternative per candidate |
| `ref_ambiguous` | 3 | no | a name matches several addons or a local path; a folder `adopt` could take from several refs (one `--ref` alternative each); a backup id prefix matching several backups (`details: {prefix, candidates}`, one `backup list <id> -t <client>` each); a `backup restore --addon` whose folder another addon owns now (one `--addon folder:<F>` each) |
| `not_found` | 3 | no | unknown name (fix `omaddons search <name>`, sending no request), path, tag, asset, transaction, run or backup id (`details: {prefix, target, newest_ids}` for a backup); a transaction or run id prefix matching several (`details.candidates`); a settings change or a backup given to `undo`; a pruned snapshot (`details.reason: "snapshot_pruned"`); nothing to undo; an addon moved to another release between plan and commit; `backup create` of a client with no `WTF/` yet (`details.target`), or, without `-t`, of none; `apply <word>` when no plan matches and the word is an addon folder omaddons did not install (fix `omaddons help apply`, one `omaddons adopt <folder> -t <client>` alternative per client); `robot-docs install-skill` without `$HOME/.claude`, or with a `--dir` whose parent is missing (`details.path`) |
| `not_managed` | 3 | no | `remove` of an unmanaged, linked or `Blizzard_*` folder, or a name no managed addon answers to. When the name is an unmanaged folder (`update`, `pin`, `unpin`, `remove`), the fix adopts it: `omaddons adopt <folder> [--ref <catalog ref>] -t <id>`, other clients holding it as alternatives |
| `plan_not_found` | 3 | no | a stored plan `apply` cannot find or read: an unknown id or prefix (`details.prefix`; a prefix several plans share is `ref_ambiguous`), an unreadable or oversized file (`details.reason: "unreadable"`, `details.plan_id`, `details.path`), or one removed while `apply` waited for its lease |
| `no_compatible_release` | 3 | no | no candidate loads on the client (`details.tried` lists them), or the ref has no release to choose: none published, none on the channel (only prereleases: fix `--channel beta`), no zip asset or `release.json`, or a channel that needs GitHub's release API |
| `game_running` | 4 | no | the running policy refuses (fix appends `--while-running`), or `undo --saved-variables restore` while the client runs; `details.reason` says why (`loaded_toc`, `legacy_client`, `policy_refuse` or `saved_variables`), and `details.running: "unknown"` appears when `/proc` could not be read and the client may be running. A failed item's error in `update`, `sync` or `copy` carries both too. Also a change to a client whose interrupted `WTF/` restore waits for the game to exit (`details.reason: "saved_variables"`, `details.journal_pending`). `details.tx` and `details.resource` when a stuck journal's fence refused the write |
| `protected_folder` | 4 | no | a change would touch a `Blizzard_*` folder: an archive containing one, `adopt` of one, an undo whose history names one, or the filesystem writer's own refusal |
| `ownership_conflict` | 4 | no | another addon owns the folder; fix appends `--force`. A folder another client sharing this AddOns directory records (`details.reason: "owned_on_shared_target"`) is never overridable: the fix is the same command on that client |
| `unmanaged_folder` | 4 | no | an unmanaged folder exists; for an addon new to the client the fix adopts it (`omaddons adopt <folder> --ref <ref> -t <id>`) with the `--force` retry as alternative, otherwise the fix appends `--force`. A symlinked folder's fix is prose (remove or rename the symlink yourself) |
| `case_collision` | 4 | no | a folder name differs only in case from an existing one (`details.folders[].existing`, the live name); AddOns already holds such a pair (`existing` the first spelling, `spellings` every one); `Interface` or `Interface/AddOns` exists in two spellings (`details.paths`) |
| `wrong_flavor` | 4 | no | a `url:`, `file:` or `--file` archive does not load on the client, or `adopt` was asked for a group whose every folder is one its release installs only on other clients (`details.unloaded_folders`) |
| `unsafe_archive` | 4 | no | traversal, absolute paths, symlinks, devices, duplicate or case-colliding entries, the compression-ratio limit; for addon archives, `details.reason` names the rule (`parent_dir`, `absolute_path`, `drive_letter`, `control_character`, `not_utf8`, `symlink`, `special_file`, `file_and_directory`, `duplicate_entry`, `case_collision`, `size_overrun`, `invalid_folder_name`, `ratio`) and `details.entry` names the entry; for `backup import`, any archive that is not exactly an export (`details.reason`) |
| `archive_too_large` | 4 | no | over the entry, uncompressed-size or download limit (also a download past it while streaming, a `git:` checkout growing past it, an API response over 64 MiB, a live folder `adopt` indexes, or a `WTF/` a backup copies, past 200,000 entries or 16 levels, a `backup import` archive or a WowAce tree past its bounds); `details.limit` is always the number, with `details.config_key` when `config.toml` can raise it, else `details.constant`, the fixed bound's name |
| `undo_conflict` | 4 | no | a later transaction touched the same folders, already undone, or a removed folder put back by hand; a newer history line this omaddons cannot read may have touched the folders or SavedVariables the undo writes back (`details.reason: "history_unreadable"`, `details.lines`) |
| `host_blocked` | 4 | no | an HTTP request or redirect to a host not on the allowlist, or a `git:` ref to a host not in `git_hosts` |
| `scope_denied` | 4 | no | the filesystem writer refused a path outside its scope; also recovery's stop for a journal whose client no longer carries its target's marker (`details.reason: "marker_mismatch"`, `details.journal_pending`; fix `omaddons doctor explain fm-concurrency_primitives-journal-marker-mismatch -t <client>`), or for a doctor repair's journal that names a skill directory omaddons did not install (`details.reason: "skill_dir"`, `details.target: null`). `details.tx` and `details.resource` when a stuck repair's fence refused the write. `robot-docs install-skill` into a folder inside a client, a Wine prefix or omaddons' own folders, or holding one (`details.reason: "skill_dir"`, `details.path`), or onto a symlinked `SKILL.md` (`details.reason: "symlink"`, `details.path`, `details.link_target`) |
| `state_too_new` | 4 | no | a state file written by a newer omaddons: a change is refused before anything is staged when its client's record or files index, or (to register a client) the registry, is newer; a journal a newer omaddons started (`details.reason: "journal_too_new"`, `details.journal_pending`; fix `omaddons doctor explain fm-concurrency_primitives-journal-too-new -t <client>`); a failure recovery cannot attribute to one client, which fails every change on every client (`details.reason: "unattributed"`): here, such a journal that does not say which client it is for. `details.tx` and `details.resource` when a stuck repair's fence refused the write (a repair journal a newer omaddons wrote refuses every change) |
| `filesystem_unsupported` | 4 | no | a `WTF/` restore cannot run on this client: `WTF/` is a mount point (`details.reason: "wtf_is_mount_point"`), the client's `Interface/.omaddons/` is on another mount (`stage_cross_mount`), or the filesystem cannot exchange two directories atomically (`no_rename_exchange`); `details.backup_dir` is the backup's folder to copy from by hand, with the game closed. Checked before anything is copied |
| `file_exists` | 4 | no | a file omaddons would create exists, at the start or created by another program meanwhile, and is never replaced (`backup export`'s destination); `details.file` |
| `locked` | 5 | yes | lock held past 5 s; `details.holder`. Also a plan's lease another `apply` held past 30 s (`details.lease`, the plan id; `details.holder` with `age_s`) |
| `check_failed` | 6 | no | `--check` or `--strict` |
| `network` | 1 | yes (no for too many redirects or an undecodable encoding) | connection failure, also mid-body; more than 5 redirects; a `Content-Encoding` omaddons does not decode; a source response it cannot read, such as a web page (`text/html`, a site's browser check) served where a release's file was asked for, refused before it is read and not asked for again; a git run that times out, or fails outside clone and fetch |
| `rate_limited` | 1 | yes | GitHub REST budget exhausted, or any host's rate-limit answer (403/429 with no requests remaining, `Retry-After` over 60 s, a 429 that outlasts the retries); `details.reset_at` |
| `download_failed` | 1 | no | the git binary could not clone or fetch a `git:` ref on another host: it needs a login (private repositories are not supported), the server refused to send the commit (fix: name a branch or tag), or another git failure |
| `http_status` | 1 | 5xx only | unexpected status; `details.status` |
| `hash_mismatch` | 1 | first attempt, and a damaged cached archive (never a backup's) | downloaded bytes did not match the expected hash, or a backup's file differs from the hash it was recorded with (`details.backup`): recorded `damaged`, fix `backup verify <id> -t <target>`; or a cached archive whose bytes no longer hash to its name (`details.blob`), when an offline or no longer served install could not download it again; fix `omaddons doctor --include-deep` |
| `plan_stale` | 1 | yes (no under `--if-match`) | another run changed the target between plan and commit, a folder `adopt` hashed changed before the lock, or a `git:` branch moved between resolve and checkout; under `--if-match`, the run is no longer what its preview showed (`details.changed`, fix the `--dry-run` form; see [Previews, plans and `--if-match`](#previews-plans-and---if-match)); `apply` refusing a plan that can no longer apply, never retryable (`details.reason`: `already_applied` with `details.txs`, `expired`, `binary_changed`, `input_missing` with `details.path`) |
| `offline` | 1 | no | `--offline` blocked a request |
| `bad_archive` | 1 | no | malformed, encrypted, or not Stored/Deflate |
| `distribution_disabled` | 1 | no | reserved for CurseForge (M5) |
| `source_disabled` | 1 | no | a source turned off in `config.toml` (the fix turns it back on); CurseForge or Wago refs and links (M5); a `git:` ref on another host when `git` is missing or older than 2.32, or in a command that does not run git |
| `auth_required` | 1 | no | a GitHub token is configured or stored but cannot be used: an unset `{ env = … }` variable, an unreadable `{ file = … }`, an empty or malformed token, or `auth github --from-gh` without a working `gh` |
| `auth_rejected` | 1 | no | GitHub answered 401 to the token (the fix clears or replaces it where it came from), or a `{ command = … }` credential failed |
| `state_corrupt` | 1 | no | an unreadable state or config file (with line and column); a SavedVariables snapshot copy that no longer matches its manifest, refused before an undo moves anything (`details.reason: "snapshot_mismatch"`); a journal holding lines this omaddons cannot read, left for manual repair (`details.reason: "journal_unreadable"`, `details.journal_pending`); a failure recovery cannot attribute to one client, which fails every change on every client (`details.reason: "unattributed"`): here, such a journal that does not say which client it is for; the recovery of a `WTF/` restore that could not finish preserving what it displaced (an I/O error, a full disk, or an identity it did not expect inside the protocol), keeping everything for the next one to resume with the same backup id (`details.reason: "preservation_incomplete"`, `details.journal_pending`). A state file that does not decode has the fix `omaddons doctor explain fm-schemas-state-corrupt`, which names the file; a stuck journal, its state's `omaddons doctor explain <id> -t <client>`. `details.tx` and `details.resource` when a stuck repair's fence refused the write (a repair journal whose first line does not decode refuses every change) |
| `io` | 1 | no (yes for a `WTF/` that changed while a backup copied it) | filesystem error; `details.path` (below `WTF/`, without account, realm or character names). A failure recovery cannot attribute to one client fails every change on every client (`details.reason: "unattributed"`, with `details.journal_pending` when a transaction is known): a journal file that cannot be read, an in-flight lock that cannot be probed, or a journal directory that cannot be listed; its fix is `omaddons doctor explain fm-concurrency_primitives-journal-unattributed`, or `omaddons doctor explain fm-schemas-state-corrupt` when no journal is named. A state file that cannot be read has that state-corrupt explanation as its fix too. `details.tx` and `details.resource` when a stuck repair's fence refused the write; a journal directory that cannot be listed refuses every change, with `details.resource` naming it and no `details.tx` |
| `internal` | 1 | no | a bug or panic; fix is `omaddons doctor` (`omaddons status --cached` when `status` or a `doctor` command failed, and a report when `status --cached` failed). Also the recovery of a `WTF/` restore that found something it did not record at a step's paths, left for manual repair (`details.reason: "unexpected_state"`, `details.journal_pending`), and of any other journal whose rollback found a step in neither its before nor its after state (`details.reason: "step_unrecognized"`, `details.journal_pending`; `details.target: null` for a doctor repair, which names no single client); a stopped recovery's fix is that state's `omaddons doctor explain <id> -t <client>`. `details.tx` and `details.resource` when a stuck repair's fence refused the write |
| `online_required` | 1 | no | a `doctor` selection holds only checks that need the network, without `--online` (`details.fms`, their ids; doctor exit 6); the fix adds `--online` |
| `doctor_refused` | 4 | no | a `doctor` command refused before changing anything; `details.reason` says why. `over_budget`: the selection's declared cost is over `--budget` (`details.budget_ms`, `details.estimated_ms`; the fix is the budget it needs, rounded up to 100 ms, and the alternative drops `--budget`). For `doctor fix`: `force_required`, `client_running`, `filesystem_mismatch`, `too_many_actions` or `fixer_conflict` (see [`doctor fix`](#doctor-fix)), or a reason a fixer gives for declining |

</details>

<details>
<summary><strong>Every warning code (55)</strong></summary>

| Code | Meaning |
|---|---|
| `journal_recovered` | an interrupted transaction was finished or rolled back |
| `delegated_recovery` | `doctor fix` finished an interrupted transaction through omaddons' own recovery, as its preview disclosed; the fix undoes one it rolled forward (`omaddons undo <tx>`), or shows history for one it rolled back |
| `journal_pending` | an interrupted transaction waits for the next change command; or, saying `journal_too_new` or `journal_unreadable`, one this omaddons cannot recover, left as it is; or, saying `marker_mismatch` or `step_unrecognized`, another client's change whose recovery stopped for manual repair (for each such stop the fix is the doctor's explanation of that state, as the error on its own client carries it); or, saying `saved_variables`, `preservation_incomplete` or `unexpected_state`, another client's `WTF/` restore that waits for its game to exit or whose recovery stopped; or a doctor repair (`repair transaction <tx>`) that waits, or whose recovery stopped (the reason in its text, and whether writes to what it protects are refused or, when it cannot be read, every change; the fix is its `omaddons doctor explain`); or a journal left behind a repair recovery could not settle (`… waits behind repair <tx>, which recovery could not settle (<reason>) …`, or for a repair, behind another client's unsettled `transaction <tx>`), with that repair's `doctor explain` as its fix |
| `tx_in_progress` | another omaddons process is committing |
| `atomic_swap_unavailable` | the AddOns filesystem has no `renameat2` exchange; two journaled renames were used |
| `reload_required` | the game client runs: `/reload` in game to pick up a replacement that keeps its loaded TOC, or restart it for anything else (a new addon folder included); the fix says which |
| `saved_variables_deferred` | the game is running, or recovery could not tell or could not write them; restore SavedVariables later with the given command |
| `saved_variables_out_of_scope` | SavedVariables of the changed addons are in a `WTF/` reached through a symlink outside the client directory (a sync folder), or under a `WTF`, `Account` or `SavedVariables` folder spelled two ways (`WTF` beside `wtf`): they are never snapshotted or written, so undo leaves them as they are. One warning per reason; it counts the files and never names them |
| `running_unknown` | `/proc` could not be read, so every client is treated as running: changes follow the running policy, SavedVariables restores wait, and `status` reports `running: null`. Once per command |
| `snapshot_cap_exceeded` | protected snapshots alone exceed the size cap |
| `retention_failed` | pruning failed after a commit or a `backup create`; the transaction or the backup stands. After a backup, the fix is `omaddons backup prune -t <id>` |
| `out_of_date` | the TOC's interface is older than the client's, or its `AllowLoadGameType` names the client and none of its interfaces is in the client's range; it loads only with "Load out of date AddOns" enabled. Never for a TOC below the client's interface floor (retail 12.0 and later: 120000), which does not load at all and is `wrong_flavor` (`below_client_floor`) |
| `folder_left_out` | an install left out a folder of the archive built for other clients only (it lists only other clients' interfaces, has no Interface the client reads, or lies below the client's floor), while the addon itself is built for this client; the message names the folder and what it lists. No fix |
| `any_flavor` | an addon installed under `--any-flavor` although its publisher labelled the file for other clients, or its folders target other clients; the message names what it allowed. No fix |
| `older_release_for_client` | a GitHub repository's newest release has no build for this client, so the newest older release whose `release.json` declares it was installed ([look-back](#a-release-per-client-github-look-back)); the message names both releases. No fix |
| `missing_dependency` | a folder an installed addon's TOC requires was not installed: no keyless source ships it, several projects do, its only build is out of date, a dependency limit was reached, or `--no-deps`; the fix names a candidate when there is one, and searches for the folder (`omaddons search <folder>`) when no keyless source ships it |
| `toc_comma_on_legacy` | a comma interface list on a client that reads only the first number |
| `client_not_launched_since_patch` | `Config.wtf` predates the client's current version |
| `widget_outdated` | `status`: the bar widget serving this omaddons state is from another build or has other files (never merely another state); it stays read-only until `omaddons omarchy install` |
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
| `local_changes_replaced` | an install or update replaced folders whose files had changed on disk (it names up to 10 and counts them all, and says how long retention keeps the snapshot); the fix is `omaddons undo <tx>`, or under `--dry-run` a reminder that the snapshot keeps them |
| `snapshot_only_copy` | a transaction moved folders that exist nowhere else into its snapshot (an unmanaged folder replaced under `--force`, an addon adopted as it was or installed from a local archive or directory, removed or replaced); undo can restore them only until retention prunes it, and the message states the configured bounds. It names up to 10 folders; `-v` says where the snapshot keeps each. The fix is `omaddons undo <tx>`, or under `--dry-run` a reminder |
| `not_locked` | `sync --locked`: entries without a version installed their newest release (one warning per client, naming them) |
| `still_required` | an addon removed (by `remove` or `undo`) is required by addons that stay, which will not load without it; the fix is `omaddons undo <tx>`, or under `--dry-run` a reminder that undo restores it |
| `orphaned_dependency` | a remove, update or undo left dependencies required by nothing; the fix is one `omaddons remove <names…> -t <id>` naming them all |
| `bridge_dependency` | a newly installed addon's other folder (a bridge or plugin) works with an addon that is not installed; nothing is installed for it, and the fix installs it (`omaddons install <name or ref> -t <id>`), or searches for it (`omaddons search <folder>`) when no keyless source ships it. Only on the addon's first install |
| `target_addons_shared` | `status`: two or more clients share one AddOns directory (an `Interface/AddOns` symlinked between them); a folder one client records is `shared` on the others, which may not record it too; the fix is advice: manage those addons from one client, or stop sharing the directory. Also on `install`, `update` and `remove` of a folder both clients recorded before that was refused: the change shows on the other client too (fix `omaddons status -t <other>`) |
| `history_lines_skipped` | `history` and `undo`: lines of `history.jsonl` could not be read (a hand edit or other damage, or a line a newer omaddons wrote; not a write a crash cut short) and were skipped, so the transactions on them are not listed and cannot be undone (undo assumes a newer one touched what it may have; see [`undo`](#undo-tx)); it counts them, and the fix is advice, since omaddons never rewrites the file |
| `download_unverified` | a WoWInterface file whose details published no usable MD5, or a Tukui download without a usable `ETag` MD5 (none, weak, unquoted or not an MD5), installed unchecked: TLS protected the transfer and the TOCs were verified. A later reuse of its cached archive (another client, a reinstall) says it again; no fix |
| `history_not_recorded` | a settings change (`config set`, a `targets` change, `auth github set` or `clear`, `export --write`, `errors dismiss`) was made, or a backup, but its history line could not be written; the fix is advice to check that the state directory is writable |
| `backup_copied` | `backup create`: the filesystem could not clone some or all of a backup's files, so they were byte-copied and take their full size; it says how many bytes. No fix |
| `backup_unsettled` | `backup create`: `WTF/` changed while it was copied, and again on a second try, so the backup is `best_effort`; it counts the files still changing and those changed, added and removed after their copy. The fix is advice: make another backup after the game exits |
| `backup_space_low` | `backup create`: less than `limits.backup_min_free_mb` is free beside the client's backups; the backup is made anyway. An automatic backup: too little was free even after pruning old automatic ones, or its byte copies would pass `limits.backup_copy_max_mb`; it was skipped. The fix is advice: free space, or change the setting |
| `errors_partial` | `errors`: a BugGrabber file could not be read completely (the game was writing it, it is over a size cap, or it could not be read); counts are from what was read. Its fix is `omaddons errors -t <id>`, after the game exits |
| `restore_rolled_back` | an interrupted `WTF/` restore was rolled back by the next change, and what it had displaced, which no other copy held (a tree the game played on, the last copy of a deleted backup, the tree from before it), is kept as a `recovered` backup; the message names only the backup and the client, and the fix restores it (`omaddons backup restore <id> -t <target>`) |
| `before_restore_skipped` | `backup restore` kept no `before_restore` backup of the current `WTF/` (the filesystem cannot clone files, or there was no `WTF/`); the restore went on, and its undo snapshot keeps the replaced tree until retention |
| `auto_backup_failed` | the automatic backup before a change failed; the change went ahead without it. The fix makes one by hand (`omaddons backup create -t <id>`) |
| `omarchy_shell_unreachable` | `omarchy install` or `remove`: Omarchy's shell could not be reached (over SSH, or with the shell stopped), so the widget's files changed but its place on the bar did not; the fix runs the command again from the desktop session (for `remove`, `omarchy plugin disable a44.omaddons`) |
| `target_id_reserved` | `-t all` selected the client whose id is `all`, not every client (an id wins over the keyword); only when `-t all` is given; the fix is advice: `omaddons targets rename all <new id>` |
| `plan_not_stored` | a dry run could not store its preview: the store is full, the plan is too large, another preview held its lock, the intent could not be normalized, or a write failed. The preview and its binding token remain valid; expired plans are cleared as they age, within 24 hours |

</details>

### Previews, plans and `--if-match`

A confirm dialog, or an agent that previews before it acts, must not run something other than what it showed. A `--dry-run` of a command that changes addons, or of a destructive command, returns `data.precondition`, a token describing exactly what the run would do and what it saw, and the real run with `--if-match <token>` does that, client by client, or nothing. `install`, `update`, `pin`, `unpin`, `adopt` (when it adopts), `sync`, `copy`, `remove`, `undo`, `backup restore`, `backup prune`, `errors dismiss`, `doctor fix`, `doctor gc` and `targets rm` bind (`doctor fix`'s and `doctor gc`'s tokens are at the report's top level, `precondition`, and neither stores a plan: [`doctor fix`](#doctor-fix), [`doctor gc`](#doctor-gc); nor does `targets rm`, whose token names the client it forgets, or everything a purge deletes: [`targets`](#targets)). An `errors` read carries its own token for each client (`dismiss.precondition`), so a dismissal can be bound to the read the user saw, not to a later preview.

The storable commands' previews also store a plan ([Stored plans](#stored-plans)), and [`omaddons apply <plan_id>`](#apply-plan_id) runs exactly it: the stored intent bound to its stored token, every source it installs from checked again, one applier per plan at a time (its lease), and each client reported in `data.outcomes[]`. No command needs to be typed again. `apply` refuses `--if-match`: the plan carries its own token.

**Lost an `apply`'s output?** Run [`omaddons plans show <plan_id>`](#plans-show-plan_id) before you preview again or change anything. It says, from history alone, whether the plan committed on every client, some or none, and never guesses from addon versions; a second `apply` would only report `already_applied` without saying whether every client committed.

```bash
omaddons remove Foo -t retail --dry-run --json     # data.precondition: "pc1:…"
omaddons remove Foo -t retail --if-match pc1:…     # exactly that removal, or plan_stale
omaddons update -t retail --dry-run --json         # data.precondition: "pc1:…"
omaddons update -t retail --if-match pc1:…         # exactly that update, or plan_stale
```

- **When it is checked.** The real run plans again and computes the token from its own plan before it stages anything. Under the commit lock, a commit by another omaddons process on that client since then is caught too.
- **Checked again under the lock.** A bound `install`, `update`, `sync`, `copy`, `pin`, `unpin` or `adopt` checks its approval twice: when it plans, before anything is staged, and again inside the commit window, after it takes the global lock and before it moves anything. The second check re-reads every folder the run replaces, removes, copies from or adopts (one `stat` per entry; a folder whose entries changed is hashed again), checks that every name it creates is still free, that every unmanaged folder a dependency relies on is still there and that the client's marker is unchanged, and requires the operations it re-judges there to equal the approved ones. A file edited, added or deleted in any of those folders after the first check, by anything, still refuses the run: `plan_stale` with `observed`, or `selection` when the operations differ (a run left with nothing to do while its preview approved something included). The message names what changed, by folder. `remove`'s re-judged operations are compared too. With `-v`, each checked target logs what the check cost (`approval checked under the lock in … us`).
- **The refusal.** A difference is `plan_stale`, not retryable (the same token can only fail again), and nothing is changed. (A run refused under the commit lock keeps the day's [automatic `WTF/` checkpoint](#automatic-backups) it made before the lock, as any change does: a backup is no change.) `fix_argv` is the same command with `--dry-run` in place of `--if-match <token>`: preview again. `details.changed` names the parts that differ:
  - `command`: the token is another command's;
  - `targets`: a client's records changed, with their generation (for `install`, `update`, `sync` and `copy`: another change committed on the client, or the client registered, since the preview);
  - `selection`: the run would now change something else (a newer release for `install` or `copy`, another set of changes);
  - `observed`: what the run saw changed (the SavedVariables an undo would restore, or whether a transaction can be undone; for the four engine commands, a file in a folder the run replaces or removes, the client's marker, a local archive or folder, the rules, the omaddons build, `update`'s skip decisions, the `sync` file, or `copy`'s source client or source folders).
- **What each token covers.**
  - `remove`'s covers the folders it moves out and the records it changes.
  - `undo`'s covers the transactions it reverts, their folder operations, the SavedVariables files each restores and those files' state now.
  - `backup prune`'s covers the clients (by key, without generations: an install between preview and run does not change which backups go), the exact backups it would delete, and each one's `backup.json` as it read it.
  - `backup restore`'s covers the client's records (with their generation), the backup, the mode and the counts, the backup's `backup.json`, and the live `WTF/`'s stamps (each entry's path, kind, size, mode and time), because the game changes `WTF/` without touching any record.
  - `install`'s, `update`'s, `sync`'s and `copy`'s cover, per client: its record generation; the folder operations, each change and each release (by release id, download address, expected hash and archive digest; a Tukui download by source, key and release id, because its address is resolved fresh); the content of every folder the run replaces or removes, managed or not (every file, edited or clean, every symlink and empty directory); every name it would create, and that it is still absent; the client's marker; local inputs (a `--file` archive's hash, a folder's listing, a git checkout's commit); which addon satisfies which dependency; the rules that judged the releases; and the omaddons build (a preview made by another build is stale). `update`'s also covers what it chose to skip and why; `sync`'s the exact bytes of its file, even a comment; `copy`'s the source client's generation and the content of every source folder it copies. No text a human reads enters a token, so a token never moves because a message changed.
  - `pin`'s and `unpin`'s cover the records and generation of each client they would change, and the record change; when nothing would change, they cover every selected client holding the addon. With `--version`, they also cover the release and every file of every folder they replace. Both tokens also cover the client's identity and the build of omaddons that made them.
  - `adopt`'s covers the folders it records, file by file (each file's path, size and SHA-256, symlinks and empty directories included), the records it writes, and the rules that identified them. A managed folder re-pointed with `--ref` changes only its record, so its token witnesses no folder. When nothing would change, the token covers every examined client's identity and generation.
- **Releases between preview and run.** A bound run keeps its command's freshness. `install` and `copy` always ask their sources again, so a release published since the preview makes the bound run `plan_stale` (`selection`). `update` and `sync` answer from releases remembered in the last 15 minutes, as they do without `--if-match` (unless `--refresh`), so inside that window a bound run installs exactly the previewed version even when a newer one appeared.
- **Per client.** The guarantee is per client. On each client a bound run commits exactly the previewed operations or none of them. A run over several clients commits one transaction per client, as every run does: when a client changes after the run's own check (another omaddons command committed there first), that client is refused and the others still commit. The run is then `partial`: `targets[].tx` names each committed transaction, `targets[].error` (`plan_stale`) each refused client, and a client with neither changed nothing. When no client committed, the run fails with that refusal. Nothing calls such a run atomic.
- **What the guarantee covers.** Other omaddons processes are serialized by the global lock. Every change, by anyone, to what the preview covered is caught up to the second check. After it, each rename re-checks only the identity of the folder it moves and that its destination is still free, so a folder replaced in that interval rolls the transaction back (`plan_stale` with `observed` for a bound run, the retryable `plan_stale` otherwise). omaddons does not promise a compare-and-swap against another program writing into `AddOns` in the last instant before a rename, because Linux offers no "rename this directory only if nothing inside it changed"; what such a write changed is kept in the transaction's snapshot, and `omaddons undo <tx>` brings it back. A folder swapped in the instant between a rename's journal line and the rename stops the transaction for repair, with nothing lost (see [`journal_pending` that does not clear](#journal_pending-that-does-not-clear)).
- **Nothing left to do.** A bound run that finds nothing left to do still compares its token. If the world moved since the preview it is `plan_stale`, not "nothing to do", so the second of two runs holding one token learns that the first one won.
- **Dry runs.** `--if-match` is opt-in: commands without it behave as before. On a command that does not bind it is a usage error, and so is a malformed token. On `install`, `update`, `pin`, `unpin`, adopting `adopt`, `sync` and `copy`, a `--dry-run` given `--if-match` checks the token as the real run would: the same preview and token while it holds, `plan_stale` once it does not. It changes no addons or settings, and may store the preview in the cache as described below. On `remove`, `undo`, `backup restore`, `backup prune` and `errors dismiss`, a `--dry-run` given one previews afresh and returns the current token.
- The token is `pc1:` and 64 hex digits. Nothing is stored: it holds as long as what it describes is unchanged, and it is no credential.
- Real runs of every binding command carry `precondition: null`. With `-v`, a dry run of `install`, `update`, `sync` or `copy` ends with its token: `Or bind a typed run to it: --if-match <token>` after the stored plan's line, or `Run it bound to this preview: --if-match <token>` when no plan was stored.

### Stored plans

The dry runs of `install`, `update`, `pin`, `unpin`, `adopt` (when adopting), `sync`, `copy`, `remove`, `undo`, `backup restore` and `backup prune` keep the preview in the cache when it would change something. Their `data.plan_id` names the file, `data.plan_expires_at` gives its expiry as an RFC 3339 timestamp, and `data.approval` describes what the preview covers. The human output ends with `Plan <id> stored for 24 hours. Run exactly this: omaddons apply <id>` (the remaining time when reusing an older plan), and `commands[]` starts with that `omaddons apply <id>` (action `apply`, as destructive as the plan's command: `true` for `remove`, `undo`, `backup restore` and `backup prune`), so an agent that takes `commands[0]` after a preview runs exactly that preview. A preview whose plan was not stored offers no `apply`. These three keys are `null` on real runs. A no-op dry run has a `null` plan id and expiry, an empty approval selection, and no storage warning. `remove`, `undo`, `backup restore` and `backup prune` keep their existing binding tokens; restore and prune still print their binding-token line by default. `errors dismiss` binds to an observation and stores no plan.

A repeated preview of the same intent and binding token reuses the same plan and its original expiry. Each plan lasts 24 hours; the store holds at most 512 plans and 32 MiB, each plan at most 256 KiB. Admission clears expired plans, never evicts a live one, and declines a plan that would exceed a quota. If the store declines or fails, `plan_id` and `plan_expires_at` stay `null` and one `plan_not_stored` warning says why; the preview, its approval summary and its token stand. Clearing this cache is safe; preview again to recreate a plan. Plan writes do not ring the widget's doorbell.

`omaddons apply <plan_id>` executes a stored plan ([`apply`](#apply-plan_id)). A plan file is never edited or deleted by `apply`; it expires with its 24 hours, and history records which transactions it committed.

### The approval summary

`data.approval` is a typed summary of a dry run. `routine` is true exactly when `reasons` is empty. Each reason describes an effect that calls for review:

| Reason | Effect |
|---|---|
| `dependency_added` | Adds an addon because another addon requires it |
| `folders_removed` | Removes folders |
| `local_changes_replaced` | Replaces files changed by hand |
| `source_changed` | Takes an installed addon from another source |
| `channel_changed` | Changes an installed addon's release channel |
| `while_running` | Overrides the running-client policy |
| `reinstall` | Reinstalls the recorded release |
| `force` | Allows an ownership takeover or replacement of an unmanaged folder |
| `downgrade` | Installs an older version |
| `destructive` | Removes addons, undoes a change, restores settings or prunes backups |

`selection` lists each change, sorted by client id, addon and operation. Each item carries `target_key` (`null` before registration), `target_id`, `addon`, `source`, `channel`, `release_id`, `asset`, `from_version`, `to_version`, `op`, `fields` (sorted record fields for an edit), `tx` (an undo's transaction) and `backup` (a restore or prune's backup). Inapplicable values are `null`, and lists are empty. Operations are `install`, `update`, `remove`, `adopt`, `edit`, `undo`, `restore` and `prune`.

`scope` describes effects beyond those items: `dependencies_added`, `folders_removed`, `local_changes_replaced: {count, folders}`, `source_changed` and `while_running`. Name lists are sorted and deduplicated across clients. For example, an ordinary update can have:

```json
{
  "routine": true,
  "reasons": [],
  "selection": [{
    "target_key": "01JB8Q2VA00000000000000002",
    "target_id": "retail",
    "addon": "Foo",
    "source": "github",
    "channel": "stable",
    "release_id": "gh:o/foo@v2",
    "asset": "Foo.zip",
    "from_version": "1",
    "to_version": "2",
    "op": "update",
    "fields": [],
    "tx": null,
    "backup": null
  }],
  "scope": {
    "dependencies_added": [],
    "folders_removed": [],
    "local_changes_replaced": {"count": 0, "folders": []},
    "source_changed": [],
    "while_running": false
  }
}
```

### stderr events

When stderr is not a terminal, or `CI` is set, or `TERM=dumb`, stderr carries NDJSON events. Otherwise it carries human text.

```json
{"event":"error","code":"source_disabled","message":"cf:61284: CurseForge arrives in M5","fix":"omaddons help install","fix_argv":["omaddons","help","install"],"retryable":false,"exit_code":1}
{"event":"warning","code":"reload_required","message":"…","fix":"/reload in game","fix_argv":null}
{"event":"log","level":"debug","message":"discovery: scanned yes, roots reused 2, parsed 0, cache written no"}
{"event":"log","level":"trace","message":"release memo gh:o/foo|stable: fresh, answered from the memo (fetched 240 s ago, window 900 s); no request"}
```

Exactly one `error` event is emitted per failure, even with `--quiet`. `debug` log events need `-v`, and `trace` log events `-vv`. A log event's `message` is for reading: its wording may change in any release, so match on `event` and `level`, never on a message. A trace names a local or `url:` ref by its kind only, and never a path below `WTF/`. `progress` events come from long commands (`install`, `update`, `outdated`, `adopt`, `sync`, `copy`, `search` and `backup create`, and the stage and commit steps of `remove`, `pin` and `unpin`) when progress is on: `{"event":"progress","phase":"download","message":"gh:o/foo@v1 on retail","current":1,"total":2}`. The phases are `fetch`, `download`, `verify`, `stage`, `commit` and `copy`. `backup create` counts files under a fixed label, never a path: `copy` as `WTF/ files`, and `verify` as `verifying` when `--verify` hashes them. Within a phase, `current` only grows and never passes `total`; `total` grows when a dependency round or a new fetch adds items. Each item appears once, after an optional announcement at `current: 0` (`adopt` announces its downloads that way). On a terminal in human mode, progress is one line that is rewritten in place.

### Naming rules

Six rules hold every key and value of the contract (M4-D55). `tests/contract/rules.rs` checks them in every gate, on every command's data, the envelope, every error variant and every event (file schemas for their kind names only, since a persisted field changes only with its file's own format):

1. **Timestamps.** Every instant is an RFC 3339 UTC string (`…Z`) under a key that ends in `_at`, or is `at` itself, the moment of the object that holds it. Its schema says `format: date-time`, so an instant is found by its type whatever its key. Every duration is an integer whose key names its unit (`_ms`, `_s`).
2. **Null versus omitted.** A key is always present, `null` when it has no value. A key may be absent only when a flag adds it (`-v`, `--raw-details`) or its description says when it is omitted.
3. **Names.** Snake case. Arrays are plural, or named by what says they hold many: a past participle naming the set an operation acted on (`restored`, `dependencies_added`, `sources_skipped`), a participle phrase naming the members by their relation (`required_by`, `shared_with`, `left_out`, `owned_on`), a present participle naming the members in that state (`running`, `existing`), or a collective noun (`evidence`, `attention`, `drift`, `stack`, `selection`). A value whose JSON type follows another key is no array name, even when it can be a list (`capabilities`' `limits[].value`). `id` for the identifier you type, and `key` for a ULID key and nothing else (a setting is a `name`, a TOML key a `toml_key`). Counts are bare nouns (`files`, `bytes`); booleans are states (`restorable`, `busy`), never `is_…`, and a list is never named like a boolean beside it (`backup prune`'s `busy_backups`, `status`'s `any_flavor_addons`).
4. **Enumerations.** Snake-case values, each from a closed set its schema lists. Free text is a string with a `maxLength`, as the `--reason` a person gave is (`reason`, at most 200 characters), never an enumeration; a vocabulary `config.toml` can extend is named for what it holds (`target_not_found`'s `details.game_mode`).
5. **Command names.** The envelope's `command`, and `help <command> --json`'s `data.command`, is the command's canonical path (`targets add`, `auth github`).
6. **`details`.** Every error's `details` matches its code's declared shape (`omaddons robot-docs errors`): each key has one shape, and every declared key is one the binary emits.

Three kinds of vocabulary keep their own spelling, and only these:

| Vocabulary | Where | Kind | Follows instead |
|---|---|---|---|
| The report's `state` values and the severities `P0` to `P3` | the `doctor` tree, and `status`'s issues | External | the doctor contract, which generic doctor consumers read by its spelling (M4-D34) |
| A permission `mode` | the `doctor` tree, and `history`'s repairs | External | POSIX's four octal digits, checked by a pattern |
| `exit_codes.general` and `exit_codes.doctor` | `capabilities` | External | the keys of the `exit_codes` map: the exit dictionaries' own names, as `commands[].dictionary` gives them |
| Game families (`wrath-legacy`) | every `family` and `families[]` | Input | the kebab case users type in `-t` and `config.toml` |
| `auto_backup`'s `daily-copy` | every `auto.mode` | Input | the value users type in `config.toml`, which the bar widget compares |
| Client-rules row names (`mainline-12.0.7`, `fallback-custom`) | `capabilities`' `client_rules[].row` | Input | the `[client_rules.<row>]` table names users type; `config.toml` can add rows, so the set is open |
| `time` in `entries[]` | `net-log` | Persisted | the `net.log` line, echoed as written |

### Stability

The envelope's `schema_version` is the contract's version; `capabilities` reports it as `contract_version`. It was 1.0 at the first tag, and is 1.1 since the first additive change after it (`display_name` on `status` and `targets`, and the `targets.<id>.name` setting; since then also the `info` action, which `search` offers). The contract is pinned by a semantic manifest, `tests/contract/manifest.json`, generated from the code: every command path, flag and positional with its default, value grammar, arity, conflicts and requirements; every output field with its type (integer and number apart), whether it is always present, whether it may be null, its bounds, enum values and variants; every error code with its exit, retryability and typed `details`; every warning code, action, failure-mode id, event, phase, log level, exit code, doctor repair operation, release environment variable, `config.toml` key and `addons.toml` key; and every persisted file kind with its own version. `tests/contract/wire_names.txt` prints the same one name per line, for review. Both are regenerated with `OMADDONS_UPDATE_WIRE_NAMES=1 cargo test --test integration contract::wire_names`, and the gate fails when they differ from the code.

Since the first tag (`v0.1.0`, contract 1.0), the gate also compares the contract with the released baseline, `tests/contract/released/<version>.json` (today `1.0.json`): a copy of the last released manifest with its provenance (contract version, tag, commit and date), which only the release step writes. A change that only adds (an output field, an enum value or a variant, a field now always present, a narrower bound; a flag, an alias, a value or an optional positional) needs a higher minor `schema_version`, such as 1.1. A change that removes or loosens anything (an output field removed, made optional or nullable, or moved between variants, an integer that may now be a fraction, a wider bound; a flag or value removed, a new requirement or conflict, a required positional, a narrower grammar, a changed default), or one the comparison cannot place, needs a higher major, such as 2.0. A persisted file's schema change needs that file kind's own version bump, not the contract's. Regenerating the golden never satisfies the comparison, and nothing in the golden's header switches it off: only the version bump does, and each bump is recorded in a comment on the vision bead. A bump also changes `schema_version` in every envelope snapshot, which are re-accepted with it.

JSON member order is deterministic (repeated runs print identical bytes) but is not part of the contract: objects are unordered, and reordering keys needs no bump. Consumers should ignore fields and enum values they do not know; the bar widget accepts any 1.x envelope.

The contract suite (`tests/contract/`) runs a matrix of success and failure cases across the commands. Every envelope the suite produces, and every JSON envelope snapshot in the repository, is validated against its command's JSON Schema (draft 2020-12, generated with the serialize contract), strictly: a key the schema does not declare fails. Every error's `details` is validated against its code's typed specification, and every stderr event against its event's schema. The converse holds too: every key a code's specification declares, and every key all codes share, is shown by a real envelope of the suite (`tests/contract/details_coverage.rs`, whose table names the scene that produces each), so a specification declares nothing the binary does not emit. A rule test holds every schema to the contract's naming rules (timestamps, null versus omitted, names, closed enumerations); the violations that predated it were listed in `tests/contract/rules_allowlist.txt` until the audit emptied it, and the list is closed: the rule test refuses any entry. Beyond the schemas, it checks key order, `ok`/`exit_code` agreement and registered codes. It also checks that each envelope's `command` is the canonical path of its argv (`help` for `--help`, `version` for `-V`), and that its exit is one that command declares. Two logged scenarios (`tests/contract/canonical_names.rs`, `argc e2e contract canonical_names`) walk every `auth` spelling and every bare parent, alias and envelope-only name. Every `fix_argv`, `alternatives[].argv` and `commands[].argv` must parse as an omaddons command line and round-trip through shell quoting to its display string. A prose fix may not name a runnable omaddons command: a scan of the source and the matrix both fail one that does. A corpus of mistyped commands, and every command name with one letter dropped or two neighbours swapped, check that a fix never names a different command that changes something. The intent corpus (`tests/contract/intent_corpus.rs`) goes further: each common wrong invocation (a client in the wrong place, a mistyped client id, `-t all`, a mistyped command, flag or value) runs on a synthetic home with three clients; its suggested fix then runs on an untouched copy of that home and must reach the exit the corpus states; and a wrong invocation that fails must leave every state file and client tree as it was, byte for byte, and send no request. The suite also checks that every failure emits exactly one stderr `error` event, that suggestions carry no presentation flags, that read-only commands accept and ignore `--dry-run` and `--reason`, that repeated runs print identical bytes (and a source scan, `tests/determinism.rs`, keeps hash-ordered maps out of serialized types), and that `status`'s recommendations run on a fresh home. Snapshots pin representative envelopes.

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
 ownership + running-        one syncfs per archive           bound runs: check the approval again
   policy checks             wait (≤ 20 ms) for timestamps      (witnessed folders, created names,
                               to settle                        marker, dependencies), then the
                             digest folders the files           re-judged operations
                               index does not cover           re-check the running state
                             copy touched SavedVariables      journal: prepared
                               to .omaddons/snapshots/<tx>/   create Interface/AddOns if never launched
                               WTF/, hashing each once        SavedVariables: keep each copy whose file's
                                                                stat is unchanged, copy the rest
                                                              snapshot state files
                                                              swap each folder (journal step per rename):
                                                                new      → RENAME_NOREPLACE
                                                                replaced → RENAME_EXCHANGE
                                                                old/removed → .omaddons/snapshots/<tx>/AddOns/
                                                                each rename: the folder it moves is the
                                                                  one checked, its destination still free
                                                              journal: swapped
                                                              write record, files index, history line
                                                              journal: committed → release the lock
                                                              then: status.json, cleanup, doorbell, retention
```

- **Interrupted runs.** The next command that changes something recovers under the global lock first, every client's journals whichever client the command changes. That is install, update, sync, pin, unpin, `adopt` when it adopts (`--apply`, folder names or `--ref`), copy, remove and undo (also when they turn out to change nothing), `config set`, `targets add`, `rm`, `rename` and `default`, `auth github set` and `clear`, `export --write`, and `backup create` (before its history line). Dry runs and read commands, the `adopt` report included, never recover. A journal is written back only into its own client, once that client's identity marker names the journal's target. A journal that reached `swapped` rolls forward, and anything earlier rolls back. A rollback also removes the transaction's snapshots, except one that still holds an addon folder, which it keeps and names in the `journal_recovered` warning. Each step records the inode (and, where the filesystem keeps one, the birth time) of both paths, so rollback reverses a rename only if it actually happened, also after a reboot that renumbered the device (btrfs subvolumes, FUSE and device-mapper volumes get new device numbers on every mount). An interrupted undo whose SavedVariables restore had not run yet restores them during recovery only when the recovering command found that client and it is not running; `config set`, `targets`, `auth github` and `export --write` never restore them. Otherwise, or when the restore fails, recovery leaves them as they are and warns `saved_variables_deferred` with the command to run after quitting. Read commands never recover anything: they report `journal_pending` (or `tx_in_progress` while another process holds the lock), and `status` lists it under attention. A command's journal warnings describe the state when it finishes: a journal it recovered appears only as `journal_recovered`, never also as `journal_pending`. The doctor's repair transactions (`<state>/journal/<tx>.repair.jsonl`) are recovered by the same commands: rolled back by inverse operations before their `done` line, rolled forward from it, and only the journal and its in-flight lock are removed (the doctor run's backups and quarantines stay). The undo of a repair is one of them: its journal's `prepared` line names the repair it reverts, and rolled forward it records an `undo` line in history. One that cannot be read, names a client whose marker changed or a skill directory omaddons did not install, or has a recorded step in neither its before nor its after state is left as it is, as one `journal_pending` warning: it never fails the command that recovers, and recovery goes on to the next journal. What it protects is fenced instead (next item). Recovery runs in three passes: first the repairs that suspend or reactivate a journal (a doctor set-aside and its undo), and any repair whose first line cannot be read, which might be one; then the folder journals; then the other repairs, a repair waiting while what it would write overlaps a folder journal the second pass could not settle. A journal behind a repair that recovery could not settle is left as it is and reported (`journal_pending`, `… waits behind repair <tx>, which recovery could not settle (<reason>) …`); on the client a command changes, or for a journal that names no client, the command fails with that repair's error, `details.journal_pending` naming the journal held back and `details.tx` the repair. An undo of a set-aside puts each journal back under the provisional name `<tx>.reactivating.jsonl` first and renames it to its own name at the end, the activations last, one per journal (a forced `doctor fix` sets aside every stuck journal it selects in one repair), so a crash in between is rolled back whole before anything recovers a journal; a repair that would reactivate a journal any other way is refused before it writes anything, and one found in a journal is read as unreadable. `doctor fix` is the one exception to recovering first: it never recovers on taking the lock. It finishes only the interrupted changes its preview listed (`delegated_planned`), after it compares its token and with the same recovery every change command runs, skipping every other journal as it would a live writer's; it reports each one's outcome in `delegated[]`, and refuses a repair that would touch an unfinished change's files (`fixer_conflict`). A SavedVariables restore that recovery would finish runs only for a client it discovered whose game is closed, as for any change.
- **Stuck repairs fence what they protect.** A repair journal that recovery cannot settle refuses only the writes that overlap what it protects: the files it wrote, the backups its rollback reads, and for a set-aside or its undo the client's record and the folders the suspended journal would recover. Every other command, client and path proceeds. Each mutating command loads these fences once, when it starts, and the one writer refuses an overlapping write with that journal's own error, plus `details.tx` and `details.resource` (the protected path, never the one the command tried to write), and the fix `omaddons doctor explain …`. A repair journal whose first line cannot be read (it does not decode, or a newer omaddons wrote it) refuses every change until it is set aside, and so does a journal directory that cannot be listed. A journal of either family that recovery would leave behind a stuck repair (one a set-aside or its undo names, or any journal behind a repair whose first line cannot be read) is fenced too, what it protects refused to every command with that repair's error. A set-aside crosses the fences of the journals it moves, and only those. A commit checks again under the lock, after its own recovery: a repair that became stuck after the command started, or one that fences the client's record or a folder the commit writes, refuses it before its journal is written. A refusal is never a verdict on what was being written: a release whose download a fence refused fails with that journal's error (an item of `update` or `remove` carries its `details` too), never as `no_compatible_release`, and nothing is remembered as not loading. Read commands always run, and recovery itself is never fenced.
- **Cached archives are checked before use.** Every archive taken from the download cache is re-hashed against its name first. One that differs is never staged: online it is downloaded again into the same place, offline the command fails with `hash_mismatch` and changes nothing. Nothing on this path deletes; `omaddons doctor --include-deep` finds damaged archives and `doctor fix` quarantines them.
- **No atomic rename.** On filesystems without `renameat2` exchange (NTFS through ntfs-3g, some FUSE mounts), the first flagged rename the filesystem refuses switches the rest of the transaction to plain journaled renames (two per replaced folder, one per new or removed folder), and omaddons reports `atomic_swap: false` with an `atomic_swap_unavailable` warning. A cross-device rename is an error, never a fallback, and the staging area must be on the same filesystem as AddOns.
- **Concurrency.** Planning and staging take no lock, so a terminal, an agent and the widget can work at once. The global lock (`~/.local/state/omaddons/lock`, an flock that is never renamed or deleted) is held for the commit window, briefly for retention's prunes, and for `config set`, `targets` changes, `auth github`, `export --write` and the renames of `omarchy install` and `remove`, which each recover a journal left by a crash first. Backups copy without the lock: `backup create` takes it only to register a client and to append each backup's history line, recovering first too. A change command that changes nothing takes it only while a journal waits for that recovery, and leaves the journal for later if the lock is busy. SavedVariables are copied and hashed before it is taken, and under it each file is only checked with `stat` (one changed in between is copied again there). An undo also makes and checks the copies it restores before the lock, and puts each back with one `RENAME_EXCHANGE` (a plain rename when the file is gone or the filesystem cannot exchange). Folder digests come from the files index, or are taken before the lock for a folder it has no entry for. The checkpoint budget is under 50 ms per commit and undo, also with 16 MiB of SavedVariables and a 64 MiB folder, and for a whole restore of a 500 MiB `WTF/` (`tests/perf.rs`, release build, on tmpfs); the M1 and M2 checkpoints measured at most 2 ms, and the M3 restore 1 ms. On a real disk, a commit's fsyncs set the hold: on btrfs on an NVMe drive each costs about 3 ms, and an install or a whole restore held the lock 130 to 180 ms. A bound run's second approval check adds about 80 µs to a window replacing two folders of 84 files, and 2.4 ms over all 5,040 files of the realistic home (`the_approval_check_under_the_lock_costs`, release build, on btrfs on an NVMe drive). The gate measures the same commits, SavedVariables copied byte by byte, and prints them, failing only past a 2-second sanity ceiling. A second run waits up to 5 seconds, then fails with `locked` (exit 5, retryable, `details.holder`).
- **Whole-`WTF/` restores** ([`backup restore`](#backup-restore)). A restore copies the backup's `WTF/` to `.omaddons/stage/<tx>.wtf/WTF/` without the lock, with the modes its manifest recorded (a `0500` directory included), and checks the copy against the manifest: entries and sizes, and content too when the backup holds hashes (`hash_mismatch` names a damaged backup, nothing changed). Under the lock it exchanges that copy with the live `WTF/` in one `RENAME_EXCHANGE` (a `RENAME_NOREPLACE` into place when the client has none) and moves the old tree to `.omaddons/snapshots/<tx>.wtf/WTF.previous`, each a journaled step, so `WTF/` is wholly the old tree or wholly the backup at every instant. There is no fallback to two renames, which would leave a moment with no `WTF/` in which a game launched writes a fresh default one: a restore needs `WTF/` to be a plain directory on the client's own mount, its stage on that mount too, and a filesystem that can exchange (btrfs, ext4, xfs, tmpfs; probed in the stage before anything is copied). Otherwise it is refused with `filesystem_unsupported`, naming the backup folder to copy from by hand. Before the exchange the record generation, the mounts and the running state are checked again under the lock. The digests `actions.jsonl` logs for these renames are `stamp:sha256:` (each entry's path, kind, size, mode and time), so `WTF/` is never content-hashed under the lock. Every transaction that puts settings back (a restore, a `--addon` restore and the undo of either) journals in `journal/<tx>.wtf.jsonl`, keeps both halves of its undo snapshot in `snapshots/<tx>.wtf/`, and the restores stage in `stage/<tx>.wtf/`. Builds before M3 cannot read those journals and deleted them unread, and an hour later their orphan sweep took the transaction's stage and snapshots. They list only names that are a bare id, so they never see these. A restore committed before its snapshots were named this way keeps its bare-named ones, and is undone the same way.
- **Recovering a restore.** A restore's journal follows its phase like any other: rolled back before `swapped`, forward from it; the renames having run never decide that. It moves `WTF/` only while its client is not running: with the game running (or possibly running, or the client not found by the recovering command), a journal with a step recorded waits, and that client's changes fail with `game_running` naming it (`details.journal_pending`); a commit window that fails checks again before recovering in process, rather than trusting the check made a moment before. A rollback that a crash interrupted is safe to repeat: a step already undone is skipped, also when a later undo has since put another tree at its path.
- **The launch race.** "Not while the game runs" is a check, not a lock on the game. A client launched in the milliseconds between the last check and the exchange may still hold its old settings in memory, and write them over the restored files when it logs out. The restore's snapshot keeps the tree it replaced, and `omaddons undo <tx>` puts it back; restoring again with the game closed gets the backup back.
- **The backup stays put.** A restore holds a shared lock on its backup's directory from its plan to its commit, so another process's `backup prune` skips it as busy, without the global lock being held through the copy. The kernel drops the lock if the restore dies, so a crashed restore never pins a backup.
- **Preservation after a crash.** Between a crash and its recovery the game may play on the restored tree, a sync tool may replace `WTF/`, or the backup may be deleted, so recovery never deletes a version of your settings before it has a durable home. A step whose paths no longer hold what it recorded proves only that something changed them, never who: its live path is left exactly as found. When what the step moved out of `WTF/` is still where it recorded it (the tree from before the restore, with another `WTF/` live now), that tree is kept; when our restored copy alone was replaced, nothing of ours is displaced and recovery goes on. Then every version that differs from every surviving copy (that tree, a restored tree the game played on, the last copy of a backup deleted since) becomes one `recovered` backup (`metadata_only`, `best_effort`), through a journaled intent: a backup id reserved in the journal, the trees moved into an assembly under the transaction's stage, described, and published into `backups/` with one rename, so no listing ever shows a half-made one. Only then does recovery clean up, warning `restore_rolled_back` with the backup's id. A recovery that dies anywhere in that protocol, or meets an I/O error, a full disk or an identity it did not expect, stops with everything in place (`preservation_incomplete`), and the next one resumes with the same id. What recovery cannot place at all stops it the same way (`unexpected_state`), keeping the journal, the stage, both snapshot halves and the in-flight lock; every later change on that client then fails until it is resolved, see [`journal_pending` that does not clear](#journal_pending-that-does-not-clear).
- **A failed recovery.** If a commit fails and recovering it in the same process fails too (the disk turned read-only, say), the journal and the staged folders are kept. Those staged folders can be the only copy of the old version. The command fails with its own error, naming the transaction in `error.details.journal_pending` and the recovery's failure in `error.details.recovery_error` (`{code, message}`). The next command that changes something (see Interrupted runs) finishes it.
- **Crash testing.** All 35 engine failpoints are armed as a real `abort` in a child process for first installs, installs, updates, removes and undos (restoring, keeping, and with the client running, deferring SavedVariables), for the M2 transaction shapes, for the records-only pin and forget, each scene with SavedVariables, and for whole-`WTF/` restores and their undos (over a played-on tree with a `0500` directory, and onto a client with none). Restores and their undos are also crashed before each of their renames and during their recovery, which then runs three times; they fail on an I/O error injected at each of their writes in turn (the staged copy, the journal, the renames, the record); the game starts while one waits to be recovered, while a restore stages and before one recovers in process; and their recovery preserves what a play session, a sync tool or a deleted backup left displaced. `WTF/` must stay wholly old or wholly new, recovery must follow the journal's phase, the replaced tree must be kept, and every displaced version must end up in exactly one `recovered` backup, also when that recovery is itself crashed or fails at each step of the preservation protocol. Every failpoint is armed in every preservation shape (a whole restore, a single addon's restore and an undo of each), and an undo of a restore whose played-on files were preserved resumes with the same backup id. With two clients, recovery defers the one that is running and preserves the other's. An automatic capture racing a restore that is staging copies the pre-restore tree, or makes no backup when it is itself crashed, and the restore then commits. Every journaled rename of a multi-folder swap is crashed too, with atomic renames and in the two-rename fallback. The state afterwards must be wholly old or wholly new, SavedVariables included, and at least ten failpoints must actually crash a scene. A failpoint off a scene's path has to let it succeed. After recovery no snapshot dir is left of a transaction that never committed, except the SavedVariables copies a crash before `prepared` leaves, which the orphan sweep must remove once they are an hour old. Recovery is crashed too: after each step a rollback undoes, as a roll-forward starts, and before an undo's SavedVariables restore. The command after it must finish the job. A journal, `history.jsonl` or `actions.jsonl` whose last line a crash cut in half must not stop recovery either. An undo crashed after its swap is also recovered while the client runs, and its SavedVariables must never be written then. A commit whose recovery fails too is finished by the next command, local edits and a replaced unmanaged folder included.
- **What this omaddons cannot read, it never acts on.** Journals carry their format's version (1.1 since milestone 3). One started by a newer major version (`journal_too_new`), or holding a complete line this omaddons cannot decode (`journal_unreadable`: an unknown line or step, a `prepared` line that does not decode, or a damaged line before the last), is never taken for "nothing ran": recovery leaves it as it is, with its in-flight lock, stage and both snapshot halves, its own client's changes fail with its `journal_pending` details, and other clients go on. A newer minor version and unknown fields are recovered as usual, and a torn last line is still a crash mid-append, ignored (a journal that is nothing but a torn `prepared` line is pruned: nothing ran). Before anything is staged, a change is refused with `state_too_new` when its client's record or files index, or (to register a client) the registry, was written by a newer omaddons, and checked again under the lock; the save that once caught it only after the swap stays as a backstop. History lines it cannot read stay skipped for `history`, and undo assumes a newer one touched whatever it may have (see [`undo`](#undo-tx)).
- **Records-only changes.** `adopt`, `pin` (without `--version`, which installs that release) and `unpin`, removing an addon whose folders are all gone, and a dependency's `required_by` bookkeeping are transactions that change records only: no folder is touched, they commit while the client runs with no reload needed, and each is undoable like any other.
- **Your local changes.** Files you edited in a managed addon are replaced by an update or a reinstall, with a `local_changes_replaced` warning naming them; the transaction snapshot keeps them, so `omaddons undo` brings them back.
- **Your `addons.toml`.** `export --write` keeps the previous file as `addons.toml.bak`, refuses to replace a file edited by hand since it last wrote it (without `--overwrite`), and never writes an empty export.
- **Backups.** A backup says what it proves. `stable` means `WTF/` was observed not to change while it was copied (each file stamped before and after its copy, the tree walked again afterwards), not that a filesystem snapshot was taken; a copy that kept changing is kept as `best_effort` and never counts as a checkpoint. `metadata_only` means no content was ever hashed, so a file damaged at the same size goes unnoticed; `verified` means every file was hashed (`--verify`). A backup is staged under `.omaddons/stage/` and renamed into `backups/` only once described, so a crash leaves no half backup, only a stage the orphan sweep removes. Backups are owner-only (`0700`), and their manifests, which name the account, realm and character, never leave the machine.

### Automatic backups

With `auto_backup` on (`daily`, the default), each client gets two kinds of checkpoint:

- **Before the day's first change.** Before the first `install`, `update`, `remove`, `sync`, `copy` or `undo` of a local day that moves folders or restores SavedVariables, `WTF/` is backed up (kind `auto`, `trigger: pre_change`). It happens before the change stages anything and outside the lock, and its history line lands just before the change's. A restore's `before_restore` backup counts as that day's checkpoint. Changes that touch only records (`pin`, `adopt`), dry runs and changes with nothing to do make none. It runs even while the game runs (it writes `WTF/` only at logout and `/reload`), and records `client_running: true`.
- **After a play session.** `omaddons backup create --auto` makes one (`trigger: post_session`) when settings changed since the last checkpoint. The widget runs it when a client stops, and for every stopped client when the widget starts. It is never made while the game runs, or while omaddons cannot tell whether it does.

Both are skipped, as a success with a reason, in these cases:

- **Nothing changed** (`unchanged`): `WTF/`, entry by entry (path, kind, size and time), is what the newest checkpoint holds.
- **A checkpoint** is a backup made here (`manual`, `auto` or `before_restore`), whole, and observed stable. An imported, recovered, partial or best-effort backup never counts, so the next trigger tries again.
- **Clones only** (`no_reflink`): `daily` makes a backup only by cloning, so it costs no space at first. Where the filesystem cannot clone files (ext4, tmpfs, NTFS) it makes none and writes nothing. Whether a clone worked is recorded from each attempt: `backup list` and `status` show `auto.capability` and the last attempt (`auto.last: {at, result}`), and `status` keeps recommending `backup create` while no checkpoint is recent, so a client automatic backups cannot cover is never silently unprotected. `daily-copy` byte-copies too, within `limits.backup_copy_max_mb`, pruning older automatic copies first.
- **Space first** (`space_low`): below `limits.backup_min_free_mb` free, eligible automatic backups are pruned oldest first (never your own, the newest checkpoint, or one younger than a day). If that is not enough, the backup is skipped with `backup_space_low`.
- **One at a time** (`in_progress`): two attempts on one client at once make one backup. The second finds the first's lock on `backups/` and skips.

**Never failing a change.** An automatic backup that fails is only `auto_backup_failed`, and the change goes ahead. A crash in one leaves the change wholly old, and no half-made backup. Backup retention then thins them by day (see [Retention](#retention)).

### Undo

- An undo is its own transaction (kind `undo`, `reverts: <tx>`), so it can be undone too.
- Every history line carries the run (command invocation) it belongs to. Bare `undo` reverts a whole run, one undo transaction per client in its own lock window. Runs are attribution, not atomicity: each client's transaction commits and undoes on its own.
- Folders come back as they were from `<client>/Interface/.omaddons/snapshots/<tx>/AddOns/`: every file's bytes and mode, directory modes, symlinks and empty directories (restored files get new timestamps, and a directory always keeps owner access); tests compare all of these. Each addon's record and files index are restored, while other addons' later changes stand.
- **SavedVariables.** Only the touched addons' own files (`WTF/…/SavedVariables/<Folder>.lua` and `.lua.bak`, account-wide and per character) are snapshotted, with a SHA-256 manifest. On undo, a file that is unchanged since is left alone, a changed one is restored, and one the new version created is moved aside. A snapshot copy that no longer matches its manifest fails the undo with `state_corrupt` before anything moves; `omaddons undo <tx> --saved-variables keep`, its fix, reverts the folders without it. With `--saved-variables auto` and the game running, folders revert (if the running policy allows), and the SavedVariables restore is deferred. You get a `saved_variables_deferred` warning carrying the command to run after quitting: `omaddons undo <tx> --saved-variables restore`. SavedVariables directories that lead outside the client directory through a symlink (a `WTF/` kept in a sync folder) are never snapshotted or written; `saved_variables_out_of_scope` counts the files undo cannot restore. Folder names under `WTF/` are matched ignoring case, as Wine does (`wtf/account/…/savedvariables/` works); a folder spelled two ways (`WTF` and `wtf`) is left alone and counted in `saved_variables_out_of_scope`, because the game reads only one of them.
- **Conflicts.** `undo_conflict` (exit 4) when a later transaction touched the same folders (undo the newest first), when the transaction was already undone, or when a folder it removed has been put back by hand. A snapshot pruned by retention gives `not_found`; for folders with no other copy, the commit said so with `snapshot_only_copy`.
- **Repairs.** The undo of a `doctor fix` repair is strict, with no best-effort mode and no flag that relaxes it. It reverts the repair only while nothing the repair touched has changed since: every path as the repair left it, every backup intact. Otherwise it refuses before anything moves. Putting a set-aside journal back is guarded as well: the client's record generation, its marker and the folders that journal would recover must not have changed. Each journal comes back under a provisional name first and takes its own name at the end, the activations last, one per journal, so a crash in between is rolled back whole before anything recovers one. Its record (the doctor run, its backups and quarantine) stays inspectable until `doctor gc`.
- **What a repair keeps is never deleted behind your back.** Only [`doctor gc`](#doctor-gc) deletes a doctor run, a repair's client half or a cache quarantine, by its retention plan: never during recovery, never while a change that needs it is unfinished (an interrupted repair or undo, a set-aside that can still be reactivated, a history line it cannot read), only with `--yes`, and bound to its preview with `--if-match`. Its preview names every undo the deletion takes away.
- **Stuck journals are suspended, never deleted.** A journal recovery cannot settle (too new, unreadable, stopped at a step it cannot place, or naming a client that is no longer its own) is set aside only with its failure mode's id, `--force` and `--yes`. The set-aside moves the journal's whole set into quarantine: its journal file, in-flight lock, state snapshot, stage and client snapshot, each into the quarantine on its own filesystem. What it moves is found by the transaction id alone, in omaddons' own folders on the clients whose identity marker checks out, never by a path the journal names, so an unreadable or newer journal never directs a write; the finding and the dry run say whether anything may remain unaccounted for (`attribution: partial`). It records guards: each touched client's record generation and marker, and, where the journal can be read, the folders it would recover and its `WTF/` as one stamp (never a path below it). `doctor undo` reactivates the set only while every guard holds, the journal back under a provisional name first and its own name last, so recovery never runs it ahead of the undo. The set is kept until `doctor gc`.
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
| `safe` (default) | Replacements that keep the loaded TOC go through with `reload_required: "reload"` (`/reload` in game). A new folder goes through with `reload_required: "restart"`: no client was observed loading a folder added while it ran on `/reload` (3.3.5a did not across three), so omaddons asks for a restart until one is. Removing a folder the client does not load goes through with `reload_required: "none"`. Removing a folder the client loads, or an update that deletes or renames the TOC it loads, is refused with `game_running` until it exits. On legacy and unrecognized custom clients, every change is refused, except on 3.3.5a, which re-reads a loaded addon's TOC on `/reload`: there a replacement that keeps the loaded TOC goes through with `reload_required: "reload"`, and a new or removed folder waits for a restart |
| `refuse` | Every change is refused |
| `allow` | Nothing is refused; the result says whether `/reload` or a restart is needed |

`--while-running` lifts these folder rules for one command, even under `refuse`. **SavedVariables are never written while the client runs**, under any policy or flag, because the game rewrites them on logout.

A whole `WTF/` restore is refused while its client runs or may run, whatever the policy or flag, and the running state is checked again under the lock as the last step before the exchange. That check can refuse the restore, but it cannot keep the game closed: the game takes none of omaddons' locks, so it can still launch in the milliseconds between that check and the exchange. It then reads one consistent tree (the exchange is atomic), and may write its settings in memory over the restored files when it logs out. The restore's `WTF.previous` keeps the pre-restore tree either way.

### Retention

Retention runs after every commit, for the target just changed. At most once a day (`RETENTION_SWEEP_INTERVAL`), after a commit or a backup, it also runs for every other registered client that discovery finds. That way a client you no longer change, or only play, is pruned too.

- A client whose directory is gone is not reached; its snapshots wait until it is back.
- Each pass is stamped in `retention_sweep.stamp` in the state directory.
- A pass that fails on a client warns `retention_failed`, naming that client, and is not stamped, so the next change or backup tries again.
- `status` never prunes: it writes nothing.

On each client:

- Snapshots younger than 24 hours and the target's most recent transaction are always kept (protected).
- A snapshot that a deferred SavedVariables restore still needs (`saved_variables_deferred`) is protected until that restore runs, the undo is undone, or a later transaction changes those addons.
- Beyond those, a snapshot is kept while it is within 7 days and fewer than 10 newer snapshots are kept. Protected snapshots count toward the 10 but are never pruned for it, so 12 snapshots from the last day all stay. Everything else is pruned.
- If the kept set exceeds 2 GiB for the target (the client-side and state-side halves of each snapshot together), its oldest unprotected snapshots are pruned until it fits; when protected snapshots alone exceed it, `snapshot_cap_exceeded` says so.
- A failed prune is only a `retention_failed` warning: the transaction stands.
- History lines are never pruned.
- Undo of a pruned transaction fails `not_found`. For folders with no other copy (an unmanaged folder replaced under `--force`, an addon adopted as it was or installed from a local archive or directory), the commit says so with `snapshot_only_copy`, stating these bounds as configured.
- The download cache is an LRU capped at 1 GiB. Archives of releases installed on any target are pinned, and so are archives a stored plan would install until that plan expires, 24 hours after its preview. These pins save downloading the archives again and can keep the cache over its cap. An archive evicted anyway is downloaded and verified again when needed.
- Expired stored plans are also deleted after every commit, as they are before a preview stores a new one: at most 64 per pass, oldest first. Free leases are collected with their plans or on the following pass. An unexpired plan, a plan being applied, and a file whose expiry cannot be read are never deleted by this sweep; unreadable files stay until the cache is emptied.
- Orphaned stage directories, in-flight locks and the snapshots of transactions that never committed are swept: a rolled-back or failed transaction's snapshots at once, a dead planner's temporary downloads as soon as its in-flight lock is free, and anything else a crash left once it is an hour old. A snapshot that still holds an addon folder or a `WTF.previous` is never swept.
- A whole restore's `WTF.previous` (the tree it replaced) is pruned with its transaction by the count and age rules above, and never counts toward the 2 GiB cap, so a few large restores never evict the snapshots undo needs for addons.
- `WTF/` backups (`.omaddons/backups/`) are never touched by transaction retention. They have their own, below.
- A repair's client-side half (`<client>/Interface/.omaddons/snapshots/<tx>.repair/`) and the cache quarantine (`~/.cache/omaddons/quarantine/`) are never pruned by retention or the orphan sweep, however old: only `doctor gc` deletes them.

**Backups.** After every backup (by `backup create` or automatic), after a restore (its source kept whatever its age) and on `backup prune`, each client's backups are thinned:

- **Always kept:** every backup younger than 24 hours (since it was made or imported here), the client's newest checkpoint (`qualifying`) whatever its age, and a backup whose `backup.json` cannot be read.
- **Your backups** (`manual`, `imported` and `recovered`, which holds a play session an interrupted restore would have lost) are only ever deleted when you ask: by id, with `--keep` or with `--older-than`, or with every other backup of a client that is no longer installed, by `targets rm <id> --purge --yes` after its preview names each one (see [`targets`](#targets)).
- **Automatic backups** are thinned by local day. For each of the newest `limits.backup_auto_keep_days` days (7) that hold any, the day's first `pre_change` backup (and its first stable one, when the first was best effort and so no checkpoint), its last `post_session` backup and every `before_restore` backup stay; the rest of those days, and every older day, go. Thinning is per day because, with two triggers a day, a bare count of 7 would cover only two or three days of play.
- **Under space pressure**, [automatic backups](#automatic-backups) first reclaim space by deleting automatic backups that are not protected above, oldest first, rechecking after each. They never delete one of your backups, the newest checkpoint or one younger than a day. A byte-copy budget only ever deletes byte-copied ones.
- **Busy:** a backup being read (a restore, an export, a verify) is never deleted under its reader; it is skipped until the next pass.
- A failed pass after a backup is only `retention_failed`: the backup stands.

The retention and archive limits live in `src/limits.rs`. All but the 24-hour minimum age, the 1-hour orphan age, the daily pass over every client, the 32 MiB ratio floor and the 5-second lock timeout can be changed in `[limits]`. The time budgets (50 ms, 2 s) live in the tests, and the failpoint list in `src/engine/tx.rs`.

---

## Network and privacy

omaddons contacts **only** these hosts. The list is checked on every redirect hop and matched exactly (subdomains are never implied), and HTTP requests must be `https` on port 443.

| Host | Used for | State |
|---|---|---|
| `github.com` | `releases/latest`, `release.json`, release asset URLs (which redirect), `releases.atom` (release notes and the look-back), commits feeds; one `doctor --online` probe | used |
| `api.github.com` | REST fallback (beta and alpha channels, releases without `release.json`); `search`'s repository search; `auth github`'s `/rate_limit`; `doctor --online`'s `/rate_limit` checks | used |
| `codeload.github.com` | zips of exact commits for `git:` refs | used |
| `objects.githubusercontent.com`, `release-assets.githubusercontent.com` | release asset downloads | used |
| `api.mmoui.com`, `cdn.wowinterface.com` | WoWInterface: the file list, project details, downloads; one `doctor --online` probe (`api.mmoui.com`) | used |
| `api.tukui.org` | Tukui: the project list, download links, downloads, changelog pages; one `doctor --online` probe | used; responses are never stored by the HTTP cache (`no-store`); the project list, without its download links, is kept in the release memo (15 minutes, and for `--offline`) |
| `repos.wowace.com` | WowAce's Subversion repository: a project's tag list (`PROPFIND`), and its tree's listings and files at one pinned revision (BugGrabber); one `doctor --online` probe | used; never `HEAD` (the server refuses it) |
| `api.curseforge.com`, `edge.forgecdn.net`, `mediafilez.forgecdn.net` | CurseForge | off until enabled (M5) |
| `addons.wago.io` | Wago | off until enabled (M5) |
| every host in `git_hosts` | `git:` refs through the git binary, never contacted over HTTP, not even by a redirect; none of the hosts above can be listed | only when you list them |

`url:` refs may download from any of the always-allowed hosts above (every row but CurseForge, Wago and `git_hosts`).

**What a stable GitHub install costs:** zero REST calls. omaddons reads `releases/latest` without following its redirect, then fetches `release.json` and the asset. Both of those redirect to `release-assets.githubusercontent.com`, so `net.log` shows five lines per repository, plus one more download for each candidate that does not load. Requests are limited to 6 at a time overall and 3 per host. Retries cover 429, 5xx and network errors, up to 3 times with full-jitter backoff (0.5 s base, 8 s cap), and honor `Retry-After` up to 60 s. That includes a connection that fails while a response or download is being read: the request is made again from the first byte, and each attempt is its own `net.log` line. Timeouts are 10 s to connect, 30 s for an API call and 300 s for a download; at most 5 redirects are followed (then `network`); an API response is capped at 64 MiB (`archive_too_large`). API requests ask for gzip, and every request says `User-Agent: omaddons/<version>`.

**What a stable WowAce install costs:** one `PROPFIND` of the project's tags (about 32 KB for BugGrabber's 98 tags), then one listing per directory of the tag's tree, its `.pkgmeta`, and one request per file: six requests for BugGrabber. A repeat check within 15 minutes costs nothing, and after that one `PROPFIND`. A tree is bounded: at most 2,000 files, 200 directories, 8 levels deep and 2,500 requests (retries included), and the download limit over all its files; past any of them it fails with `archive_too_large`, naming the bound, before anything is installed. Two names in one tree that differ only in case are `bad_archive` (`details.reason: "tree_collision"`). The only methods it uses are `PROPFIND`, with one fixed body asking for each tag's revision, and `GET`; both are retried like any other request.

**Tokens.** A GitHub token is opt-in and sent only to `api.github.com`, where it raises the REST budget from 60 to 5,000 requests an hour (search: 10 to 30 a minute). It is, in order: `OMADDONS_GITHUB_TOKEN`; config.toml's `[sources.github] credential` (`{ env = "…" }`, `{ file = "…" }` or `{ command = [...] }`, a command run only when a REST request first needs it, never under `--offline`); or the file `omaddons auth github set` writes. `GITHUB_TOKEN` and `gh`'s credentials are never read, except `gh auth token` once, on `auth github --from-gh`, and `Authorization` is dropped on any redirect to another host. `doctor --online` resolves the token and sends it to `/rate_limit` as a REST request would, a `command` credential included; `doctor` without `--online` never resolves it. With or without a token, omaddons stops sending REST requests once the response headers say 5 or fewer remain, and reports `rate_limited` with the reset time. The budget is tracked per run, so the first REST request of a run is always sent.

**Search.** Only GitHub receives what you search for, in one `search/repositories` request per run (10 a minute without a token, 30 with one, counted apart from the REST budget). The catalog is built in, and WoWInterface's index and Tukui's list are whole lists, fetched (or reused) and searched on this machine, so neither learns the query.

**The git subprocess** runs with a cleared environment. Only `PATH` is kept. `HOME` and `XDG_CONFIG_HOME` point at an empty directory omaddons owns (`~/.cache/omaddons/git-home/`, mode 0700; a run refuses with `io` if anything is put in it), so no `.netrc`, git attribute file or other per-user setting reaches git or libcurl. Then `LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_ALLOW_PROTOCOL=https`. It also gets no credential helper, no askpass, no redirects and https as the only protocol, and it may contact only hosts listed in `git_hosts`, never the hosts above. Arguments go as argv (never through a shell), and the run is in its own process group, which is killed on timeout (30 s for `ls-remote`, `init`, `checkout` and `rev-parse`, 300 s for `clone` and `fetch`). A checkout is held to the download limit (`limits.download_max_bytes`, `.git` included): git is stopped, with its process group, once the checkout grows past it (`archive_too_large`). What git prints is kept up to 64 MiB per stream. URLs with embedded credentials are refused.

**`net.log`.** Every request that reaches the network (each redirect hop and each retry) and every git run against a repository is one JSON line in `~/.local/state/omaddons/net.log`, rotated once at 10 MiB. Requests refused before sending (offline, `host_blocked`) and the `git --version` probe are not logged. Query strings and headers are never written. An illustrative line:

```json
{"time":"2026-10-14T21:00:00Z","method":"GET","host":"github.com","path":"/WeakAuras/WeakAuras2/releases/latest","status":302,"bytes":0,"duration_ms":34,"purpose":"release-latest","via":"http","error":null}
```

Production purposes for HTTP are `release-latest`, `release-json`, `releases-atom` (release notes and the look-back), `rest-releases`, `commits-atom`, `asset` (every download, WoWInterface's and Tukui's included), `wowi-filelist`, `wowi-details`, `tukui-list`, `tukui-addon`, `tukui-changelog`, `wowace-tags` (the `PROPFIND` of a WowAce project's tags, `"method":"PROPFIND"`), `wowace-tree` (a pinned directory listing), `wowace-file`, `github-search`, `rate-limit` and `doctor-probe` (each request of `doctor --online`'s checks). `wire_bytes` is what crossed the network when it differs from `bytes` (WoWInterface's file list arrives gzipped). A Tukui download link carries a token: its path is logged as `/v1/download/<slug>/…`, never with the token. git lines have `"method":"GIT"`, `"via":"git"`, the exit code as `status`, and a purpose of `ls-remote`, `clone`, `init`, `fetch`, `checkout` or `rev-parse`.

**Offline.** `--offline` or `OMADDONS_OFFLINE=1` refuses all network access, git included, and runs no credential command. Local installs, `remove`, `undo` and every read command work offline. Release metadata a command fetched is remembered in the release memo; offline, `info` answers from it with a `stale_check` warning. When `--offline` alone made the run offline, an `offline` error's and a `stale_check` warning's `fix_argv` is the same command without it; under `OMADDONS_OFFLINE=1` the fix stays prose.

**Fetch once.** Within one command each ref is fetched once, however many targets and engine runs need it: `install` without `-t` probes every live client and then installs with the same answer. `install`, `copy` and `pin --version` always ask the source (a cheap probe may confirm the remembered release). `outdated`, `update`, `sync`, `adopt`, `search` and `info` reuse an answer up to 15 minutes old (`SOURCE_RECHECK_MIN_INTERVAL`); `--refresh`, where a command has it, asks anyway.

**Account, realm and character names.** `WTF/` paths name them, so omaddons keeps them out of what it prints: SavedVariables warnings count files and never name them; a path below `WTF/` in an error, a progress line or a verify note shows them as `account N`, `realm N` and `character N`, numbered in the order of the names folded to lower case (`account ?` for one it did not list); and a backup's `manifest.json`, which lists every path, never leaves the machine. `src/privacy.rs` holds the one redactor, which backups, restores and `errors` all use; `errors` numbers its databases' accounts with it too, so `account N` means one folder in every field.

**Lua errors.** By default `errors` prints only what cannot hold a name: Lua's and the client's own messages, paths relative to `AddOns/`, accounts as `account N`. An addon's own message shows only its length, and errors other players sent appear only as counts, with the sender never kept. `--raw-details` adds the rest, with the names it recognizes replaced, and says it cannot promise to catch every name (see [`errors`](#errors)). The bar widget's notifications carry addon names, client ids and counts only.

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

auto_backup = "daily"        # "off", "daily" (copy-on-write only) or "daily-copy"
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
backup_copy_max_mb = 4096        # MiB of automatic byte copies per target (daily-copy); 0 turns it off
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
| `git_hosts` | `[]` | Hosts allowed for `git:` refs through the git binary. Never an HTTP host from [Network and privacy](#network-and-privacy) (always allowed, CurseForge or Wago, in any case): `config set` refuses one, and a `config.toml` that lists one fails to load, naming it |
| `auto_backup` | `daily` | Automatic `WTF/` checkpoints (see [Automatic backups](#automatic-backups)). `daily` makes them only by cloning (btrfs, xfs), so they cost no space at first; where the filesystem cannot clone it makes none and writes nothing. `daily-copy` also byte-copies, within `limits.backup_copy_max_mb`. `off` makes none. The default is `daily` (before milestone 3 this README said `off`), so checkpoints are made with no setup. |
| `check_interval_hours` | `6` | `status` reports `updates.stale` when the oldest check is older than this (1–48; 0 keeps the default) |
| `limits.archive_max_entries` | `20000` | Entries per archive |
| `limits.archive_max_uncompressed` | `1GiB` | Expanded size per archive |
| `limits.archive_max_ratio` | `200` | Compression ratio, checked past 32 MiB |
| `limits.download_max_bytes` | `512MiB` | Largest download, and git checkouts |
| `limits.tx_retention_count` | `10` | Snapshots kept per target within the window |
| `limits.tx_retention_days` | `7` | The retention window |
| `limits.snapshot_max_bytes` | `2GiB` | Snapshot total per target beyond 24 hours |
| `limits.blob_cache_max_bytes` | `1GiB` | Download cache (installed releases and unexpired stored plans are pinned) |
| `limits.backup_min_free_mb` | `2048` | MiB free beside a client's backups below which `backup create` warns `backup_space_low` (and makes the backup anyway); 0 to 1,048,576, where 0 turns the check off |
| `limits.backup_copy_max_mb` | `4096` | MiB of a client's automatic byte-copy backups (apparent size), `daily-copy` only: beyond it, older automatic copies are pruned first, or the backup is skipped (`backup_space_low`). 0 to 1,048,576, where 0 turns the budget off |
| `limits.backup_auto_keep_days` | `7` | Local days of automatic backups that backup retention keeps per target: for each of the newest days holding any, its first pre-change backup (and first stable one), last post-session backup, and every before-restore one (1 to 100) |
| `sources.<id>.enabled` | `true` | `false` turns a source off (`github`, `git`, `url`, `local`, `wowi`, `tukui`, `wowace`): its refs fail with `source_disabled` and make no request, and `outdated` reports its addons `unchecked`. An id outside that list is accepted and ignored, without a warning |
| `sources.github.credential` | none | Where the GitHub token comes from: `{ env = "…" }`, `{ file = "…" }` or `{ command = [...] }` (see **Tokens** under [Network and privacy](#network-and-privacy)) |
| `targets.<id or key>.label`, `.family`, `.name` | none | [Per-target overrides](#per-target-overrides) |
| `client_rules.<row>.<field>`, `rules.*` | built in | [Rules overrides](#rules-overrides-advanced) |

### Per-target overrides

```toml
[targets.retail]
label = "main"               # shown in status and targets
name = "My main"             # display_name in status and targets (the widget shows it)

[targets.mystery]
family = "wrath-legacy"      # a misdetected or unreadable client: re-picks its load rules
```

`name` replaces the client's `display_name` (plain text, cut to 200 characters; control characters are dropped). `omaddons config set targets.retail.name ""` restores the built-in name.

A `family` override re-picks the client's load-rules row and its interface, as detection would have for that family. A client whose version cannot be read gets the family's current row: on Battle.net, `vanilla` loads `_Vanilla.toc` and honours `AllowLoadGameType`. Its interface then comes from the client's own `WTF/Config.wtf` (`lastAddonVersion`), when that lies in the family's range. Otherwise installs are judged by the family's interface range alone, and `out_of_date` is not reported. On a `*-legacy` row, the client's `lastAddonVersion` always wins. A family with no row for the install kind, such as a `*-legacy` family on a Battle.net folder, keeps the fallback rules. `unknown_flavor` is not reported for a client whose family an override sets.

### Rules overrides (advanced)

The detection tables can be patched without a new release (unknown keys there warn `config_unknown_key` too, array elements named by index, such as `rules.products[0].nmae`):

- `[client_rules.<row>]` (or `[rules.client_rules.<row>]`) patches one load-rules row; an unknown row name adds a new row, which then needs every required field. Rows are `mainline`, `mainline-12.0.7`, `mainline-10.2.7`, `mainline-9.1`, `forever`, `vanilla`, `tbc`, `wrath`, `titan`, `cata`, `mists`, the five `*-legacy` rows, `fallback-battlenet` and `fallback-custom`. The fields are `family`, `install_kind`, `version_min`, `version_max`, `game_types`, `multi_toc`, `comma_interface`, `game_suffixes`, `family_suffixes`, `legacy_suffixes`, `directive_conditionals` (whether the client reads inline directives such as `## Dependencies: X [AllowLoadGameType vanilla]`, or reads every line as written; true for every current client), `interface_includes_patch`, `legacy`, `reload_rereads_toc` (whether a running client re-reads a loaded addon's TOC on `/reload`, so the `safe` running policy lets a replacement that keeps it land; true for every modern row and `wrath-legacy`) and `interface_floor` (the lowest Interface a client whose own interface has reached it loads at all, whatever the player enables; 120000 on `mainline`, `mainline-12.0.7` and `mainline-10.2.7`, none elsewhere, and 0 removes it). A new row needs `family`, `version_min`, `multi_toc`, `comma_interface`, `interface_includes_patch` and `legacy`; its `directive_conditionals` and `reload_rereads_toc` default to false, so set both for a modern client, and it has no floor unless it sets one. A row name with dots is quoted in TOML: `[client_rules."mainline-12.0.7"]`.
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
| `OMARCHY_PATH` | Where `omarchy install` also looks for Omarchy's shell (`$OMARCHY_PATH/shell`) when `omarchy-shell` is not on `PATH`. Omarchy's tools get it, with the rest of the environment, unchanged |
| `PATH` | Where `gh`, `git` and `omarchy-shell` are found; `git` runs with only `PATH` from your environment |

Debug builds also read `OMADDONS_FAILPOINTS`, `OMADDONS_FAILPOINT_DIR`, `OMADDONS_HTTP_FIXTURES`, `OMADDONS_RECORD`, `OMADDONS_PROC_ROOT`, `OMADDONS_NOW` (an RFC 3339 time that freezes the clock), `OMADDONS_FS_CLONE` (`yes` or `no`: whether the filesystem under a client clones files), `OMADDONS_FS_FREE_MB` (the free space backups see), `OMADDONS_WIDGET_ID` (a suffix of up to 32 lowercase letters, digits and `-` that installs a separate copy of the bar widget) and `OMADDONS_TEST_FIXERS` (`1` registers the doctor's test-only fixers) for testing. Release builds ignore them. `OMADDONS_ALLOW_SKIP`, `OMADDONS_REAL_MACHINE`, `OMADDONS_LIVE`, `OMADDONS_REAL_CUSTOM_ROOT`, `OMADDONS_WRITE_SEEDS`, `OMADDONS_WIDGET_OUT`, `OMADDONS_TEST_LOG`, `OMADDONS_UPDATE_SKILL`, `OMADDONS_UPDATE_WIRE_NAMES`, `OMADDONS_ADVANCE_RELEASED_BASELINE`, `BENCH_HOME`, `LIVE_HOME` and `COVERAGE_HOME` are read only by the test harness, never by the binary (see [Milestone checkpoints](#milestone-checkpoints-not-part-of-the-gate) and [Test logs and `argc e2e`](#test-logs-and-argc-e2e)).

A test fails when the binary reads a variable this section does not list.

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
| `~/.local/state/omaddons/history.jsonl` | One line per committed transaction, and per settings change (`config`, `target`, `auth`, `export`, `dismiss`, `widget`, `skill`, never undone; `target` is `null` when it names no client; a `skill` line names its folder as `dir`). A repair's line, and the undo of one, has `target: null` and lists its clients in `targets`, its operations in `repairs` and its doctor run in `doctor_run_id`. A transaction `apply` committed also records the stored plan it came from, as `plan`. A line a crash cut short is closed off by the next write, and recovery appends that transaction's line again; other unreadable lines are skipped and counted in a `history_lines_skipped` warning on `history` and `undo` | Loses history |
| `~/.local/state/omaddons/snapshots/<tx>/` (`<tx>.wtf/` for a transaction that puts settings back), `journal/`, `inflight/`, `trash/` | Undo data for state files, journals, in-flight locks, staged deletions | No |
| `~/.local/state/omaddons/journal/<tx>.reactivating.jsonl` | A suspended journal while an undo of its set-aside is unfinished: recovery settles that undo first, and no journal listing or sweep takes it for a journal of its own. Never deleted by retention, and while it exists neither is anything of its transaction | No |
| `~/.local/state/omaddons/lock` | The global lock (never delete while running) | No |
| `~/.local/state/omaddons/doctor/runs/<UTC time>__<ULID>/`, `doctor/latest` | One directory per `doctor` and `doctor fix` run (0700; `doctor diff` and `doctor explain` make none): `report.json`, the envelope it printed, and `runtime.jsonl`, the run's own writes, both read-only once it ends; a hidden `.<id>.tmp` while it starts. A `doctor fix` that repaired something also holds `actions.jsonl` (its operations, as its journal recorded them), `backups/` (what it overwrote), `quarantine/` (what it set aside) and `undo.sh` (0500, restores what it changed); a dry run's holds only `report.json` and `runtime.jsonl`. An undo of a repair makes a run of its own, of kind `undo`: its own `actions.jsonl`, `backups/` and `quarantine/`, and a `report.json` that names the run it reverted (`reverted_run`), whose directory it never writes (only what its renames move out of that run's `quarantine/` leaves it). `latest` links to the newest finished run. Deleted only by [`doctor gc`](#doctor-gc), never what an unfinished change needs | Yes; `--since` then finds no earlier run. A repair's backups and quarantines are lost with it |
| `~/.local/state/omaddons/status.json` | A derived summary, rewritten after each commit and undo | Yes |
| `~/.local/state/omaddons/retention_sweep.stamp` | When snapshot retention last passed over every client, as one RFC 3339 instant, so that pass runs at most once a day. Not a versioned document: a stamp that does not read makes the pass due | Yes; the next change or backup passes over every client again |
| `~/.local/state/omaddons/net.log`, `net.log.1` | The request log | Yes |
| `~/.cache/omaddons/blobs/`, `http/`, `discovery.json`, `tmp/`, `inflight/`, `trash/`, `git-home/` | Downloads, HTTP validators, the discovery cache, temporary downloads and git checkouts, the in-flight locks of planning and dry runs, staged deletions, git's empty home directory | Yes when no command is running; reinstalls then download again |
| `~/.cache/omaddons/releases.json` | The release memo: the newest release each fetch saw (at most 4,096 entries), failures retrying cannot change, and what downloading candidates taught (each archive's hash, and whether it loads on a client) | Yes; the next commands fetch and download again |
| `~/.cache/omaddons/plans/` (`<plan_id>.json`, `<plan_id>.lease`, `.lock`) | Stored previews (`plan` 1.0, mode 0600): the command's typed intent, its binding token, the approval summary and the lines the dry run printed; at most 512 plans and 32 MiB, each at most 256 KiB; expired plans are deleted after 24 hours. A `<plan_id>.lease` is the lock `apply` holds while it runs that plan, naming the applier's pid | Yes when no command is running (a lease, when no `apply` runs); previewing again recreates them |
| `~/.cache/omaddons/quarantine/<run id>/` | What a doctor repair set aside from the cache, at its path relative to `~/.cache/omaddons/`: a damaged archive under `blobs/<sha256>`. Never pruned by retention or the orphan sweep; only `doctor gc` deletes it, with its run | Yes; the repair's undo can then no longer put it back |
| `~/.cache/omaddons/wowi/index.json` | WoWInterface's file list, slim, its text decoded and made safe (format 1.1; a 1.0 index is decoded once on load), refreshed after its hour | Yes; the next search or dependency lookup fetches it again |
| `~/.cache/omaddons/doorbell.json` | A signal for the bar widget: rewritten (`{at, command, actor}`) by every write that can alter what `status --cached` reports, never by `status` | Yes; the next change writes it again |
| `~/.cache/omaddons/errors.json` | The errors memo: what `errors` last observed per client and database (file stamps, sessions, first-read baselines, counts per session and addon), so `status` can tell new errors without reading BugGrabber's files. No message, stack or locals text | Yes; but deleting it forgets which older errors were new, except what `errors dismiss` acknowledged (kept in `errors_ack.json`) |
| `~/.cache/omaddons/history_tail.json` | The history projection: the end of `history.jsonl` that holds its newest 16 runs with something to undo (at most 1 MiB), so `status --cached` reports the last change without reading a history of any size, and `skill_dirs`, the newest 8 folders `robot-docs install-skill` wrote, however old their lines. Believed only while the history is exactly as long as it says and ends with its last line | Yes; `--cached` says `unknown` until the next change rebuilds it |
| `~/.cache/omaddons/export.json` | The hash of the `addons.toml` `export --write` last wrote, so a hand-edited file is not overwritten | Yes; the next `--write` then needs `--overwrite` |
| `<client>/Interface/.omaddons/target.json` | The client's identity, so a moved install keeps its id | Loses identity |
| `<client>/Interface/.omaddons/snapshots/<tx>/` (`<tx>.wtf/` for a restore or the undo of one) | `AddOns/` (replaced and removed folders), `WTF/` (SavedVariables copies with a manifest) and, for a whole restore, `WTF.previous/` (the tree it replaced): most of the undo data | Loses undo |
| `<client>/Interface/.omaddons/snapshots/<tx>.repair/` | A doctor repair's client-side half: `files/` (what it overwrote there) and `quarantine/` (what it set aside there). Deleted only by [`doctor gc`](#doctor-gc), never by retention or the orphan sweep | Loses that repair's undo |
| `<client>/Interface/.omaddons/stage/`, `trash/` | Staging (same filesystem as AddOns) and staged deletions | No |
| `<client>/Interface/.omaddons/backups/<id>/` | One `WTF/` backup, owner-only (`0700`): `backup.json` (what it is and proves), `manifest.json` (every entry's stats, and hashes after `--verify`; it names the account, realm and character, so it leaves this machine only inside an archive you export), `addons.json` (the addon versions that produced it) and the `WTF/` copy (see [`backup`](#backup)) | Loses that backup |
| `~/.cache/omaddons/backups.json` | The backup inventory: what each client's backups add up to, checked against the directories before it is believed | Yes; the next `backup list` rebuilds it |
| the `<FILE>` you give `backup export` | One backup as a `.tar.zst` (mode `0600`), written through a hidden `.omaddons-…` temporary file beside it, the only other file an export ever writes | Yours |
| `~/.config/omarchy/plugins/omaddons/` | The bar widget `omarchy install` wrote: `manifest.json` and its code in an `r-<hash>/` folder, whose generated `Build.js` records the binary, the `HOME` and XDG directories it runs under and the code's hash | Use `omaddons omarchy remove`; `omarchy install` writes it again |
| `~/.config/omarchy/plugins/.omaddons.stage-<id>/`, `.omaddons.old-<id>/`, `.omaddons.trash/` | Hidden from Omarchy: an install's stage (one per run), the old widget a swap without `RENAME_EXCHANGE` parks, and the widget side's staged deletions | Yes when no install is running; the next install removes what is left |
| `~/.claude/skills/omaddons/SKILL.md`, or `SKILL.md` in a `--dir` folder | The agent guide `robot-docs install-skill` wrote, byte for byte; `SKILL.md.bak` beside it is a hand-edited copy it replaced | Yes; delete the folder to remove the skill |

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
| `src/app/` | One module per command, plus `world.rs` (discovery + registry for a run), `engine.rs` (wiring), `precondition.rs` (the preview tokens `--if-match` binds to) and `issues.rs` (`status`'s findings) |
| `src/doctor/` | The doctor: the failure-mode registry `status`'s issues come from (`registry.rs`, with its rows' detectors under `detectors/`), the finding model with stable instance keys (`finding.rs`), the tolerant view (`view.rs`: each layer, such as `config.toml`, the registry and the records, read once, bounded and only when a check asks; one that cannot be read is a finding, never a default), the selection a run's flags make (`plan.rs`), the runtime with its budgets and run directories (`runtime.rs`, `budget.rs`, `runs.rs`), `explain.rs`, the online checks (`online.rs`), `doctor fix` (`fix.rs`, and the fixers under `fixers/`: `quarantine.rs`, `recover.rs`, `set_aside.rs`, `chmod.rs`) and `doctor gc` (`gc.rs`, `gc/`) |
| `src/contract/` | The contract's enumerable facts, shared by `capabilities`, `robot-docs` and the tests: commands, actions, environment variables, features, the JSON Schemas, the stderr events, the agent guide's source (`guide/`) and the semantic manifest |
| `src/build_info/` | The build id (`build.rs` computes it from the input manifest in `inputs.rs`), which stored plans, tokens and `capabilities` name |
| `src/vocabulary.rs` | The words a client can be called by, in `-t`'s order: what intent inference recognizes as a client named in the wrong place, without running discovery |
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
| `src/sources/` | Ref grammar and every source: GitHub (release.json, REST, Atom feeds, search, token), git (GitHub without the binary, other hosts with it), WoWInterface, Tukui, WowAce (its Subversion listing and the packaging it supports), url, local, changelogs |
| `src/resolve/` | Candidate selection, the catalog, dependency resolution, adoption evidence, search, update statuses, and `ReleaseCache`, the one way a command gets release metadata (each ref fetched once per run, the release memo across runs) |
| `src/net/` | HTTP client, allowlist, guard, retries, concurrency limits, validators, net.log, record/replay |
| `src/engine/` | The transaction engine, with `restore.rs` and `restore_addons.rs` (settings put back by exchange), `preserve.rs` (what a restore's recovery keeps as a `recovered` backup), `repair_action.rs` (the data a `doctor` repair is made of: fixes, the one-operation primitives they lower to with each path's predicate before and after, and a set-aside's guards; data only) and `repair.rs` with `repair/` (the repair transaction: `lower.rs` turns a run's fixes into those primitives, each predicate captured against what the steps before it leave, and refuses what a repair may never do, such as a write below `WTF/` or a rename across mounts; it reads and never writes; `commit.rs` runs it under the lock, `inverse.rs` holds the one rollback walk, and `recover.rs` settles a repair a crash left at every recovery point), and `simulate.rs` (`journal::diagnose`: what recovery will do with each interrupted journal of either family, by recovery's own decisions on a simulated tree, writing nothing) |
| `src/fsx/` | The only code allowed to write to the filesystem: the write-scope table, the global flock and per-directory locks, `renameat2`, the reflink clone policy, stamp digests, pruning, failpoints |
| `src/state/` | Config, paths, registry, records and the files index, history, status, the release memo, `addons.toml`, the export memo, the doorbell |
| `src/backup/` | `WTF/` backups: the on-disk model, the one tree copy, the inventory projection, automatic backups, export, import and verify, filesystem hints |
| `src/lua.rs` | A parser of the Lua literals the client writes as SavedVariables, read-only and infallible |
| `src/errors/` | Lua errors: BugGrabber's databases decoded into one model, attribution, the errors memo and dismissals |
| `src/omarchy/` | The bar widget: its embedded files (`assets.rs`), rendering it for a given binary and state, `omarchy install`, `status` and `remove`, the port of Omarchy's plugin validator, and the calls to Omarchy's tools |
| `src/privacy.rs` | The one redactor for account, realm and character names in `WTF/` paths, used by backups, restores and `errors` |
| `src/clock.rs` | The clock time rules read (`OMADDONS_NOW` freezes it in debug builds) |
| `src/politeness.rs`, `src/limits.rs` | Every network-politeness constant and every safety limit, each in one place |
| `catalog/catalog.toml` | The built-in catalog, embedded with `include_str!` |
| `omarchy/` | The bar widget's QML and JavaScript, embedded by `src/omarchy/assets.rs`: `Model.js` turns envelopes into what the views show and builds every argv the widget may run, `Service.qml` is its one service (the processes, the doorbell, the triggers), `views/` holds the panel's views; its tests are under `omarchy/tests/` (bun and `qmltestrunner`) |
| `build.rs` | The digest of the widget's files, so `status` compares an installed widget with this build without hashing them on every call; the build id; and the agent guide's digest |
| `tests/`, `fuzz/` | Integration, contract, crash, concurrency and live tests; cargo-fuzz targets |

**One doorbell.** Every write that can change what `status --cached` reports rings `~/.cache/omaddons/doorbell.json` once, after the write: each commit window (undo's too, after its lock is released), a recovery that rolled a journal back or forward, a release memo write that changed the memo (`outdated` and the other fetching commands), an errors memo write that changed it (`errors`), a backup inventory write that changed it (a backup made, or a stale summary rebuilt by `backup list`), each backup's history line, and every settings command. `status` never rings it (its one write is the discovery cache a plain scan refreshes), so the bar widget can watch the cache directory for the doorbell instead of polling. The widget never takes state from the file: it reads `status --cached` again when it rings.

**Invariants the build enforces:**

- Only `src/fsx/` writes to the filesystem (`tests/single_writer.rs` scans every production item of the source, test-only items aside). Only three modules start processes, each named with its reason and held to that by the same test: git checkouts (`src/sources/git_hosts.rs`), the credential command (`src/state/config.rs`, which reads its output) and Omarchy's own tools (`src/omarchy/shell.rs`: the plugin validator, the shell's ping and rescan, bar placement and plugin disable).
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

The `fix` is the first candidate, and with `--json` `error.alternatives` holds one runnable argv for each of the others. `-t all` names every client, so a command that changes one client (`install`, `remove`) asks which; take the fix or an alternative.

### `no_compatible_release` or `wrong_flavor`

The addon has no build this client loads, or its publisher labels its files for other clients only. The message says what decided the best-ranked file: a folder and the interfaces it lists, the retail floor, or the evidence ("release.json lists Foo.zip for mainline only", "its file name … names mainline only"). With `--json`, `error.details.tried` lists each asset and why it was rejected. Check the catalog's families (`omaddons info <name>`), try `--channel beta`, or pin a release with `--version`. WeakAuras, for example, currently ships no retail build.

### `game_running`

The client is running, and one of these applies:

- the change would remove a folder it loads, or an update would drop or rename the TOC it loads;
- it is a legacy client, where the `safe` policy refuses every change (on 3.3.5a, every change but a replacement that keeps the loaded TOC);
- `running_policy` is `refuse`;
- you ran `undo --saved-variables restore`, `backup restore`, or an `undo` of a restore.

Quit the game and run the command again. For folder changes you can also add `--while-running`, but SavedVariables are never written while the game runs, whatever the flags: a `WTF/` restore is refused even when omaddons only cannot tell whether the game runs (the process table did not read), and `details.reason` is then `saved_variables` too.

### `filesystem_unsupported` on a restore

A restore replaces `WTF/` in one atomic exchange or not at all. `details.reason` says why it cannot here: `WTF/` is a mount point of its own (`wtf_is_mount_point`), `Interface/.omaddons/` is on another mount than the client (`stage_cross_mount`), or the filesystem cannot exchange two directories (`no_rename_exchange`; NTFS through ntfs-3g and some FUSE filesystems). Nothing was copied. With the game closed, copy what you need by hand from the backup's folder, `details.backup_dir`.

### `unsafe_archive`, `bad_archive` or `archive_too_large` on a backup import

`backup import` accepts only a file exactly as `backup export` wrote it, and `details.reason` says what differs (see [`backup import`](#backup-import)). A `manifest_mismatch` or `truncated` file was usually damaged or cut short in transit: copy it again and compare the export's `sha256` (in its JSON output) with the copy's. An archive edited by hand, or made by another tool, is refused by design; export the backup again instead. Nothing was kept.

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

### `undo_conflict` after a repair (`changed_since_repair`)

Something changed what the repair touched since it ran: the files in `details.paths`, a backup the undo would restore from, or a later transaction on the same paths (`details.conflicting`). Undoing the repair now could overwrite that change, so nothing moved. The repaired state stays, and the repair's backups stay in its doctor run directory. To see what changed since and in which order:

```bash
omaddons history
```

When a later transaction is the cause, the fix names it: undo it first, then the repair.

### `undo_conflict` after a set-aside (`changed_since_suspension`)

Putting a set-aside journal back would let the next command replay its instructions against a client that has changed since: its record generation, its identity marker, or the folders and `WTF/` the journal would recover. `details.changed_guards` says which. The journal stays where the set-aside put it, in the doctor run's quarantine. `doctor explain` describes what it held:

```bash
omaddons doctor explain <failure mode> -t <target>
```

For a journal a newer omaddons wrote, undo the set-aside with that newer build instead: it reads the journal it puts back, and recovers it on its next change.

The set stays readable in quarantine until `doctor gc`: the journal, its in-flight lock and its state snapshot under `<state>/omaddons/doctor/runs/<run>/quarantine/state/`, and its stage and client snapshot under `<client>/Interface/.omaddons/snapshots/<repair tx>.repair/quarantine/`, each at its path relative to where it was. The set-aside's `report.json` and `actions.jsonl` in that run directory list every move. Copy back by hand what you need; omaddons never replays it once its guards no longer hold.

### `locked` (exit 5)

Another omaddons process held the lock for more than 5 seconds. `error.details.holder` names its pid and command. It is retryable: run the command again. A second `apply` of one plan waits 30 seconds for the first instead, and its `locked` names the plan (`details.lease`).

### `hash_mismatch` on a cached archive

An archive in the download cache (`~/.cache/omaddons/blobs/<sha256>`) no longer hashes to its name: the disk or another program changed its bytes. omaddons never installs it. The error names it (`details.blob`, with `details.actual`) and changed nothing; `omaddons doctor --include-deep` finds every damaged archive. It is raised when the archive could not be downloaded again: the command ran offline, or the source no longer serves that release. Online, the same command downloads it again into the same place and goes on; nothing has to be deleted first.

```bash
omaddons install <addon> --reinstall -t <id>      # the same command, online
omaddons doctor --include-deep                    # find every damaged archive
omaddons doctor fix --only fm-caches-blob-hash-mismatch   # quarantine them
```

A cached archive that cannot be read fails the same way with `io`, naming its path.

### `plan_stale` after `apply`

`apply` refuses a plan that can no longer apply; `error.details.reason` says why, and the fix previews the plan's command again:

- `already_applied`: it ran. `details.txs` names its transactions; `omaddons history` shows them, with the plan in `plan`.
- `expired`: plans last 24 hours.
- `binary_changed`: another omaddons build made it; preview it again with this one.
- `input_missing`: a local archive, folder or `sync` file it installs from is gone (`details.path`).
- No `reason`, with `details.changed`: the world changed since the preview (a newer release, another change on the client, an edited folder), so the plan no longer describes what would happen.

### The doctor refuses without `--force --yes`

Some repairs are forced: they change how an interrupted change ends, or set aside something you may still want. `doctor fix` lists them as `needs --force --yes` and never runs one by default; the bar widget never runs one at all. `omaddons doctor explain <fm>` says what the repair does and why it needs consent. To run it, name it and consent: `omaddons doctor fix --only <fm> --force --yes`, ideally after `omaddons doctor fix --only <fm> --force --yes --dry-run`. `--force` without `--yes` is a usage error whose fix is the preview: no fix ever adds `--yes` for you.

### My agent lost the output of `apply`

Run `omaddons plans show <plan_id>` (the id the preview returned, or its first 8 characters) before anything else. `completed` means every approved client committed, and `committed` lists each transaction with its undo; `partial` names the clients that committed and those that did not, and `apply` will not resume it, so preview the command again for the rest; `pending` means nothing committed yet, and `apply <plan_id>` still runs it. `unknown` says why history cannot tell. It reads history only, so addon versions that happen to match never count as applied.

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

### No Lua errors are captured

`omaddons errors` reads what BugGrabber saved, so for a client without it, `errors` says "no Lua errors are captured: BugGrabber is not installed" and offers `omaddons install buggrabber -t <id>`, which installs that client's own build. 4.3.4 and 5.4.8 clients have no keyless build (`errors` says so). With BugGrabber installed, errors reach the disk only when the game saves its SavedVariables, at `/reload` or logout: a session still running shows nothing new yet.

### The bar icon is dimmed

The widget has nothing it can show. Open its panel to see which of these it is:

- **"omaddons not found at …":** the binary the widget was installed for moved or was removed. The panel shows the two commands that fix it: `omaddons omarchy install` from the binary you use now, or `omarchy bar set a44.omaddons binary <path>`.
- **"omaddons gave no answer the widget can read":** the binary ran but printed no envelope; the panel shows its last lines of output, or why it stopped (a timeout, say).
- **"No World of Warcraft clients found":** see [A client is missing from `status`](#a-client-is-missing-from-status).
- **The widget is from another build** (below).

### The widget says it is from another build

The binary changed since the widget was installed (an update, a rebuild), or the widget's files differ from what this binary would install. `status` says so too (`widget_outdated`). The widget stays read-only rather than run command lines another build may read differently: its panel keeps only Retry and Reinstall widget. Reinstall it with **Reinstall widget**, or run `omaddons omarchy install` from the binary you use. `omaddons doctor` reports it as `fm-plugins-widget-version-drift` and never reinstalls it itself: a reinstall runs Omarchy's own tools in your desktop session and cannot be undone.

### `omaddons doctor` says the widget fails validation

`fm-plugins-widget-invalid` means the installed widget's folder breaks a rule of Omarchy's plugin validator, as omaddons' port of it checks: usually a hand edit of its `manifest.json`, a symlink put inside it, or an Omarchy update that changed its rules after the widget was installed. Omarchy's shell may then refuse to load it. The finding's `evidence.rules` names each broken rule by its code (`default_section`, `symlink`, `entry_point_missing`, `manifest_too_large`, …) and never quotes the manifest; `omaddons omarchy status` shows each problem with its message. From the desktop session, `omaddons omarchy install` replaces every file that differs from this build's render, which clears it. When the widget was installed under other `HOME` or XDG directories, the finding gives no command: it serves that other omaddons state, so reinstall it with the binary and directories it was installed under.

### `omarchy plugin remove` says the widget is not installed

`omarchy plugin remove a44.omaddons` answers "plugin 'a44.omaddons' is not installed" because it looks for a folder named after the id, and the widget's is `omaddons`. Use `omaddons omarchy remove`, which takes it off the bar, deletes it and records the removal (see [`omarchy remove`](#omarchy-remove)).

### `journal_pending` that does not clear

Journals are recovered by the next command that changes something: install, update, sync, pin, unpin, `adopt` when it adopts, copy, remove and undo (also when they change nothing), `config set`, `targets add`, `rm`, `rename` and `default`, `auth github set` and `clear`, `export --write`, and `omarchy install` and `remove` when they publish or remove the widget. `omaddons doctor fix` finishes one too, with the same recovery and without a change of its own: `doctor fix --dry-run` lists each recovery it would run (`delegated_planned`), the run reports each outcome (`delegated[]`, a `delegated_recovery` warning per finished transaction), and `omaddons undo <tx>` reverts one it rolled forward. Dry runs and read commands, the `adopt` report included, never recover. A command whose own recovery failed (its error carries `details.journal_pending`) keeps the journal and its staged folders for exactly that. If recovery finds a step that matches neither its before nor its after state (`step_unrecognized`), or the journal's client no longer carries its target's marker (`marker_mismatch`), it stops and leaves the journal for manual repair. So it does with a journal it cannot read: one a newer omaddons started (`journal_too_new`: finish it with that version) or one holding lines this omaddons cannot decode (`journal_unreadable`). It leaves such a journal whole, with its in-flight lock, stage and snapshots. `status` and `omaddons doctor` name the state by its id (`fm-concurrency_primitives-journal-too-new`, `-unreadable`, `-unexpected-state`, `-step-unrecognized`, `-marker-mismatch` or `-unattributed`), and `omaddons doctor explain <id> -t <client>`, which the failing command's own fix names (and the `journal_pending` warning other clients' commands give), shows its evidence: the set a set-aside would move, how many folders it holds, whether one may be the only copy, and whether everything it owns could be told without trusting its lines (`attribution`). After reading it, the doctor's set-aside unblocks the client: preview it with `omaddons doctor fix --only <id> -t <client> --force --yes --dry-run`, then run it without `--dry-run`. It moves the journal, its in-flight lock, its stage and both snapshot halves into the doctor run's quarantine, each on its own filesystem, and deletes nothing; anything it lists as not accounted for stays where it is. `omaddons doctor undo <run>` puts the set back exactly, but only while nothing it depends on has changed (see [`undo_conflict` after a set-aside](#undo_conflict-after-a-set-aside-changed_since_suspension)). For a journal a newer omaddons started, finishing it with that build is still the safer way. Every later change on that client then fails until it is fixed (`error.details.journal_pending` names the transaction, `error.details.reason` why); other clients are not affected and keep reporting it as `journal_pending`. A failure recovery cannot attribute to one client affects them all: a journal that does not say which client it is for, an in-flight lock that cannot be probed, or a journal directory that cannot be listed fails every change on every client (`unattributed`), and the error's code and message say which; its fix is `omaddons doctor explain fm-concurrency_primitives-journal-unattributed`, or `omaddons doctor explain fm-schemas-state-corrupt` when the journal directory itself cannot be listed. A doctor repair (`<tx>.repair.jsonl`) names no single client, so one recovery stops for (`step_unrecognized`, `marker_mismatch`, `skill_dir`, `journal_too_new` or `journal_unreadable`) is isolated by what it protects instead (see [Transactions](#transactions)). Every command reports it once, in one of two texts:

- `repair transaction <tx> did not finish, and recovery could not settle it (<reason>); writes to what it protects are refused, and everything else proceeds`: a change that would write a file the repair wrote, or a backup its rollback needs, fails with that journal's error (`details.tx`, `details.resource` naming what it protects, `details.target: null`); every other change goes through.
- `repair transaction <tx> did not finish, and this omaddons cannot read what it changed (<reason>); every change is refused until it is set aside`: its first line does not decode, or a newer omaddons wrote it, so what it protects cannot be known, and every change fails with its error. Read commands still run.

Either way, run `omaddons doctor explain <id>` (the warning's fix names it) for its evidence. The same forced set-aside suspends a stuck repair's journal; the repair's own run directory, with the backups its rollback reads, stays where it is.

A journal behind such a repair (one a set-aside or its undo moves, or any journal at all behind a repair whose first line cannot be read) is never recovered ahead of it: `transaction <tx> on target <id> waits behind repair <tx>, which recovery could not settle (<reason>); it is left as it is`. A change on that client fails with the repair's own error, whose fix is the repair's `doctor explain`: settle the repair, and the journal behind it is recovered by the next change.

An interrupted `WTF/` restore adds three cases. While its game runs, it waits (`game_running`): quit the game and run any change. With `preservation_incomplete`, recovery could not finish giving what the restore displaced a home as a `recovered` backup (an I/O error or a full disk, say): the trees are under `Interface/.omaddons/stage/<tx>.wtf/` (in `recovered/<id>/` once moved there), and the next change resumes with the same backup id once the cause is gone. With `unexpected_state`, something the restore moved is no longer where it recorded it; everything was left exactly as found.

---

## Limitations and known issues

### What omaddons does not do (yet)

| Capability | State | Planned |
|---|---|---|
| Batched GitHub checks (GraphQL, one request for many repositories) | ❌ one `releases/latest` per repository instead | M5 |
| Completing client ids and addon names in the shell | ❌ the scripts are static ([Shell completions](#shell-completions)) | Launch-week fixes |
| `doctor health`, `doctor ls`, `doctor capabilities`, `doctor robot-docs`, `doctor --robot-triage`, and doctor checks beyond today's 31 failure modes | ❌ `doctor --quick` runs the cheap checks, the runs are folders under `~/.local/state/omaddons/doctor/runs/`, and `capabilities` and `robot-docs` describe the doctor too | Dropped from M4; may return if real use asks for them |
| CurseForge, Wago | ❌ | M5, optional, bring your own key |
| Packages, man page | ❌ build from source | M5 |
| Windows, macOS | ❌ | Never (non-goal) |
| A GUI, TUI, daemon or MCP server | ❌ | Not in v1 (non-goal) |
| Editing WeakAuras strings, ElvUI profiles or SavedVariables | ❌ | Never (non-goal) |

### Known limitations

- Discovery scans only the direct children of `~/Games` as prefixes (a nested prefix is found through a launcher config or while its client runs). A relative Lutris `exe` is resolved against the game's `working_dir` or `prefix`, where Lutris itself uses the game directory in its `pga.db`, which omaddons does not read.
- Drive-letter mapping applies to clients that Battle.net's `product.db` names. A custom client on another drive letter needs `targets add` or `extra_roots`.
- Custom clients between the five legacy versions (2.0–2.3, 3.0–3.2, 4.0–4.2, 5.0–5.3) get family `unknown`, which matches only their exact major.minor.
- On unknown custom clients, the `safe` running policy behaves as for legacy clients and refuses every change while they run.
- Do not run a pre-M3 omaddons build to change state that M3 has written. Reading with it (`status`, `list`, `history`) is safe, also while an M3 restore waits to be recovered. This is what the last M2 build (`af10e34`) does, as `tests/compat_m2.rs` observes it:
  - It skips every M3 history line (backups, restores, dismissals) with `history_lines_skipped`. Its `undo` of an install older than a restore, or a bare `undo` (it picks the newest line it can read), puts that install's SavedVariables back over the restored ones with no conflict reported. Its source also shows that a newer record refuses it only at its save, after its folders moved.
  - It recovers an interrupted M3 install, or undo of an install, as its own (the two folder shapes `tests/compat_m2.rs` crashes; an update and a remove go through the same engine but are not tested this way). It never sees an interrupted restore, `--addon` restore or undo of either (their journals, stages and snapshots are named `<tx>.wtf`, see [Transactions](#transactions)), and leaves `WTF/` and both snapshot halves alone. Each of those shapes was crashed at each of its steps: after its journal's first line, around each journaled rename, and after `swapped`, the record and the history line. With everything it left two hours old, the M2 build changed something on the same client, and the next M3 change recovered the transaction. `WTF/` came back as it was before the crashed command, rolled back, or rolled forward and then undone. Before `<tx>.wtf`, the M2 build deleted those journals unread, and an hour later its orphan sweep deleted the files a restore had replaced, from its stage or its snapshot.

  From M3 on, every build refuses what it cannot read (see [Transactions](#transactions)).
- Likewise, use the M3 build (`c0ab5e9`) only to read state that M4 has written. `tests/compat_m3.rs`, a checkpoint test, runs it against every kind of file M4 writes and still needs (repair journals and their client halves, quarantines, doctor run directories, stored plans, set-aside journals); its results will be quoted here when the milestone's checkpoint runs.
- The download cache reuses an asset whose SHA-256 is known: from the source (GitHub REST assets carry a digest) or remembered from an earlier download of that release's asset in the release memo. A release's assets are taken to be immutable, so a publisher re-uploading different bytes under the same tag and file name is not noticed while the old blob is cached (unless the source declares a different size or hash). Within one command, the same asset is still downloaded once per target that tries it. An unchanged release is never downloaded. A cached archive is re-hashed before each use (about 10 ms per 5 MB), once per command.
- A `url:` ref's release is its URL, so new content published at the same URL is never picked up.
- Lua errors are only as fresh as BugGrabber's last save, at `/reload` or logout.
- `errors` was validated on fixtures that follow BugGrabber's published structure in both writer formats; it has not yet been run against a capture from a real play session.
- `backup export`, `backup import` and `backup verify` are CLI only; the bar widget backs up, restores and prunes.
- `backup import` refuses an archive holding a symlink that leads outside `WTF/` (`symlink_escape`), so a `WTF/` with such a link backs up and restores here, but its export cannot be imported.
- "Not while the game runs" is a check, not a lock on the game: a client launched in the milliseconds before a restore's exchange may write its in-memory settings over the restored files at logout. The restore's undo keeps the tree it replaced (see [Transactions](#transactions)).
- WowAce projects that need packaging steps (externals, ignored or moved folders, build-time keywords such as `--@debug@` blocks, nolib builds) are refused with `packaging_unsupported`, never installed half-built; download a packaged release in a browser instead. A WowAce tree is fetched one request at a time.
- Private GitHub repositories and HTTP proxies are not supported.
- Shell completion is static: commands, subcommands, aliases, flags and fixed values complete; client ids and addon names do not (clap's dynamic completion is unstable, and completing them naively would run discovery on every Tab press). Completing them is planned among the launch-week fixes. A script is as current as the build that generated it: generate it again after upgrading.
- Restores need atomic renames: a whole restore exchanges `WTF/`, and `--addon` exchanges each file in place. On a filesystem without `RENAME_EXCHANGE` (NTFS through ntfs-3g, many FUSE and network filesystems), or with SavedVariables on another mount than `Interface/.omaddons/`, both are refused (`filesystem_unsupported`). The workaround: with the game closed, copy what you need by hand from the backup's own folder, `Interface/.omaddons/backups/<id>/WTF/`, a plain directory (`details.backup_dir` names it).
- Two clients sharing one AddOns directory (an AddOns symlinked between Classic clients) keep separate records: a folder one records is `shared` on the other, which cannot record it too, so manage those addons from one client. Records made on both before this check stay doubled: `status` warns `target_addons_shared`, and a change through either warns the same. The doctor has no check for them.
- A file whose `release.json` or name labels it for other clients only is refused even when its TOCs would load, unless the addon's own folder carries a TOC named for the client (see [archive grading](#how-omaddons-decides-what-loads)). A multi-client plain `Folder.toc` under such metadata is refused. [`--any-flavor`](#a-build-not-made-for-this-client---any-flavor) installs it anyway, on the client it is given for.
- A [look-back](#a-release-per-client-github-look-back) sees only what `releases.atom` holds, the 10 newest releases; the feed does not page. When those are all prereleases it probes the newest stable tag's siblings, but only as `<version><suffix>`, with the suffixes the feed's tags use. A repository that names per-client tags any other way (`mists-v3.218`), or whose feed shows no tag with a client's suffix, is not reached for that client. VuhDo (`gh:vuhdo-io/VuhDo`) is reached this way: observed 2026-10-24, its latest stable release is `v3.218-catac` and its 100 newest releases are v3.219 alphas and betas, and it installs `v3.218-mopc`, `-tbcc`, `-vanilla` and `v3.218` on Mists Classic, Anniversary, Classic Era and retail.
- The legacy running behavior was observed only on 3.3.5a: a new addon needs a restart, and a loaded addon's TOC is re-read on `/reload`. So only 3.3.5a takes replacements while it runs; the other legacy clients refuse every change until they are observed. Running detection was observed under umu and GE-Proton for retail, Classic Era, Anniversary and 3.3.5a (Mists Classic's process has the same shape). The Battle.net app itself runs as `CrBrowserMain`, so with no client open the process route relies on Battle.net's agent (`Agent.exe`, run from `ProgramData/Battle.net/Agent/`). Under umu the agent was observed (2026-10-21, the app alone open) carrying the prefix's `WINEPREFIX` (its `pfx` path, a link to the prefix root), so the route holds.
- Loading was observed in game, not only judged from TOCs: retail, Mists Classic, Classic Era, Anniversary, WoW: Forever (the 2026-10-21 stress test) and 3.3.5a each wrote the SavedVariables of the addons omaddons installed. An addon below retail 12's interface floor (OmniCC 11.2.8, interface 110207, on retail 12.1.5) did not load, as the floor predicts; omaddons now refuses to install such a build. A modern client adds newly installed addons to `AddOns.txt` only when it creates the file, so `AddOns.txt` is no load evidence. Where a modern client stores "Load out of date AddOns" is not located. The Lutris, Heroic and Steam readers follow their upstream source (Lutris v0.5.22, Heroic v2.22.4, and for closed-source Steam the readers of Lutris, protontricks and ValvePython `vdf`), cited in each reader; the Bottles layout was verified against Bottles 67.4, and `product.db` against a real Agent database and two upstream readers.

### Known issues

The README describes the intended behaviour. A place where the code at HEAD does not match it yet is listed here with its bead (`br show <id>`), and the README changes only if a fix decides otherwise.

- **A forget that no longer applies is reported as `not_managed`** (`omad-z60g`). When every folder of an addon was deleted by hand, `remove` forgets it. If one of those folders is put back in the instant between that plan and the commit lock, the run changes nothing, which is right. It then fails `not_managed`, "Foo is no longer managed in retail", although Foo is still managed and its folder is back. Run `remove` again to remove the folder. A run bound to its preview is refused correctly, as `plan_stale` with `selection`.
- **A recovery that stops at a journal no client owns forgets what it had already recovered** (`omad-ef89`). Settings commands (`config set`, `targets` changes, `auth github set`, `export --write`, `errors dismiss`), and a change command with nothing to commit, recover interrupted journals first. When such a recovery rolls some journals back or forward and then meets one that names no client, the result holds only the `journal_pending` warning for the stuck one: the `journal_recovered` warnings of the ones it finished are lost, and the widget's doorbell does not ring, so the bar catches up only at its next refresh.
- **`search` picks a client for its suggestion without knowing compatibility** (`omad-launch-week-fixes-bmcx.6`). When a result's hint is `unknown` on every selected client (every GitHub result is), the `-t` in its suggested `omaddons install` command comes from the tie-breaks alone (the default target, then a live client, the client with the most managed addons, retail), not from anything known about the addon. The install itself still verifies every TOC.
- **`doctor -t all` leaves out a client whose directory is gone** (`omad-qh12`). [`-t all`](#choosing-a-target-with--t) should select every client, as no `-t` does, but the doctor's commands resolve it to the discovered clients only, so a registered client whose directory is gone, and its findings (`fm-userland_state-target-vanished`, its journals), are missing from `doctor -t all`, `doctor explain … -t all` and `doctor fix -t all`. Plain `doctor` and `status -t all` report them.

---

## Roadmap

| Milestone | Scope | State |
|---|---|---|
| **M1: working slice** | Discovery, ClientRules and the TOC engine, fsx and the transaction engine with undo, keyless GitHub/git/url/local sources, the core CLI contract | ✅ Done (2026-10-15) |
| Follow-up: Classic and Forever gaps | Multi-flavor archives, never-launched clients, game modes, real-machine tests for every live client, the catalog × flavor probe, and the in-game load check on the real Forever client | ✅ Done (2026-10-21) |
| **M2: updates and adoption** | `outdated`, `update`, `pin`/`unpin`; WoWInterface and Tukui; changelogs; dependencies; drift detection; tiered `adopt`; `addons.toml` with `export`, `sync` and `copy`; `search`; `auth github` | ✅ Done (2026-10-16) |
| Post-M2 audit | An independent audit of M1 and M2 (engine failure paths, contract holes, integrity and privacy statements, discovery and rules fields, test blind spots, this README) and real-world checks on real clients: running detection under Proton, in-game loading on every client, legacy `/reload` behaviour | ✅ Done (2026-10-21) |
| **M3: the widget** | The Omarchy bar widget (QML, embedded in the binary, managed by `omaddons omarchy install`, `status` and `remove`), `errors` (BugGrabber, installed keyless from a new `wowace:` source), `backup` of `WTF/` (list, create, verify, restore, export and import, automatic backups) | ✅ Done (2026-10-23) |
| **M4: the agent surface** | Shipped: `capabilities`; `robot-docs guide`, `schema`, `errors` and `install-skill`; `completions`; intent inference; stored plans (`--dry-run`, then `apply`), `plans show` and `--if-match`; the doctor (`doctor`, `explain`, `fix`, `diff`, `undo`, `gc`, `--online`); bare help's discovery line and `help --json`'s `discovery`; `status` on the doctor's registry; the pre-freeze contract audit; verbosity levels; the contract's golden; the checkpoint's run on the real machine; and **the first tag, `v0.1.0`, which froze the contract at 1.0**. Dropped from M4 when it was re-scoped on 2026-11-04, and may return if real use asks for them: `doctor health`, `ls`, `capabilities`, `robot-docs` and `--robot-triage`, and further detector batches (more state-file, config, userland, permission and external-artifact checks) | ✅ Done (2026-11-08) |
| Omarchy widget overhaul | A user-first overhaul of the bar widget: a UX audit and design system first, then every view rebuilt (Updates, Installed, an addon page, Search, Lua errors, Issues, Backups, History, Settings, first run), a readable plan sheet for every confirmation, a `?` key reference and mouse support (`omad-widget-user-first-1n2p`) | 🗓️ Planned |
| Launch-week fixes | The [Known issues](#known-issues), a `--help` overhaul to the standard of `br` and `bv`, dynamic completion of client ids and addon names, and an installer for the completion scripts (`omad-launch-week-fixes-bmcx`) | 🗓️ Planned |
| **M5: accounts and release** | Optional CurseForge and Wago (bring your own credential, never cached), fingerprint adoption; release engineering: versioning, CHANGELOG, a local PKGBUILD, packaged completions and a man page. Not yet broken down into tasks | 🗓️ Planned |

The authoritative design is the pinned vision bead. Its body is revision 2, and comments added after it supersede it.

```bash
br show omad-vision-8qa      # the vision: thesis, decisions D1–D34, milestones
br ready                     # work that is ready now
```

---

## Development

### The local gate

There is no CI (decision D16). Every commit that changes code (`*.rs`, `Cargo.*`, `rust-toolchain.toml`, `deny.toml`, lint config, the catalog, the bar widget under `omarchy/`, the agent guide (`src/contract/guide/*.md`, `skills/omaddons/SKILL.md`), test fixtures and snapshots) passes the gate first:

```bash
argc gate          # cargo fmt --check, clippy --all-targets -D warnings, cargo test, the widget
                   # pipeline, ubs on changed files (Cargo.toml and Cargo.lock aside), cargo deny
                   # when Cargo.toml/Cargo.lock/deny.toml changed
argc gate --all    # also run cargo deny, and the widget's QML step, unconditionally
argc widget-test   # the widget pipeline alone, QML step included
argc widget-snapshots   # every widget state rendered offscreen into target/widget-snapshots/
```

`argc gate` runs `scripts/gate.sh`, which can be run directly. It needs `cargo`, `git`, [`ubs`](https://github.com/Dicklesworthstone/ultimate_bug_scanner), `cargo-deny` and [`bun`](https://bun.sh), and Qt 6's `qmltestrunner` (from `qt6-declarative`) when the QML step runs. Commits that touch only `.beads/`, README.md or CLAUDE.md may skip it.

`ubs` scans the changed files that still exist when its step runs, never insta's working files (`*.snap.new`). When none of them is in a language UBS scans (a change of snapshots, JSON fixtures or Markdown only), UBS exits 3, "nothing scanned": the gate prints `ubs: no file in a language UBS scans (exit 3: nothing scanned, nothing to fix)` and goes on to "gate passed". Any other non-zero exit (1, a critical finding; 2, a scan that did not finish) fails the gate. `UBS_ALLOW_NO_SCAN` is never set.

The widget pipeline (`scripts/widget-test.sh`) runs four ordered steps over a fresh temporary directory: the real binary writes the envelopes the widget reads, `bun` tests `omarchy/Model.js` over them, the binary's own argument parser checks every command line bun built, and, when anything under `omarchy/` changed, `qmltestrunner` runs the QML tests over a copy of the plugin. The first three run on every gate, because a renamed flag or a changed envelope key breaks the widget as surely as a QML edit. Nothing generated is committed.

`argc widget-snapshots` renders every state a player can meet as a PNG in `target/widget-snapshots/` (`--out` names another folder): the bar button's states, and the panel in each phase, each view loading, empty, short and long, the confirmations, the restore chooser, a change in progress, its result and a failure. `omarchy/tests/qml/tst_snapshots.qml` drives the real `Service.qml` on the mock process with envelopes the binary wrote, edited where a state needs it (150 addons, a long name). Where Omarchy is installed it draws with Omarchy's own shell kit and the current theme (`omarchy/tests/qml/kit/` holds the few stand-ins that needs); `--theme <name>` draws with another installed theme instead (`--theme rose-pine`), and `--stub-kit` with the test stubs. The gate's QML step runs the same suite with the stubs, as a smoke test that every render is written. The renders are for review, never diffed.

Other tasks: `argc fmt`, `argc lint`, `argc test [filter]`, `argc e2e [filter]` (the end-to-end suites with their logs turned up, below), `argc fuzz <target>`, `argc fuzz-all` and `argc bench` (the last three are checkpoint tasks, below).

### The agent guide

`skills/omaddons/SKILL.md` is generated: it is what `omaddons robot-docs guide` prints. Edit `src/contract/guide/guide.md` for its prose, or the registries for its tables, then run `OMADDONS_UPDATE_SKILL=1 cargo test --test integration skill::` and commit both. Never edit or merge the file by hand; after a rebase, regenerate it. `.rumdl.toml` keeps the Markdown formatter from putting a blank line between the frontmatter and the stamp in either file.

### Tests

The gate runs about 4,500 Rust tests, unit tests in `src/` and integration tests in about 130 suites under `tests/`. Every suite (`tests/<suite>.rs`, and `tests/contract/`) is a module of one test binary, `integration` (`tests/main.rs`), and its tests are named `<suite>::…`, so `cargo test --test integration restore::` runs one suite. A binary per suite linked the whole library about 130 times, and a one-line change wrote about 16 GB to the disk; one binary links it once. For the same reason debug and test builds carry line tables only, enough for file and line in panics and backtraces, and dependencies no debuginfo (`[profile.dev]`; `CARGO_PROFILE_DEV_DEBUG=true` for a debugger). About 98 more are `#[ignore]`d and run only at checkpoints or by hand: live probes, real-machine and real-client checks, time budgets and the bench homes (`tests/perf.rs`), the M2 build against M3 state (`tests/compat_m2.rs`) and the M3 build against M4 state (`tests/compat_m3.rs`), fixture recorders, the fuzz-seed writer and child-process helpers. (With milestone 3's features in there were 2,279, at the close of the post-M2 audit 1,645, and at the M2 checkpoint 1,333.) The widget adds 188 `bun` tests and the QML tests (183 test functions in `omarchy/tests/qml/`, 50 of them `tst_snapshots.qml`'s renders), run by the gate's widget pipeline. `cargo test -- --list` counts today's; the exact counts are recorded when the milestone closes.

| Suite | What it covers |
|---|---|
| `tests/contract/` | A matrix of success and failure envelopes across the commands: key order, exit codes, registered codes, suggested argvs that parse and round-trip, usage fixes that never name another changing command; schema validation of every envelope, `details` and stderr event, and every envelope snapshot; snapshots; the intent corpus, whose fixes run on a synthetic home and whose wrong invocations change nothing; the semantic manifest's report (`cargo test --test integration contract::manifest_report -- --nocapture` prints its section counts and, run before and after a change, that change's semantic diff); the semantic manifest compared with the committed golden and, from the first tag, with the released baseline (`contract::wire_names`) |
| `tests/e2e_manifest.rs` | The semantic manifest against the live binary: every flag and positional `help --json` reports is an input node with the same facts, and every key path of real envelopes, data, error details and stderr events is located in its outputs |
| `tests/engine_crash.rs` | All 35 engine failpoints, every journaled rename (atomic and two-rename), and crashes during recovery, armed as real `abort`s in a child process over SavedVariables; undo included; torn journal, history and actions lines; whole-`WTF/` restores, single addons' restores (`--addon`) and their undos run by the engine in a child of the test binary, with injected I/O failures, a game started mid-restore, the preservation of displaced trees and files (crashed and failed at each of its steps) and bind-mounted topologies |
| `tests/barrier_crash.rs` | The activation barrier (E14): a doctor set-aside of a crashed update and its undo (the reactivation), and the undo of a set-aside of two (one per client, an activation each), each committed in a child of the test binary and killed at every primitive boundary and repair failpoint, then recovered by three lock-taking commands; their recovery crashed at every inverse and repeated, or failed on I/O; a set-aside recovery cannot settle, an unreadable repair, and a repair waiting behind an unsettled folder journal. Recovery takes them in its three passes, never runs a restored journal ahead of the repair that exposed it, and leaves every state wholly before or wholly after, as E11's differential predicts |
| `tests/engine_witness.rs` | The adversarial matrix of the approval under the commit lock (debug builds): every guarded rename of four swap shapes refused at its journaled step and rolled back cleanly, bound and unbound; the residual window after a step's line, stopped for repair with nothing lost; a bound update crashed at every engine failpoint and at every rename, each recovered; a refused rename's rollback crashed twice, then finished; an injected I/O error at a rename; an unreadable or oversized witnessed folder under the lock |
| `tests/engine_concurrency.rs` | Parallel runs, duplicate installs, races between plan and commit (each run stopped at a named step), no lock while planning and staging, and the lock hold of commits and undo, measured (16 MiB of SavedVariables and a 64 MiB folder included); an install committing while a restore stages, a capture paused mid-copy blocking no change, two automatic captures making one backup, prune finding a paused export and a paused restore busy, a verify racing a prune of its backup |
| `tests/compat_m2.rs` | The last M2 build (`af10e34`, built once into `target/compat/`) against state M3 wrote: committed and interrupted restores, whole and `--addon`, then its `undo`, `install` (at once and two hours on), `status`, `list` and `history`; every M3 journal shape interrupted after `prepared`, each of which it must recover or never see; and every transaction that puts settings back crashed at each of its steps, then the M2 build two hours on, after which M3 must recover it with its undo data whole; checkpoint only, with a debug build of the current binary |
| `tests/perf.rs` | Time budgets (release build, checkpoint only) and the request-economy checks that run in the gate; the under-lock approval check's cost on the realistic home (3 clients, 30 records, 5,040 managed files, built offline by `common::realistic`), reported in release, with that home and the check's `-v` note proven in the gate; and the `capabilities` budget (under 20 ms) |
| `tests/apply.rs` | Stored previews (admission, quotas, concurrency, crashes); and bound runs checked again under the commit lock: every kind of hand edit, made before the run, before its lock and under it, refused as `observed` with nothing moved; re-judged operations that differ (`selection`); an unchanged world committing exactly its preview, as an unbound twin does; the guarantee's scope (an edit after the check committed and undoable, a folder replaced at the last moment rolled back); and `apply` end to end: every storable command's plan run and compared with a direct run, its refusals and their reasons, a write-free dry run, a plan revalidated inside the memo window; and, for every storable command, a concurrent change between a preview and `apply` (refused as `plan_stale`, naming what moved, with nothing changed) and a stale bound no-op (once a direct run did the plan's work, the old token's typed run and `apply` of the old plan are both refused); and the plan lifecycle (dry run under held locks, expiry, another build, hand edits, the second `apply`) for every storable command, missing inputs and a vanished working directory; and two appliers of one plan (one runs, the other reports `already_applied`), the 30 s lease timeout, interference between an `apply`'s targets reported per target, and a held lease that no sweep deletes; and release landings between preview and `apply` for every source (GitHub, git, WoWInterface, Tukui, WowAce) and for `install`, `update`, `sync` and `copy`, the memo refresh, and a Tukui plan applied from the cache |
| `tests/apply_crash.rs` | `apply` under crashes at every commit failpoint, recovery crashed and repeated, a killed lease holder, the reload after the lease, and I/O failures before execution: each leaves the plan applied exactly once (folders, record and one history line carrying the plan) or not at all, and a second `apply` says which |
| `tests/discover_*.rs` | The discovery matrix, launchers, scanning, custom clients, the cache |
| `tests/catalog.rs` | The 46 GitHub and 5 `git:` catalog entries replayed offline from recorded HTTP fixtures with no REST calls, and the 9 WoWInterface entries in one `filedetails` request with their MD5s (the two Tukui entries: `tests/sources_tukui.rs`) |
| `tests/sources_wowace.rs` | The WowAce source over a recording of the live server (BugGrabber's tag list, v12.1.0, v12.1.1, trunk at r130 and r152, and the 1.12-era tree at r43) and edits of it: pinned-only requests, a poisoned live tag path never asked for, reproducible archives, re-tags against unrelated commits, `sync --locked` at the recorded revision, every packaging refusal, collisions and the tree bounds |
| `tests/single_writer.rs` | Only `src/fsx/` writes to the filesystem, and only the three named modules start processes |
| `tests/determinism.rs` | No `HashMap` or `HashSet` in a type that derives `Serialize` under `src/app`, `src/output`, `src/contract` or `src/doctor` (a scan of the production source; a hash map prints in a different order on each run) |
| `tests/backup_*.rs`, `tests/restore.rs`, `tests/restore_addon.rs`, `tests/restore_undo.rs`, `tests/doorbell.rs` | `WTF/` backups: `create` over a fixture `WTF/` (exact copies, owner-only modes, capture consistency with the tree changed mid-copy through `pause` failpoints, no lock while copying, privacy on both streams), `list`, `prune`, `export`, `import` (every refusal) and `verify`, and automatic backups; whole and `--addon` restores and their undos; the doorbell's ring sites (directory locks: unit tests in `src/fsx/lock.rs`) |
| `tests/errors*.rs`, `tests/status_errors.rs`, `tests/lua.rs` | Lua errors: both SavedVariables writer formats, attribution, privacy on both streams, dismissals bound to their read, and `status`'s counts with and without `--cached` |
| `tests/preconditions.rs`, `tests/fail_closed.rs` | Previews bound by `--if-match` (`plan_stale` for each change between preview and run), and state a build cannot read refused, never acted on |
| `tests/preconditions_matrix.rs` | The `--if-match` binding's adversarial matrix (debug builds): concurrent commits at every pause before the lock, crashes at every engine step with repeated recovery and the re-run rule, injected I/O errors at the swap, two runs holding one token, a stale bound run with nothing to do recovering nothing, an unreadable folder, and a paused bound dry run holding no lock |
| `tests/omarchy.rs`, `tests/omarchy_widget.rs`, `omarchy/tests/` | The bar widget: `omarchy install`, `status` and `remove` over stub Omarchy tools (never this machine's shell), `status`'s widget block; and the widget pipeline: envelopes from the real binary, `bun` over `Model.js` and the source's security scans, the binary's parser over every argv `bun` built, `qmltestrunner` over the QML; and `omarchy/tests/contract-reads.test.js`, which records every envelope key a `Model.js` reader touches (and every literal `env.…` read in the QML) and fails on one the published schemas do not declare, so a renamed key cannot silently read as `undefined` |
| `tests/help_pages.rs` | Every command path's `-h` and `--help` pages (and bare `omaddons`), rendered at 100 columns without color and snapshotted one file per path; `help <path>` equal to `<path> --help` for every path; no line over 100 columns (80 on bare help), no ESC byte and no developer note (a bead or decision id, `TODO`, a source path, `unwrap`) on a page or in a command's definition, each check with a shrink-only allowlist (`tests/help_pages_allowlist.txt`, empty today); a path without an accepted snapshot, or a snapshot no path renders, fails naming it |
| `tests/completions.rs` | `omaddons completions`: each shell's script snapshot-tested; `bash -n` and bash's own completion function over the whole command tree in every gate; the scripts static, the same bytes on every terminal, and pure (no state read, no write, no request); `zsh -n` and `fish --no-execute` at checkpoints |
| `tests/capabilities.rs` | `capabilities`: the round trip of every command, flag, value, alias, conflict and requirement it lists, against the binary's own parser, and every command's help and pointer run through the binary; completeness against every registry; purity (no request, no write, hashed trees, nothing created on a fresh home); broken configurations and an unusable `HOME`; effective values with their source; credentials detected without being read or run; the human summary; and each check proven to catch a planted fault |
| `tests/robot_docs.rs` | `robot-docs schema` and `robot-docs errors` end to end: every path, the envelope, the events and the files listed and equal to the library's schemas; one path's entry and every path's own schema command; unknown paths' fixes run; the catalog equal to the registries; the published schemas validating live output, strictly too; byte-identical repeats in other environments; no reads, writes or requests, and no `HOME` needed; `robot-docs guide` the same bytes on every machine (a broken or overriding config, a managed or relative home, any terminal width, verbosity, directory or `-t`), equal to bare `robot-docs` and to its JSON content, its help paths and usage fixes |
| `tests/skill.rs` | The committed skill is the guide, byte for byte; its frontmatter and stamp; the compiled digest is its stamp; the regeneration's fixed point; every command the guide names parses |
| `tests/doctor.rs`, `tests/doctor_view.rs`, `tests/doctor_concurrency.rs`, `tests/doctor_caches.rs`, `tests/doctor_plugins_secrets.rs`, `tests/blobs.rs` | The doctor's runtime (run directories, its envelope and exit dictionary, budgets, isolation, `--since` and `explain`), its tolerant view over synthetic homes, the journal and lock detectors with `status`'s journal titles, the damaged-archive detector and its quarantine, the widget and credential failure modes with the chmod repair, and a damaged cached archive never staged |
| `tests/doctor_round_trip.rs` | Each production fixer (the credential chmod, the blob quarantine, the delegation, the set-aside) on its own fixture through the binary: fixed, a second fix that does nothing, `doctor undo` putting the subject back byte for byte with its modes and inodes (the delegation's rollback stands, M4-D37) with nothing outside the doctor's own record changed, and a fix aborted at its first step settled by the next recovering command and retried |
| `tests/doctor_repair.rs`, `tests/doctor_repair_fences.rs` | `doctor fix` end to end with test-only fixers (every terminal state, the dry run's purity, a preview bound across processes, concurrent fixers, the force and disabled gates, declared scopes and privacy), and a stuck repair journal refusing only the writes that overlap what it protects |
| `tests/plans_show.rs`, `tests/plans_show_commands.rs` | `plans show` reconciling a plan from history: a lost output, a partial or interrupted `apply`, a wiped plan store, history past its bound, matching versions that prove nothing, and every storable command's applied plan |
| `tests/e2e_misplaced_clients.rs`, `tests/e2e_target_typos.rs` | Intent inference: a client in the wrong place or a mistyped selector changes nothing, and the fix it gets runs |
| `tests/fsx_lock.rs` | The global lock and the in-flight locks |
| `tests/tlog_selftest.rs` | The test log itself: both files and every field, `net.log` and failpoint capture, redaction, truncation, tree differences, the failure output and the live echo (in child processes), and the e2e marker convention |
| `tests/e2e_slice2.rs` | Slice 2's exit demo, as tests: install, then `update --dry-run`, then `apply`; a release landing in between (`plan_stale`, and the re-preview shows it); a second `apply` (`already_applied`); a Tukui plan applied from the cache; a late hand edit caught under the lock; two appliers of one plan; `plans show` after a lost output; the widget's three Update scenarios held to their names and to the fixtures the binary writes |

Recorded HTTP fixtures live in `tests/fixtures/http/<scenario>/`. Debug builds replay them with `OMADDONS_HTTP_FIXTURES=<dir>` and re-record them with `OMADDONS_RECORD=1`. Recording never stores request headers, drops `set-cookie`, and strips secret query parameters from URLs and `location` headers.

### Test logs and `argc e2e`

Every M4 integration and end-to-end test that runs more than one command logs through `TestLog` (`tests/common/tlog.rs`), so a scenario that fails at its fourteenth command can be read from the thirteen before it. Each test writes two files under `target/tmp/test-logs/<suite>/<test>/`:

- `steps.jsonl`: one JSON record per line, written as it happens: the steps and notes, every command with its argv, the variables it set, its working directory, failpoints and exit, its envelope, its stderr events and the `net.log` lines it added, `pause` failpoints reached and resumed, tree hashes and the paths that differ, and each case of a table-driven test (in `cases/<case>/`);
- `transcript.md`: the same records, readable, envelopes pretty-printed.

A rerun replaces a test's directory, `cargo clean` removes them all, and nothing is committed. A failing test prints `test log: <path>` and its last 20 records in cargo's failure output, after the panic message. `OMADDONS_TEST_LOG=verbose` also echoes every record on stderr as it happens, without `--nocapture`; unset, the log writes its files only, and any other value fails the test.

```bash
argc e2e                         # every end-to-end suite, logs echoed live
argc e2e slice1                  # the suites whose name contains slice1
argc e2e all --test-threads=1    # every suite, with arguments for the test binary
argc e2e slice2 demo_a_second    # in each matching suite, the tests whose name starts with demo_a_second
```

`argc e2e [filter] [args…]` runs every suite whose module doc has a `//! e2e suite: <purpose>` line (a test name among the arguments is looked up in each selected suite, `<suite>::<name>`), with `OMADDONS_TEST_LOG=verbose`, then prints how many logs it wrote, which ones panicked or have no end record (a killed test), and the log root (`==> test logs: target/tmp/test-logs`). It exits with cargo's status, and a filter that matches no suite exits 2 naming them. It runs the same tests as the gate's `cargo test`, and deletes nothing.

The conventions:

- a test that runs more than one command logs through `TestLog` (`let log = tlog!();`), and its suite carries the `//! e2e suite:` line (`tests/tlog_selftest.rs` checks both ways);
- each phase of a scenario is a `log.step`;
- a state assertion goes through `log.tree`, `log.assert_unchanged` or `log.diff`, or `log.assert_eq`, or logs what it compares with `log.json` first;
- unit tests are table-driven, with a name per case and messages that show the input and the output;
- older suites adopt the log when a change rewrites them;
- real-machine tests and live probes never use it;
- bead comments quote the log root as `target/tmp/test-logs`.

Privacy: the log holds only synthetic homes, and refuses to hash a tree outside them. Variables whose names contain `TOKEN`, `SECRET` or `CREDENTIAL` are recorded as `<redacted>`, and their values (and any a test registers with `log.secret`) are scrubbed from every later record. Stdin and the test process's own environment are never recorded, and paths appear as `<root>`, `<target-tmp>` and `<repo>`.

### Milestone checkpoints (not part of the gate)

```bash
argc fuzz-all --time 600                                   # every fuzz target, 10 minutes each
argc bench                                                 # hyperfine medians on a synthetic home (needs hyperfine, jq)
cargo test --release --test integration perf:: -- --ignored --skip bench_home --skip live_home --test-threads=1   # the time budgets
cargo test --test integration compat_m2:: -- --ignored --nocapture   # the M2 build against M3 state (prints its table)
cargo test --test integration compat_m3:: -- --ignored --nocapture   # the M3 build (c0ab5e9) against M4 state (prints its table)
OMADDONS_LIVE=1 cargo test --test integration live_probes:: -- --ignored   # the real endpoints
OMADDONS_REAL_MACHINE=1 OMADDONS_LIVE=1 OMADDONS_ALLOW_SKIP=keyless_installs_forever \
  cargo test --test integration real_machine:: -- --ignored --test-threads=1   # this machine's clients; every addon change is undone
OMADDONS_ALLOW_SKIP=zsh_accepts_its_script,fish_accepts_its_script \
  cargo test --test integration completions:: -- --ignored   # zsh -n and fish --no-execute over the generated scripts
OMADDONS_ADVANCE_RELEASED_BASELINE=1.0 cargo test --test integration contract::wire_names   # the release step: writes tests/contract/released/1.0.json and flips the golden's header (the tag commit)
```

The release step refuses unless `output::SCHEMA_VERSION` is that version, the committed golden matches the code, the baseline does not exist yet, and the change since the previous baseline carries its bump; it is run once per release, in the tag commit.

Where zsh or fish is not installed, its check prints `SKIP` and the variable accepts it: the M4 plan pre-accepts exactly these two skips. bash's check runs in every gate and is never skipped.

A checkpoint test that cannot run (a variable unset, its client running or not installed on this machine, the Forever stress-test set still installed) prints `SKIP <test>: <reason>` on stderr and fails, so a skip never counts as a pass. `OMADDONS_ALLOW_SKIP=<test>[,<test>…]` accepts the named skips, which still print their line; the real-machine command above accepts `keyless_installs_forever` for as long as the stress-test set stays installed. The checkpoint's close reason quotes every `SKIP` line.

- **Benchmarks.** `argc bench` (`scripts/bench.sh`) builds a home of three synthetic clients with 30 addons and 5,040 managed files, one release behind, and times `status --cached`, `status` (the drift check), `outdated` cold and warm, `update --all --dry-run` warm and `update --all` cold. Each line names its binary: the release binary for what needs no network, and for cold runs a `bench-fixtures` build (release with debug assertions), because release builds never replay fixtures. Warm runs must add no `net.log` line, and a missed target exits 1. The budgets in `tests/perf.rs` check the same targets in-process.
- **Live probes.** They include `catalog_matches_declared_flavors`, which builds a synthetic client for every live product and every legacy version and dry-runs each catalog addon on each. An addon must install exactly where its publisher declares it and nowhere else, with zero REST calls. The probe prints the addon × client matrix, and publishers whose `release.json` disagrees with their own TOCs are listed in it with a date.
- **Real-machine tests.** They run against the real home and undo every addon change they make. Target registrations and `target.json` markers written on the first change stay, by design. One update is previewed and applied through its stored plan (`apply_one_update_and_undo`), then undone; the plan file the preview stores stays in `<cache>/plans/` until the store sweeps it after 24 hours, like target registrations and markers, by design. When no client has an update, the test makes one: an older Dominos tag on a modern client, else an older GroupBulletinBoard commit on a 3.3.5a client, each installed pinned and then unpinned. A test whose client is not installed on this machine (a missing target: its directory gone, or Battle.net uninstalled it) skips by name before it changes anything; it is never moved to another client. `status_reports_every_client` asserts each uninstalled client as reported missing.
- **Fuzzing.** There are 21 cargo-fuzz targets: `archive`, `atom_feed`, `backup_tar`, `bbcode_text`, `bottles`, `bpsv`, `desired_toml`, `heroic`, `html_text`, `journal`, `lutris`, `product_db`, `ref_parse`, `savedvariables`, `steam_shortcuts`, `steam_vdf`, `toc`, `tukui_addons`, `wowace_listing`, `wowi_details` and `wowi_filelist`. Each has committed seeds under `fuzz/seeds/`. They need `cargo-fuzz` and a nightly toolchain at least as new as the crate's `rust-version` (pick one with `--toolchain`). A crash input becomes a regression unit test in the owning parser.

### Work tracking

Work is tracked with beads (`br`), and agents follow `AGENTS.md`. `br ready` shows unblocked work, and `br show <id>` shows one bead.

---

## FAQ

### Why "omaddons"?

It is built alongside [Omarchy](https://omarchy.org), the Arch-based desktop that installs Battle.net through umu and Proton into `~/Games/battlenet`, and `omaddons omarchy install` puts its widget on Omarchy's bar. The CLI does not depend on Omarchy and works on any Linux desktop.

### Will it touch the addons I already have?

No. Folders it did not install are reported as unmanaged and left alone. Replacing one requires `--force`, and even that is undoable for as long as the snapshot is kept (at least 24 hours; `snapshot_only_copy` says when a snapshot holds the only copy). `adopt` brings existing addons under management as they are, without touching their files.

### Is it safe to use while WoW is running?

Yes, by default. Updates that keep the loaded TOC land and tell you to `/reload`. A new addon lands too and tells you to restart the game, since no client was observed loading a new addon folder on `/reload`. Removing a loaded addon is refused until the game exits, and you run it again then. SavedVariables are never written while the game runs. Legacy private-server clients get no changes while running, except 3.3.5a updates that keep the addon's loaded TOC, which need a `/reload`. See [Running clients](#running-clients).

### How do I update my addons?

Run `omaddons outdated` to see what has updates on every client, then `omaddons update` to install them (or `omaddons update <addon>` for some). Each client gets its own transaction, and `omaddons undo` reverts the whole update. `url:` refs are one exception: their release is the URL, so install a new URL instead. Pinned addons (`omaddons pin <addon>`, or installed with `--version`) are the other: they stay put until `omaddons unpin <addon>`.

### How do I find which addon throws Lua errors?

Install [BugGrabber](https://www.wowace.com/projects/bug-grabber) (BugSack shows them in game), play, and run `omaddons errors` after the game exits. It lists each addon's errors on every client, the likely source with how sure it is, which are new, and whether an update may fix them. `omaddons errors --check` exits 6 when something is new.

### Why is an error's message hidden?

An addon can write anything into its own error messages, a party member's name included, and nothing can reliably tell a name from other text. So `errors` shows a message only when it is one of Lua's or the game's own, whose variable parts are identifiers from code; any other shows its length. `omaddons errors --raw-details` shows them all, with your own account, realm and character names and every player link replaced, but it cannot catch every name: share its output with care.

### Does it work with Turtle WoW, Warmane or other private servers?

Yes. Any client from 1.12.1, 2.4.3, 3.3.5a, 4.3.4 or 5.4.8 with an `Interface/` folder is a supported target, identified by its executable. Its label comes from `realmlist.wtf`. Server-specific patches are not special-cased: Turtle WoW is a `vanilla-legacy` client, whether its executable reports 1.12.1 or its own 1.17 or 1.18. Give it a name with `targets add <path> --name turtle` or `targets rename`.

### Can I use CurseForge addons?

Not through CurseForge yet; optional CurseForge support with your own key is planned for milestone 5. CurseForge's terms make keys non-transferable and forbid caching, so omaddons will never bundle a key or proxy. Many CurseForge addons also publish on GitHub (`gh:Owner/Repo`). You can also download a zip in the browser and install it as a local path.

### Does it phone home?

No. There is no telemetry. Only the hosts on the allowlist are contacted, every request that reaches the network is in `omaddons net-log`, and `--offline` makes network access impossible.

### Where does undo data live, and how long is it kept?

Old folders and SavedVariables copies go to `<client>/Interface/.omaddons/snapshots/`, on the same filesystem as the addons, and state-file copies go to `~/.local/state/omaddons/snapshots/`. Per target, everything younger than 24 hours, the latest change, and whatever a deferred SavedVariables restore still needs are always kept. Beyond those, at most the newest 10 changes within 7 days are kept, capped at 2 GiB. Pruning runs after every change on that client, and once a day after a change or a backup on any client, so a client you only play is pruned too (see [Retention](#retention)).

### Will automatic backups fill my disk?

No. By default (`auto_backup = "daily"`) they are made only by cloning. On btrfs and xfs a fresh backup shares every byte with `WTF/`, and grows only as the game rewrites files afterwards. Elsewhere none is made at all. `backup list` shows each backup's apparent size, which on those filesystems overstates what it takes, next to the free space, which is what counts. Before each automatic backup, omaddons checks that `limits.backup_min_free_mb` (2 GiB) stays free, pruning old automatic backups first and skipping with `backup_space_low` otherwise. Retention keeps the automatic backups of the newest 7 days (see [Retention](#retention)). Your own backups are never deleted unless you ask. `daily-copy` makes byte copies where cloning fails, within `limits.backup_copy_max_mb` (4 GiB) per client.

### Where are my backups, and how much space do they use?

In each client's `Interface/.omaddons/backups/`, one folder per backup, readable only by you. `omaddons backup list` shows them with each one's apparent size and the space free. On btrfs and xfs a backup starts out sharing its data with the live `WTF/`, so the apparent size overstates what it takes; the free space is what counts. A backup is also a plain `WTF/` copy you can open in that folder.

### Does omaddons read my SavedVariables?

Only BugGrabber's (`BugGrabberDB`), read-only, for `omaddons errors`. Everything else it copies as files without reading what they say: into undo snapshots (hashed, so an undo restores exactly those files) and backups (hashed with `--verify`). It writes them only when you undo a change or restore a backup, and never while the game runs.

### Can I get just one addon's settings back?

Yes: `omaddons backup restore <id> --addon <name> -t <client>` puts back only that addon's SavedVariables from a backup, account-wide and for every character, and leaves the rest of `WTF/` as it is. Preview it with `--dry-run`, which also says which version of the addon wrote those settings. It is undoable, and the widget's restore chooser offers it first.

### Can a coding agent drive it?

Yes, by design. Use `--json`, read `error.fix_argv` and `commands[].argv`, pass `--actor agent --reason "…"`, and keep `undo --last-by agent` as the escape hatch. An agent is also detected automatically: without `--actor`, a run whose stdin is not a terminal is recorded as `agent`. See [For coding agents](#for-coding-agents).

### Why did `apply` refuse?

Because running the plan would no longer do what its preview showed: it already ran (`already_applied`), its 24 hours passed (`expired`), another omaddons build made it (`binary_changed`), a local file it installs from is gone (`input_missing`), or the world changed since (`details.changed`). Nothing was changed. Preview again: the error's fix is the plan's command with `--dry-run`. See [`plan_stale` after `apply`](#plan_stale-after-apply).

### Why did the widget ask me about an update?

For one of three reasons. The plan does more than the row showed: each line in the dialog is one effect from the CLI's approval summary (a new dependency, a folder removed, files you changed being replaced, another source or channel, a change while the game runs). The release changed since the row was read: the dialog opens with "Changed since you looked" and what moved. Or the view never showed which release it was, as for an install from Search. A routine update that is exactly what the row showed applies with no dialog (see [`omarchy`](#omarchy)).

### Why does `omarchy plugin remove a44.omaddons` not work?

Omarchy's own remove looks for a plugin folder named after the plugin's id, and the widget's folder is `omaddons` (its id is `a44.omaddons`). The widget is omaddons' to manage: `omaddons omarchy remove` takes it off the bar, deletes it and records the removal.

### Why no CI?

The repository is private and developed by its owner with coding agents. A local gate that must pass before every code commit replaces CI until the project is opened (decision D16).

### Does it support Windows or macOS?

No, by design. It relies on Linux specifics: `renameat2`, `/proc`, Wine prefix layouts and reflinks.

---

## For coding agents

The full agent guide is `omaddons robot-docs guide`, the same text as `skills/omaddons/SKILL.md`. An agent with no prior knowledge needs three calls: `omaddons capabilities --json` (every command, flag, code, limit and source, offline), `omaddons robot-docs guide` and `omaddons status --json`. `omaddons robot-docs install-skill` installs the guide as a Claude Code skill (or, with `--dir`, any agent's skill folder). The block below is a condensed reference.

```text
omaddons: WoW addon manager for Linux. One JSON envelope per command, typed errors with fixes as argv.

Core loop
  omaddons capabilities --json                   # what this build can do (offline, writes nothing)
  omaddons status --json                         # every client (target), running state, attention
  omaddons install <ref>... -t <target> --dry-run --json
  omaddons install <ref>... -t <target> --json --actor agent --reason "<why>"
  omaddons apply <data.plan_id> --json --actor agent   # or: exactly the preview a person saw
  omaddons plans show <plan_id> --json   # lost an apply's output? did it commit? (offline, changes nothing)
  omaddons outdated --json                       # what has updates, on every client
  omaddons update --all --dry-run --json         # then without --dry-run
  omaddons undo --last-by agent --json           # the escape hatch
  omaddons search <words> --json                 # find a ref; install verifies it
  omaddons backup create --json --actor agent --reason "<why>"   # copy every live client's WTF/
  omaddons backup create -t <target> --verify --json            # also hash every file copied
  omaddons backup list --json                    # WTF/ backups and what each proves
  omaddons backup restore <id> -t <target> --addon <name> --dry-run --json   # one addon's settings,
  omaddons backup restore <id> -t <target> --addon <name> --if-match <token> --json   # then bound
  omaddons backup verify <id> -t <target> --json # check a backup against its hashes, or record them
  omaddons errors --json                         # Lua errors: attribution {method, confidence}, new
                                                 # counts; a string-time error before --since is
                                                 # counted as uncertain, never dropped
  omaddons doctor --json                         # what is broken: findings with evidence and
                                                 # remediation.argv (changes nothing but its run)
  omaddons doctor explain <fm-id | instance> --json   # one failure mode in full (changes nothing)
  omaddons doctor fix --dry-run --json           # the repair and its token; then --if-match <token>

Refs     catalog name | gh:Owner/Repo | Owner/Repo | wowi:<id> | tukui:<slug> | wowace:<slug>
         | git:https://…#rev | url:https://… | a pasted GitHub, WoWInterface, Tukui or WowAce link
         | ./zip-or-folder
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
  doctor    0 healthy · 1 findings · 2 fix partial · 3 fix rolled back · 4 refused · 5 locked
            6 needs --online · 64 usage · 66 not found · 73 run directory not created
            74 failed (robot-docs errors: doctor_exit per code)

Rules
  - --dry-run first for anything destructive; --force only after reading the refusal.
  - Prefer apply <plan_id> for an approval a person saw: a plan_stale from apply means preview
    again, and already_applied means it ran. A dry run that stored a plan lists that apply
    first in commands[] (as destructive as the plan's command).
  - If an apply's result is unknown, run plans show before you preview again or change anything.
  - --any-flavor (offered among a refusal's alternatives) only when the user asks for that addon
    on that client; it persists until --no-any-flavor.
  - Bind the real remove, undo, backup restore or backup prune to that preview, and errors dismiss
    to the read it acknowledges: --if-match <data.precondition> (errors: dismiss.precondition).
    A plan_stale answer (not retryable) means the world changed since: preview again.
  - A forced repair (doctor fix --only <fm> --force --yes) only when the user consents to it, after
    reading its doctor explain; never add --yes on your own.
  - Restores and SavedVariables writes are refused while the game runs; never work around it.
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
