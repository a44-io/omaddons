//! The ownership model (vision §5.5 step 4): whether a transaction may write each
//! AddOns folder. Pure: it judges planned folder changes against an inventory, at plan
//! time and again under the lock.
//!
//! Per folder, first match wins:
//! 1. a `blizzard_*` key (any case) is the client's own: `protected_folder`, never
//!    overridable (D14);
//! 2. a symlinked folder (a developer's working copy) is never touched:
//!    `unmanaged_folder` (class `linked`), never overridable;
//! 3. a key whose live spelling differs from the incoming one: `case_collision` (Wine
//!    would see one folder under two names); `--force` replaces under the live
//!    spelling. The installing addon's own folder may change case freely;
//! 4. another addon's folder: `ownership_conflict`; `--force` transfers it and marks
//!    the previous owner degraded (`ownership_transferred`);
//! 5. an unmanaged folder: `unmanaged_folder`; `--force` replaces it (its snapshot
//!    keeps it undoable);
//! 6. otherwise a replacement of the addon's own folder, or an add.
//!
//! Every violation is reported in one error (`details.folders`), whose code is the
//! most serious one: protected > linked > case_collision > ownership_conflict >
//! unmanaged.

use serde_json::{Value, json};

use crate::error::{Error, ErrorCode, Result, Suggestion, Warning, WarningCode};
use crate::inventory::{FolderClass, FolderEntry, Inventory};
use crate::model::FolderName;
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
        name: FolderName,
    },
    Replace {
        name: FolderName,
        live: FolderName,
        previous: Previous,
    },
    Remove {
        live: FolderName,
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

/// A folder moved between addons by `--force`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub folder: FolderName,
    pub from: String,
    pub to: String,
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

/// Judges `changes` for the addon `installing` against `inventory`. `argv` is the
/// command line, repeated with `--force` in fixes.
pub fn check(
    changes: &[FolderChange],
    inventory: &Inventory,
    installing: &str,
    force: bool,
    argv: &[String],
) -> Result<OwnershipOutcome> {
    let me = TargetRecord::addon_key(installing);
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
        let (FolderChange::Install(name) | FolderChange::Remove(name)) = change;
        if name.is_protected() {
            refuse(
                name.as_str(),
                ErrorCode::ProtectedFolder,
                "protected",
                json!({}),
            );
            continue;
        }
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
        if let [first, second, ..] = live.as_slice() {
            refuse(
                name.as_str(),
                ErrorCode::CaseCollision,
                "case_duplicates",
                json!({ "existing": [first.name, second.name] }),
            );
            continue;
        }
        let Some(entry) = live.first() else {
            if let FolderChange::Install(name) = change {
                outcome.allowed.push(AllowedOp::Add { name: name.clone() });
            }
            continue;
        };
        let Ok(live_name) = FolderName::new(&entry.name) else {
            continue;
        };
        if let FolderChange::Remove(_) = change {
            outcome.allowed.push(AllowedOp::Remove { live: live_name });
            continue;
        }
        // Owners are addon names as recorded; identity is the record key.
        let is_me = |owner: &String| TargetRecord::addon_key(owner) == me;
        let others: Vec<&String> = entry.owners.iter().filter(|o| !is_me(o)).collect();
        let mine = entry.owners.iter().any(is_me);
        let respelled = entry.name != name.as_str();
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
                folder: live_name.clone(),
                from: from.clone(),
                to: installing.to_owned(),
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
    Err(refusal(&violations, argv))
}

/// The one error for every violation, with the most serious one's code and fix.
fn refusal(violations: &[Violation], argv: &[String]) -> Error {
    let Some(top) = violations.first() else {
        return Error::internal("an ownership refusal without violations");
    };
    let forced = || {
        let mut retry = argv.to_vec();
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
                top.extra["existing"]
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
        (ErrorCode::OwnershipConflict, _) => {
            let owner = top.extra["owner"].as_str().unwrap_or_default();
            (
                format!("{} belongs to {owner}", top.folder),
                forced(),
                vec![Suggestion::command(["omaddons", "remove", owner])],
            )
        }
        _ => (
            format!("{} is not managed by omaddons", top.folder),
            unmanaged_fix(argv),
            Vec::new(),
        ),
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

/// The fix for an unmanaged folder: rerun with `--force` until `adopt` exists (M2).
fn unmanaged_fix(argv: &[String]) -> Suggestion {
    let mut retry = argv.to_vec();
    retry.push("--force".to_owned());
    Suggestion::command(retry)
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
        check(&[change], &live(), "Me", force, &argv())
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

    #[test]
    fn allowed_ops_carry_the_live_spelling_and_who_had_it() {
        let ok = |name, force| install(name, force).unwrap().allowed.remove(0);
        let f = |n: &str| FolderName::new(n).unwrap();
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
                to: "Me".into()
            }]
        );
        assert_eq!(outcome.degraded, ["other"], "record keys");
        assert_eq!(outcome.warnings[0].code, WarningCode::OwnershipTransferred);
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
        let err = check(&changes, &live(), "me", false, &argv()).unwrap_err();
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
        let err = check(without_protected, &live(), "me", false, &argv()).unwrap_err();
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
        let outcome = check(&removals, &live(), "me", false, &argv()).unwrap();
        assert_eq!(
            outcome.allowed,
            [AllowedOp::Remove {
                live: FolderName::new("Own").unwrap()
            }],
            "a missing folder is the planner's record-only change"
        );
        let protected = [FolderChange::Remove(
            FolderName::new("Blizzard_AuctionUI").unwrap(),
        )];
        assert_eq!(
            check(&protected, &live(), "me", true, &argv())
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
        )
        .unwrap_err();
        assert_eq!(
            (err.code, err.fix.argv()),
            (ErrorCode::CaseCollision, None),
            "--force cannot pick between two spellings"
        );
    }
}
