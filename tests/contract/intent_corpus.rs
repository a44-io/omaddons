//! The run-the-fix corpus (M4-D15): common wrong invocations, each run on the synthetic
//! home `common::intent_home()`. The error, and the fix it suggests, are pinned. An
//! erroring wrong run must leave every record and client tree as it was and send no
//! request; its fix then runs on an independent restore of the same home and must reach
//! the exit the row states. Every suggestion obeys the verb rule: `help …`, a command
//! that only reads, or one of the commands the user may have meant.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;

use super::*;
use crate::common::home::Run;
use crate::common::intent::IntentHome;
use crate::common::intent_home;
use crate::common::netlog::net_log;
use crate::common::tlog::{Ran, TestLog, tlog};
use omaddons::cli::command_path;

/// One wrong (or surprising) invocation and what must follow it.
#[derive(Debug, Clone, Copy)]
pub struct Row {
    /// Without `omaddons`; `{name}` tokens take a setup step's captured value. The
    /// harness appends `--json --progress json`.
    pub argv: &'static [&'static str],
    pub kind: Kind,
    pub origin: Origin,
    /// The canonical commands the user may have meant; empty for a mistyped verb.
    pub verbs: &'static [&'static str],
    pub expect: Expect,
    /// Commands run on the home before the row, through the binary; the home is then
    /// snapshotted (`save_row`), so the wrong run and the fix see one state.
    pub setup: &'static [Step],
    /// What the row proves, in a line.
    pub note: &'static str,
    /// Checks of this row alone, run after its fix (an error or warning row).
    pub extra: Option<Extra>,
}

/// A row's own checks, given both runs and the requests each added.
pub type Extra = fn(&TestLog, &RowRuns<'_>);

/// What a row's [`Extra`] sees.
pub struct RowRuns<'a> {
    pub home: &'a IntentHome,
    pub wrong: &'a Ran,
    pub fix: &'a Ran,
    /// The `net.log` requests each run added, as `METHOD host path`.
    pub wrong_requests: &'a [String],
    pub fix_requests: &'a [String],
}

/// What the wrong run must give.
#[derive(Debug, Clone, Copy)]
pub enum Expect {
    /// An error with this exit and code; its suggestion `fix` runs to one of `fix_exit`.
    Error {
        exit: i32,
        code: &'static str,
        fix: Fix,
        fix_exit: &'static [i32],
    },
    /// Success: the run may change state, so nothing is hashed and nothing else runs.
    Ok,
    /// Success with a warning `code` whose `fix` runs to one of `fix_exit`.
    Warning {
        code: &'static str,
        fix: &'static [&'static str],
        fix_exit: &'static [i32],
    },
}

/// Which suggestion of an error the row pins and runs.
#[derive(Debug, Clone, Copy)]
pub enum Fix {
    /// `error.fix_argv`.
    Argv(&'static [&'static str]),
    /// `alternatives[0].argv`, for an error whose fix is prose (`fix_argv: null`).
    FirstAlternative(&'static [&'static str]),
}

impl Fix {
    pub const fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Argv(argv) | Self::FirstAlternative(argv) => argv,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    MisplacedBeforeVerb,
    MisplacedAfterVerb,
    ClientAsAddon,
    TargetAll,
    TargetTypo,
    CommandTypo,
    FlagTypo,
    ValueTypo,
    Argument,
    WarningFix,
    ApplyForAdopt,
    DoctorVerb,
}

impl Kind {
    /// The kind's name in case names and messages.
    pub const fn name(self) -> &'static str {
        match self {
            Self::MisplacedBeforeVerb => "misplaced_before_verb",
            Self::MisplacedAfterVerb => "misplaced_after_verb",
            Self::ClientAsAddon => "client_as_addon",
            Self::TargetAll => "target_all",
            Self::TargetTypo => "target_typo",
            Self::CommandTypo => "command_typo",
            Self::FlagTypo => "flag_typo",
            Self::ValueTypo => "value_typo",
            Self::Argument => "argument",
            Self::WarningFix => "warning_fix",
            Self::ApplyForAdopt => "apply_for_adopt",
            Self::DoctorVerb => "doctor_verb",
        }
    }
}

/// Where a row comes from: the plan's Appendix C.2, or a leaf's own surface.
#[derive(Debug, Clone, Copy)]
pub enum Origin {
    Plan,
    Leaf(&'static str),
}

/// A command run on the home before a row.
// The first rows with setup steps (the doctor's) arrive with the corpus's last part.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub argv: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    pub exit: i32,
    /// `(name, JSON pointer)`: the string at the pointer of the step's envelope, for the
    /// row's `{name}` tokens.
    pub capture: Option<(&'static str, &'static str)>,
}

/// A row of the plan's corpus with no setup.
const fn row(
    kind: Kind,
    argv: &'static [&'static str],
    verbs: &'static [&'static str],
    note: &'static str,
    expect: Expect,
) -> Row {
    Row {
        argv,
        kind,
        origin: Origin::Plan,
        verbs,
        expect,
        setup: &[],
        note,
        extra: None,
    }
}

/// An error row's expectation, its fix `error.fix_argv`.
const fn error(
    exit: i32,
    code: &'static str,
    fix: &'static [&'static str],
    fix_exit: &'static [i32],
) -> Expect {
    Expect::Error {
        exit,
        code,
        fix: Fix::Argv(fix),
        fix_exit,
    }
}

impl Row {
    /// The row with checks of its own, run after its fix.
    const fn then(self, extra: Extra) -> Self {
        Self {
            extra: Some(extra),
            ..self
        }
    }

    /// The row as a leaf's own case, not the plan's.
    const fn by_leaf(self, leaf: &'static str) -> Self {
        Self {
            origin: Origin::Leaf(leaf),
            ..self
        }
    }
}

use Kind::{
    ApplyForAdopt, Argument, ClientAsAddon, CommandTypo, DoctorVerb, FlagTypo, MisplacedAfterVerb,
    MisplacedBeforeVerb, TargetAll, TargetTypo, ValueTypo, WarningFix,
};

pub const CORPUS: &[Row] = &[
    // A client before the verb (M4-D12 hook 1).
    row(
        MisplacedBeforeVerb,
        &["retail", "update"],
        &["update"],
        "hook 1",
        error(2, "usage", &["omaddons", "update", "-t", "retail"], &[0]),
    ),
    row(
        MisplacedBeforeVerb,
        &["era", "status"],
        &["status"],
        "hook 1",
        error(2, "usage", &["omaddons", "status", "-t", "era"], &[0]),
    ),
    row(
        MisplacedBeforeVerb,
        &["wotlk", "list"],
        &["list"],
        "hook 1, a family default id",
        error(2, "usage", &["omaddons", "list", "-t", "wotlk"], &[0]),
    ),
    row(
        MisplacedBeforeVerb,
        &["sod", "install", "questie"],
        &["install"],
        "hook 1, a game mode; Questie unchanged on era",
        error(
            2,
            "usage",
            &["omaddons", "install", "questie", "-t", "sod"],
            &[0],
        ),
    ),
    // A client after the verb (hook 2).
    row(
        MisplacedAfterVerb,
        &["status", "retail"],
        &["status"],
        "hook 2, no positional",
        error(2, "usage", &["omaddons", "status", "-t", "retail"], &[0]),
    ),
    row(
        MisplacedAfterVerb,
        &["list", "era"],
        &["list"],
        "hook 2",
        error(2, "usage", &["omaddons", "list", "-t", "era"], &[0]),
    ),
    row(
        MisplacedAfterVerb,
        &["outdated", "retail"],
        &["outdated"],
        "hook 2",
        error(2, "usage", &["omaddons", "outdated", "-t", "retail"], &[0]),
    ),
    row(
        MisplacedAfterVerb,
        &["info", "dbm", "era"],
        &["info"],
        "hook 2, the one positional filled",
        error(2, "usage", &["omaddons", "info", "dbm", "-t", "era"], &[0]),
    ),
    row(
        MisplacedAfterVerb,
        &["pin", "dbm", "retail"],
        &["pin"],
        "hook 2",
        error(
            2,
            "usage",
            &["omaddons", "pin", "dbm", "-t", "retail"],
            &[0],
        ),
    ),
    row(
        MisplacedAfterVerb,
        &["unpin", "dbm", "retail"],
        &["unpin"],
        "hook 2; unchanged",
        error(
            2,
            "usage",
            &["omaddons", "unpin", "dbm", "-t", "retail"],
            &[0],
        ),
    ),
    // A client named among the addons (hook 3).
    row(
        ClientAsAddon,
        &["update", "retail"],
        &["update"],
        "hook 3; the fix updates DBM on retail",
        error(2, "usage", &["omaddons", "update", "-t", "retail"], &[0]),
    ),
    row(
        ClientAsAddon,
        &["update", "dbm", "retail"],
        &["update"],
        "hook 3; DBM is also on era and behind there",
        error(
            2,
            "usage",
            &["omaddons", "update", "dbm", "-t", "retail"],
            &[0],
        ),
    ),
    row(
        ClientAsAddon,
        &["remove", "dbm", "questie", "retail"],
        &["remove"],
        "hook 3; the fix is partial (Questie is not on retail)",
        error(
            2,
            "usage",
            &["omaddons", "remove", "dbm", "questie", "-t", "retail"],
            &[0],
        ),
    ),
    row(
        ClientAsAddon,
        &["install", "questie", "era"],
        &["install"],
        "hook 3 on install",
        error(
            2,
            "usage",
            &["omaddons", "install", "questie", "-t", "era"],
            &[0],
        ),
    ),
    row(
        ClientAsAddon,
        &["remove", "dbm", "retail"],
        &["remove"],
        "hook 3; nothing removed from era or retail",
        error(
            2,
            "usage",
            &["omaddons", "remove", "dbm", "-t", "retail"],
            &[0],
        ),
    ),
    row(
        ClientAsAddon,
        &["remove", "dbm", "hardcore"],
        &["remove"],
        "collision: the catalog.d addon Hardcore wins over the game mode",
        error(
            3,
            "target_ambiguous",
            &["omaddons", "remove", "dbm", "hardcore", "-t", "era"],
            &[0],
        ),
    )
    .by_leaf("C4"),
    row(
        ClientAsAddon,
        &["update", "dbm", "hardcore"],
        &["update"],
        "collision: a partial success, never usage",
        Expect::Ok,
    )
    .by_leaf("C4"),
    // -t all (M4-D13).
    row(
        TargetAll,
        &["update", "-t", "all"],
        &["update"],
        "M4-D13",
        Expect::Ok,
    ),
    row(
        TargetAll,
        &["install", "dbm", "-t", "all"],
        &["install"],
        "the first candidate in id order",
        error(
            3,
            "target_ambiguous",
            &["omaddons", "install", "dbm", "-t", "era"],
            &[0],
        ),
    ),
    // A mistyped client id (M4-D14).
    row(
        TargetTypo,
        &["status", "-t", "retial"],
        &["status"],
        "M4-D14",
        error(
            3,
            "target_not_found",
            &["omaddons", "status", "-t", "retail"],
            &[0],
        ),
    ),
    row(
        TargetTypo,
        &["list", "-t", "wtolk"],
        &["list"],
        "M4-D14",
        error(
            3,
            "target_not_found",
            &["omaddons", "list", "-t", "wotlk"],
            &[0],
        ),
    ),
    row(
        TargetTypo,
        &["undo", "-t", "retial"],
        &["undo"],
        "reverts retail's newest install",
        error(
            3,
            "target_not_found",
            &["omaddons", "undo", "-t", "retail"],
            &[0],
        ),
    ),
    row(
        TargetTypo,
        &["pin", "dbm", "-t", "retial"],
        &["pin"],
        "M4-D14",
        error(
            3,
            "target_not_found",
            &["omaddons", "pin", "dbm", "-t", "retail"],
            &[0],
        ),
    ),
    // A mistyped command (M2's and M3's usage fixes).
    row(
        CommandTypo,
        &["instal", "dbm", "-t", "retail"],
        &[],
        "a mutating guess becomes help",
        error(2, "usage", &["omaddons", "help", "install"], &[0]),
    ),
    row(
        CommandTypo,
        &["statsu"],
        &[],
        "a read-only guess runs",
        error(2, "usage", &["omaddons", "status"], &[0]),
    ),
    row(
        CommandTypo,
        &["lsit"],
        &[],
        "a read-only guess runs",
        error(2, "usage", &["omaddons", "list"], &[0]),
    ),
    row(
        CommandTypo,
        &["reinstall", "dbm", "-t", "retail"],
        &["install"],
        "a synonym keeps the user's intent",
        error(
            2,
            "usage",
            &["omaddons", "install", "dbm", "-t", "retail", "--reinstall"],
            &[0],
        ),
    ),
    row(
        CommandTypo,
        &["targets", "remove", "era"],
        &["targets rm"],
        "a synonym (the user's verb); the fix unregisters era on its copy",
        error(2, "usage", &["omaddons", "targets", "rm", "era"], &[0]),
    ),
    row(
        CommandTypo,
        &["config", "ls"],
        &["config"],
        "a synonym of the bare listing",
        error(2, "usage", &["omaddons", "config"], &[0]),
    ),
    row(
        CommandTypo,
        &["backup", "restor", "01M6Q2VA"],
        &[],
        "a nested mutating guess becomes help",
        error(2, "usage", &["omaddons", "help", "backup", "restore"], &[0]),
    ),
    row(
        CommandTypo,
        &["omarchy", "instal"],
        &[],
        "a nested mutating guess becomes help",
        error(
            2,
            "usage",
            &["omaddons", "help", "omarchy", "install"],
            &[0],
        ),
    ),
    // A mistyped flag.
    row(
        FlagTypo,
        &["upgrade", "--al"],
        &["update"],
        "the alias is kept; the fix updates DBM on both clients",
        error(2, "usage", &["omaddons", "upgrade", "--all"], &[0]),
    ),
    row(
        FlagTypo,
        &["install", "dbm", "--chanel", "beta", "-t", "retail"],
        &["install"],
        "corrected in place",
        error(
            2,
            "usage",
            &[
                "omaddons",
                "install",
                "dbm",
                "--channel",
                "beta",
                "-t",
                "retail",
            ],
            &[0],
        ),
    ),
    row(
        FlagTypo,
        &["status", "--json=1"],
        &["status"],
        "a refused presentation flag is dropped",
        error(2, "usage", &["omaddons", "status"], &[0]),
    ),
    row(
        FlagTypo,
        &["status", "--jsno"],
        &["status"],
        "a mistyped presentation flag is dropped",
        error(2, "usage", &["omaddons", "status"], &[0]),
    ),
    row(
        FlagTypo,
        &["status", "--cached", "--deep"],
        &["status"],
        "conflicting flags",
        error(2, "usage", &["omaddons", "help", "status"], &[0]),
    ),
    row(
        FlagTypo,
        &["list", "--protectd", "-t", "wotlk"],
        &["list"],
        "corrected in place",
        error(
            2,
            "usage",
            &["omaddons", "list", "--protected", "-t", "wotlk"],
            &[0],
        ),
    ),
    row(
        FlagTypo,
        &["export", "--overwrite"],
        &["export"],
        "the fix adds the --write that --overwrite presupposes; it writes on its copy",
        error(
            2,
            "usage",
            &["omaddons", "export", "--overwrite", "--write"],
            &[0],
        ),
    ),
    // A mistyped value.
    row(
        ValueTypo,
        &["install", "dbm", "--channel", "betaa", "-t", "retail"],
        &["install"],
        "corrected in place",
        error(
            2,
            "usage",
            &[
                "omaddons",
                "install",
                "dbm",
                "--channel",
                "beta",
                "-t",
                "retail",
            ],
            &[0],
        ),
    ),
    row(
        ValueTypo,
        &["history", "--since", "yesterday"],
        &["history"],
        "a corrected duration",
        error(2, "usage", &["omaddons", "history", "--since", "1d"], &[0]),
    ),
    row(
        ValueTypo,
        &["undo", "--last-by", "robot"],
        &["undo"],
        "nothing is close to robot: help, never a guessed actor",
        error(2, "usage", &["omaddons", "help", "undo"], &[0]),
    ),
    row(
        ValueTypo,
        &["backup", "list", "01M6"],
        &["backup list"],
        "an id too short to name a backup",
        error(2, "usage", &["omaddons", "backup", "list"], &[0]),
    ),
    row(
        ValueTypo,
        &["errors", "--limit", "0"],
        &["errors"],
        "a limit out of range",
        error(2, "usage", &["omaddons", "errors", "--limit", "10"], &[0]),
    ),
    row(
        ValueTypo,
        &["install", "http://github.com/o/r"],
        &["install"],
        "installs gh:o/r on retail, the default target",
        error(
            2,
            "ref_invalid",
            &["omaddons", "install", "https://github.com/o/r"],
            &[0],
        ),
    )
    .then(installs_on_retail),
    row(
        ValueTypo,
        &["auth", "github", "shwo"],
        &["auth github"],
        "a mistyped ACTION with nothing close: github's help",
        error(2, "usage", &["omaddons", "help", "auth", "github"], &[0]),
    ),
    // A mistaken argument.
    row(
        Argument,
        &["update", "--version", "12.1.14", "dbm"],
        &["install", "pin"],
        "a given version is an install; pins DBM at 12.1.14 on retail",
        error(
            2,
            "usage",
            &["omaddons", "install", "--version", "12.1.14", "dbm"],
            &[0],
        ),
    ),
    row(
        Argument,
        &["install", "unknownthing", "-t", "retail"],
        &["install"],
        "the fix only reads",
        error(
            3,
            "not_found",
            &["omaddons", "search", "unknownthing"],
            &[0],
        ),
    ),
    row(
        Argument,
        &["search"],
        &["search"],
        "a required argument missing",
        error(2, "usage", &["omaddons", "help", "search"], &[0]),
    ),
    row(
        Argument,
        &["copy", "--from", "era", "--to", "era"],
        &["copy"],
        "one client on both sides",
        error(2, "usage", &["omaddons", "targets"], &[0]),
    ),
    row(
        Argument,
        &["sync", "missing.toml"],
        &["sync"],
        "the fix is prose with <path>; the first alternative runs",
        Expect::Error {
            exit: 3,
            code: "not_found",
            fix: Fix::FirstAlternative(&["omaddons", "help", "sync"]),
            fix_exit: &[0],
        },
    )
    .then(names_the_missing_file),
    // A warning whose fix is a rerun.
    row(
        WarningFix,
        &["--offline", "outdated"],
        &["outdated"],
        "the warning's fix reruns online",
        Expect::Warning {
            code: "stale_check",
            fix: &["omaddons", "outdated"],
            fix_exit: &[0],
        },
    )
    .then(offline_then_online),
];

/// A row whose surface another leaf owns and that has not landed yet (common rule 18).
#[derive(Debug, Clone, Copy)]
pub struct Pending {
    pub row: Row,
    /// `app::COMMANDS` paths the row needs: its wrong argv's and its fix's.
    pub needs: &'static [&'static str],
    /// The leaf that owns the surface, and promotes the row if it lands after C4.
    pub leaf: &'static str,
}

const fn pending(leaf: &'static str, needs: &'static [&'static str], row: Row) -> Pending {
    Pending { row, needs, leaf }
}

/// A doctor-tree usage error's expectation (exit 64, M4-D40).
const fn doctor_usage(fix: &'static [&'static str], fix_exit: &'static [i32]) -> Expect {
    error(64, "usage", fix, fix_exit)
}

/// The note of E7's rows while their setup is unwritten.
const E7_SETUP: &str = "setup: E7 writes it (one committed repair by E6's fm-test-write)";

/// Rows of surfaces that have not landed, grouped by leaf. The leaf that adds the last
/// surface a row needs moves it into `CORPUS` and makes it pass, in that commit.
pub const PENDING: &[Pending] = &[
    pending(
        "A4",
        &["capabilities"],
        row(
            CommandTypo,
            &["capabilites"],
            &[],
            "a read-only guess runs",
            error(2, "usage", &["omaddons", "capabilities"], &[0]),
        ),
    ),
    pending(
        "A5",
        &["robot-docs"],
        row(
            CommandTypo,
            &["robotdocs"],
            &[],
            "bare robot-docs runs its help",
            error(2, "usage", &["omaddons", "robot-docs"], &[0]),
        ),
    ),
    pending(
        "A5",
        &["robot-docs schema"],
        row(
            CommandTypo,
            &["robot-docs", "schema", "instal"],
            &["robot-docs schema"],
            "a schema's command name, corrected",
            error(
                2,
                "usage",
                &["omaddons", "robot-docs", "schema", "install"],
                &[0],
            ),
        )
        .by_leaf("A5"),
    ),
    pending(
        "A5",
        &["robot-docs errors"],
        row(
            CommandTypo,
            &["robot-docs", "erors"],
            &[],
            "a read-only guess runs",
            error(2, "usage", &["omaddons", "robot-docs", "errors"], &[0]),
        )
        .by_leaf("A5"),
    ),
    pending(
        "B1",
        &["robot-docs guide"],
        row(
            CommandTypo,
            &["robot-docs", "gide"],
            &[],
            "a read-only guess runs",
            error(2, "usage", &["omaddons", "robot-docs", "guide"], &[0]),
        ),
    ),
    pending(
        "B3",
        &["robot-docs install-skill"],
        row(
            CommandTypo,
            &["robot-docs", "install-skil"],
            &[],
            "a mutating guess becomes help",
            error(
                2,
                "usage",
                &["omaddons", "help", "robot-docs", "install-skill"],
                &[0],
            ),
        )
        .by_leaf("B3"),
    ),
    pending(
        "B3",
        &["robot-docs install-skill"],
        row(
            FlagTypo,
            &["robot-docs", "install-skill", "--dry-rn"],
            &["robot-docs install-skill"],
            "the corpus home has no $HOME/.claude: the fix exits 3",
            error(
                2,
                "usage",
                &["omaddons", "robot-docs", "install-skill", "--dry-run"],
                &[3],
            ),
        )
        .by_leaf("B3"),
    ),
    pending(
        "A7",
        &["completions"],
        row(
            ValueTypo,
            &["completions", "bsh"],
            &["completions"],
            "a shell name corrected",
            error(2, "usage", &["omaddons", "completions", "bash"], &[0]),
        ),
    ),
    pending(
        "A7",
        &["completions"],
        row(
            CommandTypo,
            &["completion", "bash"],
            &[],
            "a read-only guess runs",
            error(2, "usage", &["omaddons", "completions", "bash"], &[0]),
        )
        .by_leaf("A7"),
    ),
    pending(
        "D5",
        &["apply"],
        row(
            ApplyForAdopt,
            &["apply"],
            &["apply", "adopt"],
            "help first; the alternative is adopt --apply (M4-D27)",
            error(2, "usage", &["omaddons", "help", "apply"], &[0]),
        ),
    ),
    pending(
        "D5",
        &["apply"],
        row(
            ApplyForAdopt,
            &["apply", "nosuchid0"],
            &["apply"],
            "no such plan",
            error(3, "plan_not_found", &["omaddons", "help", "apply"], &[0]),
        ),
    ),
    pending(
        "D5",
        &["apply"],
        row(
            ApplyForAdopt,
            &["adopt", "apply"],
            &["adopt"],
            "the user's own verb; nothing to adopt on the corpus home",
            error(2, "usage", &["omaddons", "adopt", "--apply"], &[0]),
        ),
    ),
    pending(
        "D5",
        &["apply"],
        row(
            ApplyForAdopt,
            &["aply", "01M8Q2VA"],
            &[],
            "a mutating guess becomes help",
            error(2, "usage", &["omaddons", "help", "apply"], &[0]),
        ),
    ),
    pending(
        "D9",
        &["plans show"],
        row(
            CommandTypo,
            &["plans", "shwo", "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"],
            &["plans show"],
            "the corpus home holds no such plan: the fix exits 3",
            error(
                2,
                "usage",
                &["omaddons", "plans", "show", "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"],
                &[3],
            ),
        ),
    ),
    pending(
        "D9",
        &["plans show"],
        row(
            CommandTypo,
            &["plan", "show", "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"],
            &["plans show"],
            "the corpus home holds no such plan: the fix exits 3",
            error(
                2,
                "usage",
                &["omaddons", "plans", "show", "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"],
                &[3],
            ),
        ),
    ),
    pending(
        "E3",
        &["doctor"],
        row(
            DoctorVerb,
            &["doctor", "--fix"],
            &["doctor fix"],
            "help until E6; then doctor fix --dry-run, exiting 0 or 1",
            doctor_usage(&["omaddons", "help", "doctor"], &[0]),
        ),
    ),
    pending(
        "E3",
        &["doctor explain"],
        row(
            DoctorVerb,
            &["doctor", "--explain", "fm-schemas-state-version-newer"],
            &["doctor explain"],
            "--explain is a subcommand",
            doctor_usage(
                &[
                    "omaddons",
                    "doctor",
                    "explain",
                    "fm-schemas-state-version-newer",
                ],
                &[0],
            ),
        ),
    ),
    pending(
        "E3",
        &["doctor diagnose"],
        row(
            DoctorVerb,
            &["doctor", "diagnoze"],
            &[],
            "a finding exits 1",
            doctor_usage(&["omaddons", "doctor", "diagnose"], &[0, 1]),
        ),
    ),
    pending(
        "E3",
        &["doctor"],
        row(
            DoctorVerb,
            &["doctor", "--only", "fm-nope"],
            &["doctor"],
            "an unknown failure mode",
            doctor_usage(&["omaddons", "doctor"], &[0, 1]),
        ),
    ),
    pending(
        "E3",
        &["doctor"],
        row(
            DoctorVerb,
            &["doctr"],
            &[],
            "a root-level typo uses the general dictionary (exit 2)",
            error(2, "usage", &["omaddons", "doctor"], &[0, 1]),
        ),
    ),
    pending(
        "E3",
        &["doctor explain"],
        row(
            DoctorVerb,
            &[
                "doctor",
                "explain",
                "fm-concurrency_primitives-journal-pendng",
            ],
            &["doctor explain"],
            "a failure mode id corrected",
            doctor_usage(
                &[
                    "omaddons",
                    "doctor",
                    "explain",
                    "fm-concurrency_primitives-journal-pending",
                ],
                &[0],
            ),
        ),
    ),
    pending(
        "E6",
        &["doctor fix"],
        row(
            DoctorVerb,
            &["doctor", "fix", "--force"],
            &["doctor fix"],
            "a preview, never a forced fix",
            doctor_usage(&["omaddons", "doctor", "fix", "--dry-run"], &[0, 1]),
        ),
    ),
    pending(
        "E6",
        &["doctor fix"],
        row(
            DoctorVerb,
            &["doctor", "fxi"],
            &[],
            "a mutating guess becomes help",
            doctor_usage(&["omaddons", "help", "doctor", "fix"], &[0]),
        )
        .by_leaf("E6"),
    ),
    pending(
        "E7",
        &["doctor undo", "doctor fix"],
        row(
            DoctorVerb,
            &["doctor", "undo", "latest"],
            &["doctor undo"],
            E7_SETUP,
            doctor_usage(&["omaddons", "doctor", "undo"], &[0]),
        ),
    ),
    pending(
        "E7",
        &["doctor undo", "doctor fix"],
        row(
            DoctorVerb,
            &["undo", "--run", "{run}"],
            &["doctor undo"],
            E7_SETUP,
            error(2, "usage", &["omaddons", "doctor", "undo", "{run}"], &[0]),
        )
        .by_leaf("E7"),
    ),
];

/// Whether every path of `needs` is among `commands`.
fn ready(needs: &[&str], commands: &[&str]) -> bool {
    needs.iter().all(|need| commands.contains(need))
}

/// The paths of `app::COMMANDS`.
fn command_names() -> Vec<&'static str> {
    omaddons::app::COMMANDS.iter().map(|c| c.name).collect()
}

/// The https install lands on the default target.
fn installs_on_retail(log: &TestLog, runs: &RowRuns<'_>) {
    let target = &runs.fix.data()["target"]["id"];
    log.assert_eq("target", target, &Value::from("retail"));
}

/// A prose fix (no argv), the missing file resolved in the work directory.
fn names_the_missing_file(log: &TestLog, runs: &RowRuns<'_>) {
    let error = &runs.wrong.envelope()["error"];
    log.assert_eq("fix_argv", &error["fix_argv"], &Value::Null);
    let path = error["details"]["path"].as_str().unwrap_or_default();
    let under = std::path::Path::new(path).starts_with(runs.home.work_dir());
    assert!(under, "{path:?} is not under the work directory");
}

/// Offline answers from the memo and sends nothing; the online rerun asks.
fn offline_then_online(log: &TestLog, runs: &RowRuns<'_>) {
    let offline = &runs.wrong.envelope()["meta"]["offline"];
    log.assert_eq("meta.offline", offline, &Value::from(true));
    log.assert_eq("wrong requests", &runs.wrong_requests, &[].as_slice());
    assert!(
        !runs.fix_requests.is_empty(),
        "the online rerun sent no request"
    );
}

/// `<kind>-<two-digit index within the kind>-<argv joined with _>`, every character
/// outside `[A-Za-z0-9_.-]` as `_`, the argv part cut at 60 characters. A row that is not
/// in [`CORPUS`] has index 00.
pub fn case_name(row: &Row) -> String {
    let index = CORPUS
        .iter()
        .filter(|r| r.kind == row.kind)
        .position(|r| r.argv == row.argv)
        .map_or(0, |i| i + 1);
    let argv: String = row
        .argv
        .join("_")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    format!("{}-{index:02}-{argv}", row.kind.name())
}

/// `argv` with each whole `{name}` token replaced by `vars[name]`; other tokens kept.
pub fn substitute(argv: &[&str], vars: &BTreeMap<String, String>) -> Vec<String> {
    argv.iter()
        .map(|token| {
            let name = token.strip_prefix('{').and_then(|t| t.strip_suffix('}'));
            match name {
                Some(name) => {
                    let value = vars.get(name);
                    assert!(value.is_some(), "no captured value named {name}");
                    value.cloned().unwrap_or_default()
                }
                None => (*token).to_owned(),
            }
        })
        .collect()
}

/// The verb rule (criterion 5): a suggestion is `omaddons help …`, a command that only
/// reads (or none), or one of the `verbs` the user may have meant (aliases canonical).
pub fn verb_rule_admits(argv: &[String], verbs: &[&str]) -> bool {
    if argv.get(1).map(String::as_str) == Some("help") {
        return true;
    }
    let path = command_path(argv).join(" ");
    path.is_empty() || !omaddons::app::mutates(&path) || verbs.contains(&path.as_str())
}

/// `argv` and the output every run here asks for, before any `--`.
fn with_output(argv: &[String]) -> Vec<String> {
    let at = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
    let mut out = argv.to_vec();
    out.splice(at..at, ["--json", "--progress", "json"].map(str::to_owned));
    out
}

fn as_strs(argv: &[String]) -> Vec<&str> {
    argv.iter().map(String::as_str).collect()
}

/// A copy of `run`, for `support::check`, which takes one.
fn copy_run(run: &Run) -> Run {
    Run {
        argv: run.argv.clone(),
        code: run.code,
        stdout: run.stdout.clone(),
        stderr: run.stderr.clone(),
    }
}

/// Every suggestion of an envelope's error: the fix's argv and each alternative's.
fn suggestions(error: &Value) -> Vec<Vec<String>> {
    std::iter::once(&error["fix_argv"])
        .chain(
            error["alternatives"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| &a["argv"]),
        )
        .filter(|argv| !argv.is_null())
        .map(support::strings)
        .collect()
}

/// The requests in `net.log` so far, as `METHOD host path`.
fn requests(home: &IntentHome) -> Vec<String> {
    net_log(&home.home)
        .entries
        .iter()
        .map(|e| format!("{} {}{}", e.method, e.host, e.path))
        .collect()
}

/// Where a row got to, and both its runs, kept outside the code that asserts.
#[derive(Default)]
struct Progress {
    step: &'static str,
    wrong: Option<Ran>,
    fix: Option<Ran>,
}

/// A row that failed: the step, the message, both runs and the case log, as one report.
#[derive(Debug)]
pub struct RowFailure {
    report: String,
}

impl std::fmt::Display for RowFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.report)
    }
}

impl RowFailure {
    fn new(
        name: &str,
        row: &Row,
        progress: &Progress,
        message: &str,
        log: &std::path::Path,
    ) -> Self {
        let mut report = format!("row {name}: {}: {message}\n  {}\n", progress.step, row.note);
        for (label, ran) in [("wrong", &progress.wrong), ("fix", &progress.fix)] {
            match ran {
                Some(ran) => {
                    let argv = ran.run.argv.join(" ");
                    report.push_str(&format!(
                        "{label}: omaddons {argv} → exit {}\nstdout: {}\nstderr: {}\n",
                        ran.run.code,
                        ran.run.stdout.trim_end(),
                        ran.run.stderr.trim_end()
                    ));
                }
                None => report.push_str(&format!("{label}: not run\n")),
            }
        }
        report.push_str(&format!("log: {}\n", log.display()));
        Self { report }
    }
}

/// The text of a caught panic.
fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

/// Runs `row` on `home` (see the module doc), in its own case log.
pub fn run_row(log: &TestLog, home: &IntentHome, row: &Row) -> Result<(), RowFailure> {
    run_row_with(log, home, row, &|_| {})
}

/// [`run_row`], with `after_wrong` run on the home right after the wrong run: the seam
/// the harness's self-tests use to plant a change.
pub fn run_row_with(
    log: &TestLog,
    home: &IntentHome,
    row: &Row,
    after_wrong: &dyn Fn(&IntentHome),
) -> Result<(), RowFailure> {
    let name = case_name(row);
    let case = log.case(&name);
    let mut progress = Progress::default();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        steps(&case, home, row, after_wrong, &mut progress);
    }));
    outcome.map_err(|panic| {
        RowFailure::new(
            &name,
            row,
            &progress,
            &panic_text(panic.as_ref()),
            &case.transcript_path(),
        )
    })
}

fn steps(
    case: &TestLog,
    home: &IntentHome,
    row: &Row,
    after_wrong: &dyn Fn(&IntentHome),
    progress: &mut Progress,
) {
    progress.step = "restore";
    case.step("restore");
    home.reset();

    let mut vars = BTreeMap::new();
    if !row.setup.is_empty() {
        progress.step = "setup";
        case.step("setup");
        for step in row.setup {
            let argv = with_output(&substitute(step.argv, &vars));
            let mut cmd = home.cmd(case, &as_strs(&argv));
            for (key, value) in step.env {
                cmd = cmd.env(key, value);
            }
            let ran = cmd.run();
            ran.exit(step.exit);
            if let Some((name, pointer)) = step.capture {
                let value = ran.envelope().pointer(pointer).and_then(Value::as_str);
                assert!(value.is_some(), "setup {argv:?}: no string at {pointer}");
                vars.insert(name.to_owned(), value.unwrap_or_default().to_owned());
            }
        }
        home.save_row();
    }

    progress.step = "before";
    case.step("before");
    for id in IntentHome::CLIENTS {
        case.tree(id, &home.client_dir(id));
    }
    let before = home.records();
    case.json("records before", &before);
    let requested = requests(home);

    progress.step = "wrong";
    case.step("wrong");
    let argv = with_output(&substitute(row.argv, &vars));
    let ran = progress.wrong.insert(home.cmd(case, &as_strs(&argv)).run());
    let wrong_requests: Vec<String> = requests(home).into_iter().skip(requested.len()).collect();
    let exit = match row.expect {
        Expect::Error { exit, .. } => exit,
        Expect::Ok | Expect::Warning { .. } => 0,
    };
    ran.exit(exit);
    support::check(copy_run(&ran.run), exit);
    ran.assert_private();
    let rejected = |suggestion: &[String]| {
        let path = command_path(suggestion).join(" ");
        format!(
            "the suggestion {suggestion:?} runs {path:?}, which changes something and is none of {:?}",
            row.verbs
        )
    };
    match row.expect {
        Expect::Error { code, fix, .. } => {
            case.assert_eq("code", &ran.error_code(), &Some(code));
            let error = &ran.envelope()["error"];
            let named = match fix {
                Fix::Argv(_) => &error["fix_argv"],
                Fix::FirstAlternative(_) => &error["alternatives"][0]["argv"],
            };
            case.assert_eq(
                "fix",
                &support::strings(named),
                &substitute(fix.argv(), &vars),
            );
            for suggestion in suggestions(error) {
                assert!(
                    verb_rule_admits(&suggestion, row.verbs),
                    "{}",
                    rejected(&suggestion)
                );
            }
        }
        Expect::Warning { code, fix, .. } => {
            let warnings = ran.envelope()["warnings"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            // ubs:ignore — comparing wire codes, not secrets
            let warning = warnings.iter().find(|w| w["code"] == code);
            assert!(warning.is_some(), "no {code} warning: {warnings:?}");
            let named = warning
                .map(|w| support::strings(&w["fix_argv"]))
                .unwrap_or_default();
            case.assert_eq("fix", &named, &substitute(fix, &vars));
            assert!(verb_rule_admits(&named, row.verbs), "{}", rejected(&named));
        }
        Expect::Ok => {}
    }
    after_wrong(home);

    let (pinned, fix_exit) = match row.expect {
        Expect::Error { fix, fix_exit, .. } => (fix.argv(), fix_exit),
        Expect::Warning { fix, fix_exit, .. } => (fix, fix_exit),
        // A success may change state: nothing to hash, nothing more to run.
        Expect::Ok => return,
    };
    if matches!(row.expect, Expect::Error { .. }) {
        progress.step = "after";
        case.step("after");
        for id in IntentHome::CLIENTS {
            case.assert_unchanged(id, &home.client_dir(id));
        }
        let after = home.records();
        let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
        let differ: Vec<(&String, Option<&String>, Option<&String>)> = keys
            .into_iter()
            .filter(|k| before.get(*k) != after.get(*k))
            .map(|k| (k, before.get(k), after.get(k)))
            .take(20)
            .collect();
        if !differ.is_empty() {
            case.json("records diff", &differ);
        }
        let changed: Vec<&String> = differ.iter().map(|(k, _, _)| *k).collect();
        assert!(changed.is_empty(), "records changed: {changed:?}");
        case.assert_eq("records", &after, &before);
        case.assert_eq("requests", &wrong_requests, &Vec::<String>::new());
    }

    progress.step = "restore for the fix";
    case.step("restore for the fix");
    if row.setup.is_empty() {
        home.reset();
    } else {
        home.reset_row();
    }

    progress.step = "fix";
    case.step("fix");
    let requested = requests(home);
    let argv = with_output(&substitute(pinned.get(1..).unwrap_or_default(), &vars));
    let ran = progress.fix.insert(home.cmd(case, &as_strs(&argv)).run());
    let fix_requests: Vec<String> = requests(home).into_iter().skip(requested.len()).collect();
    let code = ran.run.code;
    assert!(
        fix_exit.contains(&code),
        "fix exit {code} is not one of {fix_exit:?}"
    );
    support::check(copy_run(&ran.run), code);
    ran.assert_private();

    if let (Some(extra), Some(wrong)) = (row.extra, progress.wrong.as_ref()) {
        progress.step = "extra";
        case.step("extra");
        let runs = RowRuns {
            home,
            wrong,
            fix: ran,
            wrong_requests: &wrong_requests,
            fix_requests: &fix_requests,
        };
        extra(case, &runs);
    }
}

/// Runs every row of `kind` on one corpus home, each in its own case log, and fails
/// listing every failing row, after all of them ran.
fn run_kind(log: &TestLog, kind: Kind) {
    let rows: Vec<&Row> = CORPUS.iter().filter(|r| r.kind == kind).collect();
    // What is not proven yet, and whose surface it waits for.
    let waiting: Vec<String> = PENDING
        .iter()
        .filter(|p| p.row.kind == kind)
        .map(|p| format!("{} (leaf {})", p.row.argv.join(" "), p.leaf))
        .collect();
    for row in &waiting {
        log.note(format!("pending: {row}"));
    }
    if rows.is_empty() {
        log.note(format!(
            "no active row of {}; {} pending",
            kind.name(),
            waiting.len()
        ));
        return;
    }
    let home = intent_home();
    log.env(&home.home);
    let mut failures = Vec::new();
    for row in &rows {
        let outcome = run_row(log, &home, row);
        let verdict = if outcome.is_ok() { "ok" } else { "FAILED" };
        let origin = match row.origin {
            Origin::Plan => "the plan".to_owned(),
            Origin::Leaf(leaf) => format!("leaf {leaf}"),
        };
        log.note(format!(
            "{}: {} ({origin}; {verdict})",
            case_name(row),
            row.note
        ));
        if let Err(failure) = outcome {
            failures.push(failure);
        }
    }
    log.step("summary");
    let failing: Vec<&str> = failures
        .iter()
        .filter_map(|f| f.report.split(':').next())
        .collect();
    log.json("failing rows", &failing);
    let reports: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert!(
        failures.is_empty(),
        "{} of {} {} rows failed\n\n{}",
        failures.len(),
        rows.len(),
        kind.name(),
        reports.join("\n")
    );
}

#[test]
fn corpus_misplaced_client_before_the_verb() {
    run_kind(&tlog!(), Kind::MisplacedBeforeVerb);
}

#[test]
fn corpus_misplaced_client_after_the_verb() {
    run_kind(&tlog!(), Kind::MisplacedAfterVerb);
}

#[test]
fn corpus_client_named_as_an_addon() {
    run_kind(&tlog!(), Kind::ClientAsAddon);
}

#[test]
fn corpus_target_all() {
    run_kind(&tlog!(), Kind::TargetAll);
}

#[test]
fn corpus_target_typos() {
    run_kind(&tlog!(), Kind::TargetTypo);
}

#[test]
fn corpus_command_typos() {
    run_kind(&tlog!(), Kind::CommandTypo);
}

#[test]
fn corpus_flag_typos() {
    run_kind(&tlog!(), Kind::FlagTypo);
}

#[test]
fn corpus_value_typos() {
    run_kind(&tlog!(), Kind::ValueTypo);
}

#[test]
fn corpus_argument_mistakes() {
    run_kind(&tlog!(), Kind::Argument);
}

#[test]
fn corpus_warning_fixes() {
    run_kind(&tlog!(), Kind::WarningFix);
}

#[test]
fn corpus_apply_for_adopt() {
    run_kind(&tlog!(), Kind::ApplyForAdopt);
}

#[test]
fn corpus_doctor_verbs() {
    run_kind(&tlog!(), Kind::DoctorVerb);
}

// ---- structural tests ----

/// A placeholder a captured `{name}` would be, for parsing a pinned argv alone.
const PLACEHOLDER: &str = "01M8Q2VA00000000000000000A";

fn placeholders(argv: &[&str]) -> Vec<String> {
    argv.iter()
        .map(|t| {
            if t.starts_with('{') && t.ends_with('}') {
                PLACEHOLDER.to_owned()
            } else {
                (*t).to_owned()
            }
        })
        .collect()
}

#[test]
fn corpus_rows_are_unique() {
    let mut argvs: Vec<&[&str]> = CORPUS.iter().map(|r| r.argv).collect();
    argvs.sort_unstable();
    let before = argvs.len();
    argvs.dedup();
    assert_eq!(argvs.len(), before, "two rows share an argv");
    let mut names: Vec<String> = CORPUS.iter().map(case_name).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), CORPUS.len(), "two rows share a case name");
}

#[test]
fn corpus_fixes_parse_and_name_commands_that_exist() {
    let pinned = CORPUS.iter().filter_map(|r| match r.expect {
        Expect::Error { fix, .. } => Some(fix.argv().to_vec()),
        Expect::Warning { fix, .. } => Some(fix.to_vec()),
        Expect::Ok => None,
    });
    let setups = CORPUS.iter().flat_map(|r| r.setup.iter()).map(|s| {
        std::iter::once("omaddons")
            .chain(s.argv.iter().copied())
            .collect::<Vec<_>>()
    });
    for argv in pinned.chain(setups) {
        let argv = placeholders(&argv);
        support::parses(&argv, &format!("{argv:?}"));
        let path = command_path(&argv).join(" ");
        // ubs:ignore — comparing command names, not secrets
        let row = omaddons::app::COMMANDS.iter().any(|c| c.name == path);
        let known = path.is_empty() || matches!(path.as_str(), "help") || row;
        assert!(known, "{argv:?} names {path:?}, no command");
    }
}

#[test]
fn corpus_fixes_follow_the_verb_rule() {
    for row in CORPUS {
        let fix = match row.expect {
            Expect::Error { fix, .. } => fix.argv(),
            Expect::Warning { fix, .. } => fix,
            Expect::Ok => continue,
        };
        let fix = placeholders(fix);
        assert!(
            verb_rule_admits(&fix, row.verbs),
            "{}: {fix:?} with {:?}",
            case_name(row),
            row.verbs
        );
    }
    let argv = |words: &str| -> Vec<String> { words.split(' ').map(str::to_owned).collect() };
    for (case, words, verbs, admitted) in [
        ("help_is_admitted", "omaddons help install", &[][..], true),
        ("a_read_is_admitted", "omaddons status", &[], true),
        (
            "another_change_is_rejected",
            "omaddons install dbm",
            &[],
            false,
        ),
        (
            "the_users_synonym",
            "omaddons install dbm -t retail --reinstall",
            &["install"],
            true,
        ),
        (
            "either_reading",
            "omaddons pin --version 1 dbm",
            &["install", "pin"],
            true,
        ),
        (
            "a_different_change",
            "omaddons remove dbm",
            &["update"],
            false,
        ),
        ("the_alias", "omaddons upgrade --all", &["update"], true),
        (
            "the_moved_client",
            "omaddons update -t retail",
            &["update"],
            true,
        ),
    ] {
        assert_eq!(
            verb_rule_admits(&argv(words), verbs),
            admitted,
            "{case}: {words} with {verbs:?}"
        );
    }
}

#[test]
fn substitution_fills_captured_values() {
    let run = "2026-10-25T00:00:00Z__01M8Q2VA00000000000000000A";
    let vars: BTreeMap<String, String> = [("run".to_owned(), run.to_owned())].into();
    let words = |w: &[&str]| -> Vec<String> { w.iter().map(|s| (*s).to_owned()).collect() };
    assert_eq!(
        substitute(&["doctor", "undo", "{run}"], &vars),
        words(&["doctor", "undo", run]),
        "whole_token"
    );
    assert_eq!(
        substitute(&["status", "-t", "retail"], &vars),
        words(&["status", "-t", "retail"]),
        "no_token"
    );
    assert_eq!(
        substitute(&["x{run}"], &vars),
        words(&["x{run}"]),
        "inside_a_word"
    );
    let missing = std::panic::catch_unwind(|| substitute(&["{nope}"], &vars));
    let message = missing.map_or_else(|p| panic_text(p.as_ref()), |_| String::new());
    assert!(message.contains("nope"), "missing_name: {message:?}");
}

#[test]
fn case_names_are_safe_file_names() {
    let named =
        |argv: &'static [&'static str], kind| case_name(&row(kind, argv, &[], "", Expect::Ok));
    assert_eq!(
        named(&["remove", "dbm", "retail"], ClientAsAddon),
        "client_as_addon-05-remove_dbm_retail"
    );
    let url = named(&["install", "http://github.com/o/r"], Kind::Argument);
    assert!(!url.contains('/') && !url.contains(':'), "{url}");
    let flag = named(&["status", "--json=1"], Kind::FlagTypo);
    assert!(!flag.contains('='), "{flag}");
    const LONG: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let long = named(&[LONG], Kind::CommandTypo);
    let tail = long.splitn(3, '-').nth(2).unwrap_or_default();
    assert_eq!(tail.len(), 60, "{long}");
    let safe = |name: &str| {
        let mut parts = name.splitn(3, '-');
        let (kind, index, argv) = (parts.next(), parts.next(), parts.next());
        kind.is_some_and(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
            && index.is_some_and(|i| i.len() == 2 && i.chars().all(|c| c.is_ascii_digit()))
            && argv.is_some_and(|a| {
                (1..=60).contains(&a.len())
                    && a.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
            })
    };
    for row in CORPUS {
        let name = case_name(row);
        assert!(safe(&name), "{name}");
    }
}

// ---- the harness's self-tests ----

#[test]
fn harness_reports_a_failing_fix_with_both_envelopes() {
    let log = tlog!();
    let home = intent_home();
    log.env(&home.home);
    let statsu = row(
        Kind::CommandTypo,
        &["statsu"],
        &[],
        "a test-only row whose fix exits 0, not 3",
        Expect::Error {
            exit: 2,
            code: "usage",
            fix: Fix::Argv(&["omaddons", "status"]),
            fix_exit: &[3],
        },
    );
    let failure = run_row(&log, &home, &statsu).err();
    let report = failure.map(|f| f.to_string()).unwrap_or_default();
    log.note(&report);
    for part in [
        "\"code\":\"usage\"",
        "\"command\":\"status\"",
        "\"exit_code\":0",
        "fix exit 0 is not one of [3]",
    ] {
        assert!(
            report.contains(part),
            "the report lacks {part:?}:\n{report}"
        );
    }
    let path = report
        .lines()
        .find_map(|l| l.strip_prefix("log: "))
        .unwrap_or_default();
    assert!(
        std::path::Path::new(path).exists(),
        "no transcript at {path:?}"
    );
}

/// Row 15, `remove dbm retail`.
fn remove_dbm_retail() -> &'static Row {
    let row = CORPUS
        .iter()
        .find(|r| r.argv == ["remove", "dbm", "retail"]);
    assert!(row.is_some(), "no remove dbm retail row");
    row.unwrap_or(&CORPUS[0])
}

#[test]
fn harness_catches_a_changed_record() {
    let log = tlog!();
    let home = intent_home();
    log.env(&home.home);
    let key = crate::common::state::key_of(&home.home, "retail");
    let record = crate::common::http::paths(&home.home.env).target_record(&key);
    let rel = record
        .strip_prefix(home.home.state())
        .unwrap()
        .display()
        .to_string();
    let plant = |_: &IntentHome| {
        let mut bytes = fs::read(&record).unwrap();
        bytes[0] ^= 0x20;
        fs::write(&record, bytes).unwrap();
    };
    let failure = run_row_with(&log, &home, remove_dbm_retail(), &plant).err();
    let report = failure.map(|f| f.to_string()).unwrap_or_default();
    log.note(&report);
    for part in [": after: ", "records", rel.as_str(), "fix: not run"] {
        assert!(
            report.contains(part),
            "the report lacks {part:?}:\n{report}"
        );
    }
}

#[test]
fn harness_catches_a_changed_client_tree() {
    let log = tlog!();
    let home = intent_home();
    log.env(&home.home);
    let toc = home
        .client_dir("retail")
        .join("Interface/AddOns/DBM-Core/DBM-Core.toc");
    let plant = |_: &IntentHome| {
        let mut text = fs::read_to_string(&toc).unwrap();
        text.push_str("## Planted: yes\n");
        fs::write(&toc, text).unwrap();
    };
    let failure = run_row_with(&log, &home, remove_dbm_retail(), &plant).err();
    let report = failure.map(|f| f.to_string()).unwrap_or_default();
    log.note(&report);
    for part in ["retail", "Interface/AddOns/DBM-Core/DBM-Core.toc"] {
        assert!(
            report.contains(part),
            "the report lacks {part:?}:\n{report}"
        );
    }
}

// ---- the pending table and criterion 5 ----

#[test]
fn ready_needs_every_path() {
    for (case, needs, commands, want) in [
        (
            "all_present",
            &["status"][..],
            &["status", "list"][..],
            true,
        ),
        (
            "one_missing",
            &["doctor undo", "doctor fix"],
            &["doctor fix"],
            false,
        ),
        ("none_present", &["doctor"], &["status"], false),
        ("empty_needs", &[], &["status"], true),
    ] {
        assert_eq!(
            ready(needs, commands),
            want,
            "{case}: {needs:?} in {commands:?}"
        );
    }
    for p in PENDING {
        assert!(
            !p.needs.is_empty(),
            "pending row {:?} names no need",
            p.row.argv
        );
    }
}

#[test]
fn pending_rows_wait_for_a_surface_that_has_not_landed() {
    let commands = command_names();
    for p in PENDING {
        let argv = p.row.argv.join(" ");
        assert!(
            !ready(p.needs, &commands),
            "pending row {argv} (surface of {}): its surfaces {:?} exist; move it from PENDING to CORPUS and make it pass (common rule 18)",
            p.leaf,
            p.needs
        );
        for need in p.needs {
            let spelled = !need.is_empty()
                && need.split(' ').all(|word| {
                    !word.is_empty()
                        && word
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                });
            assert!(spelled, "pending row {argv}: {need:?} is no command path");
        }
    }
}

#[test]
fn pending_rows_are_unique_and_not_in_the_corpus() {
    let mut argvs: Vec<&[&str]> = CORPUS
        .iter()
        .map(|r| r.argv)
        .chain(PENDING.iter().map(|p| p.row.argv))
        .collect();
    argvs.sort_unstable();
    let total = argvs.len();
    argvs.dedup();
    assert_eq!(
        argvs.len(),
        total,
        "an argv appears twice across CORPUS and PENDING"
    );
}

/// The plan's rows per kind, across `CORPUS` and `PENDING`.
fn plan_rows() -> BTreeMap<&'static str, usize> {
    let mut per_kind = BTreeMap::new();
    for row in CORPUS.iter().chain(PENDING.iter().map(|p| &p.row)) {
        if matches!(row.origin, Origin::Plan) {
            *per_kind.entry(row.kind.name()).or_insert(0) += 1;
        }
    }
    per_kind
}

#[test]
fn plan_rows_are_never_dropped() {
    // Appendix C.2's 56, plus D5's 4, D9's 2, E3's 4 and E6's 1.
    let per_kind = plan_rows();
    let total: usize = per_kind.values().sum();
    assert_eq!(total, 67, "plan rows per kind: {per_kind:?}");
}

/// The categories whose surfaces exist without any other M4 leaf.
const LANDED_CATEGORIES: [Kind; 8] = [
    MisplacedBeforeVerb,
    MisplacedAfterVerb,
    ClientAsAddon,
    TargetAll,
    TargetTypo,
    CommandTypo,
    FlagTypo,
    ValueTypo,
];

#[test]
fn every_landed_category_has_a_row() {
    for kind in LANDED_CATEGORIES {
        assert!(
            CORPUS.iter().any(|r| r.kind == kind),
            "no corpus row of {}",
            kind.name()
        );
    }
}

#[test]
#[ignore = "checkpoint: H4 and H6 run it once every M4 surface has landed"]
fn the_corpus_meets_checkpoint_criterion_5() {
    let log = tlog!();
    let mut per_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut per_origin: BTreeMap<String, usize> = BTreeMap::new();
    for row in CORPUS {
        *per_kind.entry(row.kind.name()).or_insert(0) += 1;
        let origin = match row.origin {
            Origin::Plan => "plan".to_owned(),
            Origin::Leaf(leaf) => format!("leaf {leaf}"),
        };
        *per_origin.entry(origin).or_insert(0) += 1;
    }
    let counts = format!(
        "intent corpus: {} rows\nper kind: {per_kind:?}\nper origin: {per_origin:?}",
        CORPUS.len()
    );
    log.note(&counts);
    let pending: Vec<String> = PENDING
        .iter()
        .map(|p| format!("{} (leaf {})", p.row.argv.join(" "), p.leaf))
        .collect();
    let categories = LANDED_CATEGORIES.iter().chain(&[ApplyForAdopt, DoctorVerb]);
    let missing: Vec<&str> = categories
        .filter(|kind| !CORPUS.iter().any(|r| r.kind == **kind))
        .map(|kind| kind.name())
        .collect();
    assert!(
        CORPUS.len() >= 60 && pending.is_empty() && missing.is_empty(),
        "{counts}\npending: {pending:?}\nmissing categories: {missing:?}"
    );
}
