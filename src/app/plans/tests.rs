//! `plans show`'s judgment, resolution, data and human view, over planted plans and
//! history lines.
#![cfg(test)]

use std::fs;

use serde_json::Value;

use super::*;
use crate::app::approval::{ApprovalScope, ApprovedItem, ApprovedOp};
use crate::app::intent::{
    AdoptIntent, CopyIntent, InstallIntent, PinIntent, PruneIntent, RemoveIntent, RestoreIntent,
    SyncIntent, TargetScope, UndoIntent, UndoSelection, UpdateIntent,
};
use crate::model::TargetRef;
use crate::state::plans::test_support::{paths, plan_with, plant};

const RETAIL: &str = "01JB8Q2VA00000000000000001";
const CLASSIC: &str = "01JB8Q2VA00000000000000002";

fn key(text: &str) -> TargetKey {
    text.parse().unwrap()
}

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

// ---------------------------------------------------------------------------------------
// The judgment.

fn base() -> Judgement {
    Judgement {
        stored: true,
        readable: true,
        records_history: true,
        approved: [RETAIL, CLASSIC].map(str::to_owned).into(),
        committed: BTreeSet::new(),
        complete: true,
        unreadable: 0,
        open: Vec::new(),
        expired: false,
    }
}

fn open(client: Option<&str>) -> Open {
    Open {
        tx: "01JB8Q2VJ00000000000000001".parse().unwrap(),
        client: client.map(str::to_owned),
        name: client
            .map_or("a client it does not name", |_| "retail")
            .to_owned(),
    }
}

#[test]
fn judge_cases() {
    use PlanState::{Completed, Expired, Partial, Pending, Unknown};
    let both: BTreeSet<String> = [RETAIL, CLASSIC].map(str::to_owned).into();
    let one: BTreeSet<String> = [RETAIL.to_owned()].into();
    type Edit = Box<dyn Fn(&mut Judgement)>;
    let cases: Vec<(&str, Edit, PlanState, Option<&str>)> = vec![
        (
            "file_gone_is_unknown",
            Box::new(|j| j.stored = false),
            Unknown,
            Some("FileGone"),
        ),
        (
            "unreadable_is_unknown",
            Box::new(|j| j.readable = false),
            Unknown,
            Some("Unreadable"),
        ),
        (
            "prune_is_unknown_even_after_expiry",
            Box::new(|j| {
                j.records_history = false;
                j.expired = true;
            }),
            Unknown,
            Some("NoHistoryLine"),
        ),
        (
            "no_approved_targets_is_unknown",
            Box::new(|j| j.approved.clear()),
            Unknown,
            Some("NoApprovedClients"),
        ),
        (
            "an_unapproved_target_in_history_is_unknown",
            Box::new(|j| j.committed = ["01JB8Q2VA00000000000000009".to_owned()].into()),
            Unknown,
            Some("Disagrees"),
        ),
        (
            "all_committed_is_completed",
            Box::new({
                let both = both.clone();
                move |j| j.committed = both.clone()
            }),
            Completed,
            None,
        ),
        (
            "completed_survives_a_cut_scan",
            Box::new({
                let both = both.clone();
                move |j| {
                    j.committed = both.clone();
                    j.complete = false;
                }
            }),
            Completed,
            None,
        ),
        (
            "completed_survives_unreadable_lines",
            Box::new({
                let both = both.clone();
                move |j| {
                    j.committed = both.clone();
                    j.unreadable = 3;
                }
            }),
            Completed,
            None,
        ),
        (
            "completed_survives_an_open_journal",
            Box::new({
                let both = both.clone();
                move |j| {
                    j.committed = both.clone();
                    j.open = vec![open(Some(RETAIL))];
                }
            }),
            Completed,
            None,
        ),
        (
            "a_cut_scan_short_of_all_is_unknown",
            Box::new({
                let one = one.clone();
                move |j| {
                    j.committed = one.clone();
                    j.complete = false;
                }
            }),
            Unknown,
            Some("ScanCut"),
        ),
        (
            "unreadable_lines_short_of_all_are_unknown",
            Box::new(|j| j.unreadable = 1),
            Unknown,
            Some("UnreadableLines"),
        ),
        (
            "unattributed_lines_short_of_all_are_unknown",
            Box::new({
                let one = one.clone();
                move |j| {
                    j.committed = one.clone();
                    j.unreadable = 2;
                }
            }),
            Unknown,
            Some("UnreadableLines"),
        ),
        (
            "an_open_journal_on_an_approved_target_is_unknown",
            Box::new(|j| j.open = vec![open(Some(CLASSIC))]),
            Unknown,
            Some("Unfinished"),
        ),
        (
            "an_open_journal_without_a_target_is_unknown",
            Box::new(|j| j.open = vec![open(None)]),
            Unknown,
            Some("Unfinished"),
        ),
        (
            "an_open_journal_on_another_target_does_not_matter",
            Box::new(|j| j.open = vec![open(Some("01JB8Q2VA00000000000000009"))]),
            Pending,
            None,
        ),
        (
            "some_committed_is_partial",
            Box::new({
                let one = one.clone();
                move |j| j.committed = one.clone()
            }),
            Partial,
            None,
        ),
        (
            "none_committed_after_expiry_is_expired",
            Box::new(|j| j.expired = true),
            Expired,
            None,
        ),
        (
            "none_committed_before_expiry_is_pending",
            Box::new(|_| {}),
            Pending,
            None,
        ),
    ];
    for (name, edit, state, why) in cases {
        let mut j = base();
        edit(&mut j);
        let (got, reason) = judge(&j);
        let reason = reason.map(|r| {
            let text = format!("{r:?}");
            text.split(['(', ' ']).next().unwrap_or_default().to_owned()
        });
        assert_eq!(
            (got, reason.as_deref()),
            (state, why),
            "case {name}: judgement {j:?}"
        );
    }
}

/// `judge`'s `expired` is the plan's own predicate, so `plans show` and `apply` agree at
/// the exact instant of expiry.
#[test]
fn none_committed_at_the_expiry_instant_matches_apply() {
    let plan = plan_with(at("2026-10-28T12:00:00Z"), &[]);
    for (name, now, expired) in [
        ("at the instant", plan.expires_at, false),
        (
            "a nanosecond after",
            plan.expires_at
                .checked_add(jiff::SignedDuration::from_nanos(1))
                .unwrap(),
            true,
        ),
    ] {
        let mut j = base();
        j.expired = plan.is_expired(now);
        let state = if expired {
            PlanState::Expired
        } else {
            PlanState::Pending
        };
        assert_eq!(judge(&j).0, state, "case {name}");
    }
}

#[test]
fn records_history_is_false_only_for_backup_prune() {
    let scope = || TargetScope {
        all: true,
        selector: None,
        targets: Vec::new(),
    };
    let retail = || TargetRef::parse("retail");
    let samples = [
        StoredIntent::Install(InstallIntent {
            refs: Vec::new(),
            target: retail(),
            target_id: "retail".into(),
            channel: None,
            version: None,
            file: None,
            reinstall: false,
            no_deps: false,
            force: false,
            strict: false,
            refresh: false,
            any_flavor: false,
            no_any_flavor: false,
            while_running: false,
        }),
        StoredIntent::Update(UpdateIntent {
            addons: Vec::new(),
            scope: scope(),
            channel: None,
            refresh: false,
            strict: false,
            no_deps: false,
            while_running: false,
        }),
        StoredIntent::Pin(PinIntent {
            addon: "Foo".into(),
            scope: scope(),
            version: None,
            while_running: false,
        }),
        StoredIntent::Unpin(PinIntent {
            addon: "Foo".into(),
            scope: scope(),
            version: None,
            while_running: false,
        }),
        StoredIntent::Adopt(AdoptIntent {
            folders: Vec::new(),
            apply: true,
            r#ref: None,
            scope: scope(),
            no_verify: false,
            refresh: false,
            while_running: false,
        }),
        StoredIntent::Sync(SyncIntent {
            file: "/home/u/addons.toml".to_owned().try_into().unwrap(),
            default_file: true,
            scope: scope(),
            strict: false,
            no_deps: false,
            locked: false,
            prune: false,
            while_running: false,
        }),
        StoredIntent::Copy(CopyIntent {
            from: retail(),
            from_id: "retail".into(),
            to: TargetRef::parse("classic"),
            to_id: "classic".into(),
            addons: Vec::new(),
            strict: false,
            no_deps: false,
            while_running: false,
        }),
        StoredIntent::Remove(RemoveIntent {
            addons: vec!["Foo".into()],
            target: retail(),
            target_id: "retail".into(),
            while_running: false,
        }),
        StoredIntent::Undo(UndoIntent {
            selection: UndoSelection::Tx("01JB8Q2VJ00000000000000001".parse().unwrap()),
            saved_variables: None,
            while_running: false,
        }),
        StoredIntent::BackupRestore(RestoreIntent {
            backup: "01M6Q2VA000000000000000000".parse().unwrap(),
            target: retail(),
            target_id: "retail".into(),
            addons: Vec::new(),
        }),
        StoredIntent::BackupPrune(PruneIntent {
            ids: Vec::new(),
            keep: Some(1),
            older_than: None,
            scope: scope(),
        }),
    ];
    let got: Vec<(&str, bool)> = samples
        .iter()
        .map(|intent| (intent.command_path(), records_history(intent)))
        .collect();
    let want: Vec<(&str, bool)> = crate::app::intent::StoredIntent::PATHS
        .iter()
        .map(|(path, _)| (*path, *path != "backup prune"))
        .collect();
    assert_eq!(got, want, "one sample per variant, in PATHS order");
}

// ---------------------------------------------------------------------------------------
// Resolution and the data.

/// An approval of one change on each named client.
fn approving(clients: &[(&str, &str)]) -> Approval {
    Approval {
        routine: true,
        reasons: Vec::new(),
        selection: clients
            .iter()
            .map(|(id, k)| ApprovedItem {
                target_key: Some(key(k)),
                target_id: (*id).to_owned(),
                addon: Some("Foo".into()),
                source: None,
                channel: None,
                release_id: None,
                asset: None,
                from_version: Some("1".into()),
                to_version: None,
                op: ApprovedOp::Remove,
                fields: Vec::new(),
                tx: None,
                backup: None,
            })
            .collect(),
        scope: ApprovalScope::default(),
    }
}

/// A history line committing `tx` of `plan` on `target` at `at`.
fn line(plan: &str, tx: &str, target: &str, run: &str, at: &str) -> String {
    format!(
        "{}\n",
        json!({
            "schema_version": "1.0", "tx": tx, "kind": "remove", "at": at,
            "target": target, "run": run, "plan": plan, "result": "committed",
        })
    )
}

fn write_history(paths: &Paths, lines: &[String]) {
    fs::create_dir_all(paths.state_dir()).unwrap();
    fs::write(paths.history_file(), lines.concat()).unwrap();
}

const CREATED: &str = "2026-10-28T12:00:00Z";
const LATER: &str = "2026-10-28T12:30:00Z";

fn now() -> Timestamp {
    at("2026-10-28T13:00:00Z")
}

/// A stored plan approving retail and classic, planted.
fn stored_plan(p: &Paths) -> StoredPlan {
    let mut plan = plan_with(at(CREATED), &[]);
    plan.approval = approving(&[("retail", RETAIL), ("classic", CLASSIC)]);
    plant(p, &plan);
    plan
}

#[test]
fn resolve_cases() {
    let (_dir, p) = paths();
    let plan = stored_plan(&p);
    let id = plan.plan_id.to_string();
    let err = |given: &str| show_in(&p, given, now()).map(|_| ()).unwrap_err();
    for (name, given, code) in [
        ("seven_characters_is_usage", &id[..7], ErrorCode::Usage),
        (
            "longer_than_26_is_not_found",
            "01M9P000000000000000000000ZZ",
            ErrorCode::PlanNotFound,
        ),
        (
            "not_crockford_is_not_found",
            "not-a-plan-id",
            ErrorCode::PlanNotFound,
        ),
    ] {
        let e = err(given);
        assert_eq!(e.code, code, "case {name}: given {given:?}, got {e:?}");
    }
    for (name, given) in [
        ("full_id_with_a_file", id.clone()),
        ("a_prefix_naming_one_file", id[..10].to_owned()),
        ("lowercase_is_uppercased", id[..10].to_ascii_lowercase()),
    ] {
        let got = show_in(&p, &given, now()).unwrap();
        assert_eq!(
            (
                got.data.plan_id,
                got.data.stored,
                got.data.readable,
                got.data.state
            ),
            (plan.plan_id, true, true, PlanState::Pending),
            "case {name}: given {given:?}"
        );
    }

    // Two stored plans share a prefix.
    let mut twin = plan.clone();
    twin.plan_id = format!("{}ZZZZZZ", &id[..20]).parse().unwrap();
    plant(&p, &twin);
    let e = err(&id[..12]);
    let mut candidates = vec![plan.plan_id.to_string(), twin.plan_id.to_string()];
    candidates.sort();
    assert_eq!(
        (
            e.code,
            e.details.clone(),
            e.fix.argv().map(<[String]>::to_vec)
        ),
        (
            ErrorCode::RefAmbiguous,
            json!({"prefix": &id[..12], "candidates": candidates}),
            Some(
                ["omaddons", "plans", "show", candidates[0].as_str()]
                    .map(str::to_owned)
                    .to_vec()
            ),
        ),
        "case a_prefix_naming_two_files_is_ambiguous"
    );
}

#[test]
fn resolve_through_history() {
    let (_dir, p) = paths();
    let gone = "01M9P000000000000000000PA1";
    let other = "01M9P000000000000000000PB2";
    // No file and no line: plan_not_found, its details the id given.
    let e = show_in(&p, gone, now()).map(|_| ()).unwrap_err();
    assert_eq!(
        (
            e.code,
            e.details.clone(),
            e.fix.argv().map(<[String]>::to_vec)
        ),
        (
            ErrorCode::PlanNotFound,
            json!({"plan_id": gone}),
            Some(
                ["omaddons", "help", "plans", "show"]
                    .map(str::to_owned)
                    .to_vec()
            ),
        ),
        "case no_file_and_no_history_is_plan_not_found"
    );
    // One history id: found, the file gone.
    write_history(
        &p,
        &[line(
            gone,
            "01JB8Q2VJ00000000000000001",
            RETAIL,
            "01JB8Q2VB00000000000000001",
            LATER,
        )],
    );
    let got = show_in(&p, &gone[..20], now()).unwrap();
    assert_eq!(
        (
            got.data.stored,
            got.data.readable,
            got.data.state,
            got.view.why.clone()
        ),
        (false, false, PlanState::Unknown, Some(Why::FileGone)),
        "case a_prefix_with_no_file_and_one_history_id"
    );
    assert_eq!(got.data.evidence.targets.len(), 1);
    // Two history ids share the prefix.
    write_history(
        &p,
        &[
            line(
                gone,
                "01JB8Q2VJ00000000000000001",
                RETAIL,
                "01JB8Q2VB00000000000000001",
                LATER,
            ),
            line(
                other,
                "01JB8Q2VJ00000000000000002",
                RETAIL,
                "01JB8Q2VB00000000000000002",
                LATER,
            ),
        ],
    );
    let e = show_in(&p, &gone[..20], now()).map(|_| ()).unwrap_err();
    assert_eq!(
        (e.code, e.details.clone()),
        (
            ErrorCode::RefAmbiguous,
            json!({"prefix": &gone[..20], "candidates": [gone, other]}),
        ),
        "case a_prefix_with_no_file_and_two_history_ids_is_ambiguous"
    );
}

#[test]
fn a_cut_scan_with_nothing_found_says_so_in_plan_not_found() {
    let e = not_found("01M9P000", Some(at(CREATED)), false);
    assert!(
        e.message.contains("the search stopped there"),
        "{}",
        e.message
    );
    let e = not_found("01M9P000", Some(at(CREATED)), true);
    assert!(!e.message.contains("stopped"), "{}", e.message);
    assert!(
        e.message.contains(&format!("since {CREATED}")),
        "{}",
        e.message
    );
    // A horizon a day before now moves: it is said in words, so two runs agree.
    let e = not_found("01M9P000", None, true);
    assert!(e.message.ends_with("in the last 24 hours"), "{}", e.message);
}

#[test]
fn a_partial_plan_names_what_history_records() {
    let (_dir, p) = paths();
    let plan = stored_plan(&p);
    let id = plan.plan_id.to_string();
    write_history(
        &p,
        &[line(
            &id,
            "01JB8Q2VJ00000000000000001",
            CLASSIC,
            "01JB8Q2VB00000000000000001",
            LATER,
        )],
    );
    let got = show_in(&p, &id, now()).unwrap();
    assert_eq!(got.data.state, PlanState::Partial);
    assert_eq!(got.view.unrecorded, ["retail"]);
    assert_eq!(
        got.data
            .evidence
            .targets
            .iter()
            .map(|t| t.target.as_str())
            .collect::<Vec<_>>(),
        ["classic"],
        "named by the approval when the registry does not know the key"
    );
}

#[test]
fn data_has_every_key_in_order() {
    let (_dir, p) = paths();
    let plan = stored_plan(&p);
    // The keys as serialized, in their order (a `Value` map would sort them).
    let keys = |data: &PlansShowData| -> Vec<String> {
        let text = serde_json::to_string(data).unwrap();
        let value = serde_json::to_value(data).unwrap();
        let mut keys: Vec<(usize, String)> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| (text.find(&format!("\"{k}\":")).unwrap(), k.clone()))
            .collect();
        keys.sort();
        keys.into_iter().map(|(_, k)| k).collect()
    };
    let readable = show_in(&p, &plan.plan_id.to_string(), now()).unwrap().data;
    let gone = "01M9P000000000000000000PA1";
    write_history(
        &p,
        &[line(
            gone,
            "01JB8Q2VJ00000000000000001",
            RETAIL,
            "01JB8Q2VB00000000000000001",
            LATER,
        )],
    );
    let file_gone = show_in(&p, gone, now()).unwrap().data;
    let want = [
        "plan_id",
        "state",
        "stored",
        "readable",
        "created_at",
        "expires_at",
        "build_id",
        "build_matches",
        "intent",
        "approval",
        "inputs",
        "evidence",
    ];
    assert_eq!(keys(&readable), want);
    assert_eq!(keys(&file_gone), want, "nulls included");
    let value = serde_json::to_value(&file_gone).unwrap();
    for key in [
        "created_at",
        "expires_at",
        "build_id",
        "build_matches",
        "intent",
        "approval",
    ] {
        assert_eq!(value[key], Value::Null, "{key}");
    }
    assert_eq!(value["inputs"], json!([]));
}

#[test]
fn evidence_target_names_fall_back() {
    let mut registry = Registry::default();
    let approval = approving(&[("classic", CLASSIC)]);
    let names = Names {
        registry: &registry,
        approval: Some(&approval),
    };
    assert_eq!(names.of(key(CLASSIC)), "classic", "the approval's id");
    assert_eq!(names.of(key(RETAIL)), RETAIL, "the key itself");
    registry
        .targets
        .push(crate::state::registry::RegistryEntry {
            key: key(CLASSIC),
            id: "classic-renamed".into(),
            ..sample_entry()
        });
    let names = Names {
        registry: &registry,
        approval: Some(&approval),
    };
    assert_eq!(
        names.of(key(CLASSIC)),
        "classic-renamed",
        "the registry's id"
    );
}

fn sample_entry() -> crate::state::registry::RegistryEntry {
    crate::state::registry::RegistryEntry {
        key: key(RETAIL),
        id: "retail".into(),
        install_root: "/r".into(),
        client_dir: "/r/_retail_".into(),
        flavor_dir: None,
        product: None,
        qualifier: String::new(),
        origin: crate::state::registry::Origin::Discovered,
        registered_at: at(CREATED),
    }
}

#[test]
fn undo_hints_mirror_targets_data() {
    let registry = Registry::default();
    let approval = approving(&[("retail", RETAIL), ("classic", CLASSIC)]);
    let names = Names {
        registry: &registry,
        approval: Some(&approval),
    };
    let plan: PlanId = "01M9P000000000000000000PA1".parse().unwrap();
    let pl = |tx: &str, target: &str, run: &str| PlanLine {
        plan,
        tx: tx.parse().unwrap(),
        target: Some(key(target)),
        targets: Vec::new(),
        kind: crate::model::TxKind::Update,
        run: Some(run.parse().unwrap()),
        result: TxResult::Committed,
        at: at(LATER),
    };
    let actions = |lines: &[PlanLine]| -> Vec<(Action, Vec<String>)> {
        undo_hints(lines, &names)
            .iter()
            .map(|c| (c.action(), c.argv().to_vec()))
            .collect()
    };
    let run = "01JB8Q2VB00000000000000001";
    let (retail_tx, classic_tx) = ("01JB8Q2VJ00000000000000001", "01JB8Q2VJ00000000000000002");
    let undo = |tx: &str| {
        (
            Action::Undo,
            ["omaddons", "undo", tx].map(str::to_owned).to_vec(),
        )
    };
    assert_eq!(
        actions(&[pl(retail_tx, RETAIL, run), pl(classic_tx, CLASSIC, run)]),
        [
            (
                Action::UndoRun,
                ["omaddons", "undo", "--run", run]
                    .map(str::to_owned)
                    .to_vec()
            ),
            undo(classic_tx),
            undo(retail_tx),
        ],
        "one run: its undo first, then each transaction by client"
    );
    assert_eq!(
        actions(&[pl(retail_tx, RETAIL, run)]),
        [undo(retail_tx)],
        "one"
    );
    assert_eq!(
        actions(&[
            pl(retail_tx, RETAIL, run),
            pl(classic_tx, CLASSIC, "01JB8Q2VB00000000000000002")
        ]),
        [undo(classic_tx), undo(retail_tx)],
        "two runs: no undo_run"
    );
}

// ---------------------------------------------------------------------------------------
// The human view.

fn human(outcome: &PlansShowOutcome, verbose: u8) -> String {
    let mut out = Vec::new();
    let style = Style {
        color: false,
        verbose,
        quiet: false,
    };
    outcome.render_in(&mut out, &style, &TimeZone::UTC).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn human_text_cases() {
    let (_dir, p) = paths();
    let plan = stored_plan(&p);
    let id = plan.plan_id.to_string();
    let display = shell(&plan.display_argv);

    let pending = human(&show_in(&p, &id, now()).unwrap(), 0);
    assert!(
        pending.starts_with(&format!(
            "plan {id} (remove): pending, stored until 2026-10-29 12:00, nothing applied yet\n"
        )),
        "{pending}"
    );
    assert!(
        pending.contains(&format!("approved: {display}\n  Remove Foo\n")),
        "{pending}"
    );
    assert!(
        pending.ends_with(&format!("apply it with: omaddons apply {id}\n")),
        "{pending}"
    );

    let expired = human(&show_in(&p, &id, at("2026-10-29T13:00:00Z")).unwrap(), 0);
    assert!(
        expired.contains(": expired at 2026-10-29 12:00, nothing applied\n"),
        "{expired}"
    );
    assert!(
        expired.ends_with(&format!(
            "preview again: {}\n",
            preview_again(&plan.display_argv)
        )),
        "{expired}"
    );

    write_history(
        &p,
        &[line(
            &id,
            "01JB8Q2VJ00000000000000001",
            CLASSIC,
            "01JB8Q2VB00000000000000001",
            LATER,
        )],
    );
    let partial = human(&show_in(&p, &id, now()).unwrap(), 0);
    for want in [
        ": partial, 1 of 2 clients committed\n",
        "committed:\n  classic: tx 01JB8Q2VJ0000000… at 2026-10-28 12:30 · undo: omaddons undo 01JB8Q2VJ00000000000000001\n",
        "not recorded: retail\n",
        "apply never resumes a partial plan; preview again for the rest: ",
    ] {
        assert!(partial.contains(want), "{want:?} in:\n{partial}");
    }

    let verbose = human(&show_in(&p, &id, now()).unwrap(), 1);
    for want in [
        "reasons: none (routine)\n",
        "build: ",
        "(this build)\n",
        "history: read 1 lines back to 2026-10-28 12:00 (stopped at the start)\n",
    ] {
        assert!(verbose.contains(want), "{want:?} in:\n{verbose}");
    }
    assert!(!partial.contains("history: read"), "-v only");
}

#[test]
fn a_preview_again_keeps_every_positional() {
    let argv = [
        "omaddons", "backup", "prune", "-t", "retail", "--", "01M6Q2VA",
    ]
    .map(str::to_owned);
    assert_eq!(
        preview_again(&argv),
        "omaddons backup prune -t retail --dry-run -- 01M6Q2VA"
    );
    let argv = ["omaddons", "update", "-t", "retail"].map(str::to_owned);
    assert_eq!(preview_again(&argv), "omaddons update -t retail --dry-run");
}

#[test]
fn each_unknown_reason_is_said() {
    let (_dir, p) = paths();
    let plan = stored_plan(&p);
    let id = plan.plan_id.to_string();
    let mut outcome = show_in(&p, &id, now()).unwrap();
    for (why, want) in [
        (Why::FileGone, "its file is no longer in the cache"),
        (
            Why::Unreadable("bad".into()),
            "its file cannot be read (bad)",
        ),
        (Why::NoHistoryLine, "backup prune records no history line"),
        (Why::NoApprovedClients, "its approval names no client"),
        (
            Why::Disagrees,
            "history names a client the plan did not approve",
        ),
        (
            Why::ScanCut,
            "history holds more than 20000 lines since the plan was made; the search stopped there",
        ),
        (
            Why::UnreadableLines(2),
            "2 lines of history.jsonl in that time could not be read",
        ),
        (
            Why::Unfinished {
                tx: "01JB8Q2VJ00000000000000001".parse().unwrap(),
                client: "retail".into(),
            },
            "an interrupted transaction (01JB8Q2VJ00000000000000001) on retail may hold its commit",
        ),
    ] {
        outcome.data.state = PlanState::Unknown;
        outcome.view.why = Some(why.clone());
        let text = human(&outcome, 0);
        assert!(
            text.contains(&format!(": unknown: {want}")),
            "{why:?}: {text}"
        );
    }
}

#[test]
fn an_unreadable_file_is_reported_not_trusted() {
    let (_dir, p) = paths();
    let plan = stored_plan(&p);
    fs::write(p.plan(&plan.plan_id), b"{not a plan").unwrap();
    let got = show_in(&p, &plan.plan_id.to_string(), now()).unwrap();
    assert_eq!(
        (
            got.data.stored,
            got.data.readable,
            got.data.state,
            got.data.intent.is_none()
        ),
        (true, false, PlanState::Unknown, true)
    );
    assert!(matches!(got.view.why, Some(Why::Unreadable(ref m)) if !m.is_empty()));
}
