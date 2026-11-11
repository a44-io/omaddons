//! The ownership model (vision §5.5 step 4): whether a transaction may write each
//! AddOns folder. Pure: it judges planned folder changes against an inventory, at plan
//! time and again under the lock.
//!
//! Per folder, first match wins:
//! 1. a `blizzard_*` key (any case) is the client's own: `protected_folder`, never
//!    overridable (D14);
//! 2. a symlinked folder (a developer's working copy) is never touched:
//!    `unmanaged_folder` (class `linked`), never overridable;
//! 3. a folder only another client sharing this AddOns directory records (class
//!    `shared`): `ownership_conflict` naming that client, never overridable, since a
//!    second record of one folder is what ownership prevents. A folder recorded on both
//!    (before this was refused) changes with a `target_addons_shared` warning;
//! 4. a key whose live spelling differs from the incoming one: `case_collision` (Wine
//!    would see one folder under two names), never overridable: `--force` does not
//!    pick a spelling. The installing addon's own folder may change case freely;
//! 5. another addon's folder: `ownership_conflict`; `--force` transfers it and marks
//!    the previous owner degraded (`ownership_transferred`);
//! 6. an unmanaged folder: `unmanaged_folder`; `--force` replaces it (its snapshot
//!    keeps it undoable);
//! 7. otherwise a replacement of the addon's own folder, or an add.
//!
//! Within one transaction (M2-D6) a [`WorkingView`] says what the works judged before
//! did: a folder another work already claims is an `ownership_conflict` that `--force`
//! never overrides (`details.within_transaction`); a folder another addon's work stops
//! shipping is handed over: a replacement with a `handover` transfer, which degrades
//! nobody.
//!
//! Every violation is reported in one error (`details.folders`), whose code is the
//! most serious one: protected > linked > case_collision > ownership_conflict >
//! unmanaged.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::inventory::{FolderClass, FolderEntry, Inventory, OwnedOn};
use crate::model::{FolderName, MutableFolderName, Ref};
use crate::state::records::TargetRecord;

/// A planned change to one AddOns folder, before ownership is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderChange {
    /// Put an archive folder in place (new or replacing).
    Install(FolderName),
    /// Take a folder of the addon being removed out.
    Remove(FolderName),
}

/// A change ownership allows, with the live spelling where a folder exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedOp {
    Add {
        name: MutableFolderName,
    },
    Replace {
        name: MutableFolderName,
        live: MutableFolderName,
        previous: Previous,
    },
    Remove {
        live: MutableFolderName,
    },
}

/// Who had a replaced folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Previous {
    /// The installing addon itself.
    Same,
    /// Another addon, transferred with `--force`.
    Transferred { from: String },
    /// Nobody: an unmanaged folder replaced with `--force`.
    Unmanaged,
}

/// A folder moved between addons: by `--force`, or handed over within a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub folder: FolderName,
    pub from: String,
    pub to: String,
    /// The previous owner's own work in the same transaction dropped the folder: no
    /// degraded state, no warning.
    pub handover: bool,
}

/// What the works already judged in one target's transaction do to its folders: the
/// folders they install (claims) and the folders their addons stop shipping (releases).
/// Keyed by folder key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkingView {
    claimed: BTreeMap<String, String>,
    released: BTreeMap<String, String>,
}

impl WorkingView {
    /// `addon`'s work installs `folder`.
    pub fn claim(&mut self, folder: &FolderName, addon: &str) {
        self.claimed
            .insert(folder.key().to_owned(), addon.to_owned());
    }

    /// `addon`'s work stops shipping `folder`.
    pub fn release(&mut self, folder: &FolderName, addon: &str) {
        self.released
            .insert(folder.key().to_owned(), addon.to_owned());
    }

    /// `addon`'s work failed: it stops shipping nothing after all, so `folder` is not
    /// released (a release by another addon stays).
    pub fn withdraw(&mut self, folder: &FolderName, addon: &str) {
        if self
            .released
            .get(folder.key())
            .is_some_and(|by| by == addon)
        {
            self.released.remove(folder.key());
        }
    }

    /// The addon whose work in this transaction installs `folder`.
    pub fn owner_of(&self, folder: &FolderName) -> Option<&str> {
        self.claimed.get(folder.key()).map(String::as_str)
    }

    /// The addon whose work in this transaction stops shipping `folder`.
    pub fn released_by(&self, folder: &FolderName) -> Option<&str> {
        self.released.get(folder.key()).map(String::as_str)
    }
}

/// What ownership allows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnershipOutcome {
    pub allowed: Vec<AllowedOp>,
    pub transfers: Vec<Transfer>,
    /// Addon keys whose records become `degraded` (they lost a folder).
    pub degraded: Vec<String>,
    pub warnings: Vec<Warning>,
}

/// One refused folder.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Violation {
    folder: String,
    code: ErrorCode,
    reason: &'static str,
    extra: Value,
}

/// The precedence of a refusal; higher wins the error's code.
fn rank(code: ErrorCode, reason: &str) -> u8 {
    match (code, reason) {
        (ErrorCode::ProtectedFolder, _) => 5,
        (ErrorCode::UnmanagedFolder, "linked") => 4,
        (ErrorCode::CaseCollision, _) => 3,
        (ErrorCode::OwnershipConflict, _) => 2,
        _ => 1,
    }
}

/// Where an addon new to its target comes from, so that an unmanaged folder it ships is
/// adopted under that ref instead of overwritten.
#[derive(Debug, Clone, Copy)]
pub struct AdoptFix<'a> {
    pub target: &'a str,
    pub r#ref: &'a Ref,
}

/// Judges `changes` for the addon `installing` against `inventory` alone. `argv` is the
/// command line, repeated with `--force` in fixes. `adopt` is set for an addon with no
/// record on the target: an unmanaged folder's fix adopts it.
pub fn check(
    changes: &[FolderChange],
    inventory: &Inventory,
    installing: &str,
    force: bool,
    argv: &[String],
    adopt: Option<AdoptFix<'_>>,
) -> Result<OwnershipOutcome> {
    check_in(
        changes,
        inventory,
        installing,
        force,
        argv,
        &WorkingView::default(),
        adopt,
    )
}

/// [`check`], within a transaction whose other works did what `view` says.
pub fn check_in(
    changes: &[FolderChange],
    inventory: &Inventory,
    installing: &str,
    force: bool,
    argv: &[String],
    view: &WorkingView,
    adopt: Option<AdoptFix<'_>>,
) -> Result<OwnershipOutcome> {
    let me = TargetRecord::addon_key(installing);
    // Another work of this transaction installs it: the two releases disagree, and
    // nothing on disk could make room, so `--force` does not help.
    let claimed: Vec<(&FolderName, &str)> = changes
        .iter()
        .filter_map(|c| match c {
            FolderChange::Install(name) => view
                .owner_of(name)
                .filter(|owner| TargetRecord::addon_key(owner) != me)
                .map(|owner| (name, owner)),
            FolderChange::Remove(_) => None,
        })
        .collect();
    if let Some((first, owner)) = claimed.first() {
        let names: Vec<&str> = claimed.iter().map(|(f, _)| f.as_str()).collect();
        let folders: Vec<Value> = claimed
            .iter()
            .map(|(f, o)| json!({ "folder": f.as_str(), "owner": o }))
            .collect();
        return Err(Error::new(
            ErrorCode::OwnershipConflict,
            format!(
                "{owner} and {installing} both ship {} in this transaction",
                names.join(", ")
            ),
            Suggestion::text(format!(
                "choose one of {owner} and {installing}: both ship {}",
                first.as_str()
            )),
        )
        .details(json!({ "within_transaction": true, "folders": folders })));
    }
    let mut outcome = OwnershipOutcome::default();
    let mut violations: Vec<Violation> = Vec::new();
    let mut refuse = |folder: &str, code, reason, extra| {
        violations.push(Violation {
            folder: folder.to_owned(),
            code,
            reason,
            extra,
        });
    };

    for change in changes {
        let (FolderChange::Install(folder) | FolderChange::Remove(folder)) = change;
        // Rule 1, and the only way a name becomes mutable: past here none is protected.
        let Ok(name) = folder.clone().into_mutable() else {
            refuse(
                folder.as_str(),
                ErrorCode::ProtectedFolder,
                "protected",
                json!({}),
            );
            continue;
        };
        let live: Vec<&FolderEntry> = inventory
            .folders
            .iter()
            .filter(|e| e.key == name.key() && e.class != FolderClass::Invalid)
            .collect();
        if let Some(link) = live.iter().find(|e| e.class == FolderClass::Linked) {
            refuse(
                &link.name,
                ErrorCode::UnmanagedFolder,
                "linked",
                json!({ "class": "linked" }),
            );
            continue;
        }
        if let [first, _, ..] = live.as_slice() {
            // `existing` is a name at every refusal (rule 6); every live spelling is
            // `spellings`.
            let spellings: Vec<&str> = live.iter().map(|entry| entry.name.as_str()).collect();
            refuse(
                name.as_str(),
                ErrorCode::CaseCollision,
                "case_duplicates",
                json!({ "existing": first.name, "spellings": spellings }),
            );
            continue;
        }
        let Some(entry) = live.first() else {
            if let FolderChange::Install(_) = change {
                outcome.allowed.push(AllowedOp::Add { name: name.clone() });
            }
            continue;
        };
        // The live spelling of the same key: not protected either.
        let Some(live_name) = FolderName::new(&entry.name)
            .ok()
            .and_then(|f| f.into_mutable().ok())
        else {
            continue;
        };
        // Another client sharing this AddOns directory owns it: a second record of one
        // folder is what ownership exists to prevent, so `--force` does not help.
        if let (FolderChange::Install(_), FolderClass::Shared, Some(owned)) =
            (change, entry.class, entry.owned_on.first())
        {
            refuse(
                name.as_str(),
                ErrorCode::OwnershipConflict,
                "owned_on_shared_target",
                json!({
                    "existing": entry.name,
                    "owner": owned.addon,
                    "owner_target": owned.target,
                }),
            );
            continue;
        }
        // Recorded on both before that was refused: the change shows there too.
        if let Some(owned) = entry.owned_on.first() {
            outcome.warnings.push(doubled(&live_name, owned));
        }
        if let FolderChange::Remove(_) = change {
            outcome.allowed.push(AllowedOp::Remove { live: live_name });
            continue;
        }
        // Owners are addon names as recorded; identity is the record key.
        let is_me = |owner: &String| TargetRecord::addon_key(owner) == me;
        let others: Vec<&String> = entry.owners.iter().filter(|o| !is_me(o)).collect();
        let mine = entry.owners.iter().any(is_me);
        let respelled = entry.name != name.as_str();
        // Its owner's own work in this transaction stops shipping it: handed over.
        let handed = view.released_by(name.as_folder()).filter(|by| {
            others
                .iter()
                .any(|o| TargetRecord::addon_key(o) == TargetRecord::addon_key(by))
        });
        if let (Some(from), false) = (handed, respelled) {
            outcome.transfers.push(Transfer {
                folder: live_name.as_folder().clone(),
                from: from.to_owned(),
                to: installing.to_owned(),
                handover: true,
            });
            outcome.allowed.push(AllowedOp::Replace {
                name: name.clone(),
                live: live_name,
                previous: Previous::Transferred {
                    from: from.to_owned(),
                },
            });
            continue;
        }
        let previous = match (mine, others.first()) {
            (true, None) => Previous::Same,
            // A new spelling of another addon's or an unmanaged folder: the client would
            // see one folder under two names. `--force` never picks (vision §5.5 step 4).
            (_, other) if respelled => {
                let mut extra = json!({ "existing": entry.name });
                if let (Some(map), Some(owner)) = (extra.as_object_mut(), other) {
                    map.insert("owner".into(), json!(owner));
                }
                refuse(
                    name.as_str(),
                    ErrorCode::CaseCollision,
                    "case_collision",
                    extra,
                );
                continue;
            }
            (_, Some(other)) => {
                if !force {
                    refuse(
                        name.as_str(),
                        ErrorCode::OwnershipConflict,
                        "owned_by_another",
                        json!({ "existing": entry.name, "owner": other }),
                    );
                    continue;
                }
                Previous::Transferred {
                    from: (*other).clone(),
                }
            }
            (false, None) => {
                if !force {
                    refuse(
                        name.as_str(),
                        ErrorCode::UnmanagedFolder,
                        "unmanaged",
                        json!({ "existing": entry.name }),
                    );
                    continue;
                }
                Previous::Unmanaged
            }
        };
        if let Previous::Transferred { from } = &previous {
            outcome.transfers.push(Transfer {
                folder: live_name.as_folder().clone(),
                from: from.clone(),
                to: installing.to_owned(),
                handover: false,
            });
            let from_key = TargetRecord::addon_key(from);
            if !outcome.degraded.contains(&from_key) {
                outcome.degraded.push(from_key);
            }
            outcome.warnings.push(
                Warning::new(
                    WarningCode::OwnershipTransferred,
                    format!(
                        "{} now belongs to {installing}; {from} is marked degraded",
                        live_name.as_str()
                    ),
                )
                .with_fix(Suggestion::command(["omaddons", "list", "--verbose"])),
            );
        }
        outcome.allowed.push(AllowedOp::Replace {
            name: name.clone(),
            live: live_name,
            previous,
        });
    }

    if violations.is_empty() {
        return Ok(outcome);
    }
    violations.sort_by_key(|v| std::cmp::Reverse(rank(v.code, v.reason)));
    Err(refusal(&violations, argv, adopt))
}

/// `ownership_conflict` for `folder`, which `owned`'s addon on another client sharing
/// this AddOns directory owns. Never overridable: the fix is the same command on that
/// client, the alternative removes it there.
pub fn owned_on_shared(folder: &str, owned: &OwnedOn, argv: &[String]) -> Error {
    let (addon, target) = (&owned.addon, &owned.target);
    Error::new(
        ErrorCode::OwnershipConflict,
        format!("{folder} belongs to {addon} on {target}, which shares this AddOns directory"),
        Suggestion::command(crate::discover::registry::with_target(argv, target)),
    )
    .alternative(Suggestion::command([
        "omaddons", "remove", addon, "-t", target,
    ]))
    .details(json!({
        "folder": folder,
        "reason": "owned_on_shared_target",
        "owner": addon,
        "owner_target": target,
    }))
}

/// `target_addons_shared` for a folder recorded here and by `owned` on a client sharing
/// this AddOns directory (both recorded before that was refused).
fn doubled(folder: &MutableFolderName, owned: &OwnedOn) -> Warning {
    Warning::new(
        WarningCode::TargetAddonsShared,
        format!(
            "{} is also recorded by {} on {}, which shares this AddOns directory: the change shows there too",
            folder.as_str(),
            owned.addon,
            owned.target
        ),
    )
    .with_fix(Suggestion::command([
        "omaddons",
        "status",
        "-t",
        owned.target.as_str(),
    ]))
    // The client changed here is added by the plan, which knows it.
    .about([owned.target.as_str()])
}

/// The one error for every violation, with the most serious one's code and fix.
fn refusal(violations: &[Violation], argv: &[String], adopt: Option<AdoptFix<'_>>) -> Error {
    let Some(top) = violations.first() else {
        return Error::internal("an ownership refusal without violations");
    };
    let forced = || {
        // Runnable as is: without the failing run's presentation flags.
        let mut retry = crate::cli::fix_argv(argv);
        retry.push("--force".to_owned());
        Suggestion::command(retry)
    };
    let names: Vec<&str> = violations.iter().map(|v| v.folder.as_str()).collect();
    let (message, fix, alternatives) = match (top.code, top.reason) {
        (ErrorCode::ProtectedFolder, _) => (
            format!(
                "{} belongs to the game client and is never modified",
                top.folder
            ),
            Suggestion::text("this folder belongs to the game client and is never modified"),
            Vec::new(),
        ),
        (ErrorCode::UnmanagedFolder, "linked") => (
            format!(
                "{} is a symlink, which omaddons never modifies or follows",
                top.folder
            ),
            Suggestion::text("remove or rename the symlink yourself"),
            Vec::new(),
        ),
        (ErrorCode::CaseCollision, "case_duplicates") => (
            format!(
                "AddOns holds folders that differ only in case ({}); the client sees one of them",
                top.extra["spellings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Suggestion::text("remove or rename one of the case variants first"),
            Vec::new(),
        ),
        (ErrorCode::CaseCollision, _) => {
            let existing = top.extra["existing"].as_str().unwrap_or_default();
            match top.extra["owner"].as_str() {
                Some(owner) => (
                    format!(
                        "{} would install beside {existing}, which {owner} owns",
                        top.folder
                    ),
                    Suggestion::command(["omaddons", "remove", owner]),
                    Vec::new(),
                ),
                None => (
                    format!(
                        "{} would install beside the unmanaged {existing}; the client sees one folder",
                        top.folder
                    ),
                    Suggestion::text(format!("rename or remove {existing} first")),
                    Vec::new(),
                ),
            }
        }
        (ErrorCode::OwnershipConflict, "owned_on_shared_target") => {
            let shared = owned_on_shared(
                &top.folder,
                &OwnedOn {
                    target: top.extra["owner_target"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    addon: top.extra["owner"].as_str().unwrap_or_default().to_owned(),
                },
                argv,
            );
            (
                shared.message.clone(),
                shared.fix.clone(),
                shared.alternatives.clone(),
            )
        }
        (ErrorCode::OwnershipConflict, _) => {
            let owner = top.extra["owner"].as_str().unwrap_or_default();
            (
                format!("{} belongs to {owner}", top.folder),
                forced(),
                vec![Suggestion::command(["omaddons", "remove", owner])],
            )
        }
        // An addon new here adopts the folder under its ref, the retry overwrites it; one
        // already recorded can only overwrite (a second record of one ref).
        _ => {
            let message = format!("{} is not managed by omaddons", top.folder);
            match adopt {
                Some(adopt) => {
                    let existing = top.extra["existing"].as_str().unwrap_or(&top.folder);
                    let fix = Suggestion::command([
                        "omaddons".to_owned(),
                        "adopt".to_owned(),
                        existing.to_owned(),
                        "--ref".to_owned(),
                        adopt.r#ref.to_string(),
                        "-t".to_owned(),
                        adopt.target.to_owned(),
                    ]);
                    (message, fix, vec![forced()])
                }
                None => (message, forced(), Vec::new()),
            }
        }
    };
    let folders: Vec<Value> = violations
        .iter()
        .map(|v| {
            let mut entry =
                json!({ "folder": v.folder, "code": v.code.wire(), "reason": v.reason });
            if let (Some(map), Some(extra)) = (entry.as_object_mut(), v.extra.as_object()) {
                map.extend(extra.clone());
            }
            entry
        })
        .collect();
    let mut error = Error::new(
        top.code,
        if names.len() > 1 {
            format!(
                "{message} (and {} more: {})",
                names.len() - 1,
                names[1..].join(", ")
            )
        } else {
            message
        },
        fix,
    )
    .details(json!({ "folders": folders }));
    for alternative in alternatives {
        error = error.alternative(alternative);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::Counts;

    fn entry(name: &str, class: FolderClass, owners: &[&str]) -> FolderEntry {
        FolderEntry {
            name: name.into(),
            key: name.to_ascii_lowercase(),
            class,
            owners: owners.iter().map(|o| (*o).to_owned()).collect(),
            case_duplicate: false,
            ownership_conflict: false,
            unreadable: false,
            toc: None,
            verdict: None,
            link_target: None,
            owned_on: Vec::new(),
        }
    }

    fn inventory(entries: Vec<FolderEntry>) -> Inventory {
        Inventory {
            addons_dir_missing: false,
            folders: entries,
            missing: Vec::new(),
            counts: Counts::default(),
            stray_files: 0,
        }
    }

    /// Owners as the inventory lists them: addon names, not keys.
    fn live() -> Inventory {
        inventory(vec![
            entry("Blizzard_AuctionUI", FolderClass::Protected, &[]),
            entry("Mine", FolderClass::Managed, &["Me"]),
            entry("Theirs", FolderClass::Managed, &["Other"]),
            entry("Loose", FolderClass::Unmanaged, &[]),
            entry("Linked", FolderClass::Linked, &[]),
            entry("Foo", FolderClass::Unmanaged, &[]),
            entry("Bar", FolderClass::Managed, &["Other"]),
            entry("Own", FolderClass::Managed, &["Me"]),
        ])
    }

    const ARGV: &[&str] = &["omaddons", "install", "gh:o/me"];

    fn argv() -> Vec<String> {
        ARGV.iter().map(|s| (*s).to_owned()).collect()
    }

    fn install(name: &str, force: bool) -> Result<OwnershipOutcome> {
        let change = FolderChange::Install(FolderName::new(name).unwrap());
        check(&[change], &live(), "Me", force, &argv(), None)
    }

    fn code(result: &Result<OwnershipOutcome>) -> &'static str {
        match result {
            Ok(_) => "ok",
            Err(e) => e.code.wire(),
        }
    }

    #[test]
    fn the_rule_table() {
        #[rustfmt::skip]
        let rows: &[(&str, bool, &str)] = &[
            ("blizzard_auctionui", false, "protected_folder"),
            ("blizzard_auctionui", true, "protected_folder"),
            ("BLIZZARD_NEW", true, "protected_folder"),
            ("Blizzard_X", false, "protected_folder"),
            ("Linked", false, "unmanaged_folder"),
            ("Linked", true, "unmanaged_folder"),
            ("foo", false, "case_collision"),
            ("foo", true, "case_collision"),
            ("bar", false, "case_collision"),
            ("bar", true, "case_collision"),
            ("Theirs", false, "ownership_conflict"),
            ("Theirs", true, "ok"),
            ("Loose", false, "unmanaged_folder"),
            ("Loose", true, "ok"),
            ("Mine", false, "ok"),
            ("mine", false, "ok"),
            ("Brand", false, "ok"),
        ];
        for (name, force, want) in rows {
            let result = install(name, *force);
            assert_eq!(code(&result), *want, "{name} force={force}: {result:?}");
            if let Err(e) = &result {
                assert_eq!(e.exit().code(), 4, "{name}");
            }
        }
    }

    /// A folder of another client sharing this AddOns directory.
    fn on_era(name: &str, class: FolderClass, owners: &[&str]) -> FolderEntry {
        FolderEntry {
            owned_on: vec![OwnedOn {
                target: "era".into(),
                addon: "Both".into(),
            }],
            ..entry(name, class, owners)
        }
    }

    #[test]
    fn a_shared_folder_is_refused_even_forced_and_a_doubled_one_warns() {
        let live = inventory(vec![
            on_era("Both", FolderClass::Shared, &[]),
            on_era("Twice", FolderClass::Managed, &["Me"]),
        ]);
        let argv: Vec<String> = ["omaddons", "install", "gh:o/both", "-t", "anniversary"]
            .map(str::to_owned)
            .to_vec();
        for force in [false, true] {
            let change = FolderChange::Install(FolderName::new("Both").unwrap());
            let err = check(&[change], &live, "Me", force, &argv, None).unwrap_err();
            assert_eq!(err.code, ErrorCode::OwnershipConflict, "force={force}");
            assert!(err.message.contains("Both on era"), "{}", err.message);
            assert_eq!(
                err.details["folders"][0]["owner_target"], "era",
                "{}",
                err.details
            );
            assert_eq!(
                err.fix.argv().unwrap(),
                ["omaddons", "install", "gh:o/both", "-t", "era"]
            );
            assert_eq!(
                err.alternatives[0].argv().unwrap(),
                ["omaddons", "remove", "Both", "-t", "era"]
            );
        }
        // Recorded on both before this was refused: allowed, with a warning.
        for change in [
            FolderChange::Install(FolderName::new("Twice").unwrap()),
            FolderChange::Remove(FolderName::new("Twice").unwrap()),
        ] {
            let done = check(&[change], &live, "Me", false, &argv, None).unwrap();
            assert_eq!(done.allowed.len(), 1);
            assert_eq!(done.warnings[0].code, WarningCode::TargetAddonsShared);
            assert_eq!(
                done.warnings[0].fix.as_ref().unwrap().argv().unwrap(),
                ["omaddons", "status", "-t", "era"]
            );
        }
    }

    #[test]
    fn allowed_ops_carry_the_live_spelling_and_who_had_it() {
        let ok = |name, force| install(name, force).unwrap().allowed.remove(0);
        let f = |n: &str| FolderName::new(n).unwrap().into_mutable().unwrap();
        assert_eq!(ok("Brand", false), AllowedOp::Add { name: f("Brand") });
        assert_eq!(
            ok("mine", false),
            AllowedOp::Replace {
                name: f("mine"),
                live: f("Mine"),
                previous: Previous::Same
            }
        );
        assert_eq!(
            ok("Loose", true),
            AllowedOp::Replace {
                name: f("Loose"),
                live: f("Loose"),
                previous: Previous::Unmanaged
            }
        );
        assert_eq!(
            ok("Theirs", true),
            AllowedOp::Replace {
                name: f("Theirs"),
                live: f("Theirs"),
                previous: Previous::Transferred {
                    from: "Other".into()
                }
            }
        );
    }

    #[test]
    fn a_forced_transfer_degrades_the_previous_owner() {
        let outcome = install("Theirs", true).unwrap();
        assert_eq!(
            outcome.transfers,
            [Transfer {
                folder: FolderName::new("Theirs").unwrap(),
                from: "Other".into(),
                to: "Me".into(),
                handover: false,
            }]
        );
        assert_eq!(outcome.degraded, ["other"], "record keys");
        assert_eq!(outcome.warnings[0].code, WarningCode::OwnershipTransferred);
    }

    #[test]
    fn a_forced_retry_drops_the_failing_runs_presentation_flags() {
        let shown: Vec<String> = ["omaddons", "install", "gh:o/me", "--json", "--no-color"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let change = FolderChange::Install(FolderName::new("Theirs").unwrap());
        let err = check(&[change], &live(), "Me", false, &shown, None).unwrap_err();
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "install", "gh:o/me", "--force"]
        );
    }

    #[test]
    fn fixes_and_alternatives() {
        let forced: Vec<String> = ARGV
            .iter()
            .map(|s| (*s).to_owned())
            .chain(["--force".to_owned()])
            .collect();
        let err = install("Theirs", false).unwrap_err();
        assert_eq!(err.fix.argv(), Some(forced.as_slice()));
        assert_eq!(
            err.alternatives[0].argv().unwrap(),
            ["omaddons", "remove", "Other"]
        );
        assert_eq!(err.details["folders"][0]["owner"], "Other");

        let err = install("Loose", false).unwrap_err();
        assert_eq!(err.fix.argv(), Some(forced.as_slice()));

        let err = install("foo", true).unwrap_err();
        assert_eq!(
            (err.fix.display(), err.fix.argv()),
            ("rename or remove Foo first", None),
            "--force never resolves a case collision"
        );
        assert!(err.alternatives.is_empty());
        assert!(
            err.message.contains("foo") && err.message.contains("Foo"),
            "{}",
            err.message
        );

        let err = install("bar", false).unwrap_err();
        assert_eq!(
            err.fix.argv().unwrap(),
            ["omaddons", "remove", "Other"],
            "a managed spelling twin: remove its owner"
        );
        assert!(err.alternatives.is_empty());

        let err = install("blizzard_auctionui", true).unwrap_err();
        assert_eq!(
            (err.fix.display(), err.fix.argv()),
            (
                "this folder belongs to the game client and is never modified",
                None
            )
        );
        let err = install("Linked", true).unwrap_err();
        assert_eq!(
            (
                err.fix.display(),
                err.details["folders"][0]["class"].as_str()
            ),
            ("remove or rename the symlink yourself", Some("linked"))
        );
    }

    #[test]
    fn every_violation_in_one_error_with_the_most_serious_code() {
        let changes: Vec<FolderChange> =
            ["Loose", "Theirs", "foo", "Linked", "Blizzard_Y", "Brand"]
                .iter()
                .map(|n| FolderChange::Install(FolderName::new(n).unwrap()))
                .collect();
        let err = check(&changes, &live(), "me", false, &argv(), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProtectedFolder);
        let codes: Vec<&str> = err.details["folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["code"].as_str().unwrap())
            .collect();
        assert_eq!(
            codes,
            [
                "protected_folder",
                "unmanaged_folder",
                "case_collision",
                "ownership_conflict",
                "unmanaged_folder"
            ],
            "ranked; Brand is fine"
        );
        let without_protected = &changes[..4];
        let err = check(without_protected, &live(), "me", false, &argv(), None).unwrap_err();
        assert_eq!(
            (err.code, err.details["folders"][0]["reason"].as_str()),
            (ErrorCode::UnmanagedFolder, Some("linked"))
        );
    }

    #[test]
    fn removals_and_case_duplicates() {
        let removals = [
            FolderChange::Remove(FolderName::new("own").unwrap()),
            FolderChange::Remove(FolderName::new("Gone").unwrap()),
        ];
        let outcome = check(&removals, &live(), "me", false, &argv(), None).unwrap();
        assert_eq!(
            outcome.allowed,
            [AllowedOp::Remove {
                live: FolderName::new("Own").unwrap().into_mutable().unwrap()
            }],
            "a missing folder is the planner's record-only change"
        );
        let protected = [FolderChange::Remove(
            FolderName::new("Blizzard_AuctionUI").unwrap(),
        )];
        assert_eq!(
            check(&protected, &live(), "me", true, &argv(), None)
                .unwrap_err()
                .code,
            ErrorCode::ProtectedFolder
        );

        let twins = inventory(vec![
            entry("Dup", FolderClass::Unmanaged, &[]),
            entry("dup", FolderClass::Unmanaged, &[]),
        ]);
        let err = check(
            &[FolderChange::Install(FolderName::new("DUP").unwrap())],
            &twins,
            "me",
            true,
            &argv(),
            None,
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.fix.argv()),
            (ErrorCode::CaseCollision, None),
            "--force cannot pick between two spellings"
        );
    }

    #[test]
    fn an_addon_new_to_the_target_adopts_an_unmanaged_folder_and_nothing_else() {
        let r: Ref = "gh:o/me".parse().unwrap();
        let adopt = Some(AdoptFix {
            target: "retail",
            r#ref: &r,
        });
        let judge = |name: &str, adopt| {
            let change = FolderChange::Install(FolderName::new(name).unwrap());
            check(&[change], &live(), "Me", false, &argv(), adopt).unwrap_err()
        };
        let forced: Vec<String> = argv().into_iter().chain(["--force".to_owned()]).collect();

        // New here: adopt it under the ref, or overwrite it.
        let err = judge("Loose", adopt);
        assert_eq!(
            (err.code, err.fix.argv().unwrap()),
            (
                ErrorCode::UnmanagedFolder,
                &[
                    "omaddons", "adopt", "Loose", "--ref", "gh:o/me", "-t", "retail"
                ]
                .map(String::from)[..]
            )
        );
        assert_eq!(err.alternatives.len(), 1);
        assert_eq!(err.alternatives[0].argv(), Some(forced.as_slice()));
        // Recorded already (an update): overwriting is the only way.
        let err = judge("Loose", None);
        assert_eq!(err.fix.argv(), Some(forced.as_slice()));
        assert!(err.alternatives.is_empty());
        // Never for a symlink, a protected folder or another addon's folder.
        assert_eq!(judge("Linked", adopt).fix.argv(), None);
        assert_eq!(judge("Blizzard_AuctionUI", adopt).fix.argv(), None);
        assert_eq!(judge("Theirs", adopt).fix.argv(), Some(forced.as_slice()));
    }

    fn install_in(
        name: &str,
        as_addon: &str,
        force: bool,
        view: &WorkingView,
    ) -> Result<OwnershipOutcome> {
        let change = FolderChange::Install(FolderName::new(name).unwrap());
        check_in(&[change], &live(), as_addon, force, &argv(), view, None)
    }

    #[test]
    fn a_folder_another_work_claims_conflicts_whatever_force() {
        let mut view = WorkingView::default();
        view.claim(&FolderName::new("Shared").unwrap(), "First");
        for force in [false, true] {
            let err = install_in("shared", "Second", force, &view).unwrap_err();
            assert_eq!(err.code, ErrorCode::OwnershipConflict);
            assert_eq!(err.details["within_transaction"], true);
            assert_eq!(err.details["folders"][0]["owner"], "First");
            assert!(
                err.fix.argv().is_none(),
                "a text fix: {}",
                err.fix.display()
            );
            assert!(err.message.contains("First") && err.message.contains("Second"));
        }
        assert!(
            install_in("Shared", "First", false, &view).is_ok(),
            "its own claim"
        );
    }

    #[test]
    fn a_folder_its_owner_releases_is_handed_over() {
        let mut view = WorkingView::default();
        view.release(&FolderName::new("Theirs").unwrap(), "Other");
        let outcome = install_in("Theirs", "Me", false, &view).unwrap();
        assert_eq!(
            outcome.transfers,
            [Transfer {
                folder: FolderName::new("Theirs").unwrap(),
                from: "Other".into(),
                to: "Me".into(),
                handover: true,
            }]
        );
        assert!(outcome.degraded.is_empty() && outcome.warnings.is_empty());
        assert!(matches!(
            outcome.allowed.as_slice(),
            [AllowedOp::Replace {
                previous: Previous::Transferred { .. },
                ..
            }]
        ));
        // A second claimant after the first took it: a conflict in the transaction.
        view.claim(&FolderName::new("Theirs").unwrap(), "Me");
        let err = install_in("Theirs", "Third", false, &view).unwrap_err();
        assert_eq!(err.details["within_transaction"], true);
        // Released by someone who does not own it: the usual rules.
        let mut wrong = WorkingView::default();
        wrong.release(&FolderName::new("Theirs").unwrap(), "Nobody");
        assert_eq!(
            install_in("Theirs", "Me", false, &wrong).unwrap_err().code,
            ErrorCode::OwnershipConflict
        );
    }
}
