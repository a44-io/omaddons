//! Which TOC a client loads from a folder ([`loaded_toc`]) and whether it accepts it
//! ([`verdict`]), decided the way the client decides: by the TOC it would load and the
//! Interface inside it (vision D5, §5.3), never by upstream metadata. Both read through a
//! [`FolderView`], so the same code checks an installed folder ([`DiskFolder`]) and a
//! folder inside a candidate archive.
//!
//! Every client-specific value comes from [`ClientRules`] and
//! [`RulesTable::family_range`]. Unverified readings, each a single code path: a client
//! without comma lists reads the leading integer; family names (`mainline`, `classic`)
//! count as `AllowLoadGameType` values; between case-variant TOCs the byte-wise
//! smallest name is taken.

use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use schemars::JsonSchema;
use serde::Serialize;

use super::{Toc, parse_toc};
use crate::error::{Suggestion, Warning, WarningCode};
use crate::limits::TOC_LINE_MAX;
use crate::model::{FolderName, Target};
use crate::rules::{ClientRules, Family, RulesTable};

/// A folder's top level, wherever it lives. Object-safe.
pub trait FolderView {
    /// The folder's name as present.
    fn folder_name(&self) -> &FolderName;
    /// The regular files directly inside the folder.
    fn root_files(&self) -> io::Result<Vec<String>>;
    /// A root file's bytes, keeping at most `TOC_LINE_MAX + 1` bytes of each line.
    fn read_toc(&self, file: &str) -> io::Result<Vec<u8>>;
}

/// A folder on disk. Symlinks inside it are never followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskFolder {
    path: PathBuf,
    name: FolderName,
}

impl DiskFolder {
    /// The folder at `path`; `InvalidInput` when its name cannot be a folder name.
    pub fn new(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| FolderName::new(n).ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not an addon folder", path.display()),
                )
            })?;
        Ok(Self { path, name })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl FolderView for DiskFolder {
    fn folder_name(&self) -> &FolderName {
        &self.name
    }

    fn root_files(&self) -> io::Result<Vec<String>> {
        let mut files: Vec<String> = std::fs::read_dir(&self.path)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        files.sort();
        Ok(files)
    }

    fn read_toc(&self, file: &str) -> io::Result<Vec<u8>> {
        if file.contains('/') || file == "." || file == ".." {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{file:?} is not a file name"),
            ));
        }
        let path = self.path.join(file);
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        let fd = rustix::fs::open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        read_bounded(std::fs::File::from(fd))
    }
}

/// Every line of `reader`, keeping at most `TOC_LINE_MAX + 1` bytes of each (one more
/// than the client reads, so the parser still records the truncation). Memory stays
/// bounded however long a line is.
pub fn read_bounded(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut reader = BufReader::new(reader);
    let mut out = Vec::new();
    let mut kept = 0usize;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(out);
        }
        let len = buf.len();
        for &byte in buf {
            if byte == b'\n' || byte == b'\r' {
                out.push(byte);
                kept = 0;
            } else if kept <= TOC_LINE_MAX {
                out.push(byte);
                kept += 1;
            }
        }
        reader.consume(len);
    }
}

// ---------------------------------------------------------------------------------------
// The loaded TOC.

/// How specific a TOC's suffix is; earlier wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TocLevel {
    /// A game suffix (`_Standard`, `-Forever`).
    Game,
    /// A legacy suffix (none in the builtin rows).
    Legacy,
    /// A family suffix (`_Mainline`, `_Classic`).
    Family,
    /// `Folder.toc`.
    Plain,
}

/// The TOC a client loads from a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedToc {
    /// The file name as present.
    pub file: String,
    pub level: TocLevel,
    /// As the rules row spells it, e.g. `Mainline`.
    pub suffix: Option<String>,
    /// Another TOC differs from this one only in case; the client's pick is undefined.
    pub case_ambiguous: bool,
    pub toc: Toc,
}

/// What output shows of a loaded TOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LoadedTocSummary {
    pub file: String,
    pub level: TocLevel,
    pub suffix: Option<String>,
    pub case_ambiguous: bool,
    pub interface_raw: Option<String>,
}

impl LoadedToc {
    pub fn summary(&self) -> LoadedTocSummary {
        LoadedTocSummary {
            file: self.file.clone(),
            level: self.level,
            suffix: self.suffix.clone(),
            case_ambiguous: self.case_ambiguous,
            interface_raw: self.toc.interface_raw.clone(),
        }
    }
}

/// The TOC `rules`' client loads from the folder: the folder's name (ignoring ASCII
/// case) with the most specific suffix the row lists (game, then legacy, then family,
/// each in listed order, `-` or `_` as separator), else the plain TOC. Legacy clients
/// read only the plain TOC. `None` when there is none.
pub fn loaded_toc(view: &dyn FolderView, rules: &ClientRules) -> io::Result<Option<LoadedToc>> {
    let name = view.folder_name().as_str();
    let mut ranked: Vec<((TocLevel, usize), String, Option<String>)> = Vec::new();
    for file in view.root_files()? {
        let Some(rank) = rank(&file, name, rules) else {
            continue;
        };
        ranked.push((rank.0, file, rank.1));
    }
    ranked.sort_by(|a, b| (a.0, a.1.as_bytes()).cmp(&(b.0, b.1.as_bytes())));
    let Some((best_rank, file, suffix)) = ranked.first().cloned() else {
        return Ok(None);
    };
    let case_ambiguous = ranked
        .iter()
        .skip(1)
        .any(|(r, other, _)| *r == best_rank && other.eq_ignore_ascii_case(&file));
    let toc = parse_toc(&view.read_toc(&file)?);
    Ok(Some(LoadedToc {
        file,
        level: best_rank.0,
        suffix,
        case_ambiguous,
        toc,
    }))
}

/// The rank of a root file as a TOC of folder `name`, with its suffix as the row spells
/// it; `None` when the client would not load it.
fn rank(
    file: &str,
    name: &str,
    rules: &ClientRules,
) -> Option<((TocLevel, usize), Option<String>)> {
    let stem = file.len().checked_sub(4).and_then(|cut| {
        file.get(cut..)
            .filter(|ext| ext.eq_ignore_ascii_case(".toc"))
            .and(file.get(..cut))
    })?;
    let rest = stem
        .get(..name.len())
        .filter(|head| head.eq_ignore_ascii_case(name))
        .and(stem.get(name.len()..))?;
    if rest.is_empty() {
        return Some(((TocLevel::Plain, 0), None));
    }
    if !rules.multi_toc {
        return None;
    }
    let suffix = rest.strip_prefix('-').or_else(|| rest.strip_prefix('_'))?;
    [
        (TocLevel::Game, &rules.game_suffixes),
        (TocLevel::Legacy, &rules.legacy_suffixes),
        (TocLevel::Family, &rules.family_suffixes),
    ]
    .into_iter()
    .find_map(|(level, list)| {
        list.iter()
            .position(|s| s.eq_ignore_ascii_case(suffix))
            .map(|i| ((level, i), list.get(i).cloned()))
    })
}

/// The Interface values the client reads, and whether it ignored a comma list (a client
/// without comma support reads the leading integer).
pub fn effective_interfaces(toc: &Toc, rules: &ClientRules) -> (Vec<u32>, bool) {
    if rules.comma_interface {
        return (toc.interfaces.clone(), false);
    }
    let entries = toc.interface_raw.as_deref().map_or(0, |raw| {
        raw.split(',').filter(|e| !e.trim().is_empty()).count()
    });
    (toc.interface_leading.into_iter().collect(), entries > 1)
}

// ---------------------------------------------------------------------------------------
// The verdict.

/// Whether a client accepts a folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Compatible {
        /// The best Interface is below the client's: it loads only with out-of-date
        /// addons enabled.
        out_of_date: bool,
    },
    WrongFlavor {
        #[serde(flatten)]
        reason: WrongFlavorReason,
    },
    /// No TOC the client would load.
    NoToc,
}

/// Why a folder is the wrong flavor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum WrongFlavorReason {
    /// `AllowLoadGameType` / `ExcludeLoadGameType` rule the client's game out.
    GameTypeExcluded,
    /// The loaded TOC has no Interface the client can read.
    NoInterface,
    /// No Interface value lies in the client's family range.
    InterfaceNotInFamily {
        interfaces: Vec<u32>,
        family: Family,
    },
    /// The client's own interface is unknown (0), so nothing can be proven compatible.
    TargetInterfaceUnknown,
}

/// The verdict on one folder, with the TOC it rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VerdictReport {
    pub loaded: Option<LoadedTocSummary>,
    pub verdict: Verdict,
    pub warnings: Vec<Warning>,
}

/// Whether a client (`rules`, `family`, its own `target_interface`) accepts the folder.
pub fn verdict(
    view: &dyn FolderView,
    rules: &ClientRules,
    family: Family,
    target_interface: u32,
    table: &RulesTable,
) -> io::Result<VerdictReport> {
    let loaded = loaded_toc(view, rules)?;
    let folder = view.folder_name().as_str();
    Ok(judge(
        folder,
        loaded.as_ref(),
        rules,
        family,
        target_interface,
        table,
    ))
}

/// [`verdict`] on a TOC already loaded from `folder` (`None`: there is none), for
/// callers that also keep the TOC.
pub fn judge(
    folder: &str,
    loaded: Option<&LoadedToc>,
    rules: &ClientRules,
    family: Family,
    target_interface: u32,
    table: &RulesTable,
) -> VerdictReport {
    let Some(loaded) = loaded else {
        return VerdictReport {
            loaded: None,
            verdict: Verdict::NoToc,
            warnings: Vec::new(),
        };
    };
    let mut warnings = Vec::new();
    let wrong = |reason| Verdict::WrongFlavor { reason };
    let verdict = 'v: {
        if !rules.legacy && game_type_excluded(&loaded.toc, rules) {
            break 'v wrong(WrongFlavorReason::GameTypeExcluded);
        }
        let (interfaces, comma_ignored) = effective_interfaces(&loaded.toc, rules);
        if comma_ignored {
            warnings.push(Warning::new(
                WarningCode::TocCommaOnLegacy,
                format!(
                    "{folder}: {} lists several interfaces, but this client reads only the first",
                    loaded.file
                ),
            ));
        }
        if interfaces.is_empty() {
            break 'v wrong(WrongFlavorReason::NoInterface);
        }
        let in_range: Vec<u32> = match table.family_range(family) {
            Some(range) => interfaces
                .iter()
                .copied()
                .filter(|i| range.contains(*i))
                .collect(),
            None if target_interface == 0 => {
                break 'v wrong(WrongFlavorReason::TargetInterfaceUnknown);
            }
            None => interfaces
                .iter()
                .copied()
                .filter(|i| i / 100 == target_interface / 100)
                .collect(),
        };
        let Some(best) = in_range.iter().copied().max() else {
            break 'v wrong(WrongFlavorReason::InterfaceNotInFamily { interfaces, family });
        };
        let out_of_date = best < target_interface;
        if out_of_date {
            warnings.push(
                Warning::new(
                    WarningCode::OutOfDate,
                    format!(
                        "{folder}: {} targets interface {best}, older than the client's {target_interface}; it loads only with out-of-date addons enabled",
                        loaded.file
                    ),
                )
                .with_fix(Suggestion::text(
                    "update the addon, or enable \"Load out of date AddOns\" in the client",
                )),
            );
        }
        Verdict::Compatible { out_of_date }
    };
    VerdictReport {
        loaded: Some(loaded.summary()),
        verdict,
        warnings,
    }
}

/// [`verdict`] for a target.
pub fn verify_target(
    view: &dyn FolderView,
    target: &Target,
    table: &RulesTable,
) -> io::Result<VerdictReport> {
    verdict(view, &target.rules, target.family, target.interface, table)
}

/// `AllowLoadGameType` names none of the client's game types or family-level names, or
/// `ExcludeLoadGameType` names one.
fn game_type_excluded(toc: &Toc, rules: &ClientRules) -> bool {
    let names: Vec<String> = rules
        .game_types
        .iter()
        .chain(&rules.family_suffixes)
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let named = |values: &[String]| values.iter().any(|v| names.contains(v));
    (!toc.allow_load_game_type.is_empty() && !named(&toc.allow_load_game_type))
        || named(&toc.exclude_load_game_type)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::rules::{ClientVersion, InstallKind};

    /// An in-memory folder.
    struct Memory {
        name: FolderName,
        files: BTreeMap<String, Vec<u8>>,
    }

    impl Memory {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            Self {
                name: FolderName::new(name).unwrap(),
                files: files
                    .iter()
                    .map(|(n, c)| ((*n).to_owned(), c.as_bytes().to_vec()))
                    .collect(),
            }
        }
    }

    impl FolderView for Memory {
        fn folder_name(&self) -> &FolderName {
            &self.name
        }
        fn root_files(&self) -> io::Result<Vec<String>> {
            Ok(self.files.keys().cloned().collect())
        }
        fn read_toc(&self, file: &str) -> io::Result<Vec<u8>> {
            self.files
                .get(file)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }
    }

    struct Client {
        rules: ClientRules,
        family: Family,
        interface: u32,
    }

    fn client(version: &str, kind: InstallKind) -> Client {
        let table = RulesTable::builtin();
        let v: ClientVersion = version.parse().unwrap();
        let rules = table.rules_for(&v, kind).clone();
        Client {
            interface: crate::rules::interface_for(&v, &rules),
            family: table.family_for(&v, kind),
            rules,
        }
    }

    fn bnet(version: &str) -> Client {
        client(version, InstallKind::Battlenet)
    }

    fn legacy(version: &str) -> Client {
        client(version, InstallKind::Custom)
    }

    fn judge(view: &Memory, c: &Client) -> VerdictReport {
        verdict(
            view,
            &c.rules,
            c.family,
            c.interface,
            &RulesTable::builtin(),
        )
        .unwrap()
    }

    fn loaded(view: &Memory, c: &Client) -> Option<String> {
        loaded_toc(view, &c.rules).unwrap().map(|l| l.file)
    }

    fn reason(report: &VerdictReport) -> serde_json::Value {
        serde_json::to_value(&report.verdict).unwrap()
    }

    #[test]
    fn suffix_precedence_per_client() {
        let foo = Memory::new(
            "Foo",
            &[
                ("Foo_Standard.toc", "## Interface: 120105\n"),
                ("Foo_Mainline.toc", "## Interface: 120100\n"),
                ("Foo_Classic.toc", "## Interface: 11509\n"),
                ("Foo.toc", "## Interface: 30300\n"),
            ],
        );
        for (c, expected) in [
            (bnet("12.1.5"), "Foo_Standard.toc"),
            (bnet("12.1.0"), "Foo_Mainline.toc"),
            (bnet("1.15.9"), "Foo_Classic.toc"),
            (legacy("3.3.5"), "Foo.toc"),
        ] {
            assert_eq!(
                loaded(&foo, &c).as_deref(),
                Some(expected),
                "{}",
                c.rules.name
            );
        }
        let dashes = Memory::new(
            "Foo",
            &[
                ("Foo-Classic.toc", "## Interface: 11509\n"),
                ("Foo-Vanilla.toc", "## Interface: 11509\n"),
            ],
        );
        assert_eq!(
            loaded(&dashes, &bnet("1.15.9")).as_deref(),
            Some("Foo-Vanilla.toc"),
            "game beats family"
        );
        let unlisted = Memory::new("Foo", &[("Foo_Cata.toc", "## Interface: 40402\n")]);
        assert_eq!(
            loaded(&unlisted, &bnet("12.1.5")),
            None,
            "a suffix the row does not list"
        );
        let legacy_only_suffixed =
            Memory::new("Foo", &[("Foo_Classic.toc", "## Interface: 30300\n")]);
        assert_eq!(loaded(&legacy_only_suffixed, &legacy("3.3.5")), None);
        let suffixed = loaded_toc(&foo, &bnet("12.1.5").rules).unwrap().unwrap();
        assert_eq!(
            (suffixed.level, suffixed.suffix.as_deref()),
            (TocLevel::Game, Some("Standard"))
        );
    }

    #[test]
    fn forever_loads_mainline_but_the_interface_decides() {
        let forever = bnet("1.60.1");
        let mainline_only = Memory::new("Foo", &[("Foo_Mainline.toc", "## Interface: 120105\n")]);
        let report = judge(&mainline_only, &forever);
        assert_eq!(
            report.loaded.as_ref().map(|l| l.file.as_str()),
            Some("Foo_Mainline.toc")
        );
        assert_eq!(
            reason(&report),
            serde_json::json!({"verdict": "wrong_flavor", "reason": "interface_not_in_family", "interfaces": [120_105], "family": "forever"})
        );
        let both = Memory::new(
            "Foo",
            &[
                ("Foo_Mainline.toc", "## Interface: 120105\n"),
                ("Foo-Forever.toc", "## Interface: 16001\n"),
            ],
        );
        let report = judge(&both, &forever);
        assert_eq!(report.loaded.unwrap().file, "Foo-Forever.toc");
        assert_eq!(report.verdict, Verdict::Compatible { out_of_date: false });
    }

    #[test]
    fn comma_lists() {
        let foo = Memory::new(
            "Foo",
            &[("Foo.toc", "## Interface: 120105, 50504, 11509\n")],
        );
        for c in [bnet("12.1.5"), bnet("5.5.4"), bnet("1.15.9")] {
            assert_eq!(
                judge(&foo, &c).verdict,
                Verdict::Compatible { out_of_date: false },
                "{}",
                c.rules.name
            );
        }
        for c in [bnet("2.5.6"), bnet("1.60.1")] {
            assert!(
                matches!(judge(&foo, &c).verdict, Verdict::WrongFlavor { .. }),
                "{}",
                c.rules.name
            );
        }
        let report = judge(&foo, &legacy("1.12.1"));
        assert!(matches!(report.verdict, Verdict::WrongFlavor { .. }));
        assert_eq!(
            report.warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
            [WarningCode::TocCommaOnLegacy]
        );
        let (values, ignored) = effective_interfaces(
            &parse_toc(b"## Interface: 11200, 30300\n"),
            &legacy("1.12.1").rules,
        );
        assert_eq!((values, ignored), (vec![11_200], true));
    }

    #[test]
    fn modern_classic_builds_never_verify_on_legacy_clients() {
        let foo = Memory::new(
            "Foo",
            &[
                ("Foo.toc", "## Interface: 11509\n"),
                ("Foo_Vanilla.toc", "## Interface: 11509\n"),
            ],
        );
        let report = judge(&foo, &legacy("1.12.1"));
        assert_eq!(report.loaded.unwrap().file, "Foo.toc");
        assert!(matches!(report.verdict, Verdict::WrongFlavor { .. }));
    }

    #[test]
    fn names_ignore_case() {
        let c = bnet("12.1.0");
        let lower = Memory::new("Foo", &[("foo_mainline.TOC", "## Interface: 120100\n")]);
        assert_eq!(loaded(&lower, &c).as_deref(), Some("foo_mainline.TOC"));
        let pair = Memory::new(
            "Foo",
            &[
                ("foo_mainline.TOC", "## Interface: 120100\n"),
                ("Foo_Mainline.toc", "## Interface: 120100\n"),
            ],
        );
        let l = loaded_toc(&pair, &c.rules).unwrap().unwrap();
        assert_eq!(
            (l.file.as_str(), l.case_ambiguous),
            ("Foo_Mainline.toc", true),
            "byte-wise smallest"
        );
        let other = Memory::new(
            "Foo",
            &[
                ("Bar.toc", "## Interface: 120100\n"),
                ("Foobar.toc", "## Interface: 120100\n"),
            ],
        );
        assert_eq!(judge(&other, &c).verdict, Verdict::NoToc);
    }

    #[test]
    fn out_of_date_and_newer() {
        let retail = bnet("12.1.5");
        let older = Memory::new("Foo", &[("Foo.toc", "## Interface: 120100\n")]);
        let report = judge(&older, &retail);
        assert_eq!(report.verdict, Verdict::Compatible { out_of_date: true });
        assert_eq!(report.warnings[0].code, WarningCode::OutOfDate);
        let newer = Memory::new("Foo", &[("Foo.toc", "## Interface: 120200\n")]);
        let report = judge(&newer, &retail);
        assert_eq!(
            (report.verdict, report.warnings.len()),
            (Verdict::Compatible { out_of_date: false }, 0)
        );
    }

    #[test]
    fn family_range_edges() {
        let era = bnet("1.15.9");
        for (interface, compatible) in [
            (11_299, false),
            (11_300, true),
            (14_999, true),
            (15_000, false),
        ] {
            let foo = Memory::new(
                "Foo",
                &[("Foo.toc", &format!("## Interface: {interface}\n"))],
            );
            let ok = matches!(judge(&foo, &era).verdict, Verdict::Compatible { .. });
            assert_eq!(ok, compatible, "{interface}");
        }
        let empty = Memory::new("Foo", &[("Foo.toc", "## Title: no interface\n")]);
        assert_eq!(reason(&judge(&empty, &era))["reason"], "no_interface");
    }

    #[test]
    fn unknown_family_matches_major_and_minor() {
        let table = RulesTable::builtin();
        let rules = table.fallback(InstallKind::Battlenet).clone();
        let foo = |i: u32| Memory::new("Foo", &[("Foo.toc", &format!("## Interface: {i}\n"))]);
        let judge_unknown = |view: &Memory, target: u32| {
            verdict(view, &rules, Family::Unknown, target, &table)
                .unwrap()
                .verdict
        };
        assert_eq!(
            judge_unknown(&foo(130_005), 130_000),
            Verdict::Compatible { out_of_date: false }
        );
        assert!(matches!(
            judge_unknown(&foo(130_105), 130_000),
            Verdict::WrongFlavor { .. }
        ));
        assert_eq!(
            serde_json::to_value(judge_unknown(&foo(130_000), 0)).unwrap()["reason"],
            "target_interface_unknown"
        );
    }

    #[test]
    fn game_type_directives() {
        let standard_only = Memory::new(
            "Foo",
            &[(
                "Foo.toc",
                "## Interface: 16001\n## AllowLoadGameType: standard\n",
            )],
        );
        assert_eq!(
            reason(&judge(&standard_only, &bnet("1.60.1")))["reason"],
            "game_type_excluded"
        );
        let family_name = Memory::new(
            "Foo",
            &[(
                "Foo.toc",
                "## Interface: 16001\n## AllowLoadGameType: mainline\n",
            )],
        );
        assert!(
            matches!(
                judge(&family_name, &bnet("1.60.1")).verdict,
                Verdict::Compatible { .. }
            ),
            "family names count"
        );
        let excluded = Memory::new(
            "Foo",
            &[(
                "Foo.toc",
                "## Interface: 120105\n## ExcludeLoadGameType: standard\n",
            )],
        );
        assert!(matches!(
            judge(&excluded, &bnet("12.1.5")).verdict,
            Verdict::WrongFlavor { .. }
        ));
        let legacy_ignores = Memory::new(
            "Foo",
            &[(
                "Foo.toc",
                "## Interface: 30300\n## AllowLoadGameType: standard\n",
            )],
        );
        assert!(matches!(
            judge(&legacy_ignores, &legacy("3.3.5")).verdict,
            Verdict::Compatible { .. }
        ));
    }

    #[test]
    fn bounded_reads() {
        let long = vec![b'x'; 5 * TOC_LINE_MAX];
        let mut input = b"## Title: ".to_vec();
        input.extend_from_slice(&long);
        input.extend_from_slice(b"\r\n## Version: 1\n");
        let out = read_bounded(input.as_slice()).unwrap();
        assert_eq!(out.len(), TOC_LINE_MAX + 1 + 2 + "## Version: 1\n".len());
        let toc = parse_toc(&out);
        assert_eq!(toc.version.as_deref(), Some("1"));
        assert!(
            toc.issues
                .contains(&crate::toc::TocIssue::LineTruncated { line: 1 })
        );
    }
}
