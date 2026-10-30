//! Which failure modes a run checks, and on which clients (M4-D30, §6.11), and what it
//! could not check (M4-D43): `--only` and `--skip` parsed with the closest id as a typo's
//! fix, `-t` resolved against the view without refusing a vanished client, the rows split
//! into those that run and those skipped for an unavailable layer, and the view failures a
//! report shows. Reads only the view; writes nothing.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::Serialize;

use super::finding::{Finding, FindingTarget, Subsystem};
use super::registry::{self, FAILURE_MODES, FailureMode, Layer, Tier, ids};
use super::view::{Availability, DoctorView};
use crate::cli::{fix_argv, levenshtein};
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::TargetKey;
use crate::state::registry::RegistryEntry;

/// One item of `--only` or `--skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// A registered failure mode, by id.
    Mode(&'static str),
    /// Every failure mode of a subsystem (`sockets` and `hooks` have none).
    Subsystem(Subsystem),
}

impl Pick {
    fn picks(self, row: &FailureMode) -> bool {
        match self {
            Self::Mode(id) => id == row.id,
            Self::Subsystem(subsystem) => subsystem == row.subsystem,
        }
    }
}

/// The failure modes a run checks, and the clients it checks them on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// `-t`, as typed: findings about other clients are left out (machine ones never).
    pub target: Option<String>,
    /// `--only`; empty selects every failure mode.
    pub only: Vec<Pick>,
    /// `--skip`, which wins over `--only`.
    pub skip: Vec<Pick>,
}

impl Selection {
    pub fn all() -> Self {
        Self::default()
    }

    /// Exactly the failure modes `ids` names.
    pub fn only(ids: &[&'static str]) -> Self {
        Self {
            only: ids.iter().copied().map(Pick::Mode).collect(),
            ..Self::default()
        }
    }

    /// `-t`, and every value of every `--only` and `--skip` as typed (each a comma
    /// list). An item that is neither a registered id nor a subsystem name, or an empty
    /// one, is `usage`; its fix is `argv` with exactly that item corrected (or dropped,
    /// when empty), never one that drops a flag or a named item.
    pub fn parse(
        target: Option<&str>,
        only: &[String],
        skip: &[String],
        argv: &[String],
    ) -> Result<Self> {
        Ok(Self {
            target: target.map(str::to_owned),
            only: picks("--only", only, argv)?,
            skip: picks("--skip", skip, argv)?,
        })
    }

    /// Whether `row` is checked: named by `--only` (or no `--only`), and not by `--skip`.
    pub fn includes(&self, row: &FailureMode) -> bool {
        (self.only.is_empty() || self.only.iter().any(|pick| pick.picks(row)))
            && !self.skip.iter().any(|pick| pick.picks(row))
    }

    /// Whether `--skip` names the failure mode `id`, by id or subsystem.
    pub fn skips(&self, id: &str) -> bool {
        registry::row(id).is_some_and(|row| self.skip.iter().any(|pick| pick.picks(row)))
    }

    /// The clients `-t` selects. A registered client whose directory is gone is
    /// selectable: the doctor reports it. With discovery unavailable, the registry and
    /// then the records resolve the selector, and one they do not know is
    /// [`Scope::Unresolved`], not an error: the client may be behind the broken layer.
    pub fn scope(&self, view: &DoctorView) -> Result<Scope> {
        let Some(selector) = self.target.as_deref() else {
            return Ok(Scope::All);
        };
        let key = selector.parse::<TargetKey>().ok();
        let names = |entry: &&RegistryEntry| {
            Some(entry.key) == key || entry.id.eq_ignore_ascii_case(selector)
        };
        let recorded = || key.filter(|key| view.records().is_some_and(|r| r.contains(key)));
        let one = |key: TargetKey| Scope::Targets(BTreeSet::from([TargetSel::Key(key)]));
        let Some(world) = view.world() else {
            let registered = view
                .registry()
                .and_then(|registry| registry.value.targets.iter().find(names));
            if let Some(entry) = registered {
                return Ok(one(entry.key));
            }
            if let Some(key) = recorded() {
                return Ok(one(key));
            }
            let cause = match view.availability(Layer::Discovery) {
                Availability::Unavailable { cause } => cause,
                Availability::Available => Layer::Discovery,
            };
            return Ok(Scope::Unresolved { cause });
        };
        if let Some(entry) = world.resolved.missing.iter().find(names) {
            return Ok(one(entry.key));
        }
        if let Some(key) = recorded() {
            return Ok(one(key));
        }
        let selected = world.select_in(Some(selector))?;
        Ok(Scope::Targets(
            selected
                .into_iter()
                .map(|d| match d.target.key {
                    Some(key) => TargetSel::Key(key),
                    None => TargetSel::Unregistered {
                        id: d.target.id.clone(),
                    },
                })
                .collect(),
        ))
    }
}

/// The clients a run's per-client findings are about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// No `-t`.
    All,
    Targets(BTreeSet<TargetSel>),
    /// `-t` named nothing the readable layers know, while `cause` kept discovery from
    /// running: the per-client checks are skipped.
    Unresolved {
        cause: Layer,
    },
}

impl Scope {
    /// Whether a finding about `target` is in scope; one about the machine always is.
    pub fn includes(&self, target: Option<&FindingTarget>) -> bool {
        target.is_none_or(|target| self.includes_client(Some(target.key), &target.id))
    }

    /// Whether the client registered as `key` (when it is), named `id`, is in scope.
    pub fn includes_client(&self, key: Option<TargetKey>, id: &str) -> bool {
        match self {
            Self::All => true,
            Self::Targets(selected) => match key {
                Some(key) => selected.contains(&TargetSel::Key(key)),
                None => selected.contains(&TargetSel::Unregistered { id: id.to_owned() }),
            },
            Self::Unresolved { .. } => false,
        }
    }
}

/// One selected client.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetSel {
    Key(TargetKey),
    /// A discovered client never registered, by its provisional id.
    Unregistered {
        id: String,
    },
}

// ---------------------------------------------------------------------------------------
// Parsing `--only` and `--skip`.

/// Every item of `values` (each a comma list given to `flag`), duplicates dropped.
fn picks(flag: &str, values: &[String], argv: &[String]) -> Result<Vec<Pick>> {
    let mut out: Vec<Pick> = Vec::new();
    for (occurrence, value) in values.iter().enumerate() {
        let items: Vec<&str> = value.split(',').collect();
        for (at, item) in items.iter().enumerate() {
            if item.is_empty() {
                let kept: Vec<&str> = items.iter().copied().filter(|i| !i.is_empty()).collect();
                return Err(bad_item(
                    format!("empty item in {flag}"),
                    (!kept.is_empty())
                        .then(|| with_value(argv, flag, occurrence, &kept.join(",")))
                        .flatten(),
                ));
            }
            let Some(pick) = pick_of(item) else {
                let fix = closest(item).and_then(|candidate| {
                    let mut corrected = items.clone();
                    if let Some(slot) = corrected.get_mut(at) {
                        *slot = candidate;
                    }
                    with_value(argv, flag, occurrence, &corrected.join(","))
                });
                return Err(bad_item(
                    format!("unknown failure mode or subsystem '{item}' in {flag}"),
                    fix,
                ));
            };
            if !out.contains(&pick) {
                out.push(pick);
            }
        }
    }
    Ok(out)
}

/// An item exactly as a failure-mode id or a subsystem name.
fn pick_of(item: &str) -> Option<Pick> {
    FAILURE_MODES
        .iter()
        .find(|row| row.id == item)
        .map(|row| Pick::Mode(row.id))
        .or_else(|| item.parse::<Subsystem>().ok().map(Pick::Subsystem))
}

/// The registered id or subsystem name closest to `item` (lowercased), within two edits;
/// also an id whose slug (after `fm-<subsystem>-`) is that close, when `item` does not
/// start with `fm-`. Ties go to the first in registry order, then subsystem order.
fn closest(item: &str) -> Option<&'static str> {
    let item = item.to_ascii_lowercase();
    let slugs = !item.starts_with("fm-");
    let mut candidates: Vec<(&str, &'static str)> = Vec::new();
    for row in FAILURE_MODES {
        candidates.push((row.id, row.id));
        let slug = row
            .id
            .strip_prefix("fm-")
            .and_then(|rest| rest.split_once('-'))
            .map(|(_, slug)| slug);
        if let Some(slug) = slug.filter(|_| slugs) {
            candidates.push((slug, row.id));
        }
    }
    candidates.extend(Subsystem::ALL.map(|s| (s.as_str(), s.as_str())));
    let mut best: Option<(usize, &'static str)> = None;
    for (word, names) in candidates {
        let distance = levenshtein(&item, word);
        if distance <= 2 && best.is_none_or(|(least, _)| distance < least) {
            best = Some((distance, names));
        }
    }
    best.map(|(_, names)| names)
}

/// `argv` with the value of the `occurrence`th `flag` (`--only X` or `--only=X`) set to
/// `value`, presentation flags dropped; `None` when `argv` has no such occurrence.
fn with_value(argv: &[String], flag: &str, occurrence: usize, value: &str) -> Option<Vec<String>> {
    let inline = format!("{flag}=");
    let at = argv
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, arg)| *arg == flag || arg.starts_with(&inline))
        .nth(occurrence)
        .map(|(at, _)| at)?;
    let mut argv = argv.to_vec();
    if argv.get(at).is_some_and(|arg| arg == flag) {
        *argv.get_mut(at + 1)? = value.to_owned();
    } else {
        *argv.get_mut(at)? = format!("{inline}{value}");
    }
    Some(fix_argv(&argv))
}

/// `usage` for a bad item: the corrected command when there is one, which keeps the
/// user's verb, else what the flags take.
fn bad_item(message: String, fix: Option<Vec<String>>) -> Error {
    match fix {
        Some(argv) => Error::new(ErrorCode::Usage, message, Suggestion::command(argv)).user_verb(),
        None => Error::new(
            ErrorCode::Usage,
            message,
            Suggestion::text(format!(
                "use a failure-mode id such as {}, or a subsystem name such as {}",
                ids::CONFIG_PARSE_ERROR,
                Subsystem::Schemas.as_str()
            )),
        ),
    }
}

// ---------------------------------------------------------------------------------------
// What a run checks, and what it could not.

/// A check a run did not make, and why: never a pass (M4-D43).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Skipped {
    pub id: &'static str,
    pub reason: SkipReason,
    /// The client it was skipped for; `null` when it was skipped for every client.
    pub target: Option<SkippedTarget>,
}

/// A client a check was skipped for: its id, and its key when it is registered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct SkippedTarget {
    pub id: String,
    pub key: Option<TargetKey>,
}

/// The layer whose failure kept a check from running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    ConfigUnavailable,
    RegistryUnavailable,
    DiscoveryUnavailable,
    RecordsUnavailable,
    InventoryUnavailable,
    JournalsUnavailable,
    LockUnavailable,
    HistoryUnavailable,
    CachesUnavailable,
    WidgetUnavailable,
    SkillUnavailable,
    CredentialsUnavailable,
}

impl SkipReason {
    pub const fn for_layer(layer: Layer) -> Self {
        match layer {
            Layer::Config => Self::ConfigUnavailable,
            Layer::Registry => Self::RegistryUnavailable,
            Layer::Discovery => Self::DiscoveryUnavailable,
            Layer::Records => Self::RecordsUnavailable,
            Layer::Inventory => Self::InventoryUnavailable,
            Layer::Journals => Self::JournalsUnavailable,
            Layer::Lock => Self::LockUnavailable,
            Layer::History => Self::HistoryUnavailable,
            Layer::Caches => Self::CachesUnavailable,
            Layer::Widget => Self::WidgetUnavailable,
            Layer::Skill => Self::SkillUnavailable,
            Layer::Credentials => Self::CredentialsUnavailable,
        }
    }

    pub const fn layer(self) -> Layer {
        match self {
            Self::ConfigUnavailable => Layer::Config,
            Self::RegistryUnavailable => Layer::Registry,
            Self::DiscoveryUnavailable => Layer::Discovery,
            Self::RecordsUnavailable => Layer::Records,
            Self::InventoryUnavailable => Layer::Inventory,
            Self::JournalsUnavailable => Layer::Journals,
            Self::LockUnavailable => Layer::Lock,
            Self::HistoryUnavailable => Layer::History,
            Self::CachesUnavailable => Layer::Caches,
            Self::WidgetUnavailable => Layer::Widget,
            Self::SkillUnavailable => Layer::Skill,
            Self::CredentialsUnavailable => Layer::Credentials,
        }
    }
}

/// A run's rows: the scope, those that run, and those skipped for every client.
#[derive(Debug, Clone)]
pub struct Partition<'r> {
    pub scope: Scope,
    pub run: Vec<&'r FailureMode>,
    pub skipped: Vec<Skipped>,
}

/// The layers a check reads per client.
const PER_TARGET: [Layer; 3] = [Layer::Discovery, Layer::Records, Layer::Inventory];

/// Resolves the scope first (its error before anything else loads), then splits the
/// selected rows of `tiers`: a row whose required layers are all available runs, unless
/// the scope is unresolved and it reads clients; any other is skipped once, with the root
/// cause (the first unavailable required layer's cause, in [`Layer::ALL`] order). A row
/// outside `tiers` is neither.
pub fn partition<'r>(
    view: &DoctorView,
    sel: &Selection,
    rows: &'r [FailureMode],
    tiers: &[Tier],
) -> Result<Partition<'r>> {
    let scope = sel.scope(view)?;
    let mut run = Vec::new();
    let mut skipped = Vec::new();
    for row in rows
        .iter()
        .filter(|row| sel.includes(row) && tiers.contains(&row.tier))
    {
        let reads_clients = row.requires.iter().any(|layer| PER_TARGET.contains(layer));
        let cause = match scope {
            Scope::Unresolved { cause } if reads_clients => Some(cause),
            _ => Layer::ALL
                .into_iter()
                .filter(|layer| row.requires.contains(layer))
                .find_map(|layer| match view.availability(layer) {
                    Availability::Unavailable { cause } => Some(cause),
                    Availability::Available => None,
                }),
        };
        match cause {
            None => run.push(row),
            Some(cause) => skipped.push(Skipped {
                id: row.id,
                reason: SkipReason::for_layer(cause),
                target: None,
            }),
        }
    }
    sort_skipped(&mut skipped);
    Ok(Partition {
        scope,
        run,
        skipped,
    })
}

/// After the detectors ran: each in-scope client whose record or inventory was
/// unavailable, once per ran row that reads that layer. Detectors read only what is
/// available, so they reported nothing about those clients.
pub fn target_skips(view: &DoctorView, part: &Partition<'_>) -> Vec<Skipped> {
    let mut out = Vec::new();
    for layer in [Layer::Records, Layer::Inventory] {
        let unavailable: Vec<(SkippedTarget, Layer)> = view
            .unavailable_targets(layer)
            .into_iter()
            .filter(|(target, _)| part.scope.includes_client(target.key, &target.id))
            .collect();
        for row in part.run.iter().filter(|row| row.requires.contains(&layer)) {
            out.extend(unavailable.iter().map(|(target, cause)| Skipped {
                id: row.id,
                reason: SkipReason::for_layer(*cause),
                target: Some(target.clone()),
            }));
        }
    }
    sort_skipped(&mut out);
    out
}

/// The view's failures a report shows: those of a selected row, and those that caused a
/// skip (a skip saying `config_unavailable` needs the finding that explains it), never
/// one `--skip` names; per-client ones only in scope.
pub fn visible_failures(
    view: &DoctorView,
    sel: &Selection,
    part: &Partition<'_>,
    skipped: &[Skipped],
) -> Vec<Finding> {
    let causes: BTreeSet<Layer> = skipped.iter().map(|s| s.reason.layer()).collect();
    view.failures()
        .iter()
        .filter(|failure| {
            let selected = registry::row(failure.fm).is_some_and(|row| sel.includes(row));
            (selected || causes.contains(&failure.layer)) && !sel.skips(failure.fm)
        })
        .filter_map(|failure| view.failure_finding(failure))
        .filter(|finding| part.scope.includes(finding.target.as_ref()))
        .collect()
}

/// By id, then client id (none first).
fn sort_skipped(skipped: &mut [Skipped]) {
    skipped.sort_by(|a, b| {
        let client = |s: &Skipped| s.target.as_ref().map(|t| t.id.clone());
        (a.id, client(a)).cmp(&(b.id, client(b)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::view::tests::{
        K, K2, Plant, client, home, is_root, key, plant, record_bytes, records_of, registry_bytes,
        set_mode, view_at, world_of,
    };
    use crate::doctor::view::{LayerState, Preloaded};
    use crate::state::paths::Paths;

    fn argv(words: &str) -> Vec<String> {
        shell_words::split(words).unwrap()
    }

    /// The values clap would hand over for `flag`, in order, from `argv`.
    fn values(argv: &[String], flag: &str) -> Vec<String> {
        let inline = format!("{flag}=");
        let mut out = Vec::new();
        let mut words = argv.iter();
        while let Some(word) = words.next() {
            if word == flag {
                out.extend(words.next().cloned());
            } else if let Some(value) = word.strip_prefix(&inline) {
                out.push(value.to_owned());
            }
        }
        out
    }

    fn parse(words: &str) -> Result<Selection> {
        let argv = argv(words);
        Selection::parse(
            None,
            &values(&argv, "--only"),
            &values(&argv, "--skip"),
            &argv,
        )
    }

    const PENDING: &str = ids::JOURNAL_PENDING;
    const CORRUPT: &str = ids::STATE_CORRUPT;

    #[test]
    fn parse_only_and_skip() {
        let mode = Pick::Mode;
        let sub = Pick::Subsystem;
        let ok: Vec<(&str, String, Vec<Pick>, Vec<Pick>)> = vec![
            (
                "one_id",
                format!("omaddons doctor --only {PENDING}"),
                vec![mode(PENDING)],
                vec![],
            ),
            (
                "one_subsystem",
                "omaddons doctor --only schemas".to_owned(),
                vec![sub(Subsystem::Schemas)],
                vec![],
            ),
            (
                "sockets_selects_nothing",
                "omaddons doctor --only sockets".to_owned(),
                vec![sub(Subsystem::Sockets)],
                vec![],
            ),
            (
                "comma_list",
                format!("omaddons doctor --only {PENDING},plugins"),
                vec![mode(PENDING), sub(Subsystem::Plugins)],
                vec![],
            ),
            (
                "repeated_flags",
                format!("omaddons doctor --only {PENDING} --only plugins"),
                vec![mode(PENDING), sub(Subsystem::Plugins)],
                vec![],
            ),
            (
                "inline_equals",
                format!("omaddons doctor --only={PENDING}"),
                vec![mode(PENDING)],
                vec![],
            ),
            (
                "duplicates_dropped",
                format!("omaddons doctor --only {PENDING},{PENDING} --only {PENDING}"),
                vec![mode(PENDING)],
                vec![],
            ),
            (
                "skip_beats_only",
                format!("omaddons doctor --only schemas --skip {CORRUPT}"),
                vec![sub(Subsystem::Schemas)],
                vec![mode(CORRUPT)],
            ),
        ];
        for (case, words, only, skip) in ok {
            let got = parse(&words);
            assert!(got.is_ok(), "case {case}: input {words} → got {got:?}");
            let got = got.unwrap();
            assert_eq!(
                (got.only, got.skip),
                (only, skip),
                "case {case}: input {words}"
            );
        }
        let corrupt = registry::row(CORRUPT).unwrap();
        assert!(
            !parse(&format!("omaddons doctor --only schemas --skip {CORRUPT}"))
                .unwrap()
                .includes(corrupt),
            "case skip_beats_only: --skip wins"
        );

        // `(case, input, message, fix argv)`; `None`: the prose fix, no argv.
        let typo = "fm-concurrency_primitives-journal-pendin";
        let bad: Vec<(&str, String, String, Option<String>)> = vec![
            (
                "empty_item_in_list",
                format!("omaddons doctor --only {PENDING},,plugins"),
                "empty item in --only".to_owned(),
                Some(format!("omaddons doctor --only {PENDING},plugins")),
            ),
            (
                "only_empty",
                "omaddons doctor --only ''".to_owned(),
                "empty item in --only".to_owned(),
                None,
            ),
            (
                "typo_distance_1",
                format!("omaddons doctor --only {typo}"),
                format!("unknown failure mode or subsystem '{typo}' in --only"),
                Some(format!("omaddons doctor --only {PENDING}")),
            ),
            (
                "typo_inside_a_list",
                format!("omaddons doctor --only {CORRUPT},fm-schemas-state-corupt"),
                "unknown failure mode or subsystem 'fm-schemas-state-corupt' in --only".to_owned(),
                Some(format!("omaddons doctor --only {CORRUPT},{CORRUPT}")),
            ),
            (
                "bare_slug",
                "omaddons doctor --only journal-pending".to_owned(),
                "unknown failure mode or subsystem 'journal-pending' in --only".to_owned(),
                Some(format!("omaddons doctor --only {PENDING}")),
            ),
            (
                "uppercase",
                "omaddons doctor --only=FM-SCHEMAS-STATE-CORRUPT".to_owned(),
                "unknown failure mode or subsystem 'FM-SCHEMAS-STATE-CORRUPT' in --only".to_owned(),
                Some(format!("omaddons doctor --only={CORRUPT}")),
            ),
            (
                "nothing_close",
                "omaddons doctor --only zzz".to_owned(),
                "unknown failure mode or subsystem 'zzz' in --only".to_owned(),
                None,
            ),
            (
                "skip_typo",
                "omaddons doctor --skip plugin".to_owned(),
                "unknown failure mode or subsystem 'plugin' in --skip".to_owned(),
                Some("omaddons doctor --skip plugins".to_owned()),
            ),
            (
                "presentation_flags_dropped",
                "omaddons doctor --json --only schema".to_owned(),
                "unknown failure mode or subsystem 'schema' in --only".to_owned(),
                Some("omaddons doctor --only schemas".to_owned()),
            ),
        ];
        for (case, words, message, fix) in bad {
            let got = parse(&words);
            assert!(got.is_err(), "case {case}: input {words} → got {got:?}");
            let err = got.unwrap_err();
            assert_eq!(
                (err.code, err.message.as_str()),
                (ErrorCode::Usage, message.as_str()),
                "case {case}: input {words}"
            );
            let expected = fix.as_deref().map(argv);
            assert_eq!(
                err.fix.argv().map(<[String]>::to_vec),
                expected,
                "case {case}: input {words} → fix {:?}",
                err.fix.display()
            );
            match fix {
                Some(_) => assert_eq!(
                    err.usage_fix,
                    crate::error::UsageFix::UserVerb,
                    "case {case}"
                ),
                None => assert!(
                    !err.fix.display().contains("omaddons "),
                    "case {case}: the prose names a command: {}",
                    err.fix.display()
                ),
            }
        }
    }

    #[test]
    fn includes_and_skip_precedence() {
        let ids_of = |sel: &Selection| -> Vec<&str> {
            FAILURE_MODES
                .iter()
                .filter(|row| sel.includes(row))
                .map(|row| row.id)
                .collect()
        };
        let every: Vec<&str> = FAILURE_MODES.iter().map(|row| row.id).collect();
        let schemas: Vec<&str> = FAILURE_MODES
            .iter()
            .filter(|row| row.subsystem == Subsystem::Schemas)
            .map(|row| row.id)
            .collect();
        assert!(schemas.len() >= 2, "{schemas:?}");
        let cases: Vec<(&str, Selection, Vec<&str>)> = vec![
            ("only_empty", Selection::all(), every),
            (
                "only_a_subsystem",
                Selection {
                    only: vec![Pick::Subsystem(Subsystem::Schemas)],
                    ..Selection::all()
                },
                schemas.clone(),
            ),
            ("only_an_id", Selection::only(&[PENDING]), vec![PENDING]),
            (
                "only_both_skip_the_id",
                Selection {
                    only: vec![Pick::Subsystem(Subsystem::Schemas), Pick::Mode(PENDING)],
                    skip: vec![Pick::Mode(CORRUPT)],
                    ..Selection::all()
                },
                FAILURE_MODES
                    .iter()
                    .filter(|row| {
                        (row.subsystem == Subsystem::Schemas || row.id == PENDING)
                            && row.id != CORRUPT
                    })
                    .map(|row| row.id)
                    .collect(),
            ),
        ];
        for (case, sel, expected) in cases {
            assert_eq!(ids_of(&sel), expected, "case {case}");
        }
        let skipping = Selection {
            skip: vec![Pick::Subsystem(Subsystem::Schemas)],
            ..Selection::all()
        };
        assert!(skipping.skips(CORRUPT) && !skipping.skips(PENDING));
    }

    fn selecting(target: &str) -> Selection {
        Selection {
            target: Some(target.to_owned()),
            ..Selection::all()
        }
    }

    fn one(k: &str) -> Scope {
        Scope::Targets(BTreeSet::from([TargetSel::Key(key(k))]))
    }

    /// A view whose Discovery layer holds retail (`K`), and `gone` (`K2`) as a registered
    /// client whose directory is gone.
    fn discovered(paths: &Paths) -> crate::doctor::view::DoctorView {
        let mut world = world_of(
            paths,
            vec![client("retail", &paths.home().join("retail"), Some(K))],
        );
        world.resolved.missing = vec![crate::state::registry::RegistryEntry {
            key: key(K2),
            id: "gone".to_owned(),
            install_root: paths.home().join("gone"),
            client_dir: paths.home().join("gone/_retail_"),
            flavor_dir: Some("_retail_".to_owned()),
            product: Some("wow".to_owned()),
            qualifier: "test".to_owned(),
            origin: crate::state::registry::Origin::Discovered,
            registered_at: "2026-10-14T12:00:00Z".parse().unwrap(),
        }];
        crate::doctor::view::DoctorView::preloaded(
            paths.clone(),
            Preloaded {
                world: Some(LayerState::Available(world)),
                ..Preloaded::default()
            },
        )
    }

    #[test]
    fn scope_resolution() {
        const K3: &str = "01M7A1B2C3D4E5F6G7H8J9K0MQ";
        let (_dir, paths) = home();
        let view = discovered(&paths);
        assert_eq!(
            Selection::all().scope(&view).unwrap(),
            Scope::All,
            "case no_target"
        );
        let cases = [
            ("discovered_id", "retail", one(K)),
            ("vanished_registered_id", "GONE", one(K2)),
            ("discovered_key", K, one(K)),
        ];
        for (case, selector, expected) in cases {
            let got = selecting(selector).scope(&view);
            assert_eq!(got.ok(), Some(expected), "case {case}: input -t {selector}");
        }
        let err = selecting("nosuch").scope(&view).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::TargetNotFound,
            "case unknown_with_discovery"
        );

        // A record key no client has: a record file of its own.
        let (_dir, paths) = home();
        plant(
            &paths.target_record(&key(K3)),
            &Plant::Bytes(record_bytes(K3, 1)),
        );
        let view = discovered(&paths);
        assert_eq!(
            selecting(K3).scope(&view).ok(),
            Some(one(K3)),
            "case record_key"
        );

        let broken_config = Plant::Bytes(b"a = [\n".to_vec());
        let (_dir, paths) = home();
        plant(&paths.config_file(), &broken_config);
        plant(
            &paths.registry_file(),
            &Plant::Bytes(registry_bytes(&[(K, "retail")])),
        );
        assert_eq!(
            selecting("retail").scope(&view_at(paths)).ok(),
            Some(one(K)),
            "case registry_match_without_discovery"
        );

        let (_dir, paths) = home();
        plant(&paths.config_file(), &broken_config);
        plant(&paths.registry_file(), &Plant::Bytes(b"{".to_vec()));
        plant(
            &paths.target_record(&key(K)),
            &Plant::Bytes(record_bytes(K, 1)),
        );
        assert_eq!(
            selecting(K).scope(&view_at(paths.clone())).ok(),
            Some(one(K)),
            "case record_match_without_registry"
        );
        assert_eq!(
            selecting("retail").scope(&view_at(paths)).ok(),
            Some(Scope::Unresolved {
                cause: Layer::Config
            }),
            "case unresolved"
        );
    }

    const A: &str = "fm-state_files-a-mode";

    /// A row of `tier` reading `requires`.
    fn row(requires: &'static [Layer], tier: Tier) -> FailureMode {
        FailureMode {
            id: A,
            subsystem: Subsystem::StateFiles,
            severity: crate::doctor::finding::Severity::P2,
            title: "a",
            description: "A.",
            why: "A.",
            tier,
            online: false,
            cost_ms: 1,
            requires,
            detect: None,
            fix: None,
            manual: None,
        }
    }

    const ALL_TIERS: [Tier; 4] = [Tier::Cached, Tier::Quick, Tier::Default, Tier::Deep];

    /// What `partition` did with the one row: `Ok(None)` ran it, `Ok(Some(reason))`
    /// skipped it, `Err(())` neither.
    fn fate(
        view: &crate::doctor::view::DoctorView,
        row: &FailureMode,
        tiers: &[Tier],
    ) -> std::result::Result<Option<SkipReason>, ()> {
        let rows = [*row];
        let part = partition(view, &Selection::all(), &rows, tiers).unwrap();
        match (part.run.len(), part.skipped.as_slice()) {
            (1, []) => Ok(None),
            (0, [skipped]) => Ok(Some(skipped.reason)),
            _ => Err(()),
        }
    }

    #[test]
    fn partition_reports_root_causes() {
        let broken_config = || Plant::Bytes(b"a = [\n".to_vec());
        let broken_registry = || Plant::Bytes(b"{".to_vec());
        /// `(case, config.toml, registry.json, the row's requires, its skip reason)`.
        type Case = (
            &'static str,
            Plant,
            Plant,
            &'static [Layer],
            Option<SkipReason>,
        );
        let cases: Vec<Case> = vec![
            (
                "discovery_config_broken",
                broken_config(),
                Plant::Absent,
                &[Layer::Discovery],
                Some(SkipReason::ConfigUnavailable),
            ),
            (
                "discovery_registry_broken",
                Plant::Absent,
                broken_registry(),
                &[Layer::Discovery],
                Some(SkipReason::RegistryUnavailable),
            ),
            (
                "records_registry_broken",
                Plant::Absent,
                broken_registry(),
                &[Layer::Records],
                None,
            ),
            (
                "config_and_registry_broken",
                broken_config(),
                broken_registry(),
                &[Layer::Config, Layer::Registry],
                Some(SkipReason::ConfigUnavailable),
            ),
            (
                "requires_nothing",
                broken_config(),
                broken_registry(),
                &[],
                None,
            ),
            (
                "credentials_config_broken",
                broken_config(),
                Plant::Absent,
                &[Layer::Credentials],
                Some(SkipReason::ConfigUnavailable),
            ),
        ];
        for (case, config, registry, requires, expected) in cases {
            let (_dir, paths) = home();
            plant(&paths.config_file(), &config);
            plant(&paths.registry_file(), &registry);
            let got = fate(&view_at(paths), &row(requires, Tier::Default), &ALL_TIERS);
            assert_eq!(got, Ok(expected), "case {case}: requires {requires:?}");
        }
        if !is_root() {
            let (_dir, paths) = home();
            let journal = paths.journal_dir();
            std::fs::create_dir_all(&journal).unwrap();
            set_mode(&journal, 0o000);
            let got = fate(
                &view_at(paths),
                &row(&[Layer::Journals], Tier::Default),
                &ALL_TIERS,
            );
            set_mode(&journal, 0o755);
            assert_eq!(
                got,
                Ok(Some(SkipReason::JournalsUnavailable)),
                "case journals_unlistable"
            );
        }
    }

    #[test]
    fn partition_respects_tiers() {
        let (_dir, paths) = home();
        let view = view_at(paths);
        let cases = [
            (
                "deep_outside_default",
                Tier::Deep,
                &[Tier::Cached, Tier::Quick, Tier::Default][..],
            ),
            ("quick_outside_cached", Tier::Quick, &[Tier::Cached][..]),
        ];
        for (case, tier, tiers) in cases {
            assert_eq!(fate(&view, &row(&[], tier), tiers), Err(()), "case {case}");
        }
        assert_eq!(fate(&view, &row(&[], Tier::Quick), &ALL_TIERS), Ok(None));
    }

    #[test]
    fn partition_returns_scope_errors_first() {
        if is_root() {
            return;
        }
        let (_dir, paths) = home();
        plant(&paths.history_file(), &Plant::Mode000(b"x\n".to_vec()));
        let view = view_at(paths);
        let rows = [row(&[Layer::History], Tier::Default)];
        let err = partition(&view, &selecting("nosuch"), &rows, &ALL_TIERS).unwrap_err();
        assert_eq!(err.code, ErrorCode::TargetNotFound);
        assert!(
            !view.loaded_layers().contains(&Layer::History),
            "{:?}",
            view.loaded_layers()
        );
        assert!(view.failures().is_empty(), "{:?}", view.failures());
    }

    #[test]
    fn target_skips_name_each_unavailable_target() {
        let (_dir, paths) = home();
        // Named as findings name them: by the registry.
        plant(
            &paths.registry_file(),
            &Plant::Bytes(registry_bytes(&[(K, "retail"), (K2, "era")])),
        );
        let era_dir = paths.home().join("era");
        let addons = era_dir.join("Interface/AddOns");
        plant(
            &addons.join("Foo/Foo.toc"),
            &Plant::Bytes(b"## Interface: 11508\n".to_vec()),
        );
        let retail = client("retail", &paths.home().join("retail"), Some(K));
        let era = client("era", &era_dir, Some(K2));
        let view = crate::doctor::view::DoctorView::preloaded(
            paths.clone(),
            Preloaded {
                records: Some(LayerState::Available(records_of(vec![
                    (K, false),
                    (K2, true),
                ]))),
                world: Some(LayerState::Available(world_of(
                    &paths,
                    vec![retail, era.clone()],
                ))),
                ..Preloaded::default()
            },
        );
        const B: &str = "fm-state_files-b-mode";
        const C: &str = "fm-state_files-c-mode";
        let rows = [
            row(&[Layer::Records], Tier::Default),
            FailureMode {
                id: B,
                ..row(&[Layer::Records, Layer::Inventory], Tier::Default)
            },
            FailureMode {
                id: C,
                ..row(&[], Tier::Default)
            },
        ];
        let part = partition(&view, &Selection::all(), &rows, &ALL_TIERS).unwrap();
        assert_eq!(part.run.len(), 3, "{:?}", part.skipped);
        // A detector asked for era's inventory, and its AddOns could not be listed.
        if !is_root() {
            set_mode(&addons, 0o000);
            assert!(view.inventory(&era).is_none());
            set_mode(&addons, 0o755);
        }
        let got: Vec<(&str, SkipReason, Option<String>)> = target_skips(&view, &part)
            .into_iter()
            .map(|s| (s.id, s.reason, s.target.map(|t| t.id)))
            .collect();
        let mut expected = vec![
            (A, SkipReason::RecordsUnavailable, Some("retail".to_owned())),
            (B, SkipReason::RecordsUnavailable, Some("retail".to_owned())),
        ];
        if !is_root() {
            expected.push((B, SkipReason::InventoryUnavailable, Some("era".to_owned())));
            // By id, then target id.
            expected.sort_by(|a, b| (a.0, &a.2).cmp(&(b.0, &b.2)));
        }
        assert_eq!(got, expected);
        let scoped = Partition {
            scope: one(K2),
            ..part
        };
        assert!(
            target_skips(&view, &scoped)
                .iter()
                .all(|s| s.target.as_ref().is_some_and(|t| t.id == "era")),
            "-t era leaves retail's out"
        );
    }

    /// The view failures a run with `sel` over every row shows.
    fn shown(
        view: &crate::doctor::view::DoctorView,
        sel: &Selection,
    ) -> Vec<(&'static str, Option<String>)> {
        let part = partition(view, sel, FAILURE_MODES, &ALL_TIERS).unwrap();
        let mut skipped = part.skipped.clone();
        skipped.extend(target_skips(view, &part));
        visible_failures(view, sel, &part, &skipped)
            .into_iter()
            .map(|f| (f.id, f.target.map(|t| t.id)))
            .collect()
    }

    #[test]
    fn visible_failures_rules() {
        let broken_config = Plant::Bytes(b"a = [\n".to_vec());
        let with = |only: &[&'static str], skip: &[&'static str]| Selection {
            only: only.iter().map(|id| Pick::Mode(id)).collect(),
            skip: skip.iter().map(|id| Pick::Mode(id)).collect(),
            ..Selection::all()
        };

        let (_dir, paths) = home();
        plant(&paths.config_file(), &broken_config);
        let view = view_at(paths);
        assert_eq!(
            shown(&view, &with(&[ids::WIDGET_VERSION_DRIFT], &[])),
            [],
            "case config_failure_hidden_when_unasked"
        );
        assert!(!view.loaded_layers().contains(&Layer::Config));

        let (_dir, paths) = home();
        plant(&paths.config_file(), &broken_config);
        let view = view_at(paths);
        assert_eq!(
            shown(&view, &with(&[ids::TARGET_VANISHED], &[])),
            [(ids::CONFIG_PARSE_ERROR, None)],
            "case config_failure_shown_when_it_caused_a_skip"
        );

        let (_dir, paths) = home();
        plant(&paths.config_file(), &broken_config);
        let view = view_at(paths);
        assert_eq!(
            shown(&view, &with(&[], &[ids::CONFIG_PARSE_ERROR])),
            [],
            "case config_failure_hidden_by_skip"
        );

        // Retail's record is corrupt; `-t era` leaves its failure out.
        let (_dir, paths) = home();
        plant(&paths.target_record(&key(K)), &Plant::Bytes(b"[".to_vec()));
        let world = world_of(
            &paths,
            vec![
                client("retail", &paths.home().join("retail"), Some(K)),
                client("era", &paths.home().join("era"), Some(K2)),
            ],
        );
        let preloaded = || Preloaded {
            world: Some(LayerState::Available(world_of(
                &paths,
                world.resolved.targets.clone(),
            ))),
            ..Preloaded::default()
        };
        let view = crate::doctor::view::DoctorView::preloaded(paths.clone(), preloaded());
        assert_eq!(
            shown(&view, &selecting("era")),
            [],
            "case record_failure_out_of_scope"
        );
        let view = crate::doctor::view::DoctorView::preloaded(paths.clone(), preloaded());
        assert_eq!(
            shown(&view, &selecting("retail")),
            [(CORRUPT, Some(K.to_owned()))],
            "case record_failure_in_scope (no registry: the key names it)"
        );

        let (_dir, paths) = home();
        plant(&paths.registry_file(), &Plant::Bytes(b"{".to_vec()));
        let view = view_at(paths);
        assert_eq!(
            shown(&view, &selecting("era")),
            [(CORRUPT, None)],
            "case machine_failure_in_any_scope"
        );
    }

    #[test]
    fn skipped_entries_serialize_with_their_target() {
        let entries = [
            Skipped {
                id: A,
                reason: SkipReason::ConfigUnavailable,
                target: None,
            },
            Skipped {
                id: A,
                reason: SkipReason::InventoryUnavailable,
                target: Some(SkippedTarget {
                    id: "retail".to_owned(),
                    key: Some(key(K)),
                }),
            },
        ];
        assert_eq!(
            serde_json::to_value(&entries).unwrap(),
            serde_json::json!([
                {"id": A, "reason": "config_unavailable", "target": null},
                {"id": A, "reason": "inventory_unavailable", "target": {"id": "retail", "key": K}},
            ])
        );
        for layer in Layer::ALL {
            let reason = SkipReason::for_layer(layer);
            assert_eq!(reason.layer(), layer);
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::json!(format!(
                    "{}_unavailable",
                    serde_json::to_value(layer).unwrap().as_str().unwrap()
                )),
                "{layer:?}"
            );
        }
    }

    #[test]
    fn selection_source_never_writes() {
        use crate::source_scan::{Literals, production_code};
        let code = production_code(include_str!("selection.rs"), Literals::Blank).unwrap();
        for forbidden in [
            "WriteSession",
            "mutate(",
            "prune(",
            "Op::",
            "OpenOptions",
            "fs::write",
            "lock_file",
        ] {
            assert!(
                !code.contains(forbidden),
                "src/doctor/selection.rs uses {forbidden}"
            );
        }
    }
}
