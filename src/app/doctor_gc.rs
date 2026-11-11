//! `omaddons doctor gc --before <WHEN> [--dry-run] [--if-match <token>] [--yes]` (M4-D36):
//! deletes old doctor runs and what they kept, by the retention plan
//! ([`crate::doctor::gc`]), never what an unfinished change needs. Its arguments are
//! checked before anything under `<state>` is read, so a wrong invocation changes and
//! reads nothing; a real run needs `--yes`, and binds to its preview with `--if-match`.

use std::collections::BTreeMap;
use std::io::Write;

use jiff::Timestamp;

use super::history::{parse_since, short_tx};
use crate::cli::Context;
use crate::doctor::exit::Exit;
use crate::doctor::gc::{self, DeletionKind, Disk, GcReport, GcRequest, GcSkipReason, GcSkipped};
use crate::error::{Error, ErrorCode, Suggestion};
use crate::limits::{DOCTOR_GC_SUGGESTED_AGE, DOCTOR_SCAN_MAX_ENTRIES};
use crate::output::{DoctorFailure, DoctorOutcome, Style};
use crate::state::paths::Paths;

/// `doctor gc`'s arguments (`--dry-run` and `--if-match` are global).
#[derive(Debug, Clone, clap::Args)]
pub struct GcArgs {
    /// Runs that started before this are eligible: an age or a time
    ///
    /// An age (30m, 12h, 30d, 2w) or an RFC 3339 time.
    #[arg(long, value_name = "WHEN", required = true)]
    pub before: String,
    /// Consent for a real run (not needed with --dry-run)
    #[arg(long)]
    pub yes: bool,
}

pub const GC_HELP: &str = "Examples:\n  omaddons doctor gc --before 30d --dry-run                 the retention plan: what would be deleted, what is kept and why, which undos are lost\n  omaddons doctor gc --before 30d --yes --if-match <token>  delete exactly what that preview listed";

/// What a `doctor gc` did, as the command renders it.
#[derive(Debug)]
pub struct GcRun {
    report: GcReport,
    verbose: bool,
    /// What a real run left of its plan, beside the report (exit 74).
    error: Option<Error>,
}

/// `--before`'s suggested value, [`DOCTOR_GC_SUGGESTED_AGE`] in whole days: `30d`.
pub fn suggested_age_arg() -> String {
    format!("{}d", DOCTOR_GC_SUGGESTED_AGE.as_secs() / 86_400)
}

/// Runs `doctor gc`: its arguments, then the retention plan and what it deletes.
pub fn run(ctx: &Context, args: &GcArgs) -> Result<GcRun, DoctorFailure> {
    let before = checked(ctx, args, crate::clock::now())?;
    let paths = Paths::from_env()?;
    let world = Disk {
        paths: &paths,
        actor: ctx.actor,
    };
    let report = gc::gc(
        &world,
        &GcRequest {
            before,
            dry_run: ctx.dry_run,
            if_match: ctx.if_match.as_deref(),
            argv: &ctx.argv,
        },
    )?;
    Ok(GcRun {
        error: left_behind(&report),
        report,
        verbose: ctx.output.style.detail(),
    })
}

/// The failure of a real run that left planned deletions where they were or in trash:
/// an `io` error beside the report, whose `skipped` names each with why; `None` when
/// nothing was left (a deletion already gone left nothing).
fn left_behind(r: &GcReport) -> Option<Error> {
    let left: Vec<&GcSkipped> = r
        .skipped
        .iter()
        .filter(|s| s.reason != GcSkipReason::Gone)
        .collect();
    let first = left.first()?;
    let more = match left.len() {
        1 => String::new(),
        n => format!(" and {} more", n - 1),
    };
    Some(
        Error::new(
            ErrorCode::Io,
            format!(
                "doctor gc deleted {} of its {} planned deletions, not {}{more}: {}",
                r.deletions.len(),
                r.deletions.len() + r.skipped.len(),
                first.path,
                first.message
            ),
            Suggestion::text(format!(
                "skipped lists {} and the rest left with why: what waits in trash goes with a later prune that sweeps it, and what stays where it was is planned again by the next doctor gc",
                first.path
            )),
        )
        .details(serde_json::json!({ "path": first.path })),
    )
}

/// The cutoff `--before` names, once every argument is checked: each refusal a usage
/// error (`details.flag`) whose fix is the preview with every argument corrected, the
/// user's verb kept, before anything is read.
fn checked(ctx: &Context, args: &GcArgs, now: Timestamp) -> Result<Timestamp, Error> {
    checked_args(&ctx.argv, args, ctx.target.is_some(), ctx.dry_run, now)
}

/// [`checked`] of the arguments alone: the command line, whether `-t` was given, and
/// whether it is a dry run.
fn checked_args(
    argv: &[String],
    args: &GcArgs,
    target: bool,
    dry_run: bool,
    now: Timestamp,
) -> Result<Timestamp, Error> {
    let Some(before) = parse_since(&args.before, now) else {
        return Err(refused(
            &format!(
                "--before takes an RFC 3339 time or an age (30m, 12h, 30d, 2w), not \"{}\"",
                args.before
            ),
            "--before",
            preview_fix(argv, Some(&suggested_age_arg())),
        ));
    };
    if before > now {
        return Err(refused(
            &format!(
                "--before {} is in the future: gc deletes only what started before a time that has passed",
                args.before
            ),
            "--before",
            preview_fix(argv, Some("0d")),
        ));
    }
    if target {
        return Err(refused(
            "doctor gc works on every client at once and takes no -t",
            "-t",
            preview_fix(argv, None),
        ));
    }
    if !dry_run && !args.yes {
        return Err(refused(
            "doctor gc deletes what it lists, and the undos that need it: a real run needs --yes (preview it first)",
            "--yes",
            preview_fix(argv, None),
        ));
    }
    Ok(before)
}

fn refused(message: &str, flag: &str, fix: Vec<String>) -> Error {
    Error::new(ErrorCode::Usage, message, Suggestion::command(fix))
        .details(serde_json::json!({ "flag": flag }))
        .user_verb()
}

/// The preview of `argv` with every argument a check refuses corrected: `--before` set to
/// `before` when given (added when absent), `-t` dropped, `--if-match` dropped, and
/// `--dry-run` added, presentation flags aside. `--yes` stays: a dry run accepts it.
pub(crate) fn preview_fix(argv: &[String], before: Option<&str>) -> Vec<String> {
    let mut fixed = crate::cli::without_target(argv)
        .unwrap_or_else(|| ["omaddons", "doctor", "gc"].map(str::to_owned).to_vec());
    if let Some(before) = before {
        fixed = crate::cli::fix_argv_with_value(&fixed, "--before", before).unwrap_or_else(|| {
            let mut with = fixed.clone();
            with.extend(["--before".to_owned(), before.to_owned()]);
            with
        });
    }
    super::precondition::preview_argv(&fixed)
}

impl DoctorOutcome for GcRun {
    type Report = GcReport;

    fn command(&self) -> &'static str {
        gc::COMMAND
    }

    fn report(&self) -> &GcReport {
        &self.report
    }

    fn exit(&self) -> Exit {
        if self.error.is_some() {
            Exit::IoError
        } else {
            Exit::Healthy
        }
    }

    fn error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    fn render_human(&self, w: &mut dyn Write, _style: &Style) -> std::io::Result<()> {
        render(&self.report, self.verbose, w)
    }
}

/// The human output: what is (or would be) deleted, what it costs, what is kept, where
/// `latest` goes, and the token or what remains.
fn render(r: &GcReport, verbose: bool, w: &mut dyn Write) -> std::io::Result<()> {
    let before = r.before_at.strftime("%Y-%m-%d %H:%M UTC");
    let kept = r.exclusions.len();
    if r.deletions.is_empty() && r.skipped.is_empty() {
        writeln!(w, "nothing to delete before {before} (kept {kept})")?;
    } else {
        let verb = if r.dry_run { "would delete" } else { "deleted" };
        if r.deletions.is_empty() {
            writeln!(w, "{verb} nothing, all started before {before}")?;
        } else {
            writeln!(
                w,
                "{verb} {} ({}), all started before {before}",
                what(r),
                size(r)
            )?;
        }
        for s in &r.skipped {
            writeln!(w, "  not deleted: {} ({})", s.path, s.message)?;
        }
        let only = r.deletions.iter().filter(|d| d.may_hold_only_copy).count();
        if only > 0 {
            writeln!(
                w,
                "  may hold an only copy: {only} (backups or quarantined files a repair kept)"
            )?;
        }
        if !r.lost_undos.is_empty() {
            let txs: Vec<String> = r.lost_undos.iter().map(short_tx).collect();
            writeln!(
                w,
                "  undo no longer possible: {}; `omaddons undo <tx>` still undoes the others",
                txs.join(", ")
            )?;
        }
        if verbose {
            for d in &r.deletions {
                let bytes = d.bytes.map_or_else(|| "size unknown".to_owned(), size_of);
                writeln!(w, "    {}  {}  {bytes}", kind_name(d.kind), d.path)?;
            }
        }
        if kept > 0 {
            writeln!(w, "kept {kept} (pinned): {}", pins(r))?;
        }
    }
    if verbose {
        for e in &r.exclusions {
            let pin = serde_json::to_value(e.pin).unwrap_or_default();
            writeln!(w, "    {}  {}", pin.as_str().unwrap_or_default(), e.path)?;
        }
    }
    if let Some(latest) = &r.latest {
        let deleted = latest.from.as_deref().is_some_and(|from| {
            r.deletions
                .iter()
                .any(|d| d.kind == DeletionKind::Run && d.run_id.as_deref() == Some(from))
        });
        let why = if deleted {
            "it named a run this deletes"
        } else {
            "it led to no run"
        };
        match &latest.to {
            Some(to) => writeln!(w, "latest → {to} ({why})")?,
            None => writeln!(w, "latest removed ({why}, and no finished run is left)")?,
        }
    }
    match r.remaining {
        Some(0) => {}
        Some(n) => writeln!(w, "{n} more remain eligible: run doctor gc again")?,
        None => writeln!(
            w,
            "more remain (a listing stopped at {DOCTOR_SCAN_MAX_ENTRIES} entries): run doctor gc again"
        )?,
    }
    if let Some(token) = &r.precondition {
        writeln!(
            w,
            "precondition {token} (delete exactly this: --yes --if-match {token})"
        )?;
    }
    Ok(())
}

/// "14 run directories, 2 client halves (.repair) and 1 cache quarantine".
fn what(r: &GcReport) -> String {
    let count = |kind| r.deletions.iter().filter(|d| d.kind == kind).count();
    let parts: Vec<String> = [
        (DeletionKind::Run, "run directory", "run directories"),
        (
            DeletionKind::ClientRepair,
            "client half (.repair)",
            "client halves (.repair)",
        ),
        (
            DeletionKind::CacheQuarantine,
            "cache quarantine",
            "cache quarantines",
        ),
    ]
    .into_iter()
    .filter_map(|(kind, one, many)| match count(kind) {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    })
    .collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The deletions' total size, or `size unknown` when a walk stopped at its bound.
fn size(r: &GcReport) -> String {
    r.deletions
        .iter()
        .map(|d| d.bytes)
        .sum::<Option<u64>>()
        .map_or_else(|| "size unknown".to_owned(), size_of)
}

/// Bytes in binary units: `7 B`, `3.0 KiB`, `18.4 MiB`.
fn size_of(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// "1 active_undo, 1 unfinished_journal": each pin's count, strongest first.
fn pins(r: &GcReport) -> String {
    let mut counts: BTreeMap<gc::Pin, usize> = BTreeMap::new();
    for e in &r.exclusions {
        *counts.entry(e.pin).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(pin, n)| {
            let name = serde_json::to_value(pin).unwrap_or_default();
            format!("{n} {}", name.as_str().unwrap_or_default())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn kind_name(kind: DeletionKind) -> &'static str {
    match kind {
        DeletionKind::Run => "run",
        DeletionKind::ClientRepair => "client_repair",
        DeletionKind::CacheQuarantine => "cache_quarantine",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::finding::FindingTarget;
    use crate::doctor::gc::{ExcludedKind, GcDeletion, GcExclusion, LatestChange, Pin};
    use crate::error::UsageFix;
    use crate::model::{TargetKey, TxId};

    const NOW: &str = "2026-10-30T18:03:00Z";

    fn argv(args: &[&str]) -> Vec<String> {
        ["omaddons"]
            .iter()
            .chain(args)
            .map(|a| (*a).to_owned())
            .collect()
    }

    fn args(before: &str, yes: bool) -> GcArgs {
        GcArgs {
            before: before.to_owned(),
            yes,
        }
    }

    #[test]
    fn suggested_age_arg_is_30d() {
        let now: Timestamp = NOW.parse().unwrap();
        assert_eq!(suggested_age_arg(), "30d");
        assert_eq!(
            parse_since(&suggested_age_arg(), now),
            now.checked_sub(jiff::SignedDuration::from_hours(24 * 30))
                .ok()
        );
    }

    #[test]
    fn gc_argument_checks() {
        let now: Timestamp = NOW.parse().unwrap();
        let fix = |words: &[&str]| -> Vec<String> { argv(words) };
        /// A case: its name, its argv, `--before`, `--yes`, whether `-t` was given,
        /// whether it is a dry run, and the refusal it expects (its `details.flag` and
        /// fix), if any.
        type Case<'a> = (
            &'a str,
            &'a [&'a str],
            &'a str,
            bool,
            bool,
            bool,
            Option<(&'a str, Vec<String>)>,
        );
        let cases: [Case<'_>; 9] = [
            (
                "a_preview",
                &["doctor", "gc", "--before", "30d", "--dry-run"],
                "30d",
                false,
                false,
                true,
                None,
            ),
            (
                "a_preview_with_yes",
                &["doctor", "gc", "--before", "30d", "--yes", "--dry-run"],
                "30d",
                true,
                false,
                true,
                None,
            ),
            (
                "a_consented_run",
                &["doctor", "gc", "--before", "30d", "--yes"],
                "30d",
                true,
                false,
                false,
                None,
            ),
            (
                "an_unreadable_before",
                &["doctor", "gc", "--before", "yesterday", "--dry-run"],
                "yesterday",
                false,
                false,
                true,
                Some((
                    "--before",
                    fix(&["doctor", "gc", "--before", "30d", "--dry-run"]),
                )),
            ),
            (
                "a_future_before",
                &["doctor", "gc", "--before=2099-01-01T00:00:00Z", "--yes"],
                "2099-01-01T00:00:00Z",
                true,
                false,
                false,
                Some((
                    "--before",
                    fix(&["doctor", "gc", "--before=0d", "--yes", "--dry-run"]),
                )),
            ),
            (
                "a_target",
                &[
                    "doctor",
                    "gc",
                    "--before",
                    "30d",
                    "-t",
                    "retail",
                    "--dry-run",
                ],
                "30d",
                false,
                true,
                true,
                Some(("-t", fix(&["doctor", "gc", "--before", "30d", "--dry-run"]))),
            ),
            (
                "a_flavor",
                &[
                    "doctor", "gc", "--flavor", "retail", "--before", "30d", "--yes",
                ],
                "30d",
                true,
                true,
                false,
                Some((
                    "-t",
                    fix(&["doctor", "gc", "--before", "30d", "--yes", "--dry-run"]),
                )),
            ),
            (
                "a_real_run_without_yes",
                &["doctor", "gc", "--before", "30d"],
                "30d",
                false,
                false,
                false,
                Some((
                    "--yes",
                    fix(&["doctor", "gc", "--before", "30d", "--dry-run"]),
                )),
            ),
            (
                "a_bound_run_without_yes",
                &[
                    "doctor",
                    "gc",
                    "--before",
                    "30d",
                    "--if-match",
                    "pc1:x",
                    "--json",
                ],
                "30d",
                false,
                false,
                false,
                Some((
                    "--yes",
                    fix(&["doctor", "gc", "--before", "30d", "--dry-run"]),
                )),
            ),
        ];
        for (case, words, before, yes, target, dry_run, want) in cases {
            let got = checked_args(&argv(words), &args(before, yes), target, dry_run, now);
            // Each refusal as (code, flag, fix, kept verb); `None` for an accepted line.
            let refusal = got.err().map(|error| {
                (
                    error.code,
                    error.details["flag"].as_str().map(str::to_owned),
                    error.fix.argv().map(<[String]>::to_vec),
                    error.usage_fix == UsageFix::UserVerb,
                )
            });
            let expected =
                want.map(|(flag, fix)| (ErrorCode::Usage, Some(flag.to_owned()), Some(fix), true));
            assert_eq!(refusal, expected, "{case}");
        }
    }

    #[test]
    fn a_missing_before_is_fixed_with_the_suggested_age() {
        use clap::Parser;
        for (case, words, fix) in [
            (
                "bare",
                &["doctor", "gc"][..],
                &["doctor", "gc", "--before", "30d", "--dry-run"][..],
            ),
            (
                "with_yes_and_a_target",
                &["doctor", "gc", "--yes", "-t", "retail"],
                &["doctor", "gc", "--yes", "--before", "30d", "--dry-run"],
            ),
        ] {
            let line = argv(words);
            let err = crate::cli::Cli::try_parse_from(&line).unwrap_err();
            let error = crate::cli::usage_error(&err, &line);
            assert_eq!(error.code, ErrorCode::Usage, "{case}: {error:?}");
            assert_eq!(error.details["flag"], "--before", "{case}: {error:?}");
            assert_eq!(error.fix.argv(), Some(argv(fix).as_slice()), "{case}");
            assert_eq!(error.usage_fix, UsageFix::UserVerb, "{case}");
        }
    }

    fn tx(n: u128) -> TxId {
        TxId::from_ulid(ulid::Ulid::from_parts(1_700_000_000_000, n))
    }

    fn deletion(kind: DeletionKind, path: &str, bytes: Option<u64>, only: bool) -> GcDeletion {
        GcDeletion {
            kind,
            run_id: Some("R1".to_owned()),
            target: (kind == DeletionKind::ClientRepair).then(|| FindingTarget {
                id: "retail".to_owned(),
                key: TargetKey::from_ulid(ulid::Ulid::from_parts(1, 1)),
            }),
            tx: Some(tx(1)),
            path: path.to_owned(),
            bytes,
            may_hold_only_copy: only,
        }
    }

    fn exclusion(path: &str, pin: Pin) -> GcExclusion {
        GcExclusion {
            kind: ExcludedKind::Backup,
            run_id: Some("R3".to_owned()),
            path: path.to_owned(),
            pin,
        }
    }

    fn report() -> GcReport {
        GcReport {
            dry_run: true,
            before_at: "2026-09-30T18:03:00Z".parse().unwrap(),
            deletions: vec![
                deletion(
                    DeletionKind::CacheQuarantine,
                    "/k/q/R1",
                    Some(16_777_216),
                    false,
                ),
                deletion(
                    DeletionKind::ClientRepair,
                    "/c/s/T.repair",
                    Some(2_097_152),
                    true,
                ),
                deletion(DeletionKind::Run, "/s/runs/R1", Some(419_430), true),
                deletion(DeletionKind::Run, "/s/runs/R0", Some(0), false),
            ],
            skipped: Vec::new(),
            exclusions: vec![
                exclusion("/s/runs/R3/backups/1", Pin::ActiveUndo),
                exclusion("/s/runs/R4", Pin::UnfinishedJournal),
            ],
            lost_undos: vec![tx(1)],
            latest: Some(LatestChange {
                from: Some("R1".to_owned()),
                to: Some("R2".to_owned()),
            }),
            remaining: Some(0),
            precondition: Some("pc1:abc".to_owned()),
        }
    }

    fn skipped(path: &str, reason: GcSkipReason, message: &str) -> GcSkipped {
        GcSkipped {
            kind: DeletionKind::Run,
            run_id: Some("R2".to_owned()),
            path: path.to_owned(),
            reason,
            message: message.to_owned(),
        }
    }

    /// A real run that left planned deletions fails beside its report: exit 74, an `io`
    /// error naming the first one left and how many more; one already gone left nothing
    /// (omad-4h0d.13).
    #[test]
    fn a_run_that_left_deletions_exits_74_beside_its_report() {
        let gone = skipped("/s/runs/R5", GcSkipReason::Gone, "already gone");
        let held = skipped(
            "/s/runs/R6",
            GcSkipReason::Held,
            "held by a live transaction",
        );
        let busy = skipped(
            "/s/runs/R2",
            GcSkipReason::LeftInTrash,
            "left in trash: Device or resource busy (os error 16)",
        );
        let real = GcReport {
            dry_run: false,
            precondition: None,
            ..report()
        };
        for (case, skipped, error) in [
            ("nothing_skipped", Vec::new(), None),
            ("only_gone", vec![gone.clone()], None),
            (
                "one_left",
                vec![gone.clone(), busy.clone()],
                Some((
                    "doctor gc deleted 4 of its 6 planned deletions, not /s/runs/R2: left in trash: Device or resource busy (os error 16)",
                    "/s/runs/R2",
                )),
            ),
            (
                "two_left",
                vec![busy, gone, held],
                Some((
                    "doctor gc deleted 4 of its 7 planned deletions, not /s/runs/R2 and 1 more: left in trash: Device or resource busy (os error 16)",
                    "/s/runs/R2",
                )),
            ),
        ] {
            let run = GcRun {
                error: left_behind(&GcReport {
                    skipped,
                    ..real.clone()
                }),
                report: real.clone(),
                verbose: false,
            };
            match error {
                None => {
                    assert!(run.error().is_none(), "{case}: {:?}", run.error());
                    assert_eq!(run.exit(), Exit::Healthy, "{case}");
                }
                Some((message, path)) => {
                    let err = run.error().unwrap_or_else(|| panic!("{case}: no error"));
                    assert_eq!(err.code, ErrorCode::Io, "{case}");
                    assert_eq!(err.message, message, "{case}");
                    assert_eq!(err.details["path"], path, "{case}");
                    assert_eq!(err.fix.argv(), None, "{case}: a prose fix");
                    let fix = err.fix.display();
                    assert!(fix.contains(path), "{case}: {fix}");
                    assert_eq!(run.exit(), Exit::IoError, "{case}");
                    assert_eq!(run.exit().code(), 74, "{case}");
                }
            }
        }
    }

    fn rendered(r: &GcReport, verbose: bool) -> String {
        let mut out = Vec::new();
        render(r, verbose, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn gc_human_rendering() {
        let short = short_tx(&tx(1));
        let before = "2026-09-30 18:03 UTC";
        let real = GcReport {
            dry_run: false,
            deletions: vec![deletion(DeletionKind::Run, "/s/runs/R1", Some(7), false)],
            exclusions: Vec::new(),
            lost_undos: Vec::new(),
            latest: None,
            precondition: None,
            ..report()
        };
        let nothing = GcReport {
            deletions: Vec::new(),
            lost_undos: Vec::new(),
            latest: None,
            exclusions: vec![exclusion("/s/runs/R4", Pin::ActiveRun)],
            precondition: Some("pc1:abc".to_owned()),
            ..report()
        };
        let busy = skipped(
            "/s/runs/R2",
            GcSkipReason::LeftInTrash,
            "left in trash: Device or resource busy (os error 16)",
        );
        let cases: [(&str, GcReport, bool, String); 9] = [
            (
                "a_real_run_that_left_one",
                GcReport {
                    skipped: vec![busy.clone()],
                    ..real.clone()
                },
                false,
                format!(
                    "deleted 1 run directory (7 B), all started before {before}\n  not deleted: /s/runs/R2 (left in trash: Device or resource busy (os error 16))\n"
                ),
            ),
            (
                "a_real_run_that_deleted_nothing",
                GcReport {
                    deletions: Vec::new(),
                    skipped: vec![busy],
                    ..real.clone()
                },
                false,
                format!(
                    "deleted nothing, all started before {before}\n  not deleted: /s/runs/R2 (left in trash: Device or resource busy (os error 16))\n"
                ),
            ),
            (
                "a_dry_run",
                report(),
                false,
                format!(
                    "would delete 2 run directories, 1 client half (.repair) and 1 cache quarantine (18.4 MiB), all started before {before}\n  may hold an only copy: 2 (backups or quarantined files a repair kept)\n  undo no longer possible: {short}; `omaddons undo <tx>` still undoes the others\nkept 2 (pinned): 1 unfinished_journal, 1 active_undo\nlatest → R2 (it named a run this deletes)\nprecondition pc1:abc (delete exactly this: --yes --if-match pc1:abc)\n"
                ),
            ),
            (
                "a_real_run",
                real.clone(),
                false,
                format!("deleted 1 run directory (7 B), all started before {before}\n"),
            ),
            (
                "nothing_to_delete",
                nothing,
                false,
                format!(
                    "nothing to delete before {before} (kept 1)\nprecondition pc1:abc (delete exactly this: --yes --if-match pc1:abc)\n"
                ),
            ),
            (
                "remaining",
                GcReport {
                    remaining: Some(5),
                    ..real.clone()
                },
                false,
                format!(
                    "deleted 1 run directory (7 B), all started before {before}\n5 more remain eligible: run doctor gc again\n"
                ),
            ),
            (
                "remaining_unknown",
                GcReport {
                    remaining: None,
                    ..real.clone()
                },
                false,
                format!(
                    "deleted 1 run directory (7 B), all started before {before}\nmore remain (a listing stopped at {DOCTOR_SCAN_MAX_ENTRIES} entries): run doctor gc again\n"
                ),
            ),
            (
                "a_size_unknown_and_latest_removed",
                GcReport {
                    deletions: vec![
                        deletion(DeletionKind::Run, "/s/runs/R1", None, false),
                        deletion(DeletionKind::Run, "/s/runs/R0", Some(3), false),
                    ],
                    latest: Some(LatestChange {
                        from: Some("R9".to_owned()),
                        to: None,
                    }),
                    ..real.clone()
                },
                false,
                format!(
                    "deleted 2 run directories (size unknown), all started before {before}\nlatest removed (it led to no run, and no finished run is left)\n"
                ),
            ),
            (
                "verbose",
                GcReport {
                    exclusions: vec![exclusion("/s/runs/R4", Pin::ActiveRun)],
                    ..real
                },
                true,
                format!(
                    "deleted 1 run directory (7 B), all started before {before}\n    run  /s/runs/R1  7 B\nkept 1 (pinned): 1 active_run\n    active_run  /s/runs/R4\n"
                ),
            ),
        ];
        for (case, r, verbose, want) in cases {
            assert_eq!(rendered(&r, verbose), want, "{case}");
        }
    }

    #[test]
    fn sizes_are_binary() {
        for (bytes, want) in [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1.0 KiB"),
            (19_293_798, "18.4 MiB"),
            (3 * 1024 * 1024 * 1024, "3.0 GiB"),
        ] {
            assert_eq!(size_of(bytes), want, "{bytes}");
        }
    }
}
