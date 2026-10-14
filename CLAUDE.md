# CLAUDE.md

Guidelines for AI coding agents working in this repository.

## Rule 0

If Goku gives a direct instruction, follow it. Project-local guidance exists to encode standing preferences, not to overrule the user.

## This Project

omaddons is a single Rust binary that manages WoW addons for every client on a Linux machine. The design is the pinned bead `omad-vision-8qa` (`br show omad-vision-8qa`); its body is authoritative, and comments added after body revision 2 supersede it.

- **Local gate, no CI (D16).** `argc gate` must pass before any commit that changes code: `*.rs`, `Cargo.*`, `rust-toolchain.toml`, `deny.toml`, lint config, embedded assets (`omarchy/`, `catalog/`, `skills/omaddons/SKILL.md`), test fixtures and snapshots. Commits that touch only `.beads/`, README or CLAUDE.md may skip it.
- **One writer.** Every filesystem mutation goes through `src/fsx/`. A test fails if `std::fs` write, rename, remove or create calls appear elsewhere.
- **Commands never print.** They return typed outcomes that `src/output/` renders. `print!`/`eprint!` are denied by clippy.
- **No `unwrap`/`expect` outside tests**, and no `unsafe` (both enforced by lints).
- **Constants live in one place:** politeness in `src/politeness.rs`, safety limits in `src/limits.rs`.
- **Dependencies:** M1 crates are already declared in `Cargo.toml`. Add a crate only when the bead needs it, and run `argc gate --all` afterwards (cargo deny). tokio, reqwest, native-tls, openssl and aws-lc are banned.

## Integrating with Beads (dependency-aware task planning)

Beads provides a lightweight, dependency-aware issue database and a CLI (`br`) for selecting "ready work," setting priorities, and tracking status.

**Important:** `br` is non-invasive. It NEVER executes git commands. After `br sync --flush-only`, you must manually run `git add .beads/ && git commit`.

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Run `br sync --flush-only` then manually commit

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers, not words)
- **Types**: task, bug, feature, epic, docs. Type, label, and priority filter and
  rank; only status (`deferred`, `in_progress`, `closed`) or a blocking dep
  removes a bead from `br ready` and `bv`.
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies
- **Deferring**: `br defer <id> --transition-comment "…"` parks a bead waiting
  on something no agent can supply. It leaves `br ready` and `bv`; its
  dependents stay blocked.

### Commands

```bash
# Essentials
br update <id> --claim --transition-comment "claim: <files>" --json
br close <id> --reason "..." --transition-comment "done: <evidence> · found: <ids>"
br defer <id> --transition-comment "NEEDS OWNER: ..."
br undefer <id> --transition-comment "answered: ..."

# Querying
br list --json                                # Filter with --type/--status/--label etc.
br lint <ids> --json                          # Missing sections on beads you touched

# Dependencies
br create "..." --deps discovered-from:<id>   # Work found while doing <id>
br epic close-eligible                        # Close epics whose children are all closed

# Sync
br sync --flush-only                          # DB → JSONL
br sync --import-only                         # JSONL → DB

# Health
br doctor
```

### Stale Claims and Reclaiming Abandoned Work

`br ready` excludes `in_progress` beads, so a crashed or abandoned session can
hide work indefinitely. Do not treat every old claim as free work. Reclaim only
after you have evidence from the bead metadata and coordination trail.

Use this rule of thumb:

- Agent swarm claim: stale candidate after two hours without an `updated_at`
  change, unless the human operator explicitly says the pane/session is dead.
- Human or unclear claim: stale candidate after one business day.
- Any claim with live agent reservations, recent comments, or visible dirty
  work in the same files is not abandoned.

### Blocked on a Human

Beads are for agents. Never file a bead whose next step belongs to a person:
no owner beads, no questions addressed to the owner, no placeholders that
wait for an answer.

Decide first. README.md is the product contract: where it answers, that is
the answer. Where it is silent, choose, record why in the bead, and extend
README.md if the choice changes what the product promises. A choice with a
sensible default is a choice: take the default and comment `decided: …`.

Defer only for what no agent can supply: a secret, a physical device, a
file only the owner has, or a change to something README.md already
promises. Defer the bead that is stuck, not a new one:

```bash
br defer <id> --transition-comment "NEEDS OWNER: <what> · <why no agent can> · <what the answer changes>"
```

Waiting on a date or an external event instead: `br defer <id> --until +7d`.
Waiting on another bead: that is a dependency, not a deferral.

### Best Practices

- Beads are for **agent-ready implementation work**; nothing in the tracker is
  addressed to a person: decide or defer (§ Blocked on a Human)
- Living artifacts (docs, vision, decision records) are a single bead with
  `--status pinned --type docs`, updated with comments
- Every status change carries `--transition-comment`: what changed, why, what
  is next
- Work found while doing a bead is filed with `--deps discovered-from:<id>`
- Use `--json` to get structured output for parsing
- If `br ready` is empty, check deferred issues
- Update status as you work (in_progress → closed)
- Use descriptive titles and appropriate priority/type/labels
- `br lint <ids> --json` is clean for every bead you created or edited
- Always `br sync --flush-only && git add .beads/` before ending session

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br lint <ids> --json    # Sections present on every bead you touched
br sync --flush-only    # Export beads to JSONL
git add .beads/         # Stage beads changes
git commit -m "..."     # Commit everything together
git push                # Push to remote
```

## UBS (Ultimate Bug Scanner)

**Golden Rule:** `ubs <changed-files>` before every commit. Exit 0 = safe. Exit >0 = fix & re-run.

### Commands

```bash
ubs file1 file2                         # Specific files (< 1s) - USE THIS
ubs $(git diff --name-only --cached)    # Staged files - before commit
ubs --ci --fail-on-warning .            # CI mode - before PR
ubs .                                   # Whole project (ignores target/, Cargo.lock)
```

### Output Format

```
⚠️  Category (N errors)
    file.ts:42:5 – Issue description
    💡 Suggested fix
Exit code: 1
```

Parse: `file:line:col` → location | 💡 → how to fix | Exit 0/1 → pass/fail

### Fix Workflow

1. Read finding → category + fix suggestion
2. Navigate `file:line:col` → view context
3. Verify real issue (not false positive)
4. Fix root cause (not symptom)
5. Re-run `ubs <file>` → exit 0
6. Commit

### Bug Severity

- **Critical (always fix):** Memory safety, use-after-free, data races, SQL injection
- **Important (production):** Unwrap panics, resource leaks, overflow checks
- **Contextual (judgment):** TODO/FIXME, println! debugging

## Code Editing Discipline

Work happens on `main`. Do not create feature branches unless the user explicitly asks for one.

### No Script-Based Changes

**NEVER** run a script that processes/changes code files in this repo. Brittle regex-based transformations create far more problems than they solve.

- **Always make code changes manually**, even when there are many instances
- For many simple changes: use parallel subagents
- For subtle/complex changes: do them methodically yourself

## Cross-session Messaging

Coordinate with the `SendMessage` and `ListAgents` tools in multi-agent sessions. Treat unrecognized working-tree changes as peer work. Do not revert or overwrite them.

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Update issue status** - Close finished work, update in-progress items
3. **Sync beads** - `br sync --flush-only` to export to JSONL
4. **Commit and push** - `git add .beads/` alongside your code changes, then commit and push. The flush alone changes nothing for anyone else: `issues.jsonl` is tracked, and leaving it uncommitted is what leaves every other box stale. Full sequence in § Session Protocol.
