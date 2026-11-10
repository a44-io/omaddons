//! What a command's help page adds after its options: a few examples, which `-h` shows
//! and `--help` shows when the command's own long text has none, and the related
//! commands `--help` names. One row per command path; tests hold the table to the
//! command tree (a row for every path, no row for a path that is gone), every example to
//! the parser (it parses, as a command at or below its row's path), and every related
//! command to a path that exists.

/// A command path's examples and related commands.
#[derive(Debug, Clone, Copy)]
pub struct Extras {
    /// The canonical command path (`backup restore`).
    pub path: &'static str,
    /// One to three examples, each `(command line, what it does)`; the description may be
    /// empty. A command line may be fed by a pipe or redirected, as a shell would run it.
    pub examples: &'static [(&'static str, &'static str)],
    /// Related command paths, most related first; never the row's own path.
    pub see_also: &'static [&'static str],
}

/// The row for `path`, if any.
pub fn of(path: &str) -> Option<&'static Extras> {
    EXTRAS.iter().find(|extras| extras.path == path)
}

/// Every command path's examples and related commands, in bare help's group order.
pub const EXTRAS: &[Extras] = &[
    // Look.
    Extras {
        path: "status",
        examples: &[
            ("omaddons status", "every client and what needs attention"),
            (
                "omaddons status --cached",
                "no scan: the widget's fast view",
            ),
            (
                "omaddons status -t retail --json",
                "one client, as one JSON envelope",
            ),
        ],
        see_also: &["doctor", "outdated", "errors"],
    },
    Extras {
        path: "list",
        examples: &[
            ("omaddons list", "every client's managed addons"),
            (
                "omaddons list -t forever --unmanaged",
                "also the folders omaddons did not install",
            ),
            (
                "omaddons list -v",
                "also each addon's files changed on disk",
            ),
        ],
        see_also: &["info", "adopt", "status"],
    },
    Extras {
        path: "outdated",
        examples: &[
            ("omaddons outdated", "what has updates, on every client"),
            (
                "omaddons outdated --check",
                "exit 6 when any update is available",
            ),
        ],
        see_also: &["update", "pin"],
    },
    Extras {
        path: "info",
        examples: &[
            (
                "omaddons info bigwigs",
                "where it is installed, and its newest release",
            ),
            (
                "omaddons info dbm --changelog",
                "also the notes of every newer release",
            ),
        ],
        see_also: &["search", "list"],
    },
    Extras {
        path: "search",
        examples: &[
            (
                "omaddons search damage meter -t retail",
                "every source at once, judged for retail",
            ),
            (
                "omaddons search questie --source catalog",
                "the built-in catalog only",
            ),
        ],
        see_also: &["install", "info"],
    },
    Extras {
        path: "errors",
        examples: &[
            (
                "omaddons errors",
                "which addon throws Lua errors, on every client",
            ),
            (
                "omaddons errors -t retail --since 1d",
                "one client, the last day only",
            ),
            (
                "omaddons errors dismiss -t retail",
                "mark what was read as seen",
            ),
        ],
        see_also: &["errors dismiss", "update", "status"],
    },
    Extras {
        path: "errors dismiss",
        examples: &[
            (
                "omaddons errors dismiss",
                "every client's new errors, marked seen",
            ),
            (
                "omaddons errors dismiss -t retail --dry-run",
                "what it would mark, and its token",
            ),
        ],
        see_also: &["errors"],
    },
    Extras {
        path: "export",
        examples: &[
            (
                "omaddons export > addons.toml",
                "every client's addon set, printed",
            ),
            (
                "omaddons export --write",
                "into the config directory, keeping a .bak",
            ),
        ],
        see_also: &["sync", "copy"],
    },
    Extras {
        path: "history",
        examples: &[
            ("omaddons history", "the newest changes, by whom and why"),
            (
                "omaddons history --by agent --since 12h",
                "what an agent changed today",
            ),
        ],
        see_also: &["undo", "plans show"],
    },
    Extras {
        path: "plans",
        examples: &[(
            "omaddons plans show 01M8Q2VA",
            "what a stored plan approved, and whether it ran",
        )],
        see_also: &["apply", "history"],
    },
    Extras {
        path: "plans show",
        examples: &[
            (
                "omaddons plans show 01M8Q2VA",
                "what it approved, and whether it ran",
            ),
            (
                "omaddons plans show 01M8Q2VA --json",
                "the same, as one envelope",
            ),
        ],
        see_also: &["apply", "history"],
    },
    Extras {
        path: "net-log",
        examples: &[
            ("omaddons net-log", "the 50 most recent requests, by host"),
            (
                "omaddons net-log --since 1d --json",
                "the last day's, as data",
            ),
        ],
        see_also: &["capabilities"],
    },
    Extras {
        path: "doctor",
        examples: &[
            (
                "omaddons doctor",
                "what is broken, with evidence and the fix",
            ),
            (
                "omaddons doctor explain fm-configs-config-parse-error",
                "one failure mode in full",
            ),
            (
                "omaddons doctor fix --dry-run",
                "what a repair would change",
            ),
        ],
        see_also: &["doctor explain", "doctor fix", "status"],
    },
    Extras {
        path: "doctor diagnose",
        examples: &[
            ("omaddons doctor diagnose --quick", "the cheap checks only"),
            (
                "omaddons doctor diagnose --since latest",
                "what changed since the last run",
            ),
        ],
        see_also: &["doctor explain", "doctor fix"],
    },
    Extras {
        path: "doctor explain",
        examples: &[
            (
                "omaddons doctor explain fm-configs-config-parse-error",
                "every instance",
            ),
            (
                "omaddons doctor explain fm-userland_state-target-vanished -t era",
                "one client's",
            ),
        ],
        see_also: &["doctor", "doctor fix"],
    },
    Extras {
        path: "doctor fix",
        examples: &[
            (
                "omaddons doctor fix --dry-run",
                "what it would repair, and its token",
            ),
            (
                "omaddons doctor fix --if-match <token>",
                "exactly that repair",
            ),
        ],
        see_also: &["doctor explain", "doctor undo", "doctor diff"],
    },
    Extras {
        path: "doctor diff",
        examples: &[
            (
                "omaddons doctor diff",
                "what a default doctor fix would change",
            ),
            (
                "omaddons doctor diff latest",
                "what changed since the last run",
            ),
        ],
        see_also: &["doctor fix", "doctor"],
    },
    Extras {
        path: "doctor undo",
        examples: &[
            (
                "omaddons doctor undo --dry-run",
                "what undoing the newest repair would do",
            ),
            (
                "omaddons doctor undo",
                "undo the newest repair still in effect",
            ),
        ],
        see_also: &["doctor fix", "undo", "history"],
    },
    Extras {
        path: "doctor gc",
        examples: &[
            (
                "omaddons doctor gc --before 30d --dry-run",
                "what would be deleted, and why",
            ),
            (
                "omaddons doctor gc --before 30d --yes --if-match <token>",
                "exactly that",
            ),
        ],
        see_also: &["doctor undo", "doctor"],
    },
    // Change.
    Extras {
        path: "install",
        examples: &[
            (
                "omaddons install dbm bigwigs -t retail",
                "catalog names, in one transaction",
            ),
            (
                "omaddons install gh:tullamods/OmniCC --dry-run",
                "preview it, and store the plan",
            ),
            (
                "omaddons install ./MyAddon.zip -t era",
                "a local zip or folder",
            ),
        ],
        see_also: &["search", "apply", "undo"],
    },
    Extras {
        path: "update",
        examples: &[
            ("omaddons update", "every addon on every client"),
            ("omaddons update dbm -t retail", "one addon on one client"),
            ("omaddons update --dry-run", "what would change"),
        ],
        see_also: &["outdated", "pin", "undo"],
    },
    Extras {
        path: "pin",
        examples: &[
            (
                "omaddons pin dbm",
                "hold it at its release, on every client",
            ),
            (
                "omaddons pin dbm --version 12.1.14 -t retail",
                "install that release, then hold it",
            ),
        ],
        see_also: &["unpin", "update"],
    },
    Extras {
        path: "unpin",
        examples: &[
            ("omaddons unpin dbm", "let it update again, on every client"),
            (
                "omaddons unpin dbm -t retail --dry-run",
                "what it would unpin",
            ),
        ],
        see_also: &["pin", "update"],
    },
    Extras {
        path: "adopt",
        examples: &[
            ("omaddons adopt", "where each unmanaged folder comes from"),
            (
                "omaddons adopt --apply -t retail",
                "adopt what is identified, as it is",
            ),
            (
                "omaddons adopt Bagnon --ref wowi:25068 -t retail",
                "say where a folder comes from",
            ),
        ],
        see_also: &["list", "install"],
    },
    Extras {
        path: "sync",
        examples: &[
            ("omaddons sync", "every client made to match addons.toml"),
            (
                "omaddons sync addons.toml --locked",
                "the exact versions the file records",
            ),
            (
                "omaddons sync - --prune --dry-run",
                "a list piped in, previewed with what it would remove",
            ),
        ],
        see_also: &["export", "update"],
    },
    Extras {
        path: "copy",
        examples: &[
            (
                "omaddons copy --from era --to anniversary",
                "every explicit addon of era",
            ),
            (
                "omaddons copy --from era --to anniversary Questie",
                "only the addons named",
            ),
        ],
        see_also: &["install", "export"],
    },
    Extras {
        path: "remove",
        examples: &[
            ("omaddons remove dbm -t retail", "remove it, undoably"),
            (
                "omaddons remove dbm -t retail --dry-run",
                "what it would remove, and its plan",
            ),
        ],
        see_also: &["undo", "list"],
    },
    Extras {
        path: "undo",
        examples: &[
            ("omaddons undo", "revert the last run, on every client"),
            (
                "omaddons undo --last-by agent",
                "revert what an agent did last",
            ),
            (
                "omaddons undo 01M51F4QY5KB --saved-variables restore",
                "finish after quitting",
            ),
        ],
        see_also: &["history", "remove"],
    },
    Extras {
        path: "apply",
        examples: &[
            (
                "omaddons apply 01M8Q2VA",
                "run exactly what that preview showed",
            ),
            (
                "omaddons apply 01M8Q2VA --dry-run",
                "check it against the world now",
            ),
        ],
        see_also: &["plans show", "install", "update"],
    },
    Extras {
        path: "backup",
        examples: &[
            ("omaddons backup", "every client's WTF/ backups"),
            (
                "omaddons backup create -t retail",
                "back up one client's WTF/ now",
            ),
            (
                "omaddons backup restore 01M6Q2VA --addon WeakAuras --dry-run",
                "one addon's settings back, previewed",
            ),
        ],
        see_also: &["backup list", "backup restore"],
    },
    Extras {
        path: "backup list",
        examples: &[
            (
                "omaddons backup list",
                "every client's backups, newest first",
            ),
            (
                "omaddons backup list -t retail --limit 5",
                "one client's newest five",
            ),
        ],
        see_also: &["backup create", "backup restore"],
    },
    Extras {
        path: "backup create",
        examples: &[
            (
                "omaddons backup create",
                "every live client that has a WTF/",
            ),
            (
                "omaddons backup create -t era --verify",
                "also hash every file copied",
            ),
        ],
        see_also: &["backup list", "backup verify"],
    },
    Extras {
        path: "backup restore",
        examples: &[
            (
                "omaddons backup restore 01M6Q2VA --dry-run",
                "what it would put back",
            ),
            (
                "omaddons backup restore 01M6Q2VA --addon WeakAuras",
                "one addon's settings only",
            ),
        ],
        see_also: &["backup list", "undo"],
    },
    Extras {
        path: "backup prune",
        examples: &[
            (
                "omaddons backup prune --dry-run",
                "what retention would delete now",
            ),
            (
                "omaddons backup prune -t retail --keep 3",
                "also your own beyond the newest 3",
            ),
        ],
        see_also: &["backup list"],
    },
    Extras {
        path: "backup export",
        examples: &[(
            "omaddons backup export 01M6Q2VA /run/media/me/usb/retail-wtf.tar.zst",
            "one backup, off this disk",
        )],
        see_also: &["backup import", "backup list"],
    },
    Extras {
        path: "backup import",
        examples: &[(
            "omaddons backup import /run/media/me/usb/retail-wtf.tar.zst -t retail",
            "an exported backup, back as a backup of retail",
        )],
        see_also: &["backup export", "backup restore"],
    },
    Extras {
        path: "backup verify",
        examples: &[
            (
                "omaddons backup verify 01M6Q2VA",
                "check it against its hashes",
            ),
            (
                "omaddons backup verify 01M6Q2VA --dry-run",
                "check it, recording nothing",
            ),
        ],
        see_also: &["backup list", "backup create"],
    },
    // Setup.
    Extras {
        path: "targets",
        examples: &[
            ("omaddons targets", "every client, with its paths"),
            ("omaddons targets --rescan", "ignore the discovery cache"),
            (
                "omaddons targets add /mnt/games/turtle --name turtle",
                "one discovery missed",
            ),
        ],
        see_also: &["status", "targets add"],
    },
    Extras {
        path: "targets add",
        examples: &[
            (
                "omaddons targets add /mnt/games/turtle --name turtle",
                "a root, client or AddOns",
            ),
            (
                "omaddons targets add /mnt/games/turtle --dry-run",
                "what it would register",
            ),
        ],
        see_also: &["targets", "config set"],
    },
    Extras {
        path: "targets rm",
        examples: &[
            ("omaddons targets rm warmane", "forget it; its files stay"),
            (
                "omaddons targets rm classic --purge --dry-run",
                "an uninstalled client: what a purge deletes",
            ),
            (
                "omaddons targets rm classic --purge --yes --if-match <token>",
                "delete exactly that preview",
            ),
        ],
        see_also: &["targets", "backup list"],
    },
    Extras {
        path: "targets rename",
        examples: &[(
            "omaddons targets rename wotlk warmane",
            "-t warmane from now on",
        )],
        see_also: &["targets"],
    },
    Extras {
        path: "targets default",
        examples: &[(
            "omaddons targets default retail",
            "install's client without -t",
        )],
        see_also: &["targets", "config set"],
    },
    Extras {
        path: "config",
        examples: &[
            ("omaddons config", "every setting, and which are defaults"),
            ("omaddons config get running_policy", "one setting"),
            (
                "omaddons config set default_channel beta",
                "change one, keeping comments",
            ),
        ],
        see_also: &["config set", "capabilities"],
    },
    Extras {
        path: "config get",
        examples: &[(
            "omaddons config get running_policy",
            "its value, and where it comes from",
        )],
        see_also: &["config set", "config"],
    },
    Extras {
        path: "config set",
        examples: &[
            (
                "omaddons config set default_channel beta",
                "one value, in place",
            ),
            (
                "omaddons config set git_hosts '[\"codeberg.org\"]'",
                "arrays take TOML syntax",
            ),
        ],
        see_also: &["config get", "config path"],
    },
    Extras {
        path: "config path",
        examples: &[("omaddons config path", "where config.toml is")],
        see_also: &["config"],
    },
    Extras {
        path: "auth",
        examples: &[
            (
                "omaddons auth github",
                "which token is used, and the hour's budget",
            ),
            ("omaddons auth github --from-gh", "store the token gh has"),
        ],
        see_also: &["auth github", "doctor"],
    },
    Extras {
        path: "auth github",
        examples: &[
            (
                "echo \"$TOKEN\" | omaddons auth github set",
                "store a token from standard input",
            ),
            ("omaddons auth github --from-gh", "store the token gh has"),
            ("omaddons auth github clear", "forget the stored token"),
        ],
        see_also: &["doctor"],
    },
    Extras {
        path: "omarchy",
        examples: &[
            (
                "omaddons omarchy",
                "is the bar widget installed and current?",
            ),
            ("omaddons omarchy install", "put it on the bar"),
            (
                "omaddons omarchy remove",
                "take it off the bar and delete it",
            ),
        ],
        see_also: &["omarchy install", "status"],
    },
    Extras {
        path: "omarchy status",
        examples: &[("omaddons omarchy status", "installed, current, on the bar?")],
        see_also: &["omarchy install"],
    },
    Extras {
        path: "omarchy install",
        examples: &[
            (
                "omaddons omarchy install",
                "install it and put it on the bar",
            ),
            (
                "omaddons omarchy install --section left",
                "where it goes on the bar",
            ),
            (
                "omaddons omarchy install --dry-run",
                "render and validate; write nothing",
            ),
        ],
        see_also: &["omarchy status", "omarchy remove"],
    },
    Extras {
        path: "omarchy remove",
        examples: &[
            (
                "omaddons omarchy remove",
                "take it off the bar and delete it",
            ),
            ("omaddons omarchy remove --dry-run", "what it would do"),
        ],
        see_also: &["omarchy install"],
    },
    Extras {
        path: "completions",
        examples: &[
            (
                "omaddons completions bash > ~/.local/share/bash-completion/completions/omaddons",
                "",
            ),
            ("omaddons completions zsh > ~/.zfunc/_omaddons", ""),
        ],
        see_also: &["install-completions", "help"],
    },
    Extras {
        path: "install-completions",
        examples: &[
            (
                "omaddons install-completions bash",
                "where bash-completion looks",
            ),
            (
                "omaddons install-completions zsh --dry-run",
                "where it would write, and what",
            ),
        ],
        see_also: &["completions"],
    },
    Extras {
        path: "robot-docs",
        examples: &[
            ("omaddons robot-docs", "the agent guide, as Markdown"),
            (
                "omaddons robot-docs schema install --json",
                "one command's data schema",
            ),
            (
                "omaddons robot-docs install-skill",
                "the guide as a Claude Code skill",
            ),
        ],
        see_also: &["capabilities", "help"],
    },
    Extras {
        path: "robot-docs guide",
        examples: &[("omaddons robot-docs guide", "the agent guide, as Markdown")],
        see_also: &["robot-docs install-skill", "capabilities"],
    },
    Extras {
        path: "robot-docs schema",
        examples: &[
            ("omaddons robot-docs schema --json", "every schema"),
            (
                "omaddons robot-docs schema backup restore",
                "one command's data, by its path",
            ),
        ],
        see_also: &["robot-docs errors", "capabilities"],
    },
    Extras {
        path: "robot-docs errors",
        examples: &[(
            "omaddons robot-docs errors --json",
            "every error and warning code",
        )],
        see_also: &["robot-docs schema"],
    },
    Extras {
        path: "robot-docs install-skill",
        examples: &[
            (
                "omaddons robot-docs install-skill",
                "into ~/.claude/skills/omaddons",
            ),
            (
                "omaddons robot-docs install-skill --dir ~/.codex/skills/omaddons",
                "",
            ),
        ],
        see_also: &["robot-docs guide", "doctor"],
    },
    Extras {
        path: "capabilities",
        examples: &[
            ("omaddons capabilities", "a summary of this build"),
            ("omaddons capabilities --json", "everything, as data"),
        ],
        see_also: &["robot-docs", "help"],
    },
    Extras {
        path: "help",
        examples: &[
            ("omaddons help install", "one command's whole page"),
            ("omaddons help targets add", "a nested command, by its path"),
        ],
        see_also: &["capabilities", "robot-docs guide"],
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::{CommandFactory, Parser};

    use super::*;
    use crate::cli::Cli;

    /// Every visible command path of the tree, by its canonical words.
    fn command_paths() -> BTreeSet<String> {
        fn walk(cmd: &clap::Command, path: &mut Vec<String>, out: &mut BTreeSet<String>) {
            for sub in cmd.get_subcommands().filter(|sub| !sub.is_hide_set()) {
                path.push(sub.get_name().to_owned());
                out.insert(path.join(" "));
                walk(sub, path, out);
                path.pop();
            }
        }
        let mut root = Cli::command();
        root.build();
        let mut out = BTreeSet::new();
        walk(&root, &mut Vec::new(), &mut out);
        out
    }

    /// The omaddons argv of an example's command line: from the word `omaddons` to a
    /// pipe or a redirection, as a shell would run it.
    fn argv_of(line: &str) -> Vec<String> {
        let words = shell_words::split(line).unwrap_or_else(|e| panic!("{line:?}: {e}")); // ubs:ignore — a table row's own failure
        let start = words.iter().position(|w| w == "omaddons");
        let start = start.unwrap_or_else(|| panic!("{line:?} runs no omaddons")); // ubs:ignore — a table row's own failure
        words[start..]
            .iter()
            .take_while(|w| !matches!(w.as_str(), "|" | ">" | ">>" | "<"))
            .cloned()
            .collect()
    }

    #[test]
    fn every_command_path_has_one_row_and_every_row_a_path() {
        let paths = command_paths();
        let rows: Vec<&str> = EXTRAS.iter().map(|extras| extras.path).collect();
        let unique: BTreeSet<&str> = rows.iter().copied().collect();
        assert_eq!(unique.len(), rows.len(), "a path listed twice: {rows:?}");
        for path in &paths {
            assert!(
                unique.contains(path.as_str()),
                "no help extras for `{path}`"
            );
        }
        for row in &rows {
            assert!(paths.contains(*row), "`{row}` is no command path");
        }
    }

    /// Each example parses, as its row's command or one below it, and each row has one
    /// to three of them.
    #[test]
    fn every_example_parses_as_its_command() {
        for extras in EXTRAS {
            assert!(
                (1..=3).contains(&extras.examples.len()),
                "{}: {} examples",
                extras.path,
                extras.examples.len()
            );
            for (line, _) in extras.examples {
                let argv = argv_of(line);
                if let Err(error) = Cli::try_parse_from(&argv) {
                    panic!("{}: {line:?} does not parse: {error}", extras.path); // ubs:ignore — a table row's own failure
                }
                let path = crate::cli::command_path(&argv);
                let row: Vec<&str> = extras.path.split(' ').collect();
                assert!(
                    path.len() >= row.len() && path.iter().zip(&row).all(|(a, b)| a == b),
                    "{}: {line:?} runs `{}`",
                    extras.path,
                    path.join(" ")
                );
            }
        }
    }

    #[test]
    fn every_related_command_exists_and_is_another() {
        let paths = command_paths();
        for extras in EXTRAS {
            for related in extras.see_also {
                assert!(
                    paths.contains(*related),
                    "{}: `{related}` is no command",
                    extras.path
                );
                assert_ne!(*related, extras.path, "{}: names itself", extras.path);
            }
        }
    }
}
