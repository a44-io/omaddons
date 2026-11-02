//! What form each code's fix takes (`robot-docs errors`, the guide): one phrase per code,
//! declared once beside the codes (M4-D2), true of every construction site. An exhaustive
//! match: a new code does not compile until its pattern is written.

use super::{ErrorCode, WarningCode};

impl ErrorCode {
    /// The form every instance's fix takes. `prose: ` when no site gives a command,
    /// `none: ` when there is no fix; values are `<placeholders>`, and "the same command"
    /// is the command that failed.
    pub const fn fix_pattern(self) -> &'static str {
        match self {
            Self::Usage => {
                "the corrected command line or the closest command, else `omaddons help <command>`, or prose (a template, `HOME`, an id)"
            }
            Self::RefInvalid => {
                "the same command with an `https://` ref, `omaddons config set git_hosts <hosts>` or `omaddons help install`, or prose"
            }
            Self::DesiredStateInvalid => {
                "`omaddons targets` or the same command with `--overwrite`, else prose: fix a line of `addons.toml`, or upgrade omaddons"
            }
            Self::TargetNotFound => {
                "`omaddons targets` (`--rescan`) or the same command with `-t <id>`, or prose: `omaddons targets add <path>`"
            }
            Self::TargetAmbiguous => {
                "the same command with `-t <candidate>` (`--from`/`--to` for `copy`), one alternative per other candidate"
            }
            Self::RefAmbiguous => {
                "the same command naming one candidate exactly, one alternative per candidate, or a listing to choose from"
            }
            Self::NotFound => {
                "a listing to choose from, `omaddons search <name>`, a corrected `omaddons install <ref>`, its help, or prose"
            }
            Self::NotManaged => {
                "`omaddons list -t <id>`, `omaddons install <name>` or `omaddons adopt <folder>`, or prose for a protected or linked one"
            }
            Self::PlanNotFound => {
                "`omaddons help apply`, or prose: preview the change again with its command's `--dry-run`"
            }
            Self::NoCompatibleRelease => {
                "`omaddons install <ref>` (other channel, version or `git:` ref), `omaddons targets`, or prose: pin or use a local file"
            }
            Self::GameRunning => {
                "the same command with `--while-running` for a policy refusal, else prose: quit the game first"
            }
            Self::ProtectedFolder => {
                "`omaddons list --protected` or `omaddons history`, or prose: `Blizzard_*` folders are never changed"
            }
            Self::OwnershipConflict => {
                "the same command with `--force` or on the owner's client, `omaddons list -t <id>`, or prose: choose one addon"
            }
            Self::UnmanagedFolder => {
                "`omaddons adopt <folder> --ref <ref> -t <id>`, the same command with `--force`, `omaddons list -t <id>`, or prose"
            }
            Self::CaseCollision => {
                "`omaddons remove <owner>`, or prose: remove or rename one of the case variants"
            }
            Self::WrongFlavor => {
                "`omaddons list -t <id> --unmanaged`, the same command with `-t <id>` or `--force`, or prose: use a build for this client"
            }
            Self::UnsafeArchive => {
                "prose: do not install the archive and report it to its author, or use the backup export file unmodified"
            }
            Self::ArchiveTooLarge => {
                "prose: raise the limit with `omaddons config set <key> <value>`, else change the input to fit the fixed bound"
            }
            Self::UndoConflict => {
                "`omaddons undo <tx>` of the later change or with `--saved-variables restore`, or prose: move a folder, or upgrade"
            }
            Self::HostBlocked => {
                "`omaddons config set git_hosts <hosts>` for a git host, else prose: download the file yourself or use another ref"
            }
            Self::ScopeDenied => {
                "`omaddons status`, or prose: move the path aside, or copy or export by hand to a folder of your own"
            }
            Self::StateTooNew => {
                "prose: upgrade omaddons, or finish the change with the newer omaddons that started it"
            }
            Self::FilesystemUnsupported => {
                "prose: copy what you need from `details.backup_dir` by hand, with the game closed"
            }
            Self::FileExists => "prose: choose another file name, or delete that file yourself",
            Self::Locked => "the same command again, once the other omaddons process has finished",
            Self::CheckFailed => {
                "`omaddons update`, `omaddons errors -t <id>` or `status`'s first fix, else prose pointing at the failed items in `data`"
            }
            Self::Network => "prose: check the network connection, or retry later",
            Self::RateLimited => {
                "prose: retry after `details.reset_at`, or store a GitHub token with `omaddons auth github set`"
            }
            Self::DownloadFailed => {
                "prose: what to change in the ref or the repository, then retry"
            }
            Self::HttpStatus => "prose: retry later, and check the source if it persists",
            Self::HashMismatch => {
                "`omaddons backup verify <id>`, `omaddons list -t <id> --unmanaged`, `omaddons doctor --include-deep`, else prose: retry"
            }
            Self::PlanStale => {
                "the same command again after re-planning, or the same command with `--dry-run` for `--if-match`"
            }
            Self::Offline => {
                "the same command without `--offline`, else prose: run without `--offline` and with `OMADDONS_OFFLINE` unset"
            }
            Self::BadArchive => {
                "prose: retry later, report the archive or use a packaged release, or import the unmodified export again"
            }
            Self::DistributionDisabled => "none: no command raises it yet",
            Self::SourceDisabled => {
                "`omaddons config set <source key> true` or `omaddons help install`, or prose: install `git` or adopt another ref"
            }
            Self::AuthRequired => {
                "`omaddons auth github set`, or prose: fix the credential where it comes from (variable, file, `gh auth login`)"
            }
            Self::AuthRejected => {
                "`omaddons auth github clear` for a stored token, else prose: replace or unset the credential where it came from"
            }
            Self::StateCorrupt => {
                "`omaddons status`, `omaddons backup list` or `omaddons undo <tx> --saved-variables keep`, else prose naming the file"
            }
            Self::Io => {
                "`omaddons status`, or `omaddons backup list` for a backup, else prose naming the path to fix (space, permissions)"
            }
            Self::Internal => {
                "`omaddons status` (`omaddons status --cached` when `status` failed), else prose: report the bug or see the README"
            }
            Self::OnlineRequired => "the same command with `--online`",
            Self::DoctorRefused => {
                "the same command with what the refusal names (a larger `--budget`, `--force`), one alternative without it, or prose"
            }
        }
    }
}

impl WarningCode {
    /// The form every instance's fix takes, as for [`ErrorCode::fix_pattern`]; a mirrored
    /// item failure has its error's.
    pub const fn fix_pattern(self) -> &'static str {
        match self {
            Self::JournalRecovered => {
                "`omaddons history --limit 5` to see what was finished or rolled back"
            }
            Self::DelegatedRecovery => {
                "`omaddons undo <tx>` for a transaction recovery completed, `omaddons history --limit 5` for one it rolled back"
            }
            Self::JournalPending => {
                "`omaddons status`, or prose saying what to do when this omaddons cannot recover it"
            }
            Self::TxInProgress => "prose: wait for the running omaddons command to finish",
            Self::AtomicSwapUnavailable => {
                "prose: nothing to do; the journal (or, for the widget, the next install) keeps the swap crash-safe"
            }
            Self::ReloadRequired => "prose: `/reload` in game, or restart the game client",
            Self::SavedVariablesDeferred => {
                "`omaddons undo <tx> --saved-variables restore` once the game has exited"
            }
            Self::SavedVariablesOutOfScope => {
                "prose: keep `WTF/` inside the client under one spelling; after a backup restore, copy the files by hand"
            }
            Self::RunningUnknown => {
                "prose: quit the game and add `--while-running` to change folders until `/proc` can be read"
            }
            Self::SnapshotCapExceeded => {
                "prose: raise `limits.snapshot_max_bytes` in `config.toml`, or wait 24 h"
            }
            Self::RetentionFailed => {
                "`omaddons backup prune -t <id>` after a backup or a restore, `omaddons status` after a committed change"
            }
            Self::OutOfDate => {
                "prose: update the addon, or enable \"Load out of date AddOns\" in the client"
            }
            Self::FolderLeftOut
            | Self::AnyFlavor
            | Self::OlderReleaseForClient
            | Self::TocCommaOnLegacy
            | Self::StateVersionOlder
            | Self::ExportLocalSource
            | Self::DownloadUnverified
            | Self::BackupCopied
            | Self::BeforeRestoreSkipped => "none: this warning carries no fix",
            Self::MissingDependency => {
                "`omaddons install <ref> -t <id>` (or `--reinstall` its owner), `omaddons search <folder>`, a listing, or no `--no-deps`"
            }
            Self::ClientNotLaunchedSincePatch => "prose: launch the client once",
            Self::WidgetOutdated => "`omaddons omarchy install`",
            Self::StateVersionNewer => "prose: upgrade omaddons",
            Self::ConfigUnknownKey => {
                "`omaddons config` to list the keys; the message names the closest when there is one"
            }
            Self::UnknownFlavor => {
                "prose: set the client's family with `omaddons config set targets.<id>.family <family>`"
            }
            Self::DiscoveryTruncated => "prose: add the client with `omaddons targets add <path>`",
            Self::TargetKeyDuplicate => "prose: the copy gets a new key on its first change",
            Self::CatalogOverrideInvalid => {
                "prose: fix or remove the `catalog.d` file the message names; none when the built-in catalog does not load"
            }
            Self::OwnershipTransferred => {
                "`omaddons list --verbose`, or `omaddons list -t <id> --verbose` when a same-name addon was taken over"
            }
            Self::CredentialFileMode => "prose: `chmod 600 <file>`",
            Self::AddonPinned => {
                "`omaddons unpin <name> -t <id>`; the message adds that `--version` moves the pin"
            }
            Self::StaleCheck => {
                "the same command without `--offline` when that flag alone made it offline, else prose: run it again online"
            }
            Self::DesiredUnknownKey => {
                "prose: remove or correct the unknown key on the line the message names"
            }
            Self::DesiredUnknownTarget => "`omaddons targets` to list the client ids",
            Self::AdoptNameChanged => "`omaddons list -t <id>`",
            Self::ExportMissingTarget => "`omaddons targets`",
            Self::LocalChangesReplaced => {
                "`omaddons undo <tx>`, or under `--dry-run` prose: the snapshot keeps the replaced files"
            }
            Self::SnapshotOnlyCopy => {
                "`omaddons undo <tx>` while retention keeps the snapshot, or under `--dry-run` prose: it will hold the only copy"
            }
            Self::NotLocked => {
                "prose: add `version = \"…\"` to those entries, or regenerate the file with `omaddons export`"
            }
            Self::StillRequired => {
                "`omaddons undo <tx>`, or under `--dry-run` prose: undo restores it"
            }
            Self::OrphanedDependency => "`omaddons remove <names…> -t <id>`",
            Self::BridgeDependency => {
                "`omaddons install <name or ref> -t <id>`, or `omaddons search <folder>`"
            }
            Self::TargetAddonsShared => {
                "`omaddons status -t <other>`, or prose: manage those addons from one client"
            }
            Self::HistoryLinesSkipped => "prose: omaddons never rewrites `history.jsonl`",
            Self::HistoryNotRecorded => "prose: check that the state directory is writable",
            Self::BackupUnsettled => "prose: make another backup after the game exits",
            Self::BackupSpaceLow => {
                "prose: free space or lower `limits.backup_min_free_mb`; over the copy budget, raise `limits.backup_copy_max_mb`"
            }
            Self::ErrorsPartial => "`omaddons errors -t <id>` to read the files again",
            Self::RestoreRolledBack => {
                "`omaddons backup restore <backup> -t <id>`, plus `--addon <folder>` per SavedVariables folder"
            }
            Self::AutoBackupFailed => "`omaddons backup create -t <id>`",
            Self::OmarchyShellUnreachable => {
                "`omaddons omarchy install` from the desktop session; after a remove, prose: `omarchy plugin disable <id>` there"
            }
            Self::TargetIdReserved => {
                "prose: rename the client with `omaddons targets rename all <new id>`"
            }
            Self::PlanNotStored => {
                "prose: stored plans are kept in omaddons' cache directory, which is safe to empty"
            }
            Self::Item(code) => code.fix_pattern(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::source_scan::{Literals, production_code, sources};

    /// A style rule for a phrase, not a runtime limit.
    const FIX_PATTERN_MAX_CHARS: usize = 120;

    /// Rules 1–6 of a fix pattern: one line, a phrase, commands by their canonical path,
    /// `prose: ` or `none: ` as a prefix, no milestone.
    fn check_fix_pattern(text: &str) -> Result<(), &'static str> {
        if text.chars().count() > FIX_PATTERN_MAX_CHARS {
            return Err("longer than 120 characters");
        }
        if text.contains(['\n', '\t']) {
            return Err("a tab or a newline");
        }
        if text.starts_with(' ') || text.ends_with(' ') || text.contains("  ") {
            return Err("a leading, trailing or doubled space");
        }
        if !text.starts_with(|c: char| c.is_ascii_lowercase() || c == '`') {
            return Err("it starts with neither a lowercase letter nor a backtick");
        }
        if text.ends_with('.') {
            return Err("a final period");
        }
        for milestone in ["M1", "M2", "M3", "M4", "M5"] {
            if text
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word == milestone)
            {
                return Err("it names a milestone");
            }
        }
        if text.starts_with("none:") && !text.starts_with("none: ") {
            return Err("`none:` without its space");
        }
        if text.starts_with("prose:") && !text.starts_with("prose: ") {
            return Err("`prose:` without its space");
        }
        for (index, span) in text.split('`').enumerate() {
            let Some(rest) = span.strip_prefix("omaddons ").filter(|_| index % 2 == 1) else {
                continue;
            };
            if rest.starts_with('-') {
                continue;
            }
            let words: Vec<&str> = rest.split_whitespace().collect();
            let names_a_path = (1..=words.len()).any(|n| {
                let path = words[..n].join(" ");
                crate::app::COMMANDS.iter().any(|entry| entry.name == path)
            });
            if !names_a_path {
                return Err("a backticked `omaddons …` that names no command path");
            }
        }
        Ok(())
    }

    #[test]
    fn every_fix_pattern_follows_the_style() {
        let warnings = WarningCode::ALL
            .iter()
            .map(|code| (code.wire(), code.fix_pattern()));
        for (wire, pattern) in ErrorCode::ALL
            .iter()
            .map(|code| (code.wire(), code.fix_pattern()))
            .chain(warnings)
        {
            assert_eq!(check_fix_pattern(pattern), Ok(()), "{wire}: {pattern:?}");
            assert!(
                pattern.match_indices("none: ").all(|(at, _)| at == 0),
                "{wire}: `none:` only as the prefix: {pattern:?}"
            );
        }
        assert_eq!(
            WarningCode::Item(ErrorCode::Locked).fix_pattern(),
            ErrorCode::Locked.fix_pattern(),
            "a mirrored item failure has its error's pattern"
        );
    }

    #[test]
    fn the_style_check_rejects_each_fault() {
        let cases = [
            ("too_long", "x".repeat(121), "longer"),
            ("newline", "a\nb".to_owned(), "newline"),
            ("tab", "a\tb".to_owned(), "tab"),
            ("doubled_space", "a  b".to_owned(), "doubled"),
            ("leading_space", " a".to_owned(), "leading"),
            ("capital", "The same command".to_owned(), "lowercase"),
            ("final_period", "the same command.".to_owned(), "period"),
            ("milestone", "arrives in M5".to_owned(), "milestone"),
            (
                "unknown_command",
                "`omaddons frobnicate <x>`".to_owned(),
                "no command path",
            ),
            ("none_without_space", "none:no fix".to_owned(), "`none:`"),
        ];
        for (name, text, phrase) in cases {
            let error = check_fix_pattern(&text).unwrap_err();
            assert!(error.contains(phrase), "case {name}: {text:?}: {error}");
        }
        for (name, text) in [
            ("a_flag", "`omaddons --version`"),
            ("a_nested_path", "`omaddons backup verify <id> -t <target>`"),
            (
                "prose_naming_omaddons",
                "prose: wait for the other omaddons process",
            ),
        ] {
            assert_eq!(check_fix_pattern(text), Ok(()), "case {name}: {text:?}");
        }
    }

    /// A code's construction sites in production code: where its variant is named.
    fn sites(code: ErrorCode) -> Vec<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let variant = format!("ErrorCode::{code:?}");
        let mut out = Vec::new();
        for (path, text) in sources(&root.join("src")) {
            let file = path.strip_prefix(root).unwrap().display().to_string();
            // The registry names every variant; it raises none.
            if file == "src/error.rs" || file.starts_with("src/error/") {
                continue;
            }
            let Some(production) = production_code(&text, Literals::Blank) else {
                continue;
            };
            for (at, _) in production.match_indices(&variant) {
                let after = production[at + variant.len()..].chars().next();
                if after.is_none_or(|c| !c.is_ascii_alphanumeric()) {
                    let line = production[..at].matches('\n').count() + 1;
                    out.push(format!("{file}:{line}"));
                }
            }
        }
        out
    }

    #[test]
    fn a_code_no_command_raises_has_no_site() {
        for code in ErrorCode::ALL {
            if code.fix_pattern() != "none: no command raises it yet" {
                continue;
            }
            let found = sites(*code);
            assert!(
                found.is_empty(),
                "`{code}` is raised now ({}): write its fix pattern, and its meaning, in this commit",
                found.join(", ")
            );
        }
        assert!(
            !sites(ErrorCode::PlanNotFound).is_empty(),
            "plan_not_found has its site in the plan store, so its pattern is written"
        );
    }
}
