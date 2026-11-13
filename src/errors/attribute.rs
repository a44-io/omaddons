//! Which installed folder an error most likely comes from (M3-D18), as pure functions of
//! the error's text and the client's installed folders. A stack shows where an error
//! surfaced, not its cause, so every answer carries its evidence: the rule that decided
//! ([`Method`]) and how sure it is ([`Confidence`]). Hand-written matchers, no regex;
//! each quotes the pattern it implements.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::frames::{FrameKind, frames, strip_prefix_ci};
use super::sanitize::taint_folder;

/// What an error is attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attributed {
    /// An addon's own code (the folder, in its installed spelling when installed).
    Addon(String),
    /// Only a library embedded in this folder.
    Library(String),
    /// Only the game's own code.
    Blizzard,
    Unknown,
}

/// The rule that decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// The folder a blocked or forbidden call names.
    ProtectedCall,
    /// The folder the client's own taint report names: tainted code read a secret value
    /// (omad-8u49).
    Taint,
    FirstAddonFrame,
    LibraryFrame,
    BlizzardOnly,
    None,
}

/// How sure an attribution is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// An attribution and its evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribution {
    pub target: Attributed,
    pub method: Method,
    pub confidence: Confidence,
}

/// What a frame's folder is (M3-D18 rule 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    Blizzard,
    /// `!BugGrabber` or `BugSack`: the error catcher itself.
    SelfGrabber,
    Library,
    Addon,
}

/// The folders installed in a client's `AddOns/` (protected ones included), compared
/// without ASCII case, as [`crate::model::FolderName::key`] does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderSet {
    by_key: BTreeMap<String, String>,
}

impl FolderSet {
    pub fn new<S: AsRef<str>>(folders: impl IntoIterator<Item = S>) -> Self {
        Self {
            by_key: folders
                .into_iter()
                .map(|f| (f.as_ref().to_ascii_lowercase(), f.as_ref().to_owned()))
                .collect(),
        }
    }

    /// `folder`'s installed spelling, when installed.
    pub fn get(&self, folder: &str) -> Option<&str> {
        self.by_key
            .get(&folder.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn contains(&self, folder: &str) -> bool {
        self.get(folder).is_some()
    }

    /// The one installed folder whose name ends with `tail`, when exactly one does.
    pub fn unique_suffix(&self, tail: &str) -> Option<&str> {
        let tail = tail.to_ascii_lowercase();
        let mut matches = self.by_key.iter().filter(|(key, _)| key.ends_with(&tail));
        let (_, folder) = matches.next()?;
        matches.next().is_none().then_some(folder.as_str())
    }

    /// r152's `<Folder>[-<version>]`: the longest installed folder the segment is, or
    /// starts with followed by `-`.
    fn versioned(&self, segment: &str) -> Option<&str> {
        let segment = segment.to_ascii_lowercase();
        self.by_key
            .iter()
            .filter(|(key, _)| {
                segment == **key
                    || segment
                        .strip_prefix(key.as_str())
                        .is_some_and(|rest| rest.starts_with('-'))
            })
            .max_by_key(|(key, _)| key.len())
            .map(|(_, folder)| folder.as_str())
    }
}

/// How a frame's folder was read, which decides how sure an attribution to it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// `AddOns/<Folder>/` in the path.
    Full,
    /// `AddOns/<Folder>-<digits>/`, a leftover of BugGrabber ≤ v12.0.2, read as
    /// `<Folder>`.
    Retried,
    /// A `...`-truncated path cut inside `Interface/AddOns/`.
    TruncatedPrefix,
    /// A `...`-truncated path whose cut folder name ends one installed folder's.
    TruncatedSuffix,
    /// r152's `<Folder>-<version>\`.
    Versioned,
    /// r152's `Major-Minor (Folder)`: a library embedded in the folder.
    LibraryForm,
}

/// A frame's folder, how it was read, and the path below the folder.
struct Resolved<'p> {
    folder: String,
    read: Read,
    below: &'p str,
}

/// The folder a frame's path names (M3-D18 rule 5), in its installed spelling when
/// installed; `None` when it names none, or a truncated one that matches no installed
/// folder (or more than one).
pub fn folder_of(path: &str, installed: &FolderSet, legacy: bool) -> Option<String> {
    resolve(path, installed, legacy).map(|r| r.folder)
}

fn resolve<'p>(path: &'p str, installed: &FolderSet, legacy: bool) -> Option<Resolved<'p>> {
    let spelled = |folder: &str| installed.get(folder).unwrap_or(folder).to_owned();
    // r152: `Major-Minor (Folder)`.
    if legacy
        && let Some(open) = path.rfind(" (")
        && let Some(folder) = path[open + 2..].strip_suffix(')')
        && !folder.is_empty()
    {
        return Some(Resolved {
            folder: spelled(folder),
            read: Read::LibraryForm,
            below: "",
        });
    }
    // `(?:^|[\\/])AddOns[\\/]([^\\/]+)[\\/]`
    if let Some((folder, below)) = after_addons(path) {
        if installed.contains(folder) {
            return Some(Resolved {
                folder: spelled(folder),
                read: Read::Full,
                below,
            });
        }
        // An uninstalled `X-<digits>` retries as `X`.
        if let Some((base, digits)) = folder.rsplit_once('-')
            && !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && installed.contains(base)
        {
            return Some(Resolved {
                folder: spelled(base),
                read: Read::Retried,
                below,
            });
        }
        return Some(Resolved {
            folder: folder.to_owned(),
            read: Read::Full,
            below,
        });
    }
    if let Some(rest) = path.strip_prefix("...") {
        // A cut inside `Interface/AddOns/` (`dOns/`, `ns/`): the next segment, when it is
        // an installed folder. A short tail such as `s/` also ends folder names
        // (`...s/Nameplates/` cut from `Modules/`), so an uninstalled segment is not
        // taken for one.
        if let Some(stripped) = strip_addons_tail(rest)
            && let Some((folder, below)) = split_segment(stripped)
            && let Some(installed) = installed.get(folder)
        {
            return Some(Resolved {
                folder: installed.to_owned(),
                read: Read::TruncatedPrefix,
                below,
            });
        }
        // Else the cut folder name must end exactly one installed folder's.
        let (segment, below) = split_segment(rest)?;
        let folder = installed.unique_suffix(segment)?;
        return Some(Resolved {
            folder: folder.to_owned(),
            read: Read::TruncatedSuffix,
            below,
        });
    }
    // r152: `<Folder>[-<version>]\…`.
    if legacy && let Some((segment, below)) = path.split_once('\\') {
        let folder = installed.versioned(segment)?;
        return Some(Resolved {
            folder: folder.to_owned(),
            read: Read::Versioned,
            below,
        });
    }
    None
}

/// The folder after an `AddOns` segment, and the path below it.
fn after_addons(path: &str) -> Option<(&str, &str)> {
    let mut start = 0;
    loop {
        let segment_end = path[start..].find(['/', '\\']).map(|at| start + at)?;
        if path[start..segment_end].eq_ignore_ascii_case("AddOns") {
            let rest = &path[segment_end + 1..];
            if let Some((folder, below)) = split_segment(rest)
                && !folder.is_empty()
            {
                return Some((folder, below));
            }
        }
        start = segment_end + 1;
    }
}

/// `text`'s first segment and the rest after its separator; `None` when no separator
/// follows (the segment is a file, not a folder).
fn split_segment(text: &str) -> Option<(&str, &str)> {
    let at = text.find(['/', '\\'])?;
    Some((&text[..at], &text[at + 1..]))
}

/// `rest` without its longest prefix that is a suffix of `Interface/AddOns/` (either
/// separator, without ASCII case).
fn strip_addons_tail(rest: &str) -> Option<&str> {
    const DIR: &[u8] = b"interface/addons/";
    let bytes = rest.as_bytes();
    (1..=DIR.len()).rev().find_map(|len| {
        let tail = &DIR[DIR.len() - len..];
        let matches = bytes.len() >= len
            && bytes[..len].iter().zip(tail).all(|(b, t)| {
                let b = if *b == b'\\' {
                    b'/'
                } else {
                    b.to_ascii_lowercase()
                };
                b == *t
            });
        matches.then(|| &rest[len..])
    })
}

/// The class of a frame in `folder` at `path` (M3-D18 rule 6).
pub fn classify(folder: &str, path: &str) -> FrameClass {
    let below = after_folder(folder, path);
    class_of(folder, path, below, false)
}

fn class_of(folder: &str, path: &str, below: &str, library_form: bool) -> FrameClass {
    if strip_prefix_ci(folder, "Blizzard_").is_some() || blizzard_ui(path) {
        FrameClass::Blizzard
    } else if folder.eq_ignore_ascii_case("!BugGrabber") || folder.eq_ignore_ascii_case("BugSack") {
        FrameClass::SelfGrabber
    } else if library_form || directories(below).any(is_library_segment) {
        FrameClass::Library
    } else {
        FrameClass::Addon
    }
}

/// The path below `folder`'s segment (all of it when no segment names the folder).
fn after_folder<'p>(folder: &str, path: &'p str) -> &'p str {
    let mut start = 0;
    while let Some(at) = path[start..].find(['/', '\\']) {
        let segment = &path[start..start + at];
        let named = segment.eq_ignore_ascii_case(folder)
            || strip_prefix_ci(segment, folder).is_some_and(|rest| rest.starts_with('-'));
        if named {
            return &path[start + at + 1..];
        }
        start += at + 1;
    }
    path
}

/// The directory segments of a relative path (its file name left out).
fn directories(below: &str) -> impl Iterator<Item = &str> {
    let mut segments: Vec<&str> = below.split(['/', '\\']).collect();
    segments.pop();
    segments.into_iter()
}

/// A library directory: `^(libs?|libraries|embeds?|externals?)$` and `^LibStub$`
/// without case, `^Lib[A-Z0-9]` and `^[A-Za-z]+-\d+\.\d+$` with it.
fn is_library_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "lib" | "libs" | "libraries" | "embed" | "embeds" | "external" | "externals" | "libstub"
    ) {
        return true;
    }
    let bytes = segment.as_bytes();
    if bytes.starts_with(b"Lib")
        && bytes
            .get(3)
            .is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return true;
    }
    // `^[A-Za-z]+-\d+\.\d+$`, as `CallbackHandler-1.0`.
    let Some((name, version)) = segment.split_once('-') else {
        return false;
    };
    let Some((major, minor)) = version.split_once('.') else {
        return false;
    };
    let all = |s: &str, f: fn(&u8) -> bool| !s.is_empty() && s.as_bytes().iter().all(f);
    all(name, u8::is_ascii_alphabetic)
        && all(major, u8::is_ascii_digit)
        && all(minor, u8::is_ascii_digit)
}

/// The game's own UI code: `Interface[\\/](FrameXML|SharedXML|GlueXML)[\\/]` or
/// `Interface_\w+[\\/]`.
fn blizzard_ui(path: &str) -> bool {
    let segments: Vec<&str> = path.split(['/', '\\']).collect();
    segments.windows(2).any(|pair| {
        let (first, second) = (pair[0], pair[1]);
        let ui = ["FrameXML", "SharedXML", "GlueXML"]
            .iter()
            .any(|dir| second.eq_ignore_ascii_case(dir));
        let suffixed = strip_prefix_ci(first, "Interface_").is_some_and(|rest| {
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        (first.eq_ignore_ascii_case("Interface") && ui && segments.len() > 2) || suffixed
    })
}

/// A blocked or forbidden call's folder (M3-D18 rule 1):
/// `^\[ADDON_ACTION_(BLOCKED|FORBIDDEN)\] [^']*'([^']+)'`, and r152's `event` entries'
/// first `'…'`.
fn protected_call(message: &str, is_event: bool) -> Option<&str> {
    let tagged = ["[ADDON_ACTION_BLOCKED] ", "[ADDON_ACTION_FORBIDDEN] "]
        .iter()
        .find_map(|tag| strip_prefix_ci(message, tag));
    match tagged {
        Some(rest) => first_quoted(rest),
        None if is_event => first_quoted(message),
        None => None,
    }
}

/// The folder a secret-value taint report names on its first line (after `LUA_WARNING: `,
/// [`taint_folder`]), and whether a `Lua Taint: <folder>` line of the same message names
/// another one. The quoted name wins a disagreement: it is the one the message's own
/// text attributes, and the one strict output shows.
fn tainted_by(message: &str) -> Option<(&str, bool)> {
    let message = strip_prefix_ci(message, "LUA_WARNING: ").unwrap_or(message);
    let message = message
        .split_once("|cffffd200Stack:|r")
        .map_or(message, |(m, _)| m);
    let mut lines = message.lines();
    let folder = taint_folder(lines.next()?.trim())?;
    let disagrees = lines
        .filter_map(|line| strip_prefix_ci(line.trim(), "Lua Taint: "))
        .any(|named| !named.trim().eq_ignore_ascii_case(folder));
    Some((folder, disagrees))
}

/// The first non-empty `'…'` in `text`.
fn first_quoted(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once('\'')?;
    let (name, _) = rest.split_once('\'')?;
    (!name.is_empty()).then_some(name)
}

/// The most likely source of an error (M3-D18 rules 1, 2, 3, 7 and 9): the folder of a
/// protected call; else the folder the client's taint report names (omad-8u49); else
/// the first addon frame's folder; else the first library frame's;
/// else Blizzard when any frame is the game's; else unknown. `is_event` is r152's
/// `type == "event"`.
pub fn attribute(
    message: &str,
    stack: Option<&str>,
    is_event: bool,
    installed: &FolderSet,
    legacy: bool,
) -> Attribution {
    let decided = |target, method, confidence| Attribution {
        target,
        method,
        confidence,
    };
    if let Some(folder) = protected_call(message, is_event) {
        return match installed.get(folder) {
            Some(folder) => decided(
                Attributed::Addon(folder.to_owned()),
                Method::ProtectedCall,
                Confidence::High,
            ),
            None => decided(Attributed::Unknown, Method::ProtectedCall, Confidence::Low),
        };
    }
    // The client's taint report names the addon whose taint reached the secret value;
    // the stack under it is usually the game's own frames, where tainted code surfaced.
    // Taint spreads, so the named addon may itself be a victim: high, as a protected call
    // is, and medium when the report's own `Lua Taint:` line names another folder. A
    // `Blizzard_*` folder named there is the game's: the frames decide, as before.
    if let Some((folder, disagrees)) = tainted_by(message)
        && strip_prefix_ci(folder, "Blizzard_").is_none()
    {
        return match installed.get(folder) {
            Some(folder) => decided(
                Attributed::Addon(folder.to_owned()),
                Method::Taint,
                if disagrees {
                    Confidence::Medium
                } else {
                    Confidence::High
                },
            ),
            None => decided(Attributed::Unknown, Method::Taint, Confidence::Low),
        };
    }
    let mut library = None;
    let mut blizzard = false;
    let mut custom_code = false;
    for frame in frames(message, stack, legacy) {
        if frame.kind == FrameKind::Chunk {
            custom_code = true;
        }
        let Some(path) = frame.path else {
            continue;
        };
        if blizzard_ui(path) {
            blizzard = true;
            continue;
        }
        let Some(resolved) = resolve(path, installed, legacy) else {
            continue;
        };
        let library_form = resolved.read == Read::LibraryForm;
        match class_of(&resolved.folder, path, resolved.below, library_form) {
            FrameClass::Blizzard => blizzard = true,
            FrameClass::SelfGrabber => {}
            FrameClass::Library => {
                library.get_or_insert(resolved.folder);
            }
            FrameClass::Addon => {
                let confidence = match resolved.read {
                    Read::Full if !custom_code => Confidence::High,
                    Read::Full | Read::TruncatedSuffix | Read::Versioned => Confidence::Medium,
                    Read::Retried | Read::TruncatedPrefix | Read::LibraryForm => Confidence::Low,
                };
                return decided(
                    Attributed::Addon(resolved.folder),
                    Method::FirstAddonFrame,
                    confidence,
                );
            }
        }
    }
    match library {
        Some(folder) => decided(
            Attributed::Library(folder),
            Method::LibraryFrame,
            Confidence::Medium,
        ),
        None if blizzard => decided(Attributed::Blizzard, Method::BlizzardOnly, Confidence::Low),
        None => decided(Attributed::Unknown, Method::None, Confidence::Low),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn installed() -> FolderSet {
        FolderSet::new([
            "Questie",
            "AtlasLootClassic",
            "WeakAuras",
            "WeakAurasOptions",
            "Bartender4",
            "!BugGrabber",
            "BugSack",
            "MyUI_EncounterWarnings",
            "Recount",
            "Details",
            "Baz",
            "DBM-Core",
            "Blizzard_EncounterJournal",
        ])
    }

    fn check(
        attribution: &Attribution,
        target: Attributed,
        method: Method,
        confidence: Confidence,
    ) {
        assert_eq!(
            attribution,
            &Attribution {
                target,
                method,
                confidence
            }
        );
    }

    fn addon(folder: &str) -> Attributed {
        Attributed::Addon(folder.to_owned())
    }

    #[test]
    fn library_dispatch_blames_the_addon_whose_frame_comes_first() {
        let stack = "[Interface/AddOns/Questie/Modules/QuestieEvent.lua]:88: in function <Interface/AddOns/Questie/Modules/QuestieEvent.lua:80>\n[C]: in function 'xpcall'\n[Interface/AddOns/AtlasLootClassic/Libs/CallbackHandler-1.0/CallbackHandler-1.0.lua]:109: in function 'Fire'";
        let a = attribute(
            "[Interface/AddOns/Questie/Modules/QuestieEvent.lua]:88: attempt to compare number with nil",
            Some(stack),
            false,
            &installed(),
            false,
        );
        check(
            &a,
            addon("Questie"),
            Method::FirstAddonFrame,
            Confidence::High,
        );
    }

    #[test]
    fn weakauras_custom_code_is_weakauras_with_medium_confidence() {
        let chunk = "[string \"--[==[ Error in 'Interrupt Bar' ]==] return function() end\"]";
        let a = attribute(
            &format!("{chunk}:4: attempt to perform arithmetic on a nil value"),
            Some(&format!(
                "{chunk}:4: in function <{chunk}:1>\n[C]: in function 'xpcall'\n[Interface/AddOns/WeakAuras/WeakAuras.lua]:4512: in function 'RunTrigger'"
            )),
            false,
            &installed(),
            false,
        );
        check(
            &a,
            addon("WeakAuras"),
            Method::FirstAddonFrame,
            Confidence::Medium,
        );
    }

    #[test]
    fn a_protected_call_names_its_folder() {
        let stack = "[Interface/AddOns/!BugGrabber/BugGrabber.lua]:485: in function <Interface/AddOns/!BugGrabber/BugGrabber.lua:485>\n[C]: in function 'CastSpellByName'";
        let forbidden = "[ADDON_ACTION_FORBIDDEN] AddOn 'Bartender4' tried to call the protected function 'CastSpellByName()'.";
        check(
            &attribute(forbidden, Some(stack), false, &installed(), false),
            addon("Bartender4"),
            Method::ProtectedCall,
            Confidence::High,
        );
        let blocked = "[addon_action_blocked] AddOn 'bartender4' tried to call 'X()'.";
        check(
            &attribute(blocked, None, false, &installed(), false),
            addon("Bartender4"),
            Method::ProtectedCall,
            Confidence::High,
        );
        let missing = "[ADDON_ACTION_FORBIDDEN] AddOn 'Gone' tried to call 'X()'.";
        check(
            &attribute(missing, Some(stack), false, &installed(), false),
            Attributed::Unknown,
            Method::ProtectedCall,
            Confidence::Low,
        );
        // r152's events: the first quoted name.
        check(
            &attribute(
                "Recount tried 'Recount' then 'Details'",
                None,
                true,
                &installed(),
                true,
            ),
            addon("Recount"),
            Method::ProtectedCall,
            Confidence::High,
        );
    }

    /// The client's secret-value taint report (omad-8u49): the folder it names, whatever
    /// the stack says, never over a protected call; low and unknown when that folder is
    /// not installed; medium when its `Lua Taint:` line names another; a `Blizzard_*`
    /// folder left to the frames.
    #[test]
    fn a_taint_report_names_its_folder_over_the_frames() {
        let installed = FolderSet::new(["OneForAll", "Questie", "Blizzard_MawBuffs"]);
        let report = |folder: &str, taint: &str| {
            format!(
                "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by '{folder}'\nLua Taint: {taint}"
            )
        };
        let blizzard = "[Interface/AddOns/Blizzard_MawBuffs/Blizzard_MawBuffs.lua]:4: in function 'ShouldShowMawBuffs'\n[Interface/AddOns/Blizzard_ObjectiveTracker/Blizzard_ScenarioObjectiveTracker.lua]:187: in function 'LayoutContents'";
        let plain = report("OneForAll", "OneForAll");
        check(
            &attribute(&plain, Some(blizzard), false, &installed, false),
            addon("OneForAll"),
            Method::Taint,
            Confidence::High,
        );
        // Its `LUA_WARNING:` form, with the stack embedded after the message.
        let warning = format!(
            "LUA_WARNING: {plain}\n|cffffd200Stack:|r\n[C]: in function 'secureexecuterange'\n[Interface/AddOns/Blizzard_EditMode/Shared/EditModeManager.lua]:1054: in function 'X'"
        );
        check(
            &attribute(&warning, None, false, &installed, false),
            addon("OneForAll"),
            Method::Taint,
            Confidence::High,
        );
        // An addon frame on the stack does not outrank the report.
        let questie = "[Interface/AddOns/Questie/Questie.lua]:9: in function 'X'";
        check(
            &attribute(&plain, Some(questie), false, &installed, false),
            addon("OneForAll"),
            Method::Taint,
            Confidence::High,
        );
        check(
            &attribute(
                &report("Gone", "Gone"),
                Some(blizzard),
                false,
                &installed,
                false,
            ),
            Attributed::Unknown,
            Method::Taint,
            Confidence::Low,
        );
        check(
            &attribute(
                &report("OneForAll", "Questie"),
                Some(blizzard),
                false,
                &installed,
                false,
            ),
            addon("OneForAll"),
            Method::Taint,
            Confidence::Medium,
        );
        // The game's own folder named as the taint: the frames decide, as before.
        check(
            &attribute(
                &report("Blizzard_MawBuffs", "Blizzard_MawBuffs"),
                Some(blizzard),
                false,
                &installed,
                false,
            ),
            Attributed::Blizzard,
            Method::BlizzardOnly,
            Confidence::Low,
        );
        // A look-alike is an addon's own text: the frames decide.
        check(
            &attribute(
                "GetAuraDataByIndex(): Auras cannot be accessed when secret while tainted by 'OneForAll' again",
                Some(questie),
                false,
                &installed,
                false,
            ),
            addon("Questie"),
            Method::FirstAddonFrame,
            Confidence::High,
        );
        // A protected call still wins.
        check(
            &attribute(
                "[ADDON_ACTION_BLOCKED] AddOn 'Questie' tried to call the protected function 'X()'.\nLua Taint: OneForAll",
                Some(blizzard),
                false,
                &installed,
                false,
            ),
            addon("Questie"),
            Method::ProtectedCall,
            Confidence::High,
        );
    }

    #[test]
    fn a_truncated_path_resolves_by_a_unique_installed_suffix() {
        let message = "..._EncounterWarnings/EncounterWarningsViewElements.lua:40: attempt to index a nil value";
        check(
            &attribute(message, Some(message), false, &installed(), false),
            addon("MyUI_EncounterWarnings"),
            Method::FirstAddonFrame,
            Confidence::Medium,
        );
        let ambiguous = FolderSet::new(["A_EncounterWarnings", "B_EncounterWarnings"]);
        check(
            &attribute(message, None, false, &ambiguous, false),
            Attributed::Unknown,
            Method::None,
            Confidence::Low,
        );
        // A cut inside `Interface/AddOns/`: the next segment.
        assert_eq!(
            folder_of("...dOns/Questie/Questie.lua", &installed(), false).as_deref(),
            Some("Questie")
        );
        assert_eq!(
            folder_of("...ns\\recount\\x.lua", &installed(), false).as_deref(),
            Some("Recount")
        );
        assert_eq!(
            folder_of("...s/x.lua", &installed(), false),
            None,
            "a file, not a folder"
        );
        assert_eq!(
            folder_of("...s/Nameplates/Widgets/x.lua", &installed(), false),
            None,
            "a tail of `Modules/`, not of `Interface/AddOns/`: no installed folder"
        );
    }

    #[test]
    fn r152_paths_name_versioned_folders_and_libraries() {
        check(
            &attribute(
                "Recount-3.3.5-r1\\Recount.lua:123: attempt to index field 'x' (a nil value)",
                None,
                false,
                &installed(),
                true,
            ),
            addon("Recount"),
            Method::FirstAddonFrame,
            Confidence::Medium,
        );
        check(
            &attribute(
                "CallbackHandler-1.0-6 (Details):146: attempt to call a nil value",
                None,
                false,
                &installed(),
                true,
            ),
            Attributed::Library("Details".to_owned()),
            Method::LibraryFrame,
            Confidence::Medium,
        );
        let longest = FolderSet::new(["DBM", "DBM-Core"]);
        assert_eq!(
            folder_of("DBM-Core-4.5\\DBM-Core.lua", &longest, true).as_deref(),
            Some("DBM-Core"),
            "the longest installed folder"
        );
        assert_eq!(folder_of("Unknown-1.0\\x.lua", &longest, true), None);
    }

    #[test]
    fn only_c_and_blizzard_frames_are_blizzard() {
        let stack = "[C]: in function 'error'\n[Interface/AddOns/Blizzard_EncounterJournal/Blizzard_EncounterJournal.lua]:12: in function 'x'\nInterface\\FrameXML\\UIParent.lua:5: in function 'y'\n[tail call]: ?";
        check(
            &attribute("something failed", Some(stack), false, &installed(), false),
            Attributed::Blizzard,
            Method::BlizzardOnly,
            Confidence::Low,
        );
    }

    #[test]
    fn a_library_inside_an_addon_gives_way_to_the_addons_own_frame() {
        let stack = "[Interface/AddOns/Details/Libs/LibStub/LibStub.lua]:5: in function 'x'\n[Interface/AddOns/Details/Details.lua]:10: in function 'y'";
        check(
            &attribute("boom", Some(stack), false, &installed(), false),
            addon("Details"),
            Method::FirstAddonFrame,
            Confidence::High,
        );
        let only_library = "[Interface/AddOns/Details/Libs/LibStub/LibStub.lua]:5: in function 'x'";
        check(
            &attribute("boom", Some(only_library), false, &installed(), false),
            Attributed::Library("Details".to_owned()),
            Method::LibraryFrame,
            Confidence::Medium,
        );
    }

    #[test]
    fn a_warning_reads_its_embedded_stack() {
        let message = "LUA_WARNING: [Interface/AddOns/Baz/Baz.xml]:5: Couldn't find inherited node\n|cffffd200Stack:|r\n[Interface/AddOns/Baz/Baz.lua]:20: in main chunk";
        check(
            &attribute(message, None, false, &installed(), false),
            addon("Baz"),
            Method::FirstAddonFrame,
            Confidence::High,
        );
    }

    #[test]
    fn the_error_catcher_alone_is_unknown() {
        let stack = "[Interface/AddOns/!BugGrabber/BugGrabber.lua]:485: in function 'x'\n[Interface/AddOns/BugSack/BugSack.lua]:12: in function 'y'";
        check(
            &attribute("boom", Some(stack), false, &installed(), false),
            Attributed::Unknown,
            Method::None,
            Confidence::Low,
        );
        check(
            &attribute("no frames at all", None, false, &installed(), false),
            Attributed::Unknown,
            Method::None,
            Confidence::Low,
        );
    }

    #[test]
    fn a_leftover_versioned_folder_retries_without_its_digits() {
        let path = "Interface/AddOns/Questie-12345/Questie.lua";
        assert_eq!(
            folder_of(path, &installed(), false).as_deref(),
            Some("Questie")
        );
        check(
            &attribute(
                &format!("[{path}]:3: boom"),
                None,
                false,
                &installed(),
                false,
            ),
            addon("Questie"),
            Method::FirstAddonFrame,
            Confidence::Low,
        );
        // An uninstalled folder is still named, as read.
        assert_eq!(
            folder_of("Interface\\AddOns\\Gone\\x.lua", &installed(), false).as_deref(),
            Some("Gone")
        );
        assert_eq!(
            folder_of("Interface/AddOns/questie/x.lua", &installed(), false).as_deref(),
            Some("Questie"),
            "the installed spelling"
        );
    }

    #[test]
    fn classes_follow_their_patterns_and_case_rules() {
        use FrameClass::*;
        for (folder, path, expected) in [
            (
                "Blizzard_UI",
                "Interface/AddOns/Blizzard_UI/x.lua",
                Blizzard,
            ),
            (
                "blizzard_ui",
                "Interface/AddOns/blizzard_ui/x.lua",
                Blizzard,
            ),
            ("Foo", "Interface/FrameXML/x.lua", Blizzard),
            ("Foo", "Interface_Mainline/x.lua", Blizzard),
            (
                "!BugGrabber",
                "Interface/AddOns/!BugGrabber/x.lua",
                SelfGrabber,
            ),
            ("bugsack", "Interface/AddOns/bugsack/x.lua", SelfGrabber),
            ("Foo", "Interface/AddOns/Foo/libs/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/LIBRARIES/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/Embeds/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/external/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/libstub/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/LibDBIcon/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/Lib2/x.lua", Library),
            ("Foo", "Interface/AddOns/Foo/AceEvent-3.0/x.lua", Library),
            // Case-sensitive: `Lib` needs an uppercase letter or a digit after it, and
            // a versioned directory its exact shape.
            ("Foo", "Interface/AddOns/Foo/Library/x.lua", Addon),
            ("Foo", "Interface/AddOns/Foo/libdbicon/x.lua", Addon),
            ("Foo", "Interface/AddOns/Foo/ace-3/x.lua", Addon),
            ("Foo", "Interface/AddOns/Foo/Modules/x.lua", Addon),
            // The file name is not a directory.
            ("Foo", "Interface/AddOns/Foo/LibStub.lua", Addon),
        ] {
            assert_eq!(classify(folder, path), expected, "{path}");
        }
    }

    #[test]
    fn methods_and_confidences_serialize_snake_case() {
        let methods = serde_json::to_value([
            Method::ProtectedCall,
            Method::FirstAddonFrame,
            Method::LibraryFrame,
            Method::BlizzardOnly,
            Method::None,
        ])
        .unwrap();
        assert_eq!(
            methods,
            serde_json::json!([
                "protected_call",
                "first_addon_frame",
                "library_frame",
                "blizzard_only",
                "none"
            ])
        );
        let confidences =
            serde_json::to_value([Confidence::High, Confidence::Medium, Confidence::Low]).unwrap();
        assert_eq!(confidences, serde_json::json!(["high", "medium", "low"]));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Any text and any installed set: no panic, and the same answer every time.
        #[test]
        fn attribution_never_panics_and_is_deterministic(
            message in "(\\PC|[\\[\\]:./\\\\'\"-])*",
            stack in proptest::option::of("(\\PC|[\\[\\]:./\\\\'\"\n-])*"),
            folders in proptest::collection::vec("[A-Za-z_!-]{0,8}", 0..6),
            is_event: bool,
            legacy: bool,
        ) {
            let installed = FolderSet::new(&folders);
            let first = attribute(&message, stack.as_deref(), is_event, &installed, legacy);
            let again = attribute(&message, stack.as_deref(), is_event, &installed, legacy);
            prop_assert_eq!(first, again);
            for frame in super::super::frames::frames(&message, stack.as_deref(), legacy) {
                if let Some(path) = frame.path {
                    let _ = folder_of(path, &installed, legacy);
                    let _ = classify("x", path);
                }
            }
        }
    }
}
