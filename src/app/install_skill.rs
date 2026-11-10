//! `omaddons robot-docs install-skill [--dir <DIR>]` (M4-D8): the agent guide, byte for
//! byte as `robot-docs guide` prints it, written as `SKILL.md` where coding agents load
//! skills: `$HOME/.claude/skills/omaddons` by default, or the folder `--dir` names.
//!
//! It writes through fsx as `Writer::Skill`, into that one folder only (the scope
//! `ScopeTable::with_skill_dir` builds), never through a symlink, keeps a hand-edited copy
//! as the first free one of `SKILL.md.bak`, `SKILL.md.bak.1`, … (never replacing an
//! earlier one), and records a `skill` settings line, which undo never reverts. It
//! neither stores a plan nor binds (M4-D22): the bytes come from the binary, the
//! destination is the argument, and the one outside input, the file already there, is
//! read again and decided again under the lock. There is no remove command: deleting the
//! folder removes the skill.

use std::io::Write;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::Context;
use crate::contract::actions::Action;
use crate::contract::guide::render::OTHER_SKILL_ROOTS;
use crate::contract::guide::skill_file::{SkillFile, read_skill_file};
use crate::contract::guide::stamp::inspect;
use crate::engine::fence;
use crate::engine::journal::{no_saved_variables, recover_under};
use crate::error::{CommandHint, Error, ErrorCode, Result, Suggestion};
use crate::fsx::{
    Content, GlobalLock, Op, RenameMode, WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::model::TxKind;
use crate::output::{Outcome, Style, thousands};
use crate::state::history::{self, SettingEntry};
use crate::state::paths::Paths;
use crate::state::registry::{Registry, load_registry};
use crate::state::store;

/// The envelope `command`.
pub const INSTALL_SKILL_COMMAND: &str = "robot-docs install-skill";

/// The file a skill folder holds.
const SKILL_FILE: &str = "SKILL.md";

/// Where a hand-edited `SKILL.md` is kept when the guide replaces it: this name, else this
/// name with `.1`, `.2`, … (omad-syc2).
const EDITED_COPY: &str = "SKILL.md.bak";

/// The `n`th name an edited copy may take in `dir`: `SKILL.md.bak`, then
/// `SKILL.md.bak.1`, `SKILL.md.bak.2`, …
fn backup_name(dir: &Path, n: usize) -> PathBuf {
    match n {
        0 => dir.join(EDITED_COPY),
        n => dir.join(format!("{EDITED_COPY}.{n}")),
    }
}

/// Where an edited `SKILL.md` is kept (omad-syc2). A run never replaces an earlier backup:
/// the user's own edits are not regenerable, unlike `addons.toml.bak`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Keep {
    /// Kept (under `--dry-run`, would be kept) as this new backup.
    New(PathBuf),
    /// A backup already holds exactly its bytes: no copy is made.
    Already(PathBuf),
}

/// Where `file`'s edited copy goes in `dir`: a backup (`SKILL.md.bak[.N]`, a regular
/// file within the read bound) that already holds exactly its bytes, else the first free
/// backup name. Reads at most one bounded file per name, never through a link.
fn keep_slot(dir: &Path, file: &Path) -> Keep {
    let edited = match read_skill_file(file) {
        Ok(SkillFile::Bytes(bytes)) => Some(bytes),
        _ => None,
    };
    // Some name among the first `entries + 1` is free.
    let entries = std::fs::read_dir(dir).map_or(0, Iterator::count);
    let mut free = None;
    for n in 0..=entries {
        let name = backup_name(dir, n);
        if std::fs::symlink_metadata(&name).is_err() {
            free.get_or_insert(name);
            continue;
        }
        if let (Some(edited), Ok(SkillFile::Bytes(kept))) = (&edited, read_skill_file(&name))
            && kept == *edited
        {
            return Keep::Already(name);
        }
    }
    Keep::New(free.unwrap_or_else(|| backup_name(dir, entries + 1)))
}

/// `robot-docs install-skill`'s long help.
pub const INSTALL_SKILL_HELP: &str = "\
Examples:
  omaddons robot-docs install-skill              for Claude Code
  omaddons robot-docs install-skill --dry-run    what it would write
  omaddons robot-docs install-skill --dir $HOME/.codex/skills/omaddons

Writes the agent guide, the same bytes as omaddons robot-docs guide, as SKILL.md in
the skill folder. The default needs $HOME/.claude and creates skills/omaddons in it; a
--dir folder is created when its parent exists. A SKILL.md you edited is kept as
SKILL.md.bak, or SKILL.md.bak.1, .2 and so on: an earlier backup is never replaced. A
SKILL.md that is a symlink is refused; a symlinked folder is followed and judged where it
points. It never writes into a game client, a Wine prefix or omaddons' own folders.
omaddons doctor reports an installed skill that holds another guide than this build's.
Remove the skill by deleting the folder.";

/// `robot-docs install-skill`'s arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct InstallSkillArgs {
    /// The skill folder to write SKILL.md into
    ///
    /// Default: $HOME/.claude/skills/omaddons. It is created when its parent exists.
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

/// What a run wrote, or under `--dry-run` would write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Written {
    /// There was no `SKILL.md`.
    Fresh,
    /// Another `SKILL.md` was replaced (an edited one kept as `SKILL.md.bak[.N]`).
    Replaced,
    /// It already held these bytes: nothing was written.
    Unchanged,
}

/// `robot-docs install-skill`'s data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InstallSkillData {
    pub dry_run: bool,
    /// The skill folder, canonical.
    pub dir: PathBuf,
    /// `<dir>/SKILL.md`.
    pub file: PathBuf,
    /// What the run wrote, or under `--dry-run` would write.
    pub written: Written,
    /// The guide's length in bytes.
    pub bytes: u64,
    /// The version of the omaddons that wrote it.
    pub tool_version: &'static str,
}

/// `robot-docs install-skill`'s outcome.
#[derive(Debug)]
pub struct InstallSkillOutcome {
    data: InstallSkillData,
    commands: Vec<CommandHint>,
    /// The replaced file was not a guide's: where it was kept (or would be).
    kept: Option<Keep>,
}

impl Outcome for InstallSkillOutcome {
    type Data = InstallSkillData;

    fn command(&self) -> &'static str {
        INSTALL_SKILL_COMMAND
    }

    fn data(&self) -> &InstallSkillData {
        &self.data
    }

    fn commands(&self) -> &[CommandHint] {
        &self.commands
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        for line in self.human_lines() {
            writeln!(w, "{line}")?;
        }
        Ok(())
    }
}

impl InstallSkillOutcome {
    /// The human output, one line per fact.
    fn human_lines(&self) -> Vec<String> {
        let d = &self.data;
        let file = d.file.display();
        let size = format!("({} bytes)", thousands(d.bytes));
        let mut lines = vec![match (d.dry_run, d.written) {
            (_, Written::Unchanged) => format!("the omaddons skill is up to date: {file}"),
            (false, Written::Fresh) => format!("installed the omaddons skill: {file} {size}"),
            (false, Written::Replaced) => format!("updated the omaddons skill: {file} {size}"),
            (true, Written::Fresh) => format!("would install the omaddons skill: {file} {size}"),
            (true, Written::Replaced) => format!("would update the omaddons skill: {file} {size}"),
        }];
        match (&self.kept, d.dry_run) {
            (Some(Keep::New(copy)), false) => {
                lines.push(format!("  your edited copy is kept as {}", copy.display()));
            }
            (Some(Keep::New(copy)), true) => {
                lines.push(format!(
                    "  would keep your edited copy as {}",
                    copy.display()
                ));
            }
            (Some(Keep::Already(copy)), _) => {
                lines.push(format!(
                    "  your edited copy is already kept as {}",
                    copy.display()
                ));
            }
            (None, _) => {}
        }
        for hint in &self.commands {
            lines.push(format!("other agents: {}", hint.command()));
        }
        lines
    }
}

/// Where the skill goes, before anything is canonicalized.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Destination {
    dir: PathBuf,
    /// The existing folder the run may create `dir` from.
    create_from: PathBuf,
    /// No `--dir`: Claude Code's skill folder.
    default: bool,
}

/// What the run does with what is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Unchanged,
    Fresh,
    /// `keep_edited`: the file is not some guide's own bytes, so it is kept first.
    Replaced {
        keep_edited: bool,
    },
}

impl Decision {
    fn written(self) -> Written {
        match self {
            Self::Unchanged => Written::Unchanged,
            Self::Fresh => Written::Fresh,
            Self::Replaced { .. } => Written::Replaced,
        }
    }

    fn keeps_edited(self) -> bool {
        matches!(self, Self::Replaced { keep_edited: true })
    }
}

/// `robot-docs install-skill`: the refusals and the decision first, without the lock; a
/// dry run or an unchanged skill stops there. Otherwise, under the global lock and after
/// recovering interrupted changes as `config set` does, it reads and decides again,
/// writes, and records the `skill` line.
pub fn run(ctx: &Context, args: &InstallSkillArgs) -> Result<InstallSkillOutcome> {
    let paths = Paths::from_env()?;
    let guide = crate::contract::guide::render_guide()
        .map_err(|e| Error::internal(format!("the embedded guide does not render: {e}")))?
        .into_bytes();
    let destination = resolve(&paths, args.dir.as_deref(), current_dir)?;
    let clients = if destination.default {
        // A fixed path under $HOME/.claude: a broken registry does not block it.
        Vec::new()
    } else {
        registered_client_dirs(&paths)?
    };
    let scopes = scope(&paths, &clients, &destination)?;
    let dir = scopes.skill_dir().to_path_buf();
    let file = dir.join(SKILL_FILE);
    let mut decision = decide_at(&file, &guide)?;
    let commands = hints(&paths, &dir, &guide);
    let outcome = |decision: Decision, kept: Option<Keep>| InstallSkillOutcome {
        data: InstallSkillData {
            dry_run: ctx.dry_run,
            dir: dir.clone(),
            file: file.clone(),
            written: decision.written(),
            bytes: guide.len() as u64,
            tool_version: env!("CARGO_PKG_VERSION"),
        },
        commands: commands.clone(),
        kept,
    };
    if decision == Decision::Unchanged || ctx.dry_run {
        let kept = decision.keeps_edited().then(|| keep_slot(&dir, &file));
        return Ok(outcome(decision, kept));
    }
    let guard = GlobalLock::acquire(&paths, ctx.command)?;
    ctx.notes.warn_all(&recover_under(
        &guard,
        &paths,
        &no_saved_variables,
        ctx.command,
        ctx.actor,
    ));
    // Another process may have installed, linked or edited it meanwhile: the decision
    // made under the lock is the one carried out and reported.
    decision = decide_at(&file, &guide)?;
    if decision == Decision::Unchanged {
        return Ok(outcome(decision, None));
    }
    // The fences as recovery left them.
    let scopes = scope(&paths, &clients, &destination)?;
    let mut s = WriteSession::untracked(Writer::Skill, &scopes, SKILL_FILE);
    ensure_dir_all(&mut s, &dir)?;
    let mut kept = None;
    if decision.keeps_edited() {
        // Never over an earlier backup (omad-syc2): renamed without replacing, and a name
        // another program took meanwhile moves the copy to the next free one.
        let mut tries = 0;
        kept = Some(loop {
            let keep = keep_slot(&dir, &file);
            let Keep::New(copy) = &keep else {
                break keep;
            };
            let op = Op::Rename {
                to: copy,
                mode: RenameMode::NoReplace,
            };
            match mutate(&mut s, &file, op) {
                Ok(_) => break keep,
                Err(_) if tries < 3 && std::fs::symlink_metadata(copy).is_ok() => tries += 1,
                Err(error) => return Err(error.into()),
            }
        });
    }
    let op = Op::WriteFile {
        content: Content::Bytes(&guide),
        mode: Some(0o644),
    };
    mutate(&mut s, &file, op)?;
    let entry = SettingEntry {
        dir: Some(dir.clone()),
        ..SettingEntry::now(
            TxKind::Skill,
            None,
            ctx.actor,
            ctx.reason.clone(),
            ctx.argv.clone(),
        )
    };
    if let Some(warning) = history::record_setting(&mut s, &paths, &entry) {
        ctx.notes.warn(warning);
    }
    crate::state::doorbell::ring(&paths, ctx.command, ctx.actor);
    drop(s);
    drop(guard);
    Ok(outcome(decision, kept))
}

/// The process's working directory, for a relative `--dir`.
fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|e| {
        Error::io(
            Path::new("."),
            &e,
            Suggestion::text("run it from a folder that exists, or give --dir an absolute path"),
        )
    })
}

/// The table the run writes with: the stuck journals' fences, and the one skill folder.
fn scope(
    paths: &Paths,
    clients: &[PathBuf],
    destination: &Destination,
) -> Result<crate::fsx::ScopeTable> {
    fence::table(paths, clients, None)?.with_skill_dir(&destination.dir, &destination.create_from)
}

/// Every registered client dir, so a `--dir` inside one is refused. A registry that does
/// not read, or that a newer omaddons wrote, fails the run: a refusal that cannot see the
/// clients must not pass.
fn registered_client_dirs(paths: &Paths) -> Result<Vec<PathBuf>> {
    store::refuse_newer::<Registry>(&paths.registry_file())?;
    let registry = load_registry(paths)?.value;
    Ok(registry
        .targets
        .into_iter()
        .map(|entry| entry.client_dir)
        .collect())
}

/// Where the skill goes: Claude Code's skill folder, which needs `$HOME/.claude`, or
/// `--dir` (relative to the working directory `cwd` gives), which must name a folder.
fn resolve(
    paths: &Paths,
    dir: Option<&Path>,
    cwd: impl FnOnce() -> Result<PathBuf>,
) -> Result<Destination> {
    let Some(given) = dir else {
        let claude = paths.claude_dir();
        if !claude.is_dir() {
            let mut error = Error::new(
                ErrorCode::NotFound,
                format!(
                    "Claude Code's folder {} does not exist, so there is no default place to install the skill",
                    claude.display()
                ),
                Suggestion::text("set up Claude Code first, or choose a skills folder with --dir <dir>"),
            )
            .details(serde_json::json!({ "path": claude.display().to_string() }));
            for other in other_skill_dirs(paths) {
                error = error.alternative(Suggestion::command(install_argv(&other)));
            }
            return Err(error);
        }
        return Ok(Destination {
            dir: paths.skill_dir(),
            create_from: claude,
            default: true,
        });
    };
    let usage = |message: String| {
        Error::new(
            ErrorCode::Usage,
            message,
            Suggestion::text(
                "give --dir the folder that is to hold SKILL.md, such as <root>/skills/omaddons",
            ),
        )
    };
    if given.as_os_str().is_empty() || given.file_name().is_none() {
        return Err(usage(format!(
            "--dir names the folder that holds SKILL.md, not {:?}",
            given.display().to_string()
        )));
    }
    if given.to_str().is_none() {
        return Err(usage(format!(
            "--dir {} is not valid UTF-8; omaddons records the folder in history and JSON",
            given.display()
        )));
    }
    let dir = if given.is_absolute() {
        given.to_path_buf()
    } else {
        cwd()?.join(given)
    };
    if dir.exists() && !dir.is_dir() {
        return Err(usage(format!("--dir {} is not a folder", given.display())));
    }
    let create_from = dir.parent().unwrap_or(&dir).to_path_buf();
    Ok(Destination {
        dir,
        create_from,
        default: false,
    })
}

/// What `file` holds, as the decision needs it: a symlink is refused (never read
/// through, M4-D8), and so is anything but a regular file.
fn decide_at(file: &Path, guide: &[u8]) -> Result<Decision> {
    let existing = read_skill_file(file).map_err(|e| {
        Error::io(
            file,
            &e,
            Suggestion::text("check that you can read the skill folder, then run it again"),
        )
    })?;
    match &existing {
        SkillFile::Symlink { link_target } => Err(Error::new(
            ErrorCode::ScopeDenied,
            format!(
                "{} is a symlink to {}; omaddons never writes through a symlink",
                file.display(),
                link_target.display()
            ),
            Suggestion::text(
                "install into the link's real folder with --dir <dir>, or replace the link with a regular file",
            ),
        )
        .details(serde_json::json!({
            "reason": "symlink",
            "path": file.display().to_string(),
            "link_target": link_target.display().to_string(),
        }))),
        SkillFile::NotAFile => Err(Error::new(
            ErrorCode::Io,
            format!(
                "{} exists and is not a regular file; move it away, then run it again",
                file.display()
            ),
            Suggestion::text("move it out of the skill folder, then run the command again"),
        )
        .details(serde_json::json!({ "path": file.display().to_string() }))),
        _ => Ok(decide(&existing, guide)),
    }
}

/// The decision for what is there: the guide's own bytes change nothing, a missing file is
/// fresh, and anything else is replaced, kept first unless it is some guide's unedited
/// bytes (pristine: its stamp is its own digest), which an older build can write again.
fn decide(existing: &SkillFile, guide: &[u8]) -> Decision {
    match existing {
        SkillFile::Missing => Decision::Fresh,
        SkillFile::Bytes(bytes) if bytes.as_slice() == guide => Decision::Unchanged,
        SkillFile::Bytes(bytes) => Decision::Replaced {
            keep_edited: !inspect(bytes).is_pristine(),
        },
        SkillFile::TooLarge { .. } | SkillFile::Symlink { .. } | SkillFile::NotAFile => {
            Decision::Replaced { keep_edited: true }
        }
    }
}

/// `<home>/<root>/omaddons` for each other agent's skill root that exists.
fn other_skill_dirs(paths: &Paths) -> Vec<PathBuf> {
    OTHER_SKILL_ROOTS
        .iter()
        .map(|root| paths.home().join(root))
        .filter(|root| root.is_dir())
        .map(|root| root.join("omaddons"))
        .collect()
}

/// `omaddons robot-docs install-skill --dir <dir>`.
fn install_argv(dir: &Path) -> Vec<String> {
    ["omaddons", "robot-docs", "install-skill", "--dir"]
        .into_iter()
        .map(str::to_owned)
        .chain(std::iter::once(dir.display().to_string()))
        .collect()
}

/// One `install_skill` hint per other agent's skill root that exists, unless it is the
/// folder this run wrote or checked, or already holds these bytes (it would change
/// nothing). Never written unasked.
fn hints(paths: &Paths, written: &Path, guide: &[u8]) -> Vec<CommandHint> {
    let canonical = |dir: &Path| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    other_skill_dirs(paths)
        .into_iter()
        .filter(|dir| canonical(dir) != canonical(written))
        .filter(|dir| {
            !matches!(
                read_skill_file(&dir.join(SKILL_FILE)),
                Ok(SkillFile::Bytes(bytes)) if bytes == guide
            )
        })
        .map(|dir| CommandHint::new(Action::InstallSkill, install_argv(&dir)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::guide::stamp::{digest_without_stamp, hex, line, locate};

    fn guide() -> Vec<u8> {
        crate::contract::guide::render_guide().unwrap().into_bytes()
    }

    /// `text`'s length and first line, for a failure message.
    fn shown(bytes: &[u8]) -> String {
        let lossy = String::from_utf8_lossy(bytes);
        format!(
            "{} bytes, first line {:?}",
            bytes.len(),
            lossy.lines().next().unwrap_or("")
        )
    }

    /// Another guide: a prose line added, then stamped as the renderer would.
    fn other_guide() -> Vec<u8> {
        let text = format!(
            "{}A line another guide has.\n",
            String::from_utf8(guide()).unwrap()
        );
        let located = locate(&text).unwrap();
        let digest = digest_without_stamp(&text).unwrap();
        format!(
            "{}{}\n{}",
            &text[..located.start],
            line(&hex(&digest)),
            &text[located.end..]
        )
        .into_bytes()
    }

    #[test]
    fn decide_covers_every_existing_state() {
        let guide = guide();
        let mut edited = guide.clone();
        let last = edited.len() - 2;
        edited[last] ^= 0x01;
        let cases: Vec<(&str, SkillFile, Decision)> = vec![
            ("missing", SkillFile::Missing, Decision::Fresh),
            (
                "same_bytes",
                SkillFile::Bytes(guide.clone()),
                Decision::Unchanged,
            ),
            (
                "pristine_other_guide",
                SkillFile::Bytes(other_guide()),
                Decision::Replaced { keep_edited: false },
            ),
            (
                "edited_guide",
                SkillFile::Bytes(edited),
                Decision::Replaced { keep_edited: true },
            ),
            (
                "unstamped_file",
                SkillFile::Bytes(b"# my own notes\n".to_vec()),
                Decision::Replaced { keep_edited: true },
            ),
            (
                "too_large",
                SkillFile::TooLarge { len: 2 << 20 },
                Decision::Replaced { keep_edited: true },
            ),
            (
                "empty_file",
                SkillFile::Bytes(Vec::new()),
                Decision::Replaced { keep_edited: true },
            ),
        ];
        for (case, existing, want) in cases {
            let what = match &existing {
                SkillFile::Bytes(bytes) => shown(bytes),
                other => format!("{other:?}"),
            };
            let got = decide(&existing, &guide);
            assert_eq!(got, want, "{case}: {what} → {got:?}");
        }
    }

    /// A home under a tempdir: `paths` and the canonical root.
    fn home() -> (tempfile::TempDir, PathBuf, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        (dir, root, paths)
    }

    #[test]
    fn resolve_covers_default_and_custom_dirs() {
        use std::os::unix::ffi::OsStrExt;
        let (_dir, root, paths) = home();
        let home = paths.home().to_path_buf();
        let cwd = || Ok(home.join("work"));
        let fail = || -> Result<PathBuf> { panic!("the working directory is not needed") }; // ubs:ignore — the test's own failure

        let err = resolve(&paths, None, fail).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::NotFound,
            "default_without_claude: {err}"
        );
        assert_eq!(
            err.details,
            serde_json::json!({"path": home.join(".claude").display().to_string()})
        );
        assert!(err.alternatives.is_empty(), "no other root yet: {err:?}");

        std::fs::create_dir_all(home.join(".codex/skills")).unwrap();
        let err = resolve(&paths, None, fail).unwrap_err();
        let alternatives: Vec<&[String]> =
            err.alternatives.iter().filter_map(|a| a.argv()).collect();
        let codex = home.join(".codex/skills/omaddons").display().to_string();
        assert_eq!(
            alternatives,
            [&[
                "omaddons",
                "robot-docs",
                "install-skill",
                "--dir",
                codex.as_str()
            ]
            .map(str::to_owned)[..]],
            "default_without_claude_with_codex"
        );
        for argv in alternatives {
            assert!(
                <crate::cli::Cli as clap::Parser>::try_parse_from(argv).is_ok(),
                "the alternative parses: {argv:?}"
            );
        }

        std::fs::create_dir_all(home.join(".claude")).unwrap();
        assert_eq!(
            resolve(&paths, None, fail).unwrap(),
            Destination {
                dir: paths.skill_dir(),
                create_from: paths.claude_dir(),
                default: true,
            },
            "default_with_claude"
        );
        assert_eq!(
            resolve(&paths, Some(Path::new("skills/omaddons")), cwd).unwrap(),
            Destination {
                dir: home.join("work/skills/omaddons"),
                create_from: home.join("work/skills"),
                default: false,
            },
            "relative_dir"
        );
        std::fs::write(root.join("a-file"), b"").unwrap();
        let non_utf8 = std::ffi::OsStr::from_bytes(b"/tmp/sk\xffills");
        for (case, given) in [
            ("empty_dir", PathBuf::new()),
            ("root_dir", PathBuf::from("/")),
            ("dotdot_dir", PathBuf::from("skills/..")),
            ("dir_is_a_file", root.join("a-file")),
            ("non_utf8_dir", PathBuf::from(non_utf8)),
        ] {
            let err = resolve(&paths, Some(&given), cwd).unwrap_err();
            assert_eq!(err.code, ErrorCode::Usage, "{case}: {err}");
            assert!(
                err.fix.argv().is_none() && err.details == serde_json::json!({}),
                "{case}: a prose fix and no details: {err:?}"
            );
        }
    }

    #[test]
    fn hints_cover_every_root_state() {
        let guide = guide();
        let (_dir, _root, paths) = home();
        let home = paths.home().to_path_buf();
        let codex = home.join(".codex/skills/omaddons");
        let agents = home.join(".agents/skills/omaddons");
        let written = home.join(".claude/skills/omaddons");
        let argv_of = |hints: &[CommandHint]| -> Vec<Vec<String>> {
            hints.iter().map(|h| h.argv().to_vec()).collect()
        };
        assert!(
            hints(&paths, &written, &guide).is_empty(),
            "absent: no hint"
        );

        std::fs::create_dir_all(home.join(".codex/skills")).unwrap();
        std::fs::create_dir_all(home.join(".agents/skills")).unwrap();
        let both = hints(&paths, &written, &guide);
        assert_eq!(
            argv_of(&both),
            [install_argv(&codex), install_argv(&agents)],
            "present: one hint each, in OTHER_SKILL_ROOTS order"
        );
        for hint in &both {
            assert_eq!(hint.action(), Action::InstallSkill);
            assert!(!hint.destructive());
            assert!(
                <crate::cli::Cli as clap::Parser>::try_parse_from(hint.argv()).is_ok(),
                "{:?} parses",
                hint.argv()
            );
        }

        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(codex.join(SKILL_FILE), b"other bytes\n").unwrap();
        assert_eq!(
            argv_of(&hints(&paths, &written, &guide)),
            [install_argv(&codex), install_argv(&agents)],
            "other bytes: still offered"
        );
        std::fs::write(codex.join(SKILL_FILE), &guide).unwrap();
        assert_eq!(
            argv_of(&hints(&paths, &written, &guide)),
            [install_argv(&agents)],
            "these bytes: nothing to offer"
        );
        assert_eq!(
            argv_of(&hints(&paths, &agents, &guide)),
            Vec::<Vec<String>>::new(),
            "the folder just written is never offered"
        );
    }

    /// omad-syc2: an edited copy goes to the first free backup name, never over an
    /// earlier one; one a backup already holds byte for byte is not copied again; a
    /// backup name that is a link counts as taken and is never read through.
    #[test]
    fn an_edited_copy_never_replaces_an_earlier_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let file = dir.join(SKILL_FILE);
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(SKILL_FILE, "edit C\n");
        /// What a case adds to the folder before asking.
        type Plant = fn(&Path);
        let cases: [(&str, Plant, Keep); 4] = [
            ("no backup yet", |_| {}, Keep::New(dir.join("SKILL.md.bak"))),
            (
                "one taken",
                |d| std::fs::write(d.join("SKILL.md.bak"), "edit A\n").unwrap(),
                Keep::New(dir.join("SKILL.md.bak.1")),
            ),
            (
                "two taken",
                |d| std::fs::write(d.join("SKILL.md.bak.1"), "edit B\n").unwrap(),
                Keep::New(dir.join("SKILL.md.bak.2")),
            ),
            (
                "a link to the same bytes is taken, not a backup",
                |d| {
                    std::os::unix::fs::symlink(d.join(SKILL_FILE), d.join("SKILL.md.bak.2"))
                        .unwrap()
                },
                Keep::New(dir.join("SKILL.md.bak.3")),
            ),
        ];
        for (case, plant, want) in cases {
            plant(dir);
            assert_eq!(keep_slot(dir, &file), want, "case {case}");
        }
        write("SKILL.md.bak.3", "edit C\n");
        assert_eq!(
            keep_slot(dir, &file),
            Keep::Already(dir.join("SKILL.md.bak.3")),
            "case identical bytes kept already"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.bak")).unwrap(),
            "edit A\n",
            "earlier backups untouched"
        );
    }

    /// `name` in the test skill folder.
    fn in_dir(name: &str) -> PathBuf {
        PathBuf::from("/h/.claude/skills/omaddons").join(name)
    }

    fn outcome(
        dry_run: bool,
        written: Written,
        kept: Option<Keep>,
        hints: usize,
    ) -> InstallSkillOutcome {
        let dir = PathBuf::from("/h/.claude/skills/omaddons");
        let commands = [".codex/skills/omaddons", ".agents/skills/omaddons"]
            .iter()
            .take(hints)
            .map(|d| CommandHint::new(Action::InstallSkill, install_argv(&Path::new("/h").join(d))))
            .collect();
        InstallSkillOutcome {
            data: InstallSkillData {
                dry_run,
                file: dir.join(SKILL_FILE),
                dir,
                written,
                bytes: 18_432,
                tool_version: "0.1.0",
            },
            commands,
            kept,
        }
    }

    #[test]
    fn data_serializes_in_order_with_closed_written_values() {
        let json = serde_json::to_string(&outcome(false, Written::Fresh, None, 0).data).unwrap();
        assert_eq!(
            json,
            "{\"dry_run\":false,\"dir\":\"/h/.claude/skills/omaddons\",\"file\":\"/h/.claude/skills/omaddons/SKILL.md\",\"written\":\"fresh\",\"bytes\":18432,\"tool_version\":\"0.1.0\"}"
        );
        for (written, wire) in [
            (Written::Fresh, "fresh"),
            (Written::Replaced, "replaced"),
            (Written::Unchanged, "unchanged"),
        ] {
            assert_eq!(serde_json::to_value(written).unwrap(), wire);
        }
        let schema = serde_json::to_value(schemars::schema_for!(Written)).unwrap();
        let consts: Vec<&serde_json::Value> = schema["oneOf"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|variant| &variant["const"])
            .collect();
        assert_eq!(
            consts,
            [
                &serde_json::json!("fresh"),
                &serde_json::json!("replaced"),
                &serde_json::json!("unchanged")
            ],
            "a closed enum of exactly these values: {schema}"
        );
    }

    #[test]
    fn human_output_for_each_outcome() {
        let file = "/h/.claude/skills/omaddons/SKILL.md";
        let hint = |root: &str| {
            format!(
                "other agents: omaddons robot-docs install-skill --dir /h/{root}/skills/omaddons"
            )
        };
        let new = |name: &str| Some(Keep::New(in_dir(name)));
        let cases: Vec<(&str, InstallSkillOutcome, Vec<String>)> = vec![
            (
                "fresh",
                outcome(false, Written::Fresh, None, 0),
                vec![format!(
                    "installed the omaddons skill: {file} (18,432 bytes)"
                )],
            ),
            (
                "replaced_without_a_copy",
                outcome(false, Written::Replaced, None, 0),
                vec![format!("updated the omaddons skill: {file} (18,432 bytes)")],
            ),
            (
                "replaced_keeping_a_copy",
                outcome(false, Written::Replaced, new("SKILL.md.bak"), 0),
                vec![
                    format!("updated the omaddons skill: {file} (18,432 bytes)"),
                    format!("  your edited copy is kept as {file}.bak"),
                ],
            ),
            (
                "replaced_keeping_a_numbered_copy",
                outcome(false, Written::Replaced, new("SKILL.md.bak.2"), 0),
                vec![
                    format!("updated the omaddons skill: {file} (18,432 bytes)"),
                    format!("  your edited copy is kept as {file}.bak.2"),
                ],
            ),
            (
                "replaced_already_kept",
                outcome(
                    false,
                    Written::Replaced,
                    Some(Keep::Already(in_dir("SKILL.md.bak.1"))),
                    0,
                ),
                vec![
                    format!("updated the omaddons skill: {file} (18,432 bytes)"),
                    format!("  your edited copy is already kept as {file}.bak.1"),
                ],
            ),
            (
                "unchanged",
                outcome(false, Written::Unchanged, None, 0),
                vec![format!("the omaddons skill is up to date: {file}")],
            ),
            (
                "dry_run_fresh",
                outcome(true, Written::Fresh, None, 0),
                vec![format!(
                    "would install the omaddons skill: {file} (18,432 bytes)"
                )],
            ),
            (
                "dry_run_replaced_keeping_a_copy",
                outcome(true, Written::Replaced, new("SKILL.md.bak"), 0),
                vec![
                    format!("would update the omaddons skill: {file} (18,432 bytes)"),
                    format!("  would keep your edited copy as {file}.bak"),
                ],
            ),
            (
                "fresh_with_two_hints",
                outcome(false, Written::Fresh, None, 2),
                vec![
                    format!("installed the omaddons skill: {file} (18,432 bytes)"),
                    hint(".codex"),
                    hint(".agents"),
                ],
            ),
        ];
        for (case, outcome, want) in cases {
            let mut out = Vec::new();
            outcome.render_human(&mut out, &Style::default()).unwrap();
            let got = String::from_utf8(out).unwrap();
            assert_eq!(got.lines().collect::<Vec<_>>(), want, "{case}:\n{got}");
        }
    }
}
