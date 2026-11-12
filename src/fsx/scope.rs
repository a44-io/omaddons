//! The write-scope table and the deny-list (vision §5.5).
//!
//! Every path `mutate()` touches must fall inside a row that admits the session's
//! writer; anything else is refused with `scope_denied`. The deny-list runs first and
//! cannot be overridden: `blizzard_*` AddOns folders in any case (`protected_folder`)
//! and the global lock file (`scope_denied`). It also refuses an AddOns folder name that
//! is no valid folder name, except the one a doctor flatten parks a wrapper under, for
//! the doctor alone (`.omaddons-flatten-<tx>`). Matching works on normalized absolute
//! paths and touches no file; only building the table canonicalizes its roots.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::fence::FenceSet;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::limits::SKILL_DIRS_MAX;
use crate::model::FolderName;
use crate::state::paths::{Paths, REPAIR_SUFFIX, flatten_provisional_tx};

/// Who is writing, which decides the rows a session may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Writer {
    /// `config set` and `export --write`.
    Config,
    /// `auth github set`, `--from-gh` and `clear`: the credentials directory.
    Auth,
    /// Every mutating command.
    Mutating,
    /// `undo`, the only writer of owned SavedVariables.
    Undo,
    /// Derived data from any command: caches and `net.log`.
    Derived,
    /// `backup create` (M3).
    Backup,
    /// `backup restore` (M3), the only writer of the whole `WTF/` tree.
    BackupRestore,
    /// `omarchy install|remove` (M3).
    Omarchy,
    /// `robot-docs install-skill` (M4).
    Skill,
    /// Doctor repairs (M4); never `WTF/`.
    Doctor,
    /// `install-completions`: the one shell completion file it chose, and nothing else
    /// (no history line: it records nothing, as `backup export` does not).
    Completions,
}

impl Writer {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Auth => "auth",
            Self::Mutating => "mutating",
            Self::Undo => "undo",
            Self::Derived => "derived",
            Self::Backup => "backup",
            Self::BackupRestore => "backup_restore",
            Self::Omarchy => "omarchy",
            Self::Skill => "skill",
            Self::Doctor => "doctor",
            Self::Completions => "completions",
        }
    }
}

impl fmt::Display for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A row of the write-scope table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// `<config>/omaddons/`
    Config,
    /// `<state>/omaddons/`, except the lock file.
    State,
    /// `<state>/omaddons/net.log` and `net.log.1`.
    NetLog,
    /// `<state>/omaddons/history.jsonl` alone: settings changes append their line in
    /// the session of the command that made them (vision §5.9 Attribution).
    History,
    /// `<cache>/omaddons/`
    Cache,
    /// `<client dir>/Interface/AddOns/<non-protected folder>/…`
    AddOns,
    /// `<client dir>/Interface/.omaddons/`
    ClientMeta,
    /// `<client dir>/WTF/Account/**/SavedVariables/<owned folder>.lua[.bak]`
    SavedVariables,
    /// `<client dir>/WTF/` as a whole.
    Wtf,
    /// The Omarchy widget's folder under `<plugins>` (`a44.omaddons`), and its hidden
    /// siblings: one stage per `omarchy install` (`.a44.omaddons.stage-<op>`), a parked old
    /// widget (`.a44.omaddons.old-<op>`) and the prune trash (`.a44.omaddons.trash`); also
    /// the folder builds before omad-o8g4 installed it in (`omaddons`), so an install can
    /// park a widget left there. Each accepts a rename as a whole, from `Omarchy`: the
    /// atomic swap (M3-D38).
    Omarchy,
    /// The agent skill dir.
    Skill,
    /// The HTTP fixture dir in record mode (debug builds only).
    RecordFixtures,
    /// The one file `backup export` writes, chosen at run time (M3-D34): that path
    /// exactly. (fsx's hidden temp sibling of it is fsx's own, never a `mutate` target.)
    ExportFile,
    /// The one shell completion file `install-completions` writes, chosen at run time:
    /// that path exactly, and its missing folders below the one it may create them from
    /// (created only).
    CompletionFile,
}

impl Scope {
    /// Whether `writer` may use this row.
    pub fn admits(self, writer: Writer) -> bool {
        use Writer as W;
        match self {
            Self::Config => matches!(writer, W::Config | W::Auth | W::Doctor),
            Self::State => matches!(
                writer,
                W::Mutating | W::Undo | W::Backup | W::BackupRestore | W::Doctor
            ),
            Self::NetLog | Self::Cache => true,
            Self::History => matches!(
                writer,
                W::Mutating
                    | W::Undo
                    | W::Config
                    | W::Auth
                    | W::Backup
                    | W::Omarchy
                    | W::Skill
                    | W::Doctor
            ),
            Self::AddOns => matches!(writer, W::Mutating | W::Undo | W::Doctor),
            Self::ClientMeta => matches!(
                writer,
                W::Mutating | W::Undo | W::Backup | W::BackupRestore | W::Doctor
            ),
            Self::SavedVariables => writer == W::Undo,
            Self::Wtf => writer == W::BackupRestore,
            Self::Omarchy => matches!(writer, W::Omarchy | W::Doctor),
            Self::Skill => matches!(writer, W::Skill | W::Doctor),
            Self::RecordFixtures => writer == W::Derived,
            Self::ExportFile => writer == W::Backup,
            Self::CompletionFile => writer == W::Completions,
        }
    }
}

/// Where a matched path lives, which decides where its backups go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Side {
    /// Under the user's home: the XDG dirs, the plugin and skill dirs.
    Home,
    /// Inside this (canonical) client dir.
    Client(PathBuf),
}

/// The row a path matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeMatch {
    pub scope: Scope,
    pub side: Side,
}

/// What the write does to the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Creating a directory. Scope roots (and the XDG bases above the app dirs, and
    /// `<client dir>/Interface`) accept only this.
    CreateDir,
    /// Renaming the path as a whole, or onto it: either end of a rename. Below a root it
    /// is a write like any other; at a root only `WTF` accepts it, from `BackupRestore`
    /// (a whole restore's exchange, M3-D24).
    Rename,
    /// Anything else.
    Write,
}

/// Why a path may not be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    ScopeDenied(String),
    Protected(String),
}

/// The folders whose SavedVariables an `Undo` session may restore, by folder key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnedFolders(BTreeSet<String>);

impl OwnedFolders {
    pub fn new<'a>(folders: impl IntoIterator<Item = &'a FolderName>) -> Self {
        Self(folders.into_iter().map(|f| f.key().to_owned()).collect())
    }

    pub fn contains(&self, folder: &FolderName) -> bool {
        self.0.contains(folder.key())
    }
}

/// Where a path sits relative to a row's root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Below,
    Root,
    /// A missing XDG base above an app dir (`<state>` for `<state>/omaddons`).
    Ancestor,
}

struct Candidate {
    scope: Scope,
    position: Position,
    side: Side,
    /// For `SavedVariables`: the folder the file belongs to.
    saved_variables_of: Option<String>,
}

impl Candidate {
    fn new(scope: Scope, position: Position, side: Side) -> Self {
        Self {
            scope,
            position,
            side,
            saved_variables_of: None,
        }
    }
}

/// The write-scope table: canonical roots plus the registered client dirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeTable {
    /// The (non-canonical) paths the table was built from, for the lock and trash dirs.
    paths: Paths,
    home: PathBuf,
    config: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    lock_file: PathBuf,
    history: PathBuf,
    net_log: PathBuf,
    net_log_previous: PathBuf,
    /// `<plugins>`, canonical: the widget's entries are named children of it.
    omarchy_plugins: PathBuf,
    /// The agent skill dirs: the default one first (or the one an `install-skill` run
    /// chose, [`ScopeTable::with_skill_dir`]), then those a repair names. Never empty.
    skills: Vec<PathBuf>,
    /// The existing folder an `install-skill` run creates its skill folder from: the
    /// folders strictly between it and `skills[0]` may be created. `None` otherwise.
    skill_anchor: Option<PathBuf>,
    record_fixtures: Option<PathBuf>,
    /// The file `backup export` writes, once [`ScopeTable::with_export_file`] admits it.
    export_file: Option<PathBuf>,
    /// The completion file `install-completions` writes and the existing folder its
    /// missing folders are created below, once [`ScopeTable::with_completion_file`]
    /// admits them.
    completion_file: Option<(PathBuf, PathBuf)>,
    client_dirs: Vec<PathBuf>,
    /// What stuck journals protect (M4-D62): empty unless [`ScopeTable::with_fences`].
    fences: FenceSet,
}

impl ScopeTable {
    /// Builds the table, canonicalizing every root once (client dirs may be reached
    /// through `dosdevices` symlinks). Roots that do not exist yet are canonicalized
    /// through their deepest existing ancestor.
    pub fn new(paths: &Paths, client_dirs: &[PathBuf], skill_dir: Option<&Path>) -> Result<Self> {
        let state = canonical(paths.state_dir())?;
        let mut table = Self {
            paths: paths.clone(),
            home: canonical(paths.home())?,
            config: canonical(paths.config_dir())?,
            lock_file: state.join("lock"),
            history: state.join("history.jsonl"),
            net_log: state.join("net.log"),
            net_log_previous: state.join("net.log.1"),
            state,
            cache: canonical(paths.cache_dir())?,
            omarchy_plugins: canonical(&paths.omarchy_plugins_dir())?,
            skills: vec![canonical(
                &skill_dir.map_or_else(|| paths.skill_dir(), Path::to_path_buf),
            )?],
            skill_anchor: None,
            record_fixtures: None,
            export_file: None,
            completion_file: None,
            client_dirs: Vec::new(),
            fences: FenceSet::new(),
        };
        for dir in client_dirs {
            table.add_client_dir(dir)?;
        }
        Ok(table)
    }

    /// The table with `fences`: every `mutate()` through it refuses what they protect,
    /// and `prune()` skips what they cover.
    pub fn with_fences(mut self, fences: FenceSet) -> Self {
        self.fences = fences;
        self
    }

    /// The fences this table carries.
    pub fn fences(&self) -> &FenceSet {
        &self.fences
    }

    /// A clone with no fence: recovery writes unfenced.
    pub fn without_fences(&self) -> Self {
        Self {
            fences: FenceSet::new(),
            ..self.clone()
        }
    }

    /// Admits more agent skill dirs (a repair of several installed skills), each
    /// canonicalized like the default one and writable by the Skill row's writers only.
    /// A directory already admitted counts once; more than [`SKILL_DIRS_MAX`] in all is
    /// `scope_denied`.
    pub fn with_skill_dirs(mut self, dirs: &[PathBuf]) -> Result<Self> {
        for dir in dirs {
            let dir = canonical(dir)?;
            if !self.skills.contains(&dir) {
                self.skills.push(dir);
            }
        }
        if self.skills.len() > SKILL_DIRS_MAX {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                format!(
                    "a repair names {} skill directories; at most {SKILL_DIRS_MAX} are admitted",
                    self.skills.len()
                ),
                Suggestion::command(["omaddons", "status"]),
            ));
        }
        Ok(self)
    }

    /// Whether `path` is strictly below a doctor quarantine root: a run's
    /// `<state>/doctor/runs/<run>/quarantine/`, the cache's `<cache>/quarantine/<run>/`,
    /// or a registered client's `Interface/.omaddons/snapshots/<tx>.repair/quarantine/`.
    /// It reads no file.
    pub fn is_quarantine(&self, path: &Path) -> bool {
        let words = |root: &Path| -> Option<Vec<&OsStr>> {
            path.strip_prefix(root)
                .ok()
                .map(|rel| normal_components(rel).collect())
        };
        let run = words(&self.state).is_some_and(|c| {
            matches!(c.as_slice(), [doctor, runs, _, quarantine, _, ..]
                if *doctor == "doctor" && *runs == "runs" && *quarantine == "quarantine")
        });
        let cache = words(&self.cache).is_some_and(
            |c| matches!(c.as_slice(), [quarantine, _, _, ..] if *quarantine == "quarantine"),
        );
        let client = self.client_dirs.iter().any(|client| {
            client_components(client, path).is_some_and(|c| {
                is_meta_prefix(&c)
                    && matches!(c.as_slice(), [_, _, snapshots, half, quarantine, _, ..]
                        if eq_ci(snapshots, "snapshots")
                            && half.to_str().and_then(|h| h.strip_suffix(REPAIR_SUFFIX)).is_some_and(|tx| !tx.is_empty())
                            && eq_ci(quarantine, "quarantine"))
            })
        });
        run || cache || client
    }

    /// Adds a client dir, e.g. when the registry registers a target under the lock.
    pub fn add_client_dir(&mut self, dir: &Path) -> Result<()> {
        let dir = canonical(dir)?;
        if !self.client_dirs.contains(&dir) {
            self.client_dirs.push(dir);
        }
        Ok(())
    }

    /// Makes `dir` writable by `Derived` for HTTP record mode (`OMADDONS_RECORD=1`).
    /// Release builds have no such row.
    pub fn add_record_fixtures(&mut self, dir: &Path) -> Result<()> {
        if !cfg!(debug_assertions) {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                "HTTP record mode exists only in debug builds",
                Suggestion::command(["omaddons", "status"]),
            ));
        }
        self.record_fixtures = Some(canonical(dir)?);
        Ok(())
    }

    /// Makes `file`, and nothing else, writable by `Backup`: the archive `backup export`
    /// writes (M3-D34). Its parent must exist (`io`), so a typo never creates
    /// directories; a destination inside a client dir, a Wine prefix (a directory holding
    /// `drive_c` and `dosdevices`) or omaddons' own config, state or cache dirs is
    /// `scope_denied`. Returns the canonical path to write.
    pub fn with_export_file(&mut self, file: &Path) -> Result<PathBuf> {
        let refuse = |why: String| {
            Error::new(
                ErrorCode::ScopeDenied,
                format!("{} {why}", file.display()),
                Suggestion::text("export to a folder of your own, such as a backup drive"),
            )
        };
        let (Some(name), Some(parent)) = (file.file_name(), file.parent()) else {
            return Err(refuse("names no file".to_owned()));
        };
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let parent = std::fs::canonicalize(parent).map_err(|e| {
            Error::io(
                parent,
                &e,
                Suggestion::text("create the folder first, or choose one that exists"),
            )
        })?;
        if let Some(why) = self.inside_refused(&parent) {
            return Err(refuse(why));
        }
        let target = parent.join(name);
        self.export_file = Some(target.clone());
        Ok(target)
    }

    /// omaddons' own config, state and cache dirs, canonical, by name.
    fn own_dirs(&self) -> [(&Path, &'static str); 3] {
        [
            (self.config.as_path(), "config"),
            (self.state.as_path(), "state"),
            (self.cache.as_path(), "cache"),
        ]
    }

    /// Why a destination chosen at run time (canonical) may not be written, as the phrase
    /// after its path: it is inside a registered client dir, a Wine prefix (an ancestor,
    /// itself included, holding `drive_c` and `dosdevices`), or omaddons' own config,
    /// state or cache dir. `None` when it is none of these. `backup export`'s file and
    /// `install-skill`'s folder share it.
    fn inside_refused(&self, canonical: &Path) -> Option<String> {
        if let Some(client) = self.client_dirs.iter().find(|c| canonical.starts_with(c)) {
            return Some(format!("is inside the game client {}", client.display()));
        }
        if let Some(prefix) = canonical
            .ancestors()
            .find(|a| a.join("drive_c").is_dir() && a.join("dosdevices").is_dir())
        {
            return Some(format!("is inside the Wine prefix {}", prefix.display()));
        }
        self.own_dirs()
            .into_iter()
            .find(|(root, _)| canonical.starts_with(root))
            .map(|(_, what)| format!("is inside omaddons' own {what} folder"))
    }

    /// The one skill folder a `robot-docs install-skill` run writes (M4-D8): `dir`
    /// replaces the table's skill folders, writable by the Skill row's writers alone, and
    /// the folders strictly between `create_from` and `dir` become creatable (`CreateDir`
    /// only). `create_from` must exist as a folder: the default's `$HOME/.claude`, or a
    /// `--dir`'s parent. Anything missing is `not_found` naming it, and nothing is ever
    /// created here. A folder inside a registered client dir, a Wine prefix or omaddons'
    /// own config, state or cache dir, or one holding a client dir or one of those
    /// (the Skill row admits everything below its folder), is `scope_denied` with
    /// `details.reason: "skill_dir"`. `dir` is judged where it resolves: a symlinked
    /// folder or ancestor counts as its target.
    pub fn with_skill_dir(mut self, dir: &Path, create_from: &Path) -> Result<Self> {
        let anchor = match std::fs::canonicalize(create_from) {
            Ok(anchor) if anchor.is_dir() => anchor,
            found => {
                let what = if found.is_ok() {
                    "is not a folder, so it cannot hold"
                } else {
                    "does not exist, so it cannot hold"
                };
                return Err(Error::new(
                    ErrorCode::NotFound,
                    format!(
                        "{} {what} {}; install-skill creates only the skill folder itself",
                        create_from.display(),
                        dir.display()
                    ),
                    Suggestion::text(
                        "create the parent folder first, or choose a --dir whose parent exists",
                    ),
                )
                .details(serde_json::json!({ "path": create_from.display().to_string() })));
            }
        };
        let dir = canonical(dir)?;
        if let Some(why) = self
            .inside_refused(&dir)
            .or_else(|| self.holds_refused(&dir))
        {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                format!(
                    "{} {why}; the skill goes in a folder of your own",
                    dir.display()
                ),
                Suggestion::text(
                    "install the skill into a folder of your own, outside game clients, Wine prefixes and omaddons' own folders: --dir <dir>",
                ),
            )
            .details(serde_json::json!({
                "reason": "skill_dir",
                "path": dir.display().to_string(),
            })));
        }
        self.skills = vec![dir];
        self.skill_anchor = Some(anchor);
        Ok(self)
    }

    /// Why a skill folder that holds what it must stay out of is refused: a registered
    /// client dir, or omaddons' own config, state or cache dir, below it.
    fn holds_refused(&self, dir: &Path) -> Option<String> {
        if let Some(client) = self.client_dirs.iter().find(|c| c.starts_with(dir)) {
            return Some(format!("holds the game client {}", client.display()));
        }
        self.own_dirs()
            .into_iter()
            .find(|(root, _)| root.starts_with(dir))
            .map(|(_, what)| format!("holds omaddons' own {what} folder"))
    }

    /// The skill folder the Skill row admits first, canonical: the default one, or the
    /// one [`ScopeTable::with_skill_dir`] chose.
    pub fn skill_dir(&self) -> &Path {
        self.skills.first().map_or(Path::new(""), PathBuf::as_path)
    }

    /// The one shell completion file an `install-completions` run writes: `file`, and
    /// nothing else, becomes writable by `Completions` alone, and the folders missing
    /// between it and its deepest existing ancestor become creatable (`CreateDir` only).
    /// `create_from` must exist as a folder (the shell's base: `$XDG_DATA_HOME`,
    /// `$XDG_CONFIG_HOME` or `$HOME`), so nothing above it is ever created: one missing is
    /// `not_found` naming it. A file whose folder is inside a registered client dir, a
    /// Wine prefix or omaddons' own config, state or cache dir is `scope_denied`
    /// (`details.path`). The folder is judged where it resolves; the file's own name is
    /// never resolved, so a link there is the caller's to refuse. The admitted path, its
    /// folder canonical, is [`ScopeTable::completion_file`].
    pub fn with_completion_file(mut self, file: &Path, create_from: &Path) -> Result<Self> {
        if !std::fs::canonicalize(create_from).is_ok_and(|anchor| anchor.is_dir()) {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!(
                    "{} is not a folder, so it cannot hold {}; install-completions creates only the folders below it",
                    create_from.display(),
                    file.display()
                ),
                Suggestion::text("create that folder first, then run it again"),
            )
            .details(serde_json::json!({ "path": create_from.display().to_string() })));
        }
        let (Some(name), Some(parent)) = (file.file_name(), file.parent()) else {
            return Err(Error::internal(format!(
                "the completion file {} names no file in a folder",
                file.display()
            )));
        };
        let folder = canonical(parent)?;
        if let Some(why) = self.inside_refused(&folder) {
            return Err(Error::new(
                ErrorCode::ScopeDenied,
                format!(
                    "{} {why}; a completion script goes in your shell's own folders",
                    file.display()
                ),
                Suggestion::text(
                    "point XDG_DATA_HOME or XDG_CONFIG_HOME at your own folders, or save the script yourself where your shell reads it",
                ),
            )
            .details(serde_json::json!({ "path": file.display().to_string() })));
        }
        let existing = folder
            .ancestors()
            .find(|dir| dir.is_dir())
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        self.completion_file = Some((folder.join(name), existing));
        Ok(self)
    }

    /// The file [`ScopeTable::with_completion_file`] admitted, its folder canonical.
    pub fn completion_file(&self) -> Option<&Path> {
        self.completion_file
            .as_ref()
            .map(|(file, _)| file.as_path())
    }

    /// The paths the table was built from.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// `<state>/omaddons`, canonical.
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// `<cache>/omaddons`, canonical.
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// The registered client dirs, canonical.
    pub fn client_dirs(&self) -> &[PathBuf] {
        &self.client_dirs
    }

    /// `<plugins>`, canonical.
    pub fn omarchy_plugins_dir(&self) -> &Path {
        &self.omarchy_plugins
    }

    /// Where `path` sits in the widget's row: one of its named entries under `<plugins>`
    /// (the widget, a stage, a parked old widget, the trash) is a root, anything inside
    /// one is below it, and `<plugins>` itself may only be created. Other paths under
    /// `<plugins>`, other plugins included, are not the widget's.
    fn omarchy_position(&self, path: &Path) -> Option<Position> {
        if path == self.omarchy_plugins {
            return (!self.home.starts_with(path)).then_some(Position::Ancestor);
        }
        let rel = path.strip_prefix(&self.omarchy_plugins).ok()?;
        let mut components = rel.components();
        let Some(Component::Normal(first)) = components.next() else {
            return None;
        };
        let first = first.to_str()?;
        let name = self.paths.omarchy_plugin_name();
        let legacy = self.paths.omarchy_legacy_plugin_dir();
        let ours = first == name
            || legacy.file_name().is_some_and(|legacy| legacy == first)
            || first == format!(".{name}.trash")
            || self.paths.omarchy_stage_op(first).is_some()
            || self.paths.omarchy_old_op(first).is_some();
        if !ours {
            return None;
        }
        Some(if components.next().is_none() {
            Position::Root
        } else {
            Position::Below
        })
    }

    /// Whether `path` is somewhere fsx may keep its own bookkeeping (backups, action
    /// logs): under `<state>/omaddons` or a client dir's `Interface/.omaddons`.
    pub fn is_internal(&self, path: &Path) -> bool {
        path.starts_with(&self.state)
            || self.client_dirs.iter().any(|client| {
                client_components(client, path).is_some_and(|c| c.len() >= 2 && is_meta_prefix(&c))
            })
    }

    /// Whether `path` is below a directory omaddons owns: `<config>`, `<state>` or
    /// `<cache>` (each `…/omaddons`), or a registered client's `Interface/.omaddons`.
    pub fn is_app_owned(&self, path: &Path) -> bool {
        path.starts_with(&self.config) || path.starts_with(&self.cache) || self.is_internal(path)
    }

    /// Whether `path` is inside a client's staging dir
    /// (`<client dir>/Interface/.omaddons/stage/<tx>/…`).
    pub fn is_stage(&self, path: &Path) -> bool {
        self.client_dirs.iter().any(|client| {
            client_components(client, path).is_some_and(|c| {
                c.len() >= 4 && is_meta_prefix(&c) && c.get(2).is_some_and(|s| eq_ci(s, "stage"))
            })
        })
    }

    /// Checks one normalized absolute path for `writer`: the deny-list first, then the
    /// rows.
    pub fn check(
        &self,
        writer: Writer,
        owned: &OwnedFolders,
        path: &Path,
        access: Access,
    ) -> std::result::Result<ScopeMatch, Refusal> {
        if path == self.lock_file {
            return Err(Refusal::ScopeDenied(format!(
                "{} is the global lock file; only the lock itself writes it",
                path.display()
            )));
        }
        let in_app_dirs = [&self.config, &self.state, &self.cache]
            .into_iter()
            .any(|root| path.starts_with(root));
        if !in_app_dirs {
            check_protected(path, writer)?;
        }
        let candidates = self.candidates(path);
        if candidates.is_empty() {
            return Err(Refusal::ScopeDenied(format!(
                "{} is outside every write scope",
                path.display()
            )));
        }
        for candidate in &candidates {
            if !candidate.scope.admits(writer) {
                continue;
            }
            // A whole `WTF/` is renamed only by a restore, and the widget's entries only by
            // its install and remove, which their rows admit alone.
            let renames_root = access == Access::Rename
                && matches!(
                    (candidate.scope, writer),
                    (Scope::Wtf, _) | (Scope::Omarchy, Writer::Omarchy)
                )
                && candidate.position == Position::Root;
            if candidate.position != Position::Below && access != Access::CreateDir && !renames_root
            {
                continue;
            }
            if let Some(folder) = &candidate.saved_variables_of {
                let owned_folder = FolderName::new(folder).is_ok_and(|f| owned.contains(&f));
                if !owned_folder {
                    continue;
                }
            }
            return Ok(ScopeMatch {
                scope: candidate.scope,
                side: candidate.side.clone(),
            });
        }
        let rows: Vec<String> = candidates
            .iter()
            .map(|c| format!("{:?}", c.scope))
            .collect();
        Err(Refusal::ScopeDenied(format!(
            "writer {writer} may not {} {} (scope {})",
            match access {
                Access::CreateDir => "create",
                Access::Rename => "rename",
                Access::Write => "write",
            },
            path.display(),
            rows.join(", ")
        )))
    }

    fn candidates(&self, path: &Path) -> Vec<Candidate> {
        let mut found = Vec::new();
        for (scope, root) in [
            (Scope::Config, &self.config),
            (Scope::State, &self.state),
            (Scope::Cache, &self.cache),
        ] {
            if path == root {
                found.push(Candidate::new(scope, Position::Root, Side::Home));
            } else if path.starts_with(root) {
                found.push(Candidate::new(scope, Position::Below, Side::Home));
            } else if root.starts_with(path) && !self.home.starts_with(path) {
                // A missing XDG base such as `<state>`, but never home or above it.
                found.push(Candidate::new(scope, Position::Ancestor, Side::Home));
            }
        }
        if path == self.history {
            found.push(Candidate::new(Scope::History, Position::Below, Side::Home));
        }
        if path == self.net_log || path == self.net_log_previous {
            found.push(Candidate::new(Scope::NetLog, Position::Below, Side::Home));
        } else if self.net_log.starts_with(path) && !self.home.starts_with(path) {
            // Every writer may log a request, so every writer may create the directories
            // net.log lives in (`<state>/omaddons` and missing XDG bases).
            found.push(Candidate::new(
                Scope::NetLog,
                Position::Ancestor,
                Side::Home,
            ));
        }
        if self.export_file.as_deref() == Some(path) {
            found.push(Candidate::new(
                Scope::ExportFile,
                Position::Below,
                Side::Home,
            ));
        }
        // An `install-completions` run's file, and the folders missing below its deepest
        // existing ancestor: created only, by `check`'s rule for any position but `Below`.
        if let Some((file, existing)) = &self.completion_file {
            if path == file.as_path() {
                found.push(Candidate::new(
                    Scope::CompletionFile,
                    Position::Below,
                    Side::Home,
                ));
            } else if file.starts_with(path) && path != existing && path.starts_with(existing) {
                found.push(Candidate::new(
                    Scope::CompletionFile,
                    Position::Ancestor,
                    Side::Home,
                ));
            }
        }
        if let Some(position) = self.omarchy_position(path) {
            found.push(Candidate::new(Scope::Omarchy, position, Side::Home));
        }
        let home_rows = self
            .skills
            .iter()
            .map(|skill| (Scope::Skill, Some(skill)))
            .chain([(Scope::RecordFixtures, self.record_fixtures.as_ref())]);
        for (scope, root) in home_rows {
            let Some(root) = root else { continue };
            if path == root {
                found.push(Candidate::new(scope, Position::Root, Side::Home));
            } else if path.starts_with(root) {
                found.push(Candidate::new(scope, Position::Below, Side::Home));
            }
        }
        // An `install-skill` run's folders strictly between its anchor and its skill folder
        // (the default's `$HOME/.claude/skills`): created only, by `check`'s rule for any
        // position but `Below`.
        if let (Some(anchor), Some(skill)) = (&self.skill_anchor, self.skills.first())
            && path != skill.as_path()
            && skill.starts_with(path)
            && path != anchor.as_path()
            && path.starts_with(anchor)
        {
            found.push(Candidate::new(Scope::Skill, Position::Ancestor, Side::Home));
        }
        for client in &self.client_dirs {
            let Some(c) = client_components(client, path) else {
                continue;
            };
            let side = || Side::Client(client.clone());
            let first = c.first().copied();
            // For `Interface/AddOns` and `Interface/.omaddons` (two components) the path
            // is the row's root; deeper paths are below it.
            let position = if c.len() == 2 {
                Position::Root
            } else {
                Position::Below
            };
            match c.len() {
                0 => {}
                _ if first.is_some_and(|s| eq_ci(s, "WTF")) => {
                    let position = if c.len() == 1 {
                        Position::Root
                    } else {
                        Position::Below
                    };
                    found.push(Candidate::new(Scope::Wtf, position, side()));
                    if let Some(folder) = saved_variables_folder(&c) {
                        found.push(Candidate {
                            saved_variables_of: Some(folder),
                            ..Candidate::new(Scope::SavedVariables, Position::Below, side())
                        });
                    }
                }
                _ if !first.is_some_and(|s| eq_ci(s, "Interface")) => {}
                1 => found.push(Candidate::new(Scope::ClientMeta, Position::Root, side())),
                _ if c.get(1).is_some_and(|s| eq_ci(s, "AddOns")) => {
                    found.push(Candidate::new(Scope::AddOns, position, side()));
                }
                _ if is_meta_prefix(&c) => {
                    found.push(Candidate::new(Scope::ClientMeta, position, side()));
                }
                _ => {}
            }
        }
        found
    }
}

/// The deny-list's AddOns rule: the first component under any `Interface/AddOns` must
/// be a valid, non-protected folder name. One exception, for the doctor alone: a
/// flatten's provisional folder, `.omaddons-flatten-<tx>` exactly (M4-D31), which no
/// valid folder name can spell.
fn check_protected(path: &Path, writer: Writer) -> std::result::Result<(), Refusal> {
    let components: Vec<&OsStr> = normal_components(path).collect();
    for window in components.windows(3) {
        let [interface, addons, folder] = window else {
            continue;
        };
        if !(eq_ci(interface, "Interface") && eq_ci(addons, "AddOns")) {
            continue;
        }
        if writer == Writer::Doctor
            && folder
                .to_str()
                .is_some_and(|n| flatten_provisional_tx(n).is_some())
        {
            continue;
        }
        let name = folder.to_string_lossy();
        match FolderName::new(&name) {
            Err(err) => {
                return Err(Refusal::ScopeDenied(format!("{}: {err}", path.display())));
            }
            Ok(folder) if folder.is_protected() => {
                return Err(Refusal::Protected(format!(
                    "{} is inside {folder}, which belongs to the client and is never modified",
                    path.display()
                )));
            }
            Ok(_) => {}
        }
    }
    Ok(())
}

/// For `WTF/Account/<1 or more>/SavedVariables/<Folder>.lua[.bak]`, the folder name.
fn saved_variables_folder(c: &[&OsStr]) -> Option<String> {
    let [wtf, account, middle @ .., saved, file] = c else {
        return None;
    };
    if middle.is_empty()
        || !eq_ci(wtf, "WTF")
        || !eq_ci(account, "Account")
        || !eq_ci(saved, "SavedVariables")
    {
        return None;
    }
    let file = file.to_str()?;
    let lower = file.to_ascii_lowercase();
    let stem_len = [".lua.bak", ".lua"]
        .into_iter()
        .find_map(|suffix| lower.strip_suffix(suffix).map(str::len))?;
    file.get(..stem_len)
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
}

/// `[Interface, .omaddons, …]`, ASCII case-insensitive.
fn is_meta_prefix(c: &[&OsStr]) -> bool {
    matches!(c, [interface, meta, ..] if eq_ci(interface, "Interface") && eq_ci(meta, ".omaddons"))
}

/// The components of `path` below `client`, if it is inside it.
fn client_components<'a>(client: &Path, path: &'a Path) -> Option<Vec<&'a OsStr>> {
    path.strip_prefix(client)
        .ok()
        .map(|rel| normal_components(rel).collect())
}

fn normal_components(path: &Path) -> impl Iterator<Item = &OsStr> {
    path.components().filter_map(|c| match c {
        Component::Normal(name) => Some(name),
        _ => None,
    })
}

fn eq_ci(name: &OsStr, expected: &str) -> bool {
    name.as_encoded_bytes()
        .eq_ignore_ascii_case(expected.as_bytes())
}

/// Canonicalizes `path` through its deepest existing ancestor, appending the missing
/// rest, so roots that do not exist yet still compare against canonical targets.
fn canonical(path: &Path) -> Result<PathBuf> {
    let fail = |err: &io::Error| Error::io(path, err, Suggestion::command(["omaddons", "status"]));
    if !path.is_absolute() {
        let err = io::Error::new(io::ErrorKind::InvalidInput, "path is not absolute");
        return Err(fail(&err));
    }
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(mut base) => {
                base.extend(missing.iter().rev());
                return Ok(base);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                match (existing.parent(), existing.file_name()) {
                    (Some(parent), Some(name)) => {
                        missing.push(name.to_owned());
                        existing = parent;
                    }
                    _ => return Err(fail(&err)),
                }
            }
            Err(err) => return Err(fail(&err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roots under a directory that does not exist, so canonicalization keeps them.
    const ROOT: &str = "/omaddons-scope-test-nonexistent";

    fn table() -> ScopeTable {
        let paths = Paths::with_roots(
            format!("{ROOT}/home"),
            format!("{ROOT}/home/.config"),
            format!("{ROOT}/home/.local/state"),
            format!("{ROOT}/home/.cache"),
        );
        let clients = [PathBuf::from(format!("{ROOT}/games/wow/_retail_"))];
        ScopeTable::new(&paths, &clients, None).unwrap()
    }

    fn p(rel: &str) -> PathBuf {
        PathBuf::from(format!("{ROOT}/{rel}"))
    }

    fn owned(names: &[&str]) -> OwnedFolders {
        let folders: Vec<FolderName> = names.iter().map(|n| FolderName::new(n).unwrap()).collect();
        OwnedFolders::new(&folders)
    }

    const ALL: [Writer; 11] = [
        Writer::Config,
        Writer::Auth,
        Writer::Mutating,
        Writer::Undo,
        Writer::Derived,
        Writer::Backup,
        Writer::BackupRestore,
        Writer::Omarchy,
        Writer::Skill,
        Writer::Doctor,
        Writer::Completions,
    ];

    /// Every writer, and whether each may write `path` with `access`.
    fn allowed(path: &str, access: Access, owned_set: &OwnedFolders) -> Vec<Writer> {
        let t = table();
        ALL.into_iter()
            .filter(|&w| t.check(w, owned_set, &p(path), access).is_ok())
            .collect()
    }

    #[test]
    fn with_skill_dirs_admits_each_dir_for_skill_and_doctor_only() {
        let t = table()
            .with_skill_dirs(&[p("home/.codex/skills/omaddons"), p("work/skills/omaddons")])
            .unwrap();
        let none = owned(&[]);
        for (case, path, want) in [
            (
                "default_dir",
                "home/.claude/skills/omaddons/SKILL.md",
                vec![Writer::Skill, Writer::Doctor],
            ),
            (
                "second_dir",
                "home/.codex/skills/omaddons/SKILL.md",
                vec![Writer::Skill, Writer::Doctor],
            ),
            (
                "third_dir",
                "work/skills/omaddons/SKILL.md",
                vec![Writer::Skill, Writer::Doctor],
            ),
            ("sibling", "work/skills/other/SKILL.md", vec![]),
        ] {
            let got: Vec<Writer> = ALL
                .into_iter()
                .filter(|&w| t.check(w, &none, &p(path), Access::Write).is_ok())
                .collect();
            assert_eq!(got, want, "{case}: {path} → {got:?}");
        }
    }

    #[test]
    fn with_skill_dirs_refuses_more_than_skill_dirs_max() {
        // The default dir counts: SKILL_DIRS_MAX - 1 more fit, a duplicate counts once.
        let dirs: Vec<PathBuf> = (1..SKILL_DIRS_MAX)
            .map(|n| p(&format!("work/{n}/skills/omaddons")))
            .collect();
        let mut with_duplicate = dirs.clone();
        with_duplicate.push(dirs[0].clone());
        assert!(
            table().with_skill_dirs(&with_duplicate).is_ok(),
            "{SKILL_DIRS_MAX} in all"
        );
        let mut one_more = dirs;
        one_more.push(p("work/extra/skills/omaddons"));
        let refused = table().with_skill_dirs(&one_more).unwrap_err();
        assert_eq!(refused.code, ErrorCode::ScopeDenied, "{}", refused.message);
    }

    #[test]
    fn is_quarantine_recognizes_exactly_the_three_roots() {
        let t = table();
        let state = "home/.local/state/omaddons";
        let tx = "01JB8Q2VA00000000000000000";
        let snapshots = "games/wow/_retail_/Interface/.omaddons/snapshots";
        for (case, path, want) in [
            (
                "run_quarantine",
                format!("{state}/doctor/runs/R/quarantine/state/x"),
                true,
            ),
            (
                "cache_quarantine",
                "home/.cache/omaddons/quarantine/R/blobs/x".to_owned(),
                true,
            ),
            (
                "client_quarantine",
                format!("{snapshots}/{tx}.repair/quarantine/Interface/AddOns/X"),
                true,
            ),
            (
                "run_backups",
                format!("{state}/doctor/runs/R/backups/1"),
                false,
            ),
            (
                "run_root",
                format!("{state}/doctor/runs/R/quarantine"),
                false,
            ),
            (
                "cache_root",
                "home/.cache/omaddons/quarantine/R".to_owned(),
                false,
            ),
            (
                "plain_snapshot",
                format!("{snapshots}/{tx}/quarantine/x"),
                false,
            ),
            (
                "wtf_snapshot",
                format!("{snapshots}/{tx}.wtf/quarantine/x"),
                false,
            ),
            (
                "repair_files",
                format!("{snapshots}/{tx}.repair/files/1"),
                false,
            ),
            (
                "unregistered_client",
                format!(
                    "games/wow/_classic_/Interface/.omaddons/snapshots/{tx}.repair/quarantine/x"
                ),
                false,
            ),
            ("elsewhere", "home/x".to_owned(), false),
        ] {
            let path = p(&path);
            assert_eq!(t.is_quarantine(&path), want, "{case}: {}", path.display());
        }
    }

    #[test]
    fn an_export_file_is_one_path_for_backup_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mk = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let home = mk("home");
        let client = mk("games/wow/_retail_");
        let usb = mk("usb");
        mk("prefix/drive_c/users/me");
        mk("prefix/dosdevices");
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        for app in [paths.config_dir(), paths.state_dir(), paths.cache_dir()] {
            std::fs::create_dir_all(app).unwrap();
        }
        let mut t = ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        for (dest, why) in [
            (client.join("WTF.tar.zst"), "inside the game client"),
            (
                root.join("prefix/drive_c/users/me/w.tar.zst"),
                "inside the Wine prefix",
            ),
            (paths.config_dir().join("w.tar.zst"), "own config folder"),
            (paths.state_dir().join("w.tar.zst"), "own state folder"),
            (paths.cache_dir().join("w.tar.zst"), "own cache folder"),
        ] {
            let err = t.with_export_file(&dest).unwrap_err();
            assert_eq!(err.code, ErrorCode::ScopeDenied, "{dest:?}");
            assert!(err.message.contains(why), "{}", err.message);
        }
        let err = t
            .with_export_file(&root.join("nope/w.tar.zst"))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Io, "a missing parent is never created");

        let file = t.with_export_file(&usb.join("w.tar.zst")).unwrap();
        assert_eq!(file, usb.join("w.tar.zst"));
        let none = OwnedFolders::default();
        for writer in ALL {
            assert_eq!(
                t.check(writer, &none, &file, Access::Write).is_ok(),
                writer == Writer::Backup,
                "{writer}"
            );
        }
        for other in [usb.join("other"), usb.clone(), root.join("w.tar.zst")] {
            for access in [Access::Write, Access::CreateDir, Access::Rename] {
                assert!(
                    t.check(Writer::Backup, &none, &other, access).is_err(),
                    "{other:?} {access:?}"
                );
            }
        }
    }

    #[test]
    fn a_completion_file_is_one_path_and_its_missing_folders_for_completions_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mk = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let home = mk("home");
        let data = mk("home/.local/share");
        let client = mk("games/wow/_retail_");
        mk("prefix/drive_c/users/me");
        mk("prefix/dosdevices");
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        for app in [paths.config_dir(), paths.state_dir(), paths.cache_dir()] {
            std::fs::create_dir_all(app).unwrap();
        }
        let base = || ScopeTable::new(&paths, std::slice::from_ref(&client), None).unwrap();
        for (case, file, create_from, code, why) in [
            (
                "missing_base",
                root.join("nope/bash-completion/completions/omaddons"),
                root.join("nope"),
                ErrorCode::NotFound,
                "is not a folder",
            ),
            (
                "inside_a_client",
                client.join("fish/completions/omaddons.fish"),
                client.clone(),
                ErrorCode::ScopeDenied,
                "inside the game client",
            ),
            (
                "inside_a_prefix",
                root.join("prefix/drive_c/users/me/.zfunc/_omaddons"),
                root.join("prefix/drive_c/users/me"),
                ErrorCode::ScopeDenied,
                "inside the Wine prefix",
            ),
            (
                "inside_own_config",
                paths.config_dir().join("fish/completions/omaddons.fish"),
                home.join(".config"),
                ErrorCode::ScopeDenied,
                "own config folder",
            ),
        ] {
            let err = base()
                .with_completion_file(&file, &create_from)
                .unwrap_err();
            assert_eq!(err.code, code, "{case}: {err}");
            assert!(err.message.contains(why), "{case}: {}", err.message);
        }

        let file = data.join("bash-completion/completions/omaddons");
        let t = base().with_completion_file(&file, &data).unwrap();
        assert_eq!(t.completion_file(), Some(file.as_path()));
        let none = OwnedFolders::default();
        for writer in ALL {
            assert_eq!(
                t.check(writer, &none, &file, Access::Write).is_ok(),
                writer == Writer::Completions,
                "{writer}: the file"
            );
        }
        for (case, folder, create) in [
            ("its_folder", data.join("bash-completion/completions"), true),
            ("a_missing_ancestor", data.join("bash-completion"), true),
            ("the_existing_base", data.clone(), false),
            ("a_sibling", data.join("bash-completion/other"), false),
            ("another_shells_folder", home.join(".zfunc"), false),
        ] {
            assert_eq!(
                t.check(Writer::Completions, &none, &folder, Access::CreateDir)
                    .is_ok(),
                create,
                "{case}: create {folder:?}"
            );
            for access in [Access::Write, Access::Rename] {
                assert!(
                    t.check(Writer::Completions, &none, &folder, access)
                        .is_err(),
                    "{case}: {access:?} {folder:?}"
                );
            }
        }
        for other in [
            data.join("bash-completion/completions/other"),
            data.join("omaddons"),
            paths.skill_dir().join("SKILL.md"),
            paths.history_file(),
            paths.config_file(),
        ] {
            assert!(
                t.check(Writer::Completions, &none, &other, Access::Write)
                    .is_err(),
                "{other:?}"
            );
        }
    }

    // -----------------------------------------------------------------------------------
    // `install-skill`'s one chosen skill folder (M4-D8).

    /// A real home under a tempdir: a registered client dir outside any prefix, a Wine
    /// prefix, the three app dirs and `$HOME/.claude`.
    struct Real {
        _dir: tempfile::TempDir,
        root: PathBuf,
        paths: Paths,
        client: PathBuf,
    }

    fn real() -> Real {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mk = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        };
        let home = mk("home");
        let client = mk("games/wow/_retail_/Interface/AddOns")
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        mk("prefix/drive_c/users/me");
        mk("prefix/dosdevices");
        mk("home/.claude");
        let paths = Paths::with_roots(
            &home,
            home.join(".config"),
            home.join(".local/state"),
            home.join(".cache"),
        );
        for app in [paths.config_dir(), paths.state_dir(), paths.cache_dir()] {
            std::fs::create_dir_all(app).unwrap();
        }
        Real {
            _dir: dir,
            root,
            paths,
            client,
        }
    }

    impl Real {
        fn table(&self) -> ScopeTable {
            ScopeTable::new(&self.paths, std::slice::from_ref(&self.client), None).unwrap()
        }

        fn at(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        /// Every entry under the root, by relative path and kind, no link followed.
        fn listing(&self) -> BTreeSet<String> {
            fn walk(base: &Path, dir: &Path, found: &mut BTreeSet<String>) {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let path = entry.unwrap().path();
                    let meta = std::fs::symlink_metadata(&path).unwrap();
                    let rel = path.strip_prefix(base).unwrap().display().to_string();
                    let kind = if meta.file_type().is_symlink() {
                        "L"
                    } else if meta.is_dir() {
                        "D"
                    } else {
                        "F"
                    };
                    found.insert(format!("{kind} {rel}"));
                    if meta.is_dir() {
                        walk(base, &path, found);
                    }
                }
            }
            let mut found = BTreeSet::new();
            walk(&self.root, &self.root, &mut found);
            found
        }
    }

    /// The writers of `ALL` that `t` lets write `path` with `access`.
    fn writers(t: &ScopeTable, path: &Path, access: Access) -> Vec<Writer> {
        let none = OwnedFolders::default();
        ALL.into_iter()
            .filter(|&w| t.check(w, &none, path, access).is_ok())
            .collect()
    }

    #[test]
    fn the_history_row_admits_the_skill_writer() {
        let t = table();
        let none = OwnedFolders::default();
        let history = p("home/.local/state/omaddons/history.jsonl");
        let matched = t.check(Writer::Skill, &none, &history, Access::Write);
        assert_eq!(matched.map(|m| m.scope), Ok(Scope::History));
        let registry = p("home/.local/state/omaddons/registry.json");
        assert!(
            t.check(Writer::Skill, &none, &registry, Access::Write)
                .is_err(),
            "the rest of the state dir stays refused"
        );
    }

    #[test]
    fn a_chosen_skill_dir_replaces_the_slot_for_skill_and_doctor_only() {
        use Writer as W;
        let home = real();
        let parent = home.at("home/.codex/skills");
        std::fs::create_dir_all(&parent).unwrap();
        let dir = parent.join("omaddons");
        let t = home.table().with_skill_dir(&dir, &parent).unwrap();
        assert_eq!(t.skill_dir(), dir.as_path());
        let cases = [
            (
                "skill_file",
                dir.join("SKILL.md"),
                Access::Write,
                vec![W::Skill, W::Doctor],
            ),
            (
                "backup_rename",
                dir.join("SKILL.md.bak"),
                Access::Rename,
                vec![W::Skill, W::Doctor],
            ),
            // omad-syc2: a later edit's copy, never over an earlier backup.
            (
                "numbered_backup_rename",
                dir.join("SKILL.md.bak.2"),
                Access::Rename,
                vec![W::Skill, W::Doctor],
            ),
            (
                "the_folder_created",
                dir.clone(),
                Access::CreateDir,
                vec![W::Skill, W::Doctor],
            ),
            ("the_folder_written", dir.clone(), Access::Write, vec![]),
            (
                "the_default_folder",
                home.at("home/.claude/skills/omaddons/SKILL.md"),
                Access::Write,
                vec![],
            ),
            (
                "a_sibling",
                parent.join("other/SKILL.md"),
                Access::Write,
                vec![],
            ),
        ];
        for (case, path, access, want) in cases {
            let got = writers(&t, &path, access);
            assert_eq!(got, want, "{case}: {} {access:?} → {got:?}", path.display());
        }
    }

    #[test]
    fn skill_dirs_inside_clients_prefixes_or_own_dirs_are_refused() {
        let home = real();
        std::fs::create_dir_all(home.paths.state_dir().join("x")).unwrap();
        std::os::unix::fs::symlink(&home.client, home.at("home/link")).unwrap();
        std::os::unix::fs::symlink(home.paths.state_dir().join("x"), home.at("home/sk")).unwrap();
        let client = home.client.clone();
        let cases: Vec<(&str, PathBuf, PathBuf, &str)> = vec![
            (
                "client_dir_itself",
                client.clone(),
                client.clone(),
                "is inside the game client",
            ),
            (
                "inside_client_interface",
                client.join("Interface/skills"),
                client.join("Interface/skills"),
                "is inside the game client",
            ),
            (
                "inside_wine_prefix",
                home.at("prefix/drive_c/users/me/skills"),
                home.at("prefix/drive_c/users/me/skills"),
                "is inside the Wine prefix",
            ),
            (
                "inside_config",
                home.paths.config_dir().join("skills"),
                home.paths.config_dir().join("skills"),
                "is inside omaddons' own config folder",
            ),
            (
                "inside_state",
                home.paths.state_dir().join("skills"),
                home.paths.state_dir().join("skills"),
                "is inside omaddons' own state folder",
            ),
            (
                "inside_cache",
                home.paths.cache_dir().join("skills"),
                home.paths.cache_dir().join("skills"),
                "is inside omaddons' own cache folder",
            ),
            (
                "through_a_symlinked_ancestor",
                home.at("home/link/skills"),
                client.join("skills"),
                "is inside the game client",
            ),
            (
                "symlinked_dir_into_state",
                home.at("home/sk"),
                home.paths.state_dir().join("x"),
                "is inside omaddons' own state folder",
            ),
            (
                "holding_a_client",
                home.at("games"),
                home.at("games"),
                "holds the game client",
            ),
            (
                "holding_the_state_dir",
                home.at("home/.local"),
                home.at("home/.local"),
                "holds omaddons' own state folder",
            ),
        ];
        for (case, dir, canonical, why) in cases {
            let before = home.listing();
            let err = home.table().with_skill_dir(&dir, &home.root).unwrap_err();
            assert_eq!(err.code, ErrorCode::ScopeDenied, "{case}: {}", err.message);
            assert!(err.message.contains(why), "{case}: {}", err.message);
            assert_eq!(
                err.details,
                serde_json::json!({
                    "reason": "skill_dir",
                    "path": canonical.display().to_string(),
                }),
                "{case}"
            );
            assert!(
                err.code.check_details(&err.details).is_ok(),
                "{case}: the details fit scope_denied's spec: {}",
                err.details
            );
            let fix = err.fix.display();
            assert!(
                fix.contains('<') && !fix.contains("omaddons "),
                "{case}: a prose fix naming no runnable command: {fix}"
            );
            assert_eq!(home.listing(), before, "{case}: nothing was created");
        }
        let planted = serde_json::json!({"reason": "skill_dirs", "path": "/x"});
        assert!(
            ErrorCode::ScopeDenied.check_details(&planted).is_err(),
            "an unknown reason is refused by the spec"
        );
    }

    #[test]
    fn a_missing_create_from_is_not_found() {
        let home = real();
        std::fs::write(home.at("a-file"), b"").unwrap();
        for (case, create_from, why) in [
            ("missing", home.at("nope"), "does not exist"),
            ("a_file", home.at("a-file"), "is not a folder"),
        ] {
            let before = home.listing();
            let err = home
                .table()
                .with_skill_dir(&create_from.join("omaddons"), &create_from)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::NotFound, "{case}: {}", err.message);
            assert!(err.message.contains(why), "{case}: {}", err.message);
            assert_eq!(
                err.details,
                serde_json::json!({"path": create_from.display().to_string()}),
                "{case}"
            );
            assert!(
                err.code.check_details(&err.details).is_ok(),
                "{case}: the details fit not_found's spec: {}",
                err.details
            );
            assert_eq!(home.listing(), before, "{case}: nothing was created");
        }
    }

    #[test]
    fn only_folders_between_the_anchor_and_the_skill_dir_are_creatable() {
        use Access::{CreateDir, Rename, Write};
        let home = real();
        let claude = home.at("home/.claude");
        let t = home
            .table()
            .with_skill_dir(&claude.join("skills/omaddons"), &claude)
            .unwrap();
        let cases = [
            ("the_anchor", "home/.claude", CreateDir, false),
            ("skills_created", "home/.claude/skills", CreateDir, true),
            ("skills_written", "home/.claude/skills", Write, false),
            ("skills_renamed", "home/.claude/skills", Rename, false),
            (
                "another_skill",
                "home/.claude/skills/other",
                CreateDir,
                false,
            ),
            (
                "the_folder_created",
                "home/.claude/skills/omaddons",
                CreateDir,
                true,
            ),
            (
                "the_folder_written",
                "home/.claude/skills/omaddons",
                Write,
                false,
            ),
            (
                "the_skill_file",
                "home/.claude/skills/omaddons/SKILL.md",
                Write,
                true,
            ),
            (
                "claude_settings",
                "home/.claude/settings.json",
                Write,
                false,
            ),
        ];
        let none = OwnedFolders::default();
        for (case, rel, access, want) in cases {
            let path = home.at(rel);
            let got = t.check(Writer::Skill, &none, &path, access);
            assert_eq!(got.is_ok(), want, "{case}: {rel} {access:?} → {got:?}");
        }
        for writer in [Writer::Mutating, Writer::Config, Writer::Omarchy] {
            assert!(
                t.check(writer, &none, &home.at("home/.claude/skills"), CreateDir)
                    .is_err(),
                "{writer} never creates it"
            );
        }

        // A `--dir` whose parent is the anchor: only the folder itself is created.
        let work = home.at("work/skills");
        std::fs::create_dir_all(&work).unwrap();
        let t = home
            .table()
            .with_skill_dir(&work.join("omaddons"), &work)
            .unwrap();
        for (case, rel, want) in [
            ("custom_folder", "work/skills/omaddons", true),
            ("custom_anchor", "work/skills", false),
            ("above_the_anchor", "work", false),
            ("default_skills", "home/.claude/skills", false),
        ] {
            let got = t.check(Writer::Skill, &none, &home.at(rel), CreateDir);
            assert_eq!(got.is_ok(), want, "{case}: {rel} → {got:?}");
        }
    }

    #[test]
    fn a_symlinked_skill_dir_is_judged_where_it_points() {
        let home = real();
        let real_skill = home.at("home/real-skill");
        std::fs::create_dir_all(&real_skill).unwrap();
        std::fs::create_dir_all(home.at("home/.claude/skills")).unwrap();
        let link = home.at("home/.claude/skills/omaddons");
        std::os::unix::fs::symlink(&real_skill, &link).unwrap();
        let t = home
            .table()
            .with_skill_dir(&link, &home.at("home/.claude"))
            .unwrap();
        assert_eq!(t.skill_dir(), real_skill.as_path());
        assert_eq!(
            writers(&t, &real_skill.join("SKILL.md"), Access::Write),
            vec![Writer::Skill, Writer::Doctor]
        );
    }

    #[test]
    fn without_with_skill_dir_the_table_is_unchanged() {
        let t = table();
        assert_eq!(t.skill_dir(), p("home/.claude/skills/omaddons").as_path());
        assert_eq!(
            writers(
                &t,
                &p("home/.claude/skills/omaddons/SKILL.md"),
                Access::Write
            ),
            vec![Writer::Skill, Writer::Doctor]
        );
        assert_eq!(
            writers(&t, &p("home/.claude/skills"), Access::CreateDir),
            vec![],
            "no folder above the skill folder is creatable"
        );
    }

    #[test]
    fn rows_admit_exactly_their_writers() {
        use Writer as W;
        let none = OwnedFolders::default();
        let cases: Vec<(&str, Vec<Writer>)> = vec![
            (
                "home/.config/omaddons/config.toml",
                vec![W::Config, W::Auth, W::Doctor],
            ),
            (
                "home/.local/state/omaddons/registry.json",
                vec![W::Mutating, W::Undo, W::Backup, W::BackupRestore, W::Doctor],
            ),
            (
                "home/.local/state/omaddons/history.jsonl",
                vec![
                    W::Config,
                    W::Auth,
                    W::Mutating,
                    W::Undo,
                    W::Backup,
                    W::BackupRestore,
                    W::Omarchy,
                    W::Skill,
                    W::Doctor,
                ],
            ),
            ("home/.local/state/omaddons/net.log", ALL.to_vec()),
            ("home/.local/state/omaddons/net.log.1", ALL.to_vec()),
            ("home/.cache/omaddons/blobs/x", ALL.to_vec()),
            (
                "games/wow/_retail_/Interface/AddOns/WeakAuras/x.lua",
                vec![W::Mutating, W::Undo, W::Doctor],
            ),
            (
                "games/wow/_retail_/Interface/.omaddons/stage/T/x",
                vec![W::Mutating, W::Undo, W::Backup, W::BackupRestore, W::Doctor],
            ),
            ("games/wow/_retail_/WTF/Config.wtf", vec![W::BackupRestore]),
            (
                "home/.config/omarchy/plugins/a44.omaddons/Panel.qml",
                vec![W::Omarchy, W::Doctor],
            ),
            (
                "home/.config/omarchy/plugins/.a44.omaddons.stage-01JB8Q2VA00000000000000000/x",
                vec![W::Omarchy, W::Doctor],
            ),
            ("home/.config/omarchy/plugins/.a44.omaddons.stage/x", vec![]),
            (
                "home/.config/omarchy/plugins/.omaddons.stage-01JB8Q2VA00000000000000000/x",
                vec![],
            ),
            ("home/.config/omarchy/plugins/beads/Panel.qml", vec![]),
            (
                "home/.claude/skills/omaddons/SKILL.md",
                vec![W::Skill, W::Doctor],
            ),
            ("games/wow/_retail_/Wow.exe", vec![]),
            ("games/wow/_retail_/Data/data.001", vec![]),
            ("games/wow/Battle.net.exe", vec![]),
            ("home/elsewhere", vec![]),
            ("home/.config/other-tool/x", vec![]),
        ];
        for (path, writers) in cases {
            assert_eq!(allowed(path, Access::Write, &none), writers, "{path}");
        }
    }

    /// The widget's entries under `<plugins>` (M3-D38): only `Omarchy` renames one as a
    /// whole (the swap, a stage into the trash), anything inside one is an ordinary
    /// write, `<plugins>` itself may only be created, and a name that is not exactly one
    /// of them (a fixed stage, a stage without a ULID, another plugin) is no one's.
    #[test]
    fn only_the_widget_renames_its_entries() {
        use Writer as W;
        let none = OwnedFolders::default();
        let plugins = "home/.config/omarchy/plugins";
        let stage = format!("{plugins}/.a44.omaddons.stage-01JB8Q2VA00000000000000000");
        let old = format!("{plugins}/.a44.omaddons.old-01JB8Q2VA00000000000000000");
        let trash = format!("{plugins}/.a44.omaddons.trash");
        let widget = format!("{plugins}/a44.omaddons");
        // Where builds before omad-o8g4 installed it: an install parks a widget left there.
        let legacy = format!("{plugins}/omaddons");
        for root in [&widget, &legacy, &stage, &old, &trash] {
            assert_eq!(
                allowed(root, Access::Rename, &none),
                vec![W::Omarchy],
                "{root}"
            );
            assert_eq!(allowed(root, Access::Write, &none), vec![], "{root}");
            assert_eq!(
                allowed(root, Access::CreateDir, &none),
                vec![W::Omarchy, W::Doctor],
                "{root}"
            );
            assert_eq!(
                allowed(&format!("{root}/r-0123/Service.qml"), Access::Write, &none),
                vec![W::Omarchy, W::Doctor],
                "{root}"
            );
        }
        assert_eq!(
            allowed(plugins, Access::CreateDir, &none),
            vec![W::Omarchy, W::Doctor]
        );
        for refused in [Access::Write, Access::Rename] {
            assert_eq!(allowed(plugins, refused, &none), vec![], "{refused:?}");
        }
        for stranger in [
            format!("{plugins}/.a44.omaddons.stage"),
            format!("{plugins}/.a44.omaddons.stage-notaulid"),
            format!("{plugins}/.a44.omaddons.old-01JB8Q2VA00000000000000000x"),
            format!("{plugins}/beads"),
            format!("{plugins}/.a44.omaddons.trash2"),
            format!("{plugins}/omaddons2"),
            format!("{plugins}/.omaddons.trash"),
        ] {
            for access in [Access::Write, Access::Rename, Access::CreateDir] {
                assert_eq!(
                    allowed(&stranger, access, &none),
                    vec![],
                    "{stranger} {access:?}"
                );
            }
        }
        // Its history line goes through the History row.
        let t = table();
        let history = p("home/.local/state/omaddons/history.jsonl");
        let decided = t.check(W::Omarchy, &none, &history, Access::Write);
        assert_eq!(decided.map(|m| m.scope), Ok(Scope::History));
    }

    /// A whole restore exchanges `WTF` itself (M3-D24): only `BackupRestore` may rename
    /// the root, and nothing may write it otherwise. Its history line is admitted by the
    /// `State` row, which comes before the narrower `History` row.
    #[test]
    fn only_a_restore_renames_the_wtf_root() {
        use Writer as W;
        let none = OwnedFolders::default();
        let root = "games/wow/_retail_/WTF";
        assert_eq!(allowed(root, Access::Rename, &none), vec![W::BackupRestore]);
        assert_eq!(allowed(root, Access::Write, &none), vec![]);
        for writer in [W::Mutating, W::Undo, W::Doctor] {
            assert!(
                !allowed(root, Access::Rename, &none).contains(&writer),
                "{writer:?}"
            );
        }
        // Other roots still take no rename, a restore's included.
        for other in [
            "games/wow/_retail_/Interface/AddOns",
            "games/wow/_retail_/Interface/.omaddons",
            "home/.local/state/omaddons",
        ] {
            assert_eq!(allowed(other, Access::Rename, &none), vec![], "{other}");
        }
        // Below a root a rename is a write like any other.
        assert_eq!(
            allowed(
                "games/wow/_retail_/Interface/.omaddons/stage/T/WTF",
                Access::Rename,
                &none
            ),
            vec![W::Mutating, W::Undo, W::Backup, W::BackupRestore, W::Doctor]
        );
        let t = table();
        let history = p("home/.local/state/omaddons/history.jsonl");
        let decided = t.check(W::BackupRestore, &none, &history, Access::Write);
        assert_eq!(decided.map(|m| m.scope), Ok(Scope::State));
    }

    #[test]
    fn roots_and_bases_accept_only_directory_creation() {
        use Writer as W;
        let none = OwnedFolders::default();
        for (path, writers) in [
            (
                "games/wow/_retail_/Interface",
                vec![W::Mutating, W::Undo, W::Backup, W::BackupRestore, W::Doctor],
            ),
            (
                "games/wow/_retail_/Interface/AddOns",
                vec![W::Mutating, W::Undo, W::Doctor],
            ),
            (
                "games/wow/_retail_/Interface/.omaddons",
                vec![W::Mutating, W::Undo, W::Backup, W::BackupRestore, W::Doctor],
            ),
            // net.log's directories are creatable by every writer, since any command
            // may log a request.
            ("home/.local/state/omaddons", ALL.to_vec()),
            ("home/.local/state", ALL.to_vec()),
            ("home/.cache", ALL.to_vec()),
        ] {
            assert_eq!(allowed(path, Access::CreateDir, &none), writers, "{path}");
            assert_eq!(allowed(path, Access::Write, &none), vec![], "{path}");
        }
        assert_eq!(
            allowed("games/wow/_retail_", Access::CreateDir, &none),
            vec![]
        );
        assert_eq!(allowed("home", Access::CreateDir, &none), vec![]);
    }

    #[test]
    fn components_below_a_client_dir_match_case_insensitively() {
        let none = OwnedFolders::default();
        for path in [
            "games/wow/_retail_/interface/addons/Foo/x.lua",
            "games/wow/_retail_/INTERFACE/AddOns/Foo",
            "games/wow/_retail_/Interface/.OMADDONS/target.json",
        ] {
            assert!(
                allowed(path, Access::Write, &none).contains(&Writer::Mutating),
                "{path}"
            );
        }
    }

    #[test]
    fn protected_folders_are_refused_for_every_writer_in_any_case() {
        let t = table();
        let none = OwnedFolders::default();
        for folder in ["Blizzard_Foo", "BLIZZARD_FOO", "blizzard_foo"] {
            for rel in [
                format!("games/wow/_retail_/Interface/AddOns/{folder}"),
                format!("games/wow/_retail_/Interface/AddOns/{folder}/x.lua"),
                format!("games/unregistered/Interface/AddOns/{folder}/x.lua"),
            ] {
                for writer in ALL {
                    for access in [Access::Write, Access::CreateDir] {
                        let refusal = t.check(writer, &none, &p(&rel), access).unwrap_err();
                        assert!(matches!(refusal, Refusal::Protected(_)), "{rel} {writer}");
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_folder_names_under_addons_are_refused() {
        let t = table();
        let refusal = t
            .check(
                Writer::Mutating,
                &OwnedFolders::default(),
                &p("games/wow/_retail_/Interface/AddOns/.hidden/x"),
                Access::Write,
            )
            .unwrap_err();
        assert!(matches!(refusal, Refusal::ScopeDenied(_)), "{refusal:?}");
    }

    #[test]
    fn a_flattens_provisional_folder_is_the_doctors_alone() {
        let tx = "01JB8Q2VA00000000000000000";
        let addons = "games/wow/_retail_/Interface/AddOns";
        let none = owned(&[]);
        for (case, rel, access, want) in [
            (
                "provisional",
                format!("{addons}/.omaddons-flatten-{tx}"),
                Access::Rename,
                vec![Writer::Doctor],
            ),
            (
                "below_it",
                format!("{addons}/.omaddons-flatten-{tx}/Same"),
                Access::Rename,
                vec![Writer::Doctor],
            ),
            (
                "lowercase_id",
                format!("{addons}/.omaddons-flatten-{}", tx.to_ascii_lowercase()),
                Access::Rename,
                vec![],
            ),
            (
                "not_an_id",
                format!("{addons}/.omaddons-flatten-Same"),
                Access::Rename,
                vec![],
            ),
            (
                "other_temp_name",
                format!("{addons}/.omaddons-{tx}"),
                Access::Rename,
                vec![],
            ),
            (
                "protected_below_it",
                format!("{addons}/.omaddons-flatten-{tx}/Interface/AddOns/Blizzard_Foo"),
                Access::Write,
                vec![],
            ),
            (
                "unregistered_client",
                format!("games/unregistered/Interface/AddOns/.omaddons-flatten-{tx}"),
                Access::Rename,
                vec![],
            ),
        ] {
            let got = allowed(&rel, access, &none);
            assert_eq!(got, want, "{case}: {rel} → {got:?}");
        }
        let refusal = table()
            .check(
                Writer::Mutating,
                &none,
                &p(&format!("{addons}/.omaddons-flatten-{tx}")),
                Access::Rename,
            )
            .unwrap_err();
        assert!(
            matches!(&refusal, Refusal::ScopeDenied(m) if m.contains("starts with a dot")),
            "another writer meets the folder-name rule: {refusal:?}"
        );
    }

    #[test]
    fn the_lock_file_is_never_writable() {
        let t = table();
        for writer in ALL {
            for access in [Access::Write, Access::CreateDir] {
                let refusal = t
                    .check(
                        writer,
                        &OwnedFolders::default(),
                        &p("home/.local/state/omaddons/lock"),
                        access,
                    )
                    .unwrap_err();
                assert!(matches!(refusal, Refusal::ScopeDenied(_)), "{writer}");
            }
        }
    }

    #[test]
    fn saved_variables_need_undo_and_an_owned_folder() {
        let account = "games/wow/_retail_/WTF/Account/123#1/SavedVariables";
        let character = "games/wow/_retail_/WTF/Account/123#1/Realm/Char/SavedVariables";
        let mine = owned(&["WeakAuras"]);
        for dir in [account, character] {
            for file in ["WeakAuras.lua", "WeakAuras.lua.bak", "weakauras.LUA"] {
                let path = format!("{dir}/{file}");
                assert_eq!(
                    allowed(&path, Access::Write, &mine),
                    vec![Writer::Undo, Writer::BackupRestore],
                    "{path}"
                );
                assert_eq!(
                    allowed(&path, Access::Write, &OwnedFolders::default()),
                    vec![Writer::BackupRestore]
                );
            }
            for other in ["Plater.lua", "WeakAuras.txt", ".lua"] {
                let path = format!("{dir}/{other}");
                assert_eq!(
                    allowed(&path, Access::Write, &mine),
                    vec![Writer::BackupRestore],
                    "{path}"
                );
            }
        }
        let blizzard = "games/wow/_retail_/WTF/Account/123#1/SavedVariables.lua";
        assert_eq!(
            allowed(blizzard, Access::Write, &mine),
            vec![Writer::BackupRestore]
        );
        let doctor = table().check(
            Writer::Doctor,
            &mine,
            &p(&format!("{account}/WeakAuras.lua")),
            Access::Write,
        );
        assert!(doctor.is_err(), "the doctor never touches WTF");
    }

    #[test]
    fn record_fixtures_row_is_debug_only_and_derived_only() {
        let mut t = table();
        let dir = p("repo/tests/fixtures/http");
        let result = t.add_record_fixtures(&dir);
        if cfg!(debug_assertions) {
            result.unwrap();
            let file = dir.join("github.json");
            let none = OwnedFolders::default();
            assert!(
                t.check(Writer::Derived, &none, &file, Access::Write)
                    .is_ok()
            );
            assert!(
                t.check(Writer::Mutating, &none, &file, Access::Write)
                    .is_err()
            );
        } else {
            assert!(result.is_err());
        }
    }

    #[test]
    fn stage_and_internal_paths() {
        let t = table();
        assert!(t.is_stage(&p("games/wow/_retail_/Interface/.omaddons/stage/T/Foo/x")));
        assert!(t.is_stage(&p("games/wow/_retail_/interface/.omaddons/STAGE/T")));
        assert!(!t.is_stage(&p("games/wow/_retail_/Interface/.omaddons/stage")));
        assert!(!t.is_stage(&p("games/wow/_retail_/Interface/AddOns/stage/T")));
        assert!(t.is_internal(&p("home/.local/state/omaddons/snapshots/T/files")));
        assert!(t.is_internal(&p("games/wow/_retail_/Interface/.omaddons/snapshots/T")));
        assert!(!t.is_internal(&p("games/wow/_retail_/Interface/AddOns/Foo")));
        assert!(!t.is_internal(&p("home/.cache/omaddons/x")));
    }

    #[test]
    fn symlinked_client_dirs_are_canonicalized() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("Interface/AddOns")).unwrap();
        let link = dir.path().join("x-drive");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let paths = Paths::with_roots(
            dir.path(),
            dir.path().join("c"),
            dir.path().join("s"),
            dir.path().join("k"),
        );
        let t = ScopeTable::new(&paths, &[link], None).unwrap();
        let canonical_real = std::fs::canonicalize(&real).unwrap();
        let target = canonical_real.join("Interface/AddOns/Foo");
        let matched = t
            .check(
                Writer::Mutating,
                &OwnedFolders::default(),
                &target,
                Access::Write,
            )
            .unwrap();
        assert_eq!(
            matched,
            ScopeMatch {
                scope: Scope::AddOns,
                side: Side::Client(canonical_real)
            }
        );
    }
}
