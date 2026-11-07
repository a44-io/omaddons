//! The contract's golden (M4-D56): the semantic manifest, `tests/contract/manifest.json`,
//! and its readable projection, `tests/contract/wire_names.txt`, generated from the code
//! and committed. The gate compares twice: with the newest released baseline
//! (`tests/contract/released/<major>.<minor>.json`, written only by the release step),
//! whenever one exists, through the classifier, so a change that does not carry its
//! `schema_version` bump fails however the golden was regenerated; and with the committed
//! golden, byte for byte. [`REGENERATE`] rewrites the golden; it never touches a released
//! baseline and never satisfies the first comparison.
//!
//! `contract_compat` is the only test that reads or writes the repository's golden: the
//! suite runs on parallel threads, so the updater and the comparisons run in one test.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use omaddons::contract::manifest::{
    Bump, ChangeKind, Changes, Label, Manifest, NodeKind, classify, ids,
};
use omaddons::contract::wire;
use omaddons::state::store::SchemaVersion;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::support;

/// The command that regenerates the current golden.
pub const REGENERATE: &str =
    "OMADDONS_UPDATE_WIRE_NAMES=1 cargo test --test integration contract::wire_names";

/// Where the golden and the released baselines live: `tests/contract/` in the repository,
/// or any directory with the same layout.
#[derive(Debug, Clone)]
pub struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    pub fn repo() -> Self {
        Self::at(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/contract"))
    }

    pub fn at(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    pub fn manifest_json(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    pub fn wire_names_txt(&self) -> PathBuf {
        self.dir.join("wire_names.txt")
    }

    pub fn released_dir(&self) -> PathBuf {
        self.dir.join("released")
    }

    /// `path` as messages name it, `tests/contract/<path below the workspace>`.
    fn name(&self, path: &Path) -> String {
        let below = path.strip_prefix(&self.dir).unwrap_or(path);
        format!("tests/contract/{}", below.display())
    }
}

/// What a run of the gate does besides comparing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    /// Rewrite the current golden first ([`Mode::UPDATE_VAR`] is `1`).
    pub update_golden: bool,
    /// Write a released baseline at this version first ([`Mode::ADVANCE_VAR`]): the
    /// release step, run once per release, in the tag commit.
    pub advance: Option<SchemaVersion>,
}

impl Mode {
    pub const UPDATE_VAR: &'static str = "OMADDONS_UPDATE_WIRE_NAMES";
    pub const ADVANCE_VAR: &'static str = "OMADDONS_ADVANCE_RELEASED_BASELINE";

    /// The mode the variables give: [`Mode::UPDATE_VAR`] unset or empty compares only,
    /// exactly `1` regenerates, and any other value is refused, so a typo never compares
    /// silently instead. [`Mode::ADVANCE_VAR`] names a canonical `MAJOR.MINOR` to release,
    /// and never goes with the updater: a baseline is cut only from a golden that already
    /// matched the code.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Mode, String> {
        let update_golden = match lookup(Self::UPDATE_VAR).as_deref() {
            None | Some("") => false,
            Some("1") => true,
            Some(other) => {
                return Err(format!(
                    "{} must be 1 to regenerate the current golden (it is {other:?}); unset it to compare only",
                    Self::UPDATE_VAR
                ));
            }
        };
        let advance = match lookup(Self::ADVANCE_VAR).as_deref() {
            None | Some("") => None,
            Some(value) => match value.parse::<SchemaVersion>() {
                Ok(version) if version.to_string() == value => Some(version),
                _ => {
                    return Err(format!(
                        "{} must be a contract version such as 1.0 (it is {value:?})",
                        Self::ADVANCE_VAR
                    ));
                }
            },
        };
        if update_golden && advance.is_some() {
            return Err(format!(
                "{} and {} are exclusive: regenerate and commit the golden first, then advance the baseline from it",
                Self::UPDATE_VAR,
                Self::ADVANCE_VAR
            ));
        }
        Ok(Mode {
            update_golden,
            advance,
        })
    }

    pub fn from_env() -> Result<Mode, String> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }
}

/// The golden's header: informational, computed every run, and read by no comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub contract: SchemaVersion,
    /// A released baseline exists.
    pub frozen: bool,
}

/// The two files of the current golden, as the code gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Golden {
    pub manifest_json: String,
    pub wire_names_txt: String,
}

/// Where a released baseline came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub contract_version: String,
    pub tag: String,
    pub commit: String,
    pub created_at: String,
}

/// The newest released baseline.
#[derive(Debug, Clone)]
pub struct Baseline {
    pub version: SchemaVersion,
    pub manifest: Manifest,
    pub provenance: Provenance,
    pub file: PathBuf,
}

/// What a run of the gate noted, what failed (all of it, in order) and what it wrote.
#[derive(Debug, Default)]
pub struct Outcome {
    pub notes: Vec<String>,
    pub failures: Vec<String>,
    pub written: Vec<PathBuf>,
}

/// The header for `version`: frozen when `released/` holds any entry (a misnamed one then
/// fails the comparison with the released baseline).
pub fn header(ws: &Workspace, version: SchemaVersion) -> Result<Header, String> {
    let dir = ws.released_dir();
    let frozen = match fs::read_dir(&dir) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(e) => return Err(format!("{} cannot be listed: {e}", ws.name(&dir))),
    };
    Ok(Header {
        contract: version,
        frozen,
    })
}

/// The golden texts of `current` under `header`.
pub fn golden(current: &Manifest, header: &Header) -> Golden {
    let value = json!({"contract": header.contract.to_string(), "frozen": header.frozen});
    let manifest_json = current.render(&[("header", &value)]);
    let mut wire_names_txt = format!(
        "# contract {}\n# frozen: {}\n",
        header.contract, header.frozen
    );
    for line in wire::names(current) {
        wire_names_txt.push_str(&line);
        wire_names_txt.push('\n');
    }
    Golden {
        manifest_json,
        wire_names_txt,
    }
}

/// The newest released baseline, or `None` when `released/` is absent or empty. Every
/// entry must be a regular file named `<major>.<minor>.json` in canonical spelling, so a
/// misnamed or leftover file fails closed instead of being skipped.
pub fn newest_baseline(ws: &Workspace) -> Result<Option<Baseline>, String> {
    let dir = ws.released_dir();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{} cannot be listed: {e}", ws.name(&dir))),
    };
    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{} cannot be listed: {e}", ws.name(&dir)))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let version = name.strip_suffix(".json").and_then(|stem| {
            stem.parse::<SchemaVersion>()
                .ok()
                .filter(|version| version.to_string() == stem)
        });
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        match (is_file, version) {
            (true, Some(version)) => versions.push((version, path)),
            _ => {
                return Err(format!(
                    "{} is not a released baseline (they are named <major>.<minor>.json); a leftover .tmp file is from an interrupted release step: delete it",
                    ws.name(&path)
                ));
            }
        }
    }
    let Some((version, file)) = versions.into_iter().max_by_key(|(version, _)| *version) else {
        return Ok(None);
    };
    let shown = ws.name(&file);
    let text = fs::read_to_string(&file).map_err(|e| format!("{shown} cannot be read: {e}"))?;
    let manifest = Manifest::parse(&text).map_err(|e| {
        format!(
            "{shown} does not parse ({e}): a released baseline is never edited; restore it from git"
        )
    })?;
    let provenance = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| value.get("provenance").cloned())
        .ok_or_else(|| format!("{shown} has no provenance: a released baseline is never edited; restore it from git"))?;
    let provenance: Provenance = serde_json::from_value(provenance)
        .map_err(|e| format!("{shown}'s provenance does not read ({e}): restore it from git"))?;
    if provenance.contract_version != version.to_string() {
        return Err(format!(
            "{shown}'s provenance names contract {}, not {version}: restore it from git",
            provenance.contract_version
        ));
    }
    Ok(Some(Baseline {
        version,
        manifest,
        provenance,
        file,
    }))
}

/// The noun a change of a node of `kind` is counted under, in the summary's order.
const NOUNS: &[&str] = &[
    "command",
    "flag",
    "positional",
    "output field",
    "output variant",
    "error code",
    "warning code",
    "action",
    "failure mode",
    "event",
    "phase",
    "log level",
    "exit code",
    "doctor op",
    "config key",
    "addons.toml key",
    "environment variable",
    "file kind",
    "node",
    "manifest format",
];

fn noun(kind: Option<NodeKind>) -> &'static str {
    use NodeKind as K;
    match kind {
        Some(K::Command) => "command",
        Some(K::Flag) => "flag",
        Some(K::Positional) => "positional",
        Some(K::OutputPosition) => "output field",
        Some(K::OutputVariant) => "output variant",
        Some(K::ErrorCode) => "error code",
        Some(K::WarningCode) => "warning code",
        Some(K::Action) => "action",
        Some(K::FailureMode) => "failure mode",
        Some(K::Event) => "event",
        Some(K::Phase) => "phase",
        Some(K::LogLevel) => "log level",
        Some(K::ExitCode) => "exit code",
        Some(K::DoctorOp) => "doctor op",
        Some(K::ConfigKey) => "config key",
        Some(K::DesiredPosition | K::DesiredVariant) => "addons.toml key",
        Some(K::EnvVar) => "environment variable",
        Some(K::FileKind | K::FilePosition | K::FileVariant) => "file kind",
        None => "node",
    }
}

/// `1 flag, 2 output fields`: the wire changes' distinct nodes, not counting one folded
/// under an added or removed ancestor, per noun in [`NOUNS`]' order.
fn summary(changes: &Changes, baseline: &Manifest, current: &Manifest) -> String {
    let moved: BTreeSet<&str> = changes
        .changes
        .iter()
        .filter(|c| c.kind != ChangeKind::Changed)
        .map(|c| c.node.as_str())
        .collect();
    let folded = |node: &str| {
        let mut at = ids::parent(node);
        while let Some(ancestor) = at {
            if moved.contains(ancestor.as_str()) {
                return true;
            }
            at = ids::parent(&ancestor);
        }
        false
    };
    let mut nodes: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for change in changes.changes.iter().filter(|c| c.persisted.is_none()) {
        if folded(&change.node) {
            continue;
        }
        let word = if change.node == "format" {
            "manifest format"
        } else {
            let node = current
                .nodes
                .get(&change.node)
                .or_else(|| baseline.nodes.get(&change.node));
            noun(node.and_then(|n| n.kind()))
        };
        nodes.entry(word).or_default().insert(change.node.as_str());
    }
    NOUNS
        .iter()
        .filter_map(|word| {
            let n = nodes.get(word)?.len();
            Some(if n == 1 {
                format!("1 {word}")
            } else {
                format!("{n} {word}s")
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The smallest version above `baseline` that `bump` accepts.
fn required_version(baseline: SchemaVersion, bump: Bump) -> SchemaVersion {
    match bump {
        Bump::Major => SchemaVersion::new(baseline.major + 1, 0),
        Bump::Minor | Bump::None => SchemaVersion::new(baseline.major, baseline.minor + 1),
    }
}

/// Comparison 1: `current` against the released baseline at the contract's `version`.
/// The note on success; on failure, the change list with the bump it needs and how to
/// make it. Reads neither the golden nor its header.
pub fn compare_with_released(
    baseline: &Baseline,
    current: &Manifest,
    version: SchemaVersion,
) -> Result<String, String> {
    let b = baseline.version;
    if version < b {
        return Err(format!(
            "output::SCHEMA_VERSION is {version}, older than the released baseline {b}: a contract version never goes back"
        ));
    }
    let changes = classify(&baseline.manifest, current);
    let required = changes.wire_bump();
    let wire_ok = required.is_satisfied(b, version);
    let kinds = changes.unsatisfied_kinds();
    let head = format!(
        "compared with the released baseline (contract {b}, tag {})",
        baseline.provenance.tag
    );
    if wire_ok && kinds.is_empty() {
        let (additive, breaking, unclassified) = changes.counts();
        return Ok(if changes.is_empty() {
            format!("{head}: no change")
        } else {
            format!(
                "{head}: {additive} additive, {breaking} breaking, {unclassified} unclassified changes; schema_version {version} carries them"
            )
        });
    }
    let unclassified = changes
        .changes
        .iter()
        .any(|c| c.persisted.is_none() && c.label == Label::Unclassified);
    let mut lines = Vec::new();
    if wire_ok {
        lines.push(format!("{head}:"));
    } else {
        let verdict = match (required, unclassified) {
            (_, true) => "breaking or unclassified",
            (Bump::Major, false) => "breaking",
            _ => "additive",
        };
        lines.push(format!(
            "{head}, this change is {verdict} ({}):",
            summary(&changes, &baseline.manifest, current)
        ));
    }
    lines.push(changes.render().trim_end().to_owned());
    if !wire_ok {
        let needed = required_version(b, required);
        let kind = if required == Bump::Major {
            "a breaking"
        } else {
            "an additive"
        };
        let short = if version > b {
            format!(" (schema_version is {version}, which does not satisfy it)")
        } else {
            String::new()
        };
        lines.push(format!(
            "{kind} change needs schema_version {needed}: set output::SCHEMA_VERSION = \"{needed}\", regenerate the current golden{short}"
        ));
        lines.push(format!(
            "({REGENERATE}), and record it in a comment on the vision bead (omad-vision-8qa)"
        ));
        if unclassified {
            lines.push("an unclassified change counts as breaking until the classifier can place it: review the rule in src/contract/manifest/classify.rs (make a rule more conservative, never less)".to_owned());
        }
    }
    for kind in kinds {
        let n = changes
            .changes
            .iter()
            .filter(|c| c.persisted.as_deref() == Some(kind.kind.as_str()))
            .count();
        let (word, needed) = match (
            kind.required,
            kind.baseline.as_deref().and_then(|v| v.parse().ok()),
        ) {
            (Bump::Major, Some(v)) => ("major", required_version(v, Bump::Major).to_string()),
            (Bump::Major, None) => ("major", "a new major".to_owned()),
            (_, Some(v)) => ("minor", required_version(v, Bump::Minor).to_string()),
            (_, None) => ("minor", "a new minor".to_owned()),
        };
        lines.push(format!(
            "persisted file {} changed ({n} changes) and needs its own {word} version bump: {} -> at least {needed}; bump it where the kind's version is declared (its `Versioned` impl, or `engine::journal::CURRENT` for the folder journal) with its migration rule, then regenerate the current golden. The contract's schema_version is not affected.",
            kind.kind,
            kind.baseline.as_deref().unwrap_or("none"),
        ));
    }
    Err(lines.join("\n"))
}

/// At most this many projection lines are listed per side of a difference.
const SHOWN_LINES: usize = 20;

/// The lines of `text` that hold its header: `"header"` in the manifest, `#` lines in the
/// projection.
fn header_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with("  \"header\": ") || line.starts_with("# "))
        .collect()
}

fn without_header(text: &str) -> Vec<&str> {
    let header = header_lines(text);
    text.lines().filter(|line| !header.contains(line)).collect()
}

/// Up to [`SHOWN_LINES`] of `lines`, each prefixed, then how many more.
fn listed(out: &mut Vec<String>, prefix: &str, lines: &[&str]) {
    for line in lines.iter().take(SHOWN_LINES) {
        out.push(format!("    {prefix} {line}"));
    }
    if lines.len() > SHOWN_LINES {
        out.push(format!("    … and {} more", lines.len() - SHOWN_LINES));
    }
}

/// Comparison 2: both committed files equal `expected` byte for byte, header included.
pub fn compare_with_golden(
    ws: &Workspace,
    expected: &Golden,
    current: &Manifest,
) -> Result<(), String> {
    let read = |path: &Path| fs::read_to_string(path).ok();
    let (manifest_path, names_path) = (ws.manifest_json(), ws.wire_names_txt());
    let committed_manifest = read(&manifest_path);
    let committed_names = read(&names_path);
    if committed_manifest.as_deref() == Some(expected.manifest_json.as_str())
        && committed_names.as_deref() == Some(expected.wire_names_txt.as_str())
    {
        return Ok(());
    }
    let mut out = vec!["the current golden is out of date with the code:".to_owned()];
    for (path, committed, want) in [
        (&manifest_path, &committed_manifest, &expected.manifest_json),
        (&names_path, &committed_names, &expected.wire_names_txt),
    ] {
        let shown = ws.name(path);
        let Some(committed) = committed else {
            out.push(format!(
                "  {shown} is missing: generate it with {REGENERATE}"
            ));
            continue;
        };
        if committed == want {
            continue;
        }
        if without_header(committed) == without_header(want) {
            out.push(format!(
                "  the header of {shown} says {:?} where the code gives {:?}: the header is generated (contract = output::SCHEMA_VERSION, frozen = whether tests/contract/released/ holds a baseline); regenerate, never edit it",
                header_lines(committed).join(" "),
                header_lines(want).join(" ")
            ));
            continue;
        }
        if path == &manifest_path {
            let line = committed
                .lines()
                .zip(want.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| committed.lines().count().min(want.lines().count()))
                + 1;
            out.push(format!(
                "  {shown} differs (first difference at line {line})"
            ));
        } else {
            let in_file: BTreeSet<&str> = without_header(committed).into_iter().collect();
            let in_code: BTreeSet<&str> = without_header(want).into_iter().collect();
            let only_file: Vec<&str> = in_file.difference(&in_code).copied().collect();
            let only_code: Vec<&str> = in_code.difference(&in_file).copied().collect();
            out.push(format!(
                "  {shown} differs: {} lines only in the file, {} only in the code",
                only_file.len(),
                only_code.len()
            ));
            listed(&mut out, "-", &only_file);
            listed(&mut out, "+", &only_code);
        }
    }
    match committed_manifest.as_deref().map(Manifest::parse) {
        Some(Ok(committed)) => {
            let changes = classify(&committed, current);
            if !changes.is_empty() {
                out.push("semantic difference (committed -> code):".to_owned());
                out.push(changes.render().trim_end().to_owned());
            }
        }
        Some(Err(e)) => out.push(format!(
            "semantic difference: the committed manifest.json does not parse ({e})"
        )),
        None => {}
    }
    out.push(format!(
        "regenerate it with {REGENERATE} and commit it with the change"
    ));
    Err(out.join("\n"))
}

/// Rewrites each golden file whose bytes differ from `expected`, through a hidden
/// temporary sibling and a rename; never anything under `released/`. The paths written.
pub fn write_golden(ws: &Workspace, expected: &Golden) -> Result<Vec<PathBuf>, String> {
    let mut written = Vec::new();
    for (path, text) in [
        (ws.manifest_json(), &expected.manifest_json),
        (ws.wire_names_txt(), &expected.wire_names_txt),
    ] {
        if fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temporary = ws.dir.join(format!(".{name}.tmp"));
        fs::write(&temporary, text)
            .and_then(|()| fs::rename(&temporary, &path))
            .map_err(|e| format!("{} could not be written: {e}", ws.name(&path)))?;
        written.push(path);
    }
    Ok(written)
}

/// The release step: writes `released/<version>.json`, the manifest with its provenance,
/// and returns its path. It refuses, writing nothing, at the first check that fails: the
/// version is not the contract's; the committed golden does not match the code; the
/// baseline exists already; the version is not newer than the newest baseline, or the
/// change since it does not carry its bump; the provenance cannot be had (asked last, so
/// no refusal needs `git`).
pub fn advance(
    ws: &Workspace,
    version: SchemaVersion,
    current: &Manifest,
    contract: SchemaVersion,
    provenance: &dyn Fn() -> Result<Provenance, String>,
) -> Result<PathBuf, String> {
    release_to(ws, version, current, contract, provenance).map(|(path, _)| path)
}

/// [`advance`], also returning the provenance it recorded.
fn release_to(
    ws: &Workspace,
    version: SchemaVersion,
    current: &Manifest,
    contract: SchemaVersion,
    provenance: &dyn Fn() -> Result<Provenance, String>,
) -> Result<(PathBuf, Provenance), String> {
    if version != contract {
        return Err(format!(
            "the release step advances to {version}, but output::SCHEMA_VERSION is {contract}"
        ));
    }
    let expected = golden(current, &header(ws, contract)?);
    compare_with_golden(ws, &expected, current).map_err(|e| {
        format!(
            "the current golden must match the code before a release: regenerate and commit it first ({REGENERATE})\n{e}"
        )
    })?;
    let path = ws.released_dir().join(format!("{version}.json"));
    if path.exists() {
        let tag = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| value["provenance"]["tag"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "an unreadable tag".to_owned());
        return Err(format!(
            "{} exists (contract {version} was released at tag {tag}): a released baseline is never rewritten",
            ws.name(&path)
        ));
    }
    if let Some(baseline) = newest_baseline(ws)? {
        if baseline.version >= version {
            return Err(format!(
                "contract {version} is not newer than the released {}",
                baseline.version
            ));
        }
        compare_with_released(&baseline, current, version)?;
    }
    let mut recorded = provenance()
        .map_err(|e| format!("the release step records the commit it was generated from: {e}"))?;
    // The file's own version, which `newest_baseline` checks against its name.
    recorded.contract_version = version.to_string();
    let value = serde_json::to_value(&recorded).map_err(|e| format!("provenance: {e}"))?;
    let text = current.render(&[("provenance", &value)]);
    let dir = ws.released_dir();
    let temporary = dir.join(format!(".{version}.json.tmp"));
    fs::create_dir_all(&dir)
        .and_then(|()| fs::write(&temporary, text))
        .and_then(|()| fs::rename(&temporary, &path))
        .map_err(|e| format!("{} could not be written: {e}", ws.name(&path)))?;
    Ok((path, recorded))
}

/// The provenance of a baseline released now at `version`: the tag the crate version
/// gives (the tag is made after the commit that holds the baseline, so it cannot be
/// read), the commit `git rev-parse HEAD` names, and the time in whole UTC seconds.
pub fn real_provenance(version: SchemaVersion) -> Result<Provenance, String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map_err(|e| format!("git rev-parse HEAD could not run: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let hex = commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !hex {
        return Err(format!(
            "git rev-parse HEAD printed {commit:?}, not a 40-digit commit"
        ));
    }
    let now = jiff::Timestamp::now().as_second();
    let created_at = jiff::Timestamp::from_second(now)
        .map_err(|e| format!("the clock: {e}"))?
        .to_string();
    Ok(Provenance {
        contract_version: version.to_string(),
        tag: format!("v{}", env!("CARGO_PKG_VERSION")),
        commit,
        created_at,
    })
}

/// The gate's run on `ws`: the release step when `mode` asks (a refusal is then the run's
/// only failure), the header, the updater when `mode` asks or a release was written, then
/// comparison 1 whenever a released baseline exists, then comparison 2. Every failure is
/// collected; the updater writes even when comparison 1 then fails, so the semantic diff
/// shows in `git diff`, and the run still fails.
pub fn gate(
    ws: &Workspace,
    mode: &Mode,
    current: &Manifest,
    version: SchemaVersion,
    provenance: &dyn Fn() -> Result<Provenance, String>,
) -> Outcome {
    let mut out = Outcome::default();
    out.notes.push(match (mode.update_golden, mode.advance) {
        (_, Some(release)) => format!(
            "mode: release contract {release} ({}={release}), then regenerate the golden's header and compare",
            Mode::ADVANCE_VAR
        ),
        (true, None) => format!(
            "mode: regenerate the current golden ({}=1), then compare",
            Mode::UPDATE_VAR
        ),
        (false, None) => "mode: compare".to_owned(),
    });
    let mut update_golden = mode.update_golden;
    if let Some(release) = mode.advance {
        match release_to(ws, release, current, version, provenance) {
            Ok((path, recorded)) => {
                let short: String = recorded.commit.chars().take(12).collect();
                out.notes.push(format!(
                    "wrote {} (contract {release}, tag {}, commit {short}); the current golden's header now says frozen: true; commit both in the tag commit",
                    ws.name(&path),
                    recorded.tag
                ));
                out.written.push(path);
                // The header is computed after the baseline exists: only its lines change.
                update_golden = true;
            }
            Err(e) => {
                out.failures.push(e);
                return out;
            }
        }
    }
    let header = match header(ws, version) {
        Ok(header) => Some(header),
        Err(e) => {
            out.failures.push(e);
            None
        }
    };
    let expected = header.map(|header| golden(current, &header));
    if let (true, Some(expected)) = (update_golden, &expected) {
        match write_golden(ws, expected) {
            Ok(written) if written.is_empty() => {
                out.notes
                    .push("the current golden is up to date".to_owned());
            }
            Ok(written) => {
                let names: Vec<String> = written.iter().map(|path| ws.name(path)).collect();
                out.notes.push(format!(
                    "rewrote {} ({} nodes)",
                    names.join(" and "),
                    current.nodes.len()
                ));
                out.written.extend(written);
            }
            Err(e) => out.failures.push(e),
        }
    }
    match newest_baseline(ws) {
        Ok(None) => out.notes.push(
            "no released baseline: only the comparison with the current golden runs (the first baseline, released/1.0.json, is written at the first tag)".to_owned(),
        ),
        Ok(Some(baseline)) => {
            out.notes
                .push(format!("comparing with {}", ws.name(&baseline.file)));
            match compare_with_released(&baseline, current, version) {
                Ok(note) => out.notes.push(note),
                Err(e) => out.failures.push(e),
            }
        }
        Err(e) => out.failures.push(e),
    }
    if let Some(expected) = &expected
        && let Err(e) = compare_with_golden(ws, expected, current)
    {
        out.failures.push(e);
    }
    out
}

/// The gate as the variables ask, on this build's manifest and contract version.
pub fn run_from_env(ws: &Workspace) -> Outcome {
    let mode = match Mode::from_env() {
        Ok(mode) => mode,
        Err(e) => {
            return Outcome {
                failures: vec![e],
                ..Outcome::default()
            };
        }
    };
    let version = match omaddons::output::SCHEMA_VERSION.parse::<SchemaVersion>() {
        Ok(version) => version,
        Err(e) => {
            return Outcome {
                failures: vec![format!("output::SCHEMA_VERSION: {e}")],
                ..Outcome::default()
            };
        }
    };
    let release = mode.advance.unwrap_or(version);
    let provenance = || real_provenance(release);
    gate(ws, &mode, support::manifest(), version, &provenance)
}

#[test]
fn contract_compat() {
    let out = run_from_env(&Workspace::repo());
    for note in &out.notes {
        println!("{note}");
    }
    assert!(out.failures.is_empty(), "{}", out.failures.join("\n\n"));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use omaddons::contract::manifest::FORMAT;
    use serde_json::{Map, Value, json};

    use super::*;

    fn v(major: u32, minor: u32) -> SchemaVersion {
        SchemaVersion::new(major, minor)
    }

    /// A manifest of `nodes`, through `Manifest::parse` as a committed file is read.
    fn synthetic(nodes: &[(&str, Value)]) -> Manifest {
        let nodes: Map<String, Value> = nodes
            .iter()
            .map(|(id, node)| ((*id).to_owned(), node.clone()))
            .collect();
        Manifest::parse(&json!({"format": FORMAT, "nodes": nodes}).to_string()).unwrap()
    }

    /// `install` with one output field and a flag, and an action.
    fn base() -> Vec<(&'static str, Value)> {
        vec![
            (
                "action apply",
                json!({"destructive": false, "kind": "action"}),
            ),
            (
                "command install",
                json!({"kind": "command", "mutates": true}),
            ),
            (
                "input install --channel",
                json!({"kind": "flag", "grammar": "enum", "values": ["alpha", "beta", "stable"]}),
            ),
            (
                "output install data",
                json!({"kind": "output_position", "types": ["object"]}),
            ),
            (
                "output install data.dry_run",
                json!({"kind": "output_position", "nullable": false, "required": true, "types": ["boolean"]}),
            ),
        ]
    }

    fn with(mut nodes: Vec<(&'static str, Value)>, extra: &[(&'static str, Value)]) -> Manifest {
        nodes.extend(extra.iter().cloned());
        synthetic(&nodes)
    }

    fn without(nodes: Vec<(&'static str, Value)>, gone: &[&str]) -> Manifest {
        let kept: Vec<_> = nodes
            .into_iter()
            .filter(|(id, _)| !gone.contains(id))
            .collect();
        synthetic(&kept)
    }

    fn provenance(version: &str) -> Value {
        json!({"contract_version": version, "tag": "v0.1.0", "commit": "0000000", "created_at": "2026-11-07T00:00:00Z"})
    }

    /// Writes `released/<name>` as the release step would.
    fn release(ws: &Workspace, name: &str, m: &Manifest, contract: &str) {
        fs::create_dir_all(ws.released_dir()).unwrap();
        let text = m.render(&[("provenance", &provenance(contract))]);
        fs::write(ws.released_dir().join(name), text).unwrap();
    }

    fn baseline_of(m: &Manifest) -> Baseline {
        Baseline {
            version: v(1, 0),
            manifest: m.clone(),
            provenance: serde_json::from_value(provenance("1.0")).unwrap(),
            file: PathBuf::from("released/1.0.json"),
        }
    }

    #[test]
    fn mode_from_lookup_cases() {
        let cases: [(&str, Option<&str>, Result<bool, ()>); 6] = [
            ("none", None, Ok(false)),
            ("update_one", Some("1"), Ok(true)),
            ("update_empty_is_off", Some(""), Ok(false)),
            ("update_true_fails", Some("true"), Err(())),
            ("update_zero_fails", Some("0"), Err(())),
            ("update_space_one_fails", Some(" 1"), Err(())),
        ];
        for (case, value, want) in cases {
            let vars: BTreeMap<&str, &str> = value
                .map(|value| (Mode::UPDATE_VAR, value))
                .into_iter()
                .collect();
            let got = Mode::from_lookup(|key| vars.get(key).map(|v| (*v).to_owned()));
            match (got, want) {
                (Ok(mode), Ok(update)) => assert_eq!(
                    mode,
                    Mode {
                        update_golden: update,
                        advance: None
                    },
                    "{case}: {value:?}"
                ),
                (Err(message), Err(())) => assert!(
                    message.contains(Mode::UPDATE_VAR)
                        && message.contains(&format!("{:?}", value.unwrap())),
                    "{case}: {message}"
                ),
                (got, want) => panic!("{case}: {value:?} gave {got:?}, wanted {want:?}"),
            }
        }
    }

    #[test]
    fn header_cases() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        assert_eq!(
            header(&ws, v(1, 0)).unwrap(),
            Header {
                contract: v(1, 0),
                frozen: false
            },
            "no_released_dir"
        );
        fs::create_dir_all(ws.released_dir()).unwrap();
        assert!(!header(&ws, v(1, 0)).unwrap().frozen, "empty_released_dir");
        fs::write(ws.released_dir().join("1.0.json"), "{}").unwrap();
        assert!(header(&ws, v(1, 0)).unwrap().frozen, "one_file");
        assert_eq!(
            header(&ws, v(3, 7)).unwrap().contract,
            v(3, 7),
            "contract_is_the_version_given"
        );
    }

    #[test]
    fn golden_texts_have_the_documented_layout() {
        let m = synthetic(&base()[..3]);
        let open = golden(
            &m,
            &Header {
                contract: v(1, 0),
                frozen: false,
            },
        );
        let lines: Vec<&str> = open.manifest_json.lines().collect();
        assert_eq!(
            &lines[..3],
            [
                "{",
                "  \"format\": 1,",
                "  \"header\": {\"contract\":\"1.0\",\"frozen\":false},"
            ],
            "{}",
            open.manifest_json
        );
        assert_eq!(
            Manifest::parse(&open.manifest_json).unwrap(),
            m,
            "the header changes nothing a comparison reads"
        );
        let mut names = String::from("# contract 1.0\n# frozen: false\n");
        for line in wire::names(&m) {
            assert!(!line.starts_with('#'), "a projection line starts with #");
            names.push_str(&line);
            names.push('\n');
        }
        assert_eq!(open.wire_names_txt, names);
        let frozen = golden(
            &m,
            &Header {
                contract: v(1, 0),
                frozen: true,
            },
        );
        assert_eq!(
            without_header(&frozen.manifest_json),
            without_header(&open.manifest_json),
            "frozen changes the header lines only"
        );
        assert_eq!(
            without_header(&frozen.wire_names_txt),
            without_header(&open.wire_names_txt)
        );
        assert!(
            frozen
                .wire_names_txt
                .starts_with("# contract 1.0\n# frozen: true\n")
        );
    }

    /// What a case puts in its workspace.
    type Setup<'a> = &'a dyn Fn(&Workspace);

    #[test]
    fn baseline_listing_cases() {
        let m = synthetic(&base());
        let listed = |setup: Setup<'_>| {
            let dir = tempfile::tempdir().unwrap();
            let ws = Workspace::at(dir.path());
            setup(&ws);
            newest_baseline(&ws).map(|b| b.map(|b| b.version))
        };
        let ok: [(&str, Setup<'_>, Option<SchemaVersion>); 4] = [
            ("absent_dir", &|_| {}, None),
            (
                "empty_dir",
                &|ws| fs::create_dir_all(ws.released_dir()).unwrap(),
                None,
            ),
            (
                "one_baseline",
                &|ws| release(ws, "1.0.json", &m, "1.0"),
                Some(v(1, 0)),
            ),
            (
                "newest_of_three",
                &|ws| {
                    for name in ["1.0", "1.2", "1.10"] {
                        release(ws, &format!("{name}.json"), &m, name);
                    }
                },
                Some(v(1, 10)),
            ),
        ];
        for (case, setup, want) in ok {
            assert_eq!(listed(setup), Ok(want), "{case}");
        }
        let failing: [(&str, Setup<'_>, &str); 7] = [
            (
                "misnamed_file_fails",
                &|ws| release(ws, "baseline.json", &m, "1.0"),
                "is not a released baseline",
            ),
            (
                "leading_zero_name_fails",
                &|ws| release(ws, "01.0.json", &m, "1.0"),
                "is not a released baseline",
            ),
            (
                "leftover_tmp_fails",
                &|ws| release(ws, ".1.1.json.tmp", &m, "1.1"),
                "leftover .tmp",
            ),
            (
                "directory_entry_fails",
                &|ws| fs::create_dir_all(ws.released_dir().join("1.0.json")).unwrap(),
                "is not a released baseline",
            ),
            (
                "unparsable_fails",
                &|ws| {
                    fs::create_dir_all(ws.released_dir()).unwrap();
                    fs::write(ws.released_dir().join("1.0.json"), "{not json").unwrap();
                },
                "does not parse",
            ),
            (
                "provenance_missing_fails",
                &|ws| {
                    fs::create_dir_all(ws.released_dir()).unwrap();
                    fs::write(ws.released_dir().join("1.0.json"), m.render(&[])).unwrap();
                },
                "has no provenance",
            ),
            (
                "provenance_version_mismatch_fails",
                &|ws| release(ws, "1.0.json", &m, "1.1"),
                "names contract 1.1, not 1.0",
            ),
        ];
        for (case, setup, want) in failing {
            let got = listed(setup);
            assert!(
                got.as_ref().is_err_and(|message| message.contains(want)),
                "{case}: {got:?}"
            );
        }
    }

    #[test]
    fn required_version_cases() {
        let m = synthetic(&base());
        let added = with(
            base(),
            &[(
                "output install data.size_bytes",
                json!({"kind": "output_position", "nullable": false, "required": true, "types": ["integer"]}),
            )],
        );
        let removed = without(base(), &["input install --channel"]);
        let compare = |baseline_version: SchemaVersion, current: &Manifest, version| {
            let mut baseline = baseline_of(&m);
            baseline.version = baseline_version;
            baseline.provenance.contract_version = baseline_version.to_string();
            compare_with_released(&baseline, current, version)
        };
        assert!(
            compare(v(1, 0), &added, v(1, 0))
                .is_err_and(|e| e.contains("needs schema_version 1.1")),
            "minor_from_1_0"
        );
        assert!(
            compare(v(1, 9), &added, v(1, 9))
                .is_err_and(|e| e.contains("needs schema_version 1.10")),
            "minor_from_1_9"
        );
        assert!(
            compare(v(1, 3), &removed, v(1, 3))
                .is_err_and(|e| e.contains("needs schema_version 2.0")),
            "major_from_1_3"
        );
        assert!(
            compare(v(1, 0), &removed, v(1, 1))
                .is_err_and(|e| e.contains("(schema_version is 1.1, which does not satisfy it)")),
            "major_at_1_1_does_not_satisfy"
        );
        assert!(
            compare(v(1, 0), &added, v(1, 1)).is_ok(),
            "minor_at_1_1_passes"
        );
        assert!(
            compare(v(1, 0), &removed, v(2, 0)).is_ok(),
            "major_at_2_0_passes"
        );
        assert!(
            compare(v(1, 2), &m, v(1, 1))
                .is_err_and(|e| e.contains("a contract version never goes back")),
            "older_than_baseline_fails"
        );
        assert!(
            compare(v(1, 0), &m, v(1, 1)).is_ok_and(|note| note.ends_with(": no change")),
            "higher_without_change_passes"
        );
        assert_eq!(
            compare(v(1, 0), &m, v(1, 0)),
            Ok(
                "compared with the released baseline (contract 1.0, tag v0.1.0): no change"
                    .to_owned()
            ),
            "identical_passes"
        );
    }

    #[test]
    fn compat_message_matches_the_documented_form() {
        let m = synthetic(&base());
        let field = (
            "output install data.size_bytes",
            json!({"kind": "output_position", "nullable": false, "required": true, "types": ["integer"]}),
        );
        let failure = |current: &Manifest| {
            compare_with_released(&baseline_of(&m), current, v(1, 0)).unwrap_err()
        };

        let one = failure(&with(base(), std::slice::from_ref(&field)));
        let lines: Vec<&str> = one.lines().collect();
        assert_eq!(
            lines[0],
            "compared with the released baseline (contract 1.0, tag v0.1.0), this change is additive (1 output field):",
            "{one}"
        );
        assert_eq!(lines[1], "additive (1):", "{one}");
        assert!(
            lines[2].starts_with("  + output install data.size_bytes"),
            "{one}"
        );
        assert_eq!(
            lines[3],
            "an additive change needs schema_version 1.1: set output::SCHEMA_VERSION = \"1.1\", regenerate the current golden",
            "{one}"
        );
        assert_eq!(
            lines[4],
            format!(
                "({REGENERATE}), and record it in a comment on the vision bead (omad-vision-8qa)"
            )
        );
        assert_eq!(lines.len(), 5, "{one}");

        let removed = failure(&without(base(), &["input install --channel"]));
        assert!(
            removed.contains("this change is breaking (1 flag):")
                && removed.contains("a breaking change needs schema_version 2.0"),
            "removed_flag: {removed}"
        );

        let unknown = failure(&with(
            base(),
            &[("widget gauge", json!({"kind": "widget"}))],
        ));
        assert!(
            unknown.contains("this change is breaking or unclassified (1 node):")
                && unknown.contains("an unclassified change counts as breaking"),
            "unknown_kind_node: {unknown}"
        );

        let files = [
            (
                "file target_record",
                json!({"kind": "file_kind", "version": "1.0"}),
            ),
            (
                "file target_record .",
                json!({"kind": "file_position", "types": ["object"]}),
            ),
        ];
        let file_base = with(base(), &files);
        let file_added = with(
            base(),
            &[
                files[0].clone(),
                files[1].clone(),
                (
                    "file target_record name",
                    json!({"kind": "file_position", "nullable": false, "required": true, "types": ["string"]}),
                ),
            ],
        );
        let persisted =
            compare_with_released(&baseline_of(&file_base), &file_added, v(1, 0)).unwrap_err();
        assert!(
            persisted
                .starts_with("compared with the released baseline (contract 1.0, tag v0.1.0):\n")
                && persisted
                    .contains("persisted file target_record changed (1 changes) and needs its own")
                && !persisted.contains("this change is"),
            "persisted_only: {persisted}"
        );

        let mixed = failure(&without(
            {
                let mut nodes = base();
                nodes.extend([field.clone(), ("event log", json!({"kind": "event"}))]);
                nodes
            },
            &["input install --channel"],
        ));
        assert!(
            mixed.contains("(1 flag, 1 output field, 1 event):")
                && mixed.contains("breaking (1):")
                && mixed.contains("additive (2):"),
            "mixed: {mixed}"
        );

        let folded = failure(&synthetic(&base()[..1]));
        assert!(
            folded.contains("this change is breaking (1 command):"),
            "folded_children: {folded}"
        );
    }

    fn workspace_with(ws: &Workspace, golden: &Golden) {
        fs::write(ws.manifest_json(), &golden.manifest_json).unwrap();
        fs::write(ws.wire_names_txt(), &golden.wire_names_txt).unwrap();
    }

    #[test]
    fn golden_mismatch_messages() {
        let open = Header {
            contract: v(1, 0),
            frozen: false,
        };
        let m = synthetic(&base());
        let expected = golden(&m, &open);
        let old = synthetic(&base()[..4]);
        let stale = golden(&old, &open);
        let compare = |setup: &dyn Fn(&Workspace)| {
            let dir = tempfile::tempdir().unwrap();
            let ws = Workspace::at(dir.path());
            setup(&ws);
            compare_with_golden(&ws, &expected, &m)
        };

        assert_eq!(
            compare(&|ws| workspace_with(ws, &expected)),
            Ok(()),
            "equal"
        );

        let body = compare(&|ws| workspace_with(ws, &stale)).unwrap_err();
        assert!(
            body.starts_with("the current golden is out of date with the code:")
                && body.contains("tests/contract/manifest.json differs (first difference at line ")
                && body.contains("tests/contract/wire_names.txt differs: 0 lines only in the file, 1 only in the code")
                && body.contains("    + output install data.dry_run")
                && body.contains("semantic difference (committed -> code):")
                && body.contains("+ output install data.dry_run")
                && body.ends_with(&format!("regenerate it with {REGENERATE} and commit it with the change")),
            "body_differs: {body}"
        );

        let header_only = compare(&|ws| {
            workspace_with(
                ws,
                &golden(
                    &m,
                    &Header {
                        contract: v(1, 0),
                        frozen: true,
                    },
                ),
            );
        })
        .unwrap_err();
        assert!(
            header_only.contains("the header of tests/contract/manifest.json says")
                && header_only.contains("the header of tests/contract/wire_names.txt says")
                && header_only.contains("regenerate, never edit it")
                && !header_only.contains("semantic difference"),
            "header_only_differs: {header_only}"
        );

        let missing =
            compare(&|ws| fs::write(ws.wire_names_txt(), &expected.wire_names_txt).unwrap())
                .unwrap_err();
        assert!(
            missing.contains(&format!(
                "tests/contract/manifest.json is missing: generate it with {REGENERATE}"
            )),
            "missing_manifest: {missing}"
        );
        let missing =
            compare(&|ws| fs::write(ws.manifest_json(), &expected.manifest_json).unwrap())
                .unwrap_err();
        assert!(
            missing.contains("tests/contract/wire_names.txt is missing"),
            "missing_names: {missing}"
        );

        let unparsable = compare(&|ws| {
            fs::write(ws.manifest_json(), "{broken").unwrap();
            fs::write(ws.wire_names_txt(), &expected.wire_names_txt).unwrap();
        })
        .unwrap_err();
        assert!(
            unparsable.contains("semantic difference: the committed manifest.json does not parse"),
            "unparsable_committed_manifest: {unparsable}"
        );

        let many: Vec<(String, Value)> = (0..25)
            .map(|n| {
                (
                    format!("action a{n:02}"),
                    json!({"destructive": false, "kind": "action"}),
                )
            })
            .collect();
        let many: Vec<(&str, Value)> = many
            .iter()
            .map(|(id, node)| (id.as_str(), node.clone()))
            .collect();
        let big = synthetic(&many);
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        workspace_with(&ws, &golden(&synthetic(&[]), &open));
        let long = compare_with_golden(&ws, &golden(&big, &open), &big).unwrap_err();
        assert!(
            long.contains("25 only in the code") && long.contains("    … and 5 more"),
            "more_than_twenty_lines: {long}"
        );
    }

    #[test]
    fn write_golden_cases() {
        let open = Header {
            contract: v(1, 0),
            frozen: false,
        };
        let expected = golden(&synthetic(&base()), &open);
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());

        let written = write_golden(&ws, &expected).unwrap();
        assert_eq!(
            written,
            [ws.manifest_json(), ws.wire_names_txt()],
            "absent_writes_both"
        );
        assert_eq!(
            compare_with_golden(&ws, &expected, &synthetic(&base())),
            Ok(())
        );

        fs::write(ws.wire_names_txt(), "stale\n").unwrap();
        let stamp = fs::metadata(ws.manifest_json())
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            write_golden(&ws, &expected).unwrap(),
            [ws.wire_names_txt()],
            "one_differs_writes_one"
        );
        assert_eq!(
            write_golden(&ws, &expected).unwrap(),
            Vec::<PathBuf>::new(),
            "equal_writes_nothing"
        );
        assert_eq!(
            fs::metadata(ws.manifest_json())
                .unwrap()
                .modified()
                .unwrap(),
            stamp,
            "an equal file is not rewritten"
        );
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["manifest.json", "wire_names.txt"],
            "no_tmp_left, never_creates_released"
        );
    }

    /// Every file under `dir` with its bytes.
    fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(at) = pending.pop() {
            for entry in fs::read_dir(&at).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap());
                }
            }
        }
        out
    }

    #[test]
    fn gate_order_cases() {
        let open = Header {
            contract: v(1, 0),
            frozen: false,
        };
        let m = synthetic(&base());
        let no_release = || Err("no release step here".to_owned());
        let compare = Mode {
            update_golden: false,
            advance: None,
        };
        let update = Mode {
            update_golden: true,
            advance: None,
        };

        // Compare only: nothing is written, and a stale golden fails.
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        workspace_with(&ws, &golden(&synthetic(&base()[..4]), &open));
        let before = tree(dir.path());
        let out = gate(&ws, &compare, &m, v(1, 0), &no_release);
        assert_eq!(tree(dir.path()), before, "compare_only_writes_nothing");
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);

        // Update, then compare: passes with no baseline.
        let out = gate(&ws, &update, &m, v(1, 0), &no_release);
        assert!(
            out.failures.is_empty(),
            "update_then_compare_passes_without_baseline: {:?}",
            out.failures
        );
        assert_eq!(out.written, [ws.manifest_json(), ws.wire_names_txt()]);
        assert!(
            out.notes
                .iter()
                .any(|n| n.starts_with("no released baseline"))
        );

        // A breaking change against a baseline: the golden is rewritten, the run still
        // fails on comparison 1, and the baseline is untouched.
        release(&ws, "1.0.json", &m, "1.0");
        let released = tree(&ws.released_dir());
        let breaking = without(base(), &["input install --channel"]);
        let out = gate(&ws, &update, &breaking, v(1, 0), &no_release);
        assert_eq!(
            out.written.len(),
            2,
            "the golden is rewritten: {:?}",
            out.notes
        );
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert!(
            out.failures[0].contains("needs schema_version 2.0"),
            "update_with_breaking_change_still_fails: {:?}",
            out.failures
        );
        assert_eq!(
            tree(&ws.released_dir()),
            released,
            "released/ is never written"
        );

        // A stale golden and a missing bump: both reported, comparison 1's first.
        let out = gate(&ws, &update, &m, v(1, 0), &no_release);
        assert!(
            out.failures.is_empty(),
            "back to the baseline: {:?}",
            out.failures
        );
        let out = gate(&ws, &compare, &breaking, v(1, 0), &no_release);
        assert_eq!(
            out.failures.len(),
            2,
            "both_failures_reported: {:?}",
            out.failures
        );
        assert!(out.failures[0].starts_with("compared with the released baseline"));
        assert!(out.failures[1].starts_with("the current golden is out of date"));
    }

    #[test]
    fn mode_advance_cases() {
        let mode = |update: Option<&str>, advance: Option<&str>| {
            let vars: BTreeMap<&str, &str> =
                [(Mode::UPDATE_VAR, update), (Mode::ADVANCE_VAR, advance)]
                    .into_iter()
                    .filter_map(|(key, value)| value.map(|value| (key, value)))
                    .collect();
            Mode::from_lookup(|key| vars.get(key).map(|v| (*v).to_owned()))
        };
        for (case, value, want) in [
            ("advance_one_zero", "1.0", Some(v(1, 0))),
            ("advance_two_three", "2.3", Some(v(2, 3))),
            ("advance_empty_is_off", "", None),
        ] {
            assert_eq!(
                mode(None, Some(value)),
                Ok(Mode {
                    update_golden: false,
                    advance: want
                }),
                "{case}: {value:?}"
            );
        }
        for (case, value) in [
            ("advance_leading_zero_fails", "01.0"),
            ("advance_three_parts_fails", "1.0.0"),
            ("advance_v_prefix_fails", "v1.0"),
            ("advance_word_fails", "latest"),
        ] {
            let got = mode(None, Some(value));
            assert!(
                got.as_ref().is_err_and(
                    |e| e.contains(Mode::ADVANCE_VAR) && e.contains(&format!("{value:?}"))
                ),
                "{case}: {got:?}"
            );
        }
        let both = mode(Some("1"), Some("1.0"));
        assert!(
            both.as_ref().is_err_and(|e| e.contains("are exclusive")),
            "both_set_fails: {both:?}"
        );
    }

    /// A release closure with fixed values that counts its calls.
    struct Releaser {
        calls: std::cell::Cell<usize>,
        fail: bool,
    }

    impl Releaser {
        fn new(fail: bool) -> Self {
            Self {
                calls: std::cell::Cell::new(0),
                fail,
            }
        }

        fn provenance(&self) -> Result<Provenance, String> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err("no git here".to_owned());
            }
            Ok(Provenance {
                contract_version: "set by advance".to_owned(),
                tag: "v0.1.0".to_owned(),
                commit: "0".repeat(40),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            })
        }
    }

    fn releasing(version: SchemaVersion) -> Mode {
        Mode {
            update_golden: false,
            advance: Some(version),
        }
    }

    fn updating() -> Mode {
        Mode {
            update_golden: true,
            advance: None,
        }
    }

    #[test]
    fn advance_refusal_cases() {
        let m = synthetic(&base());
        let breaking = without(base(), &["input install --channel"]);
        let open = Header {
            contract: v(1, 0),
            frozen: false,
        };
        let current_golden = |ws: &Workspace| workspace_with(ws, &golden(&m, &open));
        let first_release = |ws: &Workspace| {
            current_golden(ws);
            let ok = Releaser::new(false);
            let out = gate(ws, &releasing(v(1, 0)), &m, v(1, 0), &|| ok.provenance());
            assert!(
                out.failures.is_empty(),
                "the first release: {:?}",
                out.failures
            );
        };
        // (case, setup, the release, the contract version, the manifest, the phrase)
        type Refusal<'a> = (
            &'a str,
            Setup<'a>,
            SchemaVersion,
            SchemaVersion,
            &'a Manifest,
            &'a str,
            bool,
        );
        let cases: [Refusal<'_>; 7] = [
            (
                "wrong_version",
                &current_golden,
                v(1, 1),
                v(1, 0),
                &m,
                "the release step advances to 1.1, but output::SCHEMA_VERSION is 1.0",
                false,
            ),
            (
                "golden_missing",
                &|_| {},
                v(1, 0),
                v(1, 0),
                &m,
                "is missing",
                false,
            ),
            (
                "golden_stale",
                &|ws| workspace_with(ws, &golden(&synthetic(&base()[..4]), &open)),
                v(1, 0),
                v(1, 0),
                &m,
                "the current golden must match the code before a release",
                false,
            ),
            (
                "baseline_exists",
                &first_release,
                v(1, 0),
                v(1, 0),
                &m,
                "tests/contract/released/1.0.json exists (contract 1.0 was released at tag v0.1.0): a released baseline is never rewritten",
                false,
            ),
            (
                "not_newer",
                &|ws| {
                    release(ws, "1.1.json", &m, "1.1");
                    workspace_with(
                        ws,
                        &golden(
                            &m,
                            &Header {
                                contract: v(1, 0),
                                frozen: true,
                            },
                        ),
                    );
                },
                v(1, 0),
                v(1, 0),
                &m,
                "contract 1.0 is not newer than the released 1.1",
                false,
            ),
            (
                "previous_unsatisfied",
                &|ws| {
                    first_release(ws);
                    let out = gate(ws, &updating(), &breaking, v(1, 1), &|| Err(String::new()));
                    assert_eq!(out.written.len(), 2, "the golden regenerated at 1.1");
                },
                v(1, 1),
                v(1, 1),
                &breaking,
                "a breaking change needs schema_version 2.0",
                false,
            ),
            (
                "provenance_error",
                &current_golden,
                v(1, 0),
                v(1, 0),
                &m,
                "the release step records the commit it was generated from: no git here",
                true,
            ),
        ];
        for (case, setup, release_to, contract, current, phrase, called) in cases {
            let dir = tempfile::tempdir().unwrap();
            let ws = Workspace::at(dir.path());
            setup(&ws);
            let before = tree(dir.path());
            let releaser = Releaser::new(case == "provenance_error");
            let out = gate(&ws, &releasing(release_to), current, contract, &|| {
                releaser.provenance()
            });
            assert_eq!(out.failures.len(), 1, "{case}: {:?}", out.failures);
            assert!(
                out.failures[0].contains(phrase),
                "{case}: {}",
                out.failures[0]
            );
            assert!(out.written.is_empty(), "{case}: wrote {:?}", out.written);
            assert_eq!(
                tree(dir.path()),
                before,
                "{case}: the workspace is unchanged"
            );
            assert_eq!(
                releaser.calls.get(),
                usize::from(called),
                "{case}: provenance asked"
            );
        }
    }

    #[test]
    fn a_release_writes_its_baseline_and_flips_the_header() {
        let m = synthetic(&base());
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::at(dir.path());
        workspace_with(
            &ws,
            &golden(
                &m,
                &Header {
                    contract: v(1, 0),
                    frozen: false,
                },
            ),
        );
        let releaser = Releaser::new(false);
        let out = gate(&ws, &releasing(v(1, 0)), &m, v(1, 0), &|| {
            releaser.provenance()
        });
        assert!(out.failures.is_empty(), "first_release: {:?}", out.failures);
        assert_eq!(releaser.calls.get(), 1);
        let baseline = ws.released_dir().join("1.0.json");
        assert_eq!(
            out.written,
            [baseline.clone(), ws.manifest_json(), ws.wire_names_txt()],
            "the baseline, then both golden files"
        );
        assert!(
            out.notes.iter().any(|n| n.starts_with(
                "wrote tests/contract/released/1.0.json (contract 1.0, tag v0.1.0, commit 000000000000)"
            )),
            "{:?}",
            out.notes
        );
        let frozen = golden(
            &m,
            &Header {
                contract: v(1, 0),
                frozen: true,
            },
        );
        assert_eq!(
            fs::read_to_string(ws.manifest_json()).unwrap(),
            frozen.manifest_json
        );
        assert_eq!(
            fs::read_to_string(ws.wire_names_txt()).unwrap(),
            frozen.wire_names_txt
        );

        // baseline_file_layout
        let text = fs::read_to_string(&baseline).unwrap();
        assert_eq!(
            Manifest::parse(&text).unwrap(),
            m,
            "the baseline is the manifest"
        );
        let recorded = json!({"commit": "0".repeat(40), "contract_version": "1.0", "created_at": "2026-01-01T00:00:00Z", "tag": "v0.1.0"});
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["provenance"],
            recorded
        );
        assert_eq!(
            text,
            m.render(&[("provenance", &recorded)]),
            "the whole text"
        );
        let leftovers: Vec<_> = fs::read_dir(ws.released_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name != "1.0.json")
            .collect();
        assert!(leftovers.is_empty(), "no .tmp left: {leftovers:?}");

        // The gate right after it passes, comparing an unchanged manifest with itself.
        let out = gate(
            &ws,
            &Mode {
                update_golden: false,
                advance: None,
            },
            &m,
            v(1, 0),
            &|| Err(String::new()),
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert!(
            out.notes.iter().any(|n| n.ends_with(": no change")),
            "{:?}",
            out.notes
        );

        // previous_satisfied: a breaking change released as 2.0 at contract 2.0.
        let breaking = without(base(), &["input install --channel"]);
        let out = gate(&ws, &updating(), &breaking, v(2, 0), &|| Err(String::new()));
        assert!(
            out.failures.is_empty(),
            "regenerated at 2.0: {:?}",
            out.failures
        );
        let out = gate(&ws, &releasing(v(2, 0)), &breaking, v(2, 0), &|| {
            releaser.provenance()
        });
        assert!(
            out.failures.is_empty(),
            "previous_satisfied: {:?}",
            out.failures
        );
        assert!(ws.released_dir().join("2.0.json").is_file());
        let out = gate(
            &ws,
            &Mode {
                update_golden: false,
                advance: None,
            },
            &breaking,
            v(2, 0),
            &|| Err(String::new()),
        );
        assert!(
            out.failures.is_empty()
                && out
                    .notes
                    .iter()
                    .any(|n| n == "comparing with tests/contract/released/2.0.json"),
            "comparison 1 now uses 2.0: {:?} {:?}",
            out.notes,
            out.failures
        );

        // A release is cut once: advancing to 2.0 again refuses and writes nothing.
        let before = tree(dir.path());
        let again = advance(&ws, v(2, 0), &breaking, v(2, 0), &|| releaser.provenance());
        assert!(
            again
                .as_ref()
                .is_err_and(|e| e.contains("a released baseline is never rewritten")),
            "{again:?}"
        );
        assert_eq!(tree(dir.path()), before);
    }

    #[test]
    fn real_provenance_shape() {
        let p = real_provenance(v(1, 0)).unwrap();
        assert!(
            p.commit.len() == 40
                && p.commit
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{p:?}"
        );
        assert_eq!(p.tag, format!("v{}", env!("CARGO_PKG_VERSION")));
        assert_eq!(p.contract_version, "1.0");
        assert!(
            p.created_at.parse::<jiff::Timestamp>().is_ok()
                && p.created_at.ends_with('Z')
                && !p.created_at.contains('.'),
            "whole UTC seconds: {p:?}"
        );
    }
}
