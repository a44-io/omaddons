//! The bar widget's contract with the CLI (M3-D2, M3-D41): `omarchy/Model.js`'s
//! template table and path registry against the binary's own, the widget constants
//! against `politeness.rs` and `limits.rs`, and the two pipeline steps around bun's
//! tests. `write_widget_fixtures` writes real envelopes for bun's reducers;
//! `validate_built_argv` parses exactly the argv bun built. Both are `#[ignore]`: the
//! widget pipeline (`scripts/gate.sh`, `argc widget-test`) runs them in order, with a
//! fresh `$OMADDONS_WIDGET_OUT`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use common::home::Home;
use common::http::{gh_feed, gh_latest, gh_release, github_search};
use common::tukui::{elvui, tukui_list};
use common::wowi::{FileEntry, wowi_filelist};
use omaddons::app::precondition::BINDING_COMMANDS;
use omaddons::app::{self, command_name};
use omaddons::cli::Cli;
use serde_json::Value;
use support::{
    DESTRUCTIVE, Rel, behind, cli_paths, constant, fill, leading_path, model_js, out_dir, parse,
    positional_and_option_values, strings, templates, widget_flags, write_fixture,
};

/// `CLI_PATHS` has one row per registry path, with the registry's `mutates`, the
/// destructive paths and the binding paths (A4's `BINDING_COMMANDS`).
#[test]
fn cli_paths_mirror_the_registry() {
    let rows = cli_paths();
    let registry: BTreeSet<&str> = app::COMMANDS.iter().map(|c| c.name).collect();
    let listed: BTreeSet<&str> = rows.keys().map(String::as_str).collect();
    assert_eq!(listed, registry, "CLI_PATHS rows differ from app::COMMANDS");
    for (path, row) in &rows {
        assert_eq!(row["mutates"], app::mutates(path), "{path}: mutates");
        assert_eq!(
            row["destructive"],
            DESTRUCTIVE.contains(&path.as_str()),
            "{path}: destructive"
        );
        assert_eq!(
            row["binds"],
            BINDING_COMMANDS.contains(&path.as_str()),
            "{path}: binds"
        );
    }
    for path in DESTRUCTIVE.iter().chain(BINDING_COMMANDS) {
        assert!(registry.contains(path), "{path} is no registry path");
    }
}

/// Every template is a registry command whose `mutates` is the registry's, previews
/// what is destructive, sets no flag the widget owns, and keeps every slot one argv
/// element where a hostile value cannot become a flag (M3-D2).
#[test]
fn every_template_is_a_registry_command_with_safe_slots() {
    let owned = widget_flags();
    for (name, template) in templates() {
        let argv = strings(&template["argv"]);
        let path = leading_path(&argv);
        assert!(path.is_some(), "{name}: no command path");
        let path = path.unwrap_or_default();
        let mutates = template["mutates"] == true;
        assert_eq!(mutates, app::mutates(&path), "{name}: mutates");
        if template["confirm"] == true {
            assert!(
                DESTRUCTIVE.contains(&path.as_str()),
                "{name}: confirm on {path}"
            );
        }
        if DESTRUCTIVE.contains(&path.as_str()) {
            assert!(template["confirm"] == true, "{name}: {path} needs confirm");
        }
        if mutates {
            assert!(
                template.get("lane").is_none(),
                "{name}: a mutation has no lane"
            );
        } else {
            assert!(
                template["lane"] == "local" || template["lane"] == "network",
                "{name}: lane"
            );
        }
        let separator = argv.iter().position(|a| a == "--");
        for (i, part) in argv.iter().enumerate() {
            let after = separator.is_some_and(|s| i > s);
            let flag = part.split('=').next().unwrap_or(part);
            if part.starts_with('-') && Some(i) != separator {
                assert!(!after, "{name}: flag {part} after --");
                assert!(
                    !owned.contains(&flag.to_owned()) && flag != "--dry-run",
                    "{name}: {flag} is the widget's"
                );
            }
            let Some(open) = part.find('{') else { continue };
            let slot = &part[open + 1..part.len() - 1];
            assert!(part.ends_with('}'), "{name}: malformed slot {part}");
            assert!(
                ["target", "addon", "ref", "backup", "query"].contains(&slot),
                "{name}: unknown slot {part}"
            );
            if slot == "query" {
                assert!(!mutates, "{name}: free text in a mutation");
            }
            if open == 0 {
                let target_value =
                    slot == "target" && i > 0 && (argv[i - 1] == "-t" || argv[i - 1] == "--target");
                assert!(after || target_value, "{name}: positional {part} before --");
            } else {
                assert!(
                    part.starts_with("--") && part[..open].ends_with('=') && !after,
                    "{name}: {part} is not --opt={{slot}} before --"
                );
            }
        }
    }
}

/// `omarchy/manifest.json` is exactly the planned manifest (F1, M3-D5, M3-D10); E1 stamps
/// its `version` at install.
#[test]
fn the_manifest_is_the_planned_one() {
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy/manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let expected = serde_json::json!({
      "schemaVersion": 1,
      "id": "a44.omaddons",
      "name": "omaddons",
      "version": "0.0.0-unstamped",
      "author": "a44",
      "description": "World of Warcraft addons on every client: updates, Lua errors and WTF backups, from the omaddons CLI.",
      "kinds": ["service", "bar-widget"],
      "entryPoints": { "service": "Service.qml", "barWidget": "BarWidget.qml" },
      "barWidget": {
        "displayName": "omaddons",
        "description": "One gamepad for every WoW client: a badge for pending addon updates, urgent when something is broken or an addon threw Lua errors, and a keyboard panel to update, undo, back up and restore. Every action is an omaddons command line, recorded and undoable.",
        "category": "Games",
        "aliases": ["omaddons", "wow", "warcraft", "addons"],
        "allowMultiple": false,
        "defaultSection": "right",
        "defaults": { "binary": "", "panelWidth": 540, "autoCheck": "On", "notifyUpdates": "Off", "notifyErrors": "On", "refreshIntervalSec": 300 },
        "schema": [
          { "key": "binary", "type": "path", "label": "omaddons binary", "defaultValue": "", "description": "Empty runs the binary that installed the widget (omaddons omarchy install stamps its path). Set a path to run another build." },
          { "key": "panelWidth", "type": "integer", "label": "Panel width", "min": 400, "max": 800, "step": 20, "defaultValue": 540 },
          { "key": "autoCheck", "type": "enum", "label": "Check for updates when due", "options": ["On", "Off"], "defaultValue": "On", "description": "Run omaddons outdated in the background when a client's last check is older than check_interval_hours in omaddons' config.toml." },
          { "key": "notifyUpdates", "type": "enum", "label": "Notify about new updates", "options": ["On", "Off"], "defaultValue": "Off" },
          { "key": "notifyErrors", "type": "enum", "label": "Notify about Lua errors after a session", "options": ["On", "Off"], "defaultValue": "On" },
          { "key": "refreshIntervalSec", "type": "integer", "label": "Fallback refresh (seconds)", "min": 30, "max": 3600, "step": 30, "defaultValue": 300, "description": "Safety-net refresh behind the doorbell; 15 s while the panel is open." }
        ]
      }
    });
    assert_eq!(manifest, expected);
}

/// The widget's id is a literal in manifest.json only; every other use goes through
/// `Build.PLUGIN_ID`, so E1's debug id seam yields a separate widget (F7 check 8). An
/// id that merely starts with it (`a44.omaddons-fixture`) is another id.
#[test]
fn the_plugin_id_is_a_literal_in_the_manifest_only() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy");
    let mut stack = vec![root.clone()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_code = path.extension().is_some_and(|e| e == "qml" || e == "js");
            if !is_code {
                continue;
            }
            seen += 1;
            let text = fs::read_to_string(&path).unwrap();
            for (at, _) in text.match_indices("a44.omaddons") {
                let next = text[at + "a44.omaddons".len()..].chars().next();
                assert!(
                    next.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_'),
                    "{}: the literal widget id",
                    path.strip_prefix(&root).unwrap().display()
                );
            }
        }
    }
    assert!(seen > 0, "no QML or JS under omarchy/");
}

/// The embedded asset table is exactly the runtime files under `omarchy/`, its tests and
/// a generated `Build.js` aside, so a view added there is embedded in the same commit
/// (E1). A row naming a missing file already fails the build, through `include_str!`.
#[test]
fn every_runtime_file_is_embedded() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy");
    let mut on_disk = BTreeSet::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if relative == "tests" || relative == "Build.js" {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                on_disk.insert(relative);
            }
        }
    }
    let embedded: BTreeSet<String> = omaddons::omarchy::assets::ASSETS
        .iter()
        .map(|(path, _)| (*path).to_owned())
        .collect();
    assert_eq!(on_disk, embedded);
}

/// The flags the widget owns and puts first are the binary's global flags, so a clap
/// rename fails here and in `validate_built_argv`.
#[test]
fn the_widget_flags_are_global_flags_of_the_binary() {
    use clap::CommandFactory;
    let cli = Cli::command();
    let globals: BTreeSet<String> = cli
        .get_arguments()
        .filter(|a| a.is_global_set())
        .filter_map(|a| a.get_long().map(|l| format!("--{l}")))
        .collect();
    for flag in widget_flags()
        .iter()
        .map(String::as_str)
        .chain(["--dry-run"])
    {
        assert!(
            globals.contains(flag),
            "{flag} is no global flag of omaddons"
        );
    }
}

/// A network read's inactivity deadline outlasts the longest request politeness.rs
/// allows: every attempt timing out, and every retry waiting the longest Retry-After.
#[test]
fn the_network_deadline_outlasts_the_longest_polite_request() {
    use omaddons::politeness::{API_READ_TIMEOUT, MAX_RETRIES, RETRY_AFTER_CAP};
    let deadline = constant(&model_js(), "WIDGET_NETWORK_DEADLINE_SEC");
    let longest = u64::from(MAX_RETRIES + 1) * API_READ_TIMEOUT.as_secs()
        + u64::from(MAX_RETRIES) * RETRY_AFTER_CAP.as_secs();
    assert!(
        deadline >= longest,
        "WIDGET_NETWORK_DEADLINE_SEC {deadline} < {longest}"
    );
}

/// Every template's worst-case output stays under `WIDGET_OUTPUT_MAX_BYTES`. The worst
/// case is every Battle.net flavor plus two custom clients, 400 addons each, all with
/// an update. Templates that list every addon are measured at 10 and 30 addons and
/// extrapolated per addon; one-addon templates are bounded by one client's share at
/// one addon; `search` and the changelog by their limits.
#[test]
fn the_output_cap_bounds_every_template() {
    use omaddons::limits::{
        CHANGELOG_MAX_CHARS, CHANGELOG_MAX_ENTRIES, ERRORS_LOCALS_MAX_CHARS,
        ERRORS_MESSAGE_MAX_CHARS, ERRORS_SHOWN_PER_ADDON, ERRORS_STACK_LINES_VERBOSE,
        SEARCH_MAX_RESULTS, SEARCH_SUMMARY_MAX_CHARS,
    };
    // A raw stack frame's characters (a path and a function name).
    const FRAME: u64 = 512;
    const CLIENTS: u64 = 12;
    const ADDONS: u64 = 400;
    // Room per row for what the limits do not cap (names, refs, dates), and per answer.
    const ROW: u64 = 4096;
    const ANSWER: u64 = 64 * 1024;
    let cap = constant(&model_js(), "WIDGET_OUTPUT_MAX_BYTES");
    let t = templates();
    let measured = ["status", "status_full", "list", "check", "update_all"];
    let sizes = |n: usize| -> BTreeMap<&str, u64> {
        let home = behind(n);
        let mut out = BTreeMap::new();
        for name in measured {
            let run = home.exec(&fill(&t, name, &[]));
            run.expect(0);
            out.insert(name, run.stdout.len() as u64);
        }
        out
    };
    let (small, large) = (sizes(10), sizes(30));
    let mut per_client = BTreeMap::new();
    for name in measured {
        // These list every addon; a flat answer means the homes did not differ.
        if matches!(name, "list" | "check" | "update_all") {
            assert!(large[name] > small[name], "{name}: {small:?} {large:?}");
        }
        let slope = large[name].saturating_sub(small[name]).div_ceil(20);
        let base = small[name].saturating_sub(10 * slope);
        per_client.insert(name, (base, slope));
        let worst = CLIENTS * (base + ADDONS * slope);
        assert!(worst < cap, "{name}: worst case {worst} bytes ≥ {cap}");
    }
    let (base, slope) = per_client["update_all"];
    for (name, template) in &t {
        let argv = template["argv"].to_string();
        let bound = if measured.contains(&name.as_str()) {
            continue;
        } else if name == "update_target" {
            Some(base + ADDONS * slope)
        } else if argv.contains("{query}") {
            Some(SEARCH_MAX_RESULTS as u64 * (SEARCH_SUMMARY_MAX_CHARS as u64 * 6 + ROW) + ANSWER)
        } else if argv.contains("--changelog") {
            Some(CHANGELOG_MAX_ENTRIES as u64 * (CHANGELOG_MAX_CHARS as u64 * 6 + ROW) + ANSWER)
        } else if name == "errors" {
            // `--limit` addons per client, each listing at most ERRORS_SHOWN_PER_ADDON
            // strict errors.
            let args = template["argv"].as_array().unwrap();
            let at = args.iter().position(|a| a == "--limit").unwrap();
            let limit: u64 = args[at + 1].as_str().unwrap().parse().unwrap();
            Some(CLIENTS * (limit * ERRORS_SHOWN_PER_ADDON as u64 * ROW + ANSWER))
        } else if name == "errors_raw" {
            // One client, one addon, each error with its raw message, locals and the
            // verbose stack beside the strict one.
            let raw = (ERRORS_MESSAGE_MAX_CHARS + ERRORS_LOCALS_MAX_CHARS) as u64 * 6
                + 2 * ERRORS_STACK_LINES_VERBOSE as u64 * FRAME * 6;
            Some(ERRORS_SHOWN_PER_ADDON as u64 * (raw + ROW) + ANSWER)
        } else if name == "dismiss_errors" {
            // Counts per client, never a list of errors.
            Some(CLIENTS * ROW + ANSWER)
        } else if argv.contains("{addon}") || argv.contains("{ref}") {
            Some(base + slope)
        } else {
            None
        };
        assert!(bound.is_some(), "{name}: bound its output in this test");
        assert!(
            bound.is_some_and(|b| b < cap),
            "{name}: worst case {bound:?} bytes ≥ {cap}"
        );
    }
}

/// Pipeline step 3: every argv bun built (`built-argv.json`) parses with the binary's
/// clap as the command it was built for, with the widget's flags first and every
/// hostile slot value arriving whole as its positional or option value; every CLI argv
/// the classifier accepted parses as the path it reported, and every one it refused is
/// refused for the stated reason. Nothing is substituted here.
#[test]
#[ignore = "run by the widget pipeline after bun wrote built-argv.json"]
fn validate_built_argv() {
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(out_dir().join("built-argv.json")).unwrap())
            .unwrap();
    let built = doc["built"].as_array().unwrap();
    assert!(!built.is_empty(), "bun built nothing");
    let names: BTreeSet<&str> = built
        .iter()
        .filter_map(|e| e["template"].as_str())
        .collect();
    let shipped = templates();
    assert_eq!(
        names,
        shipped.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        "bun skipped a template"
    );
    for entry in built {
        let argv = strings(&entry["argv"]);
        let cli = parse(&argv);
        assert_eq!(argv[0], doc["binary"], "{argv:?}");
        assert_eq!(
            argv[1..5],
            ["--json", "--actor", "widget", "--reason"],
            "{argv:?}: the standard flags come first"
        );
        // Past `--` clap reads a flag as one more positional, and parsing still works:
        // only slot values may follow it.
        let slots: Vec<&str> = entry["slots"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(Value::as_str)
            .collect();
        if let Some(separator) = argv.iter().position(|a| a == "--") {
            for part in &argv[separator + 1..] {
                assert!(
                    slots.contains(&part.as_str()),
                    "{argv:?}: {part:?} after -- is no slot value"
                );
            }
        }
        assert!(cli.global.json, "{argv:?}: --json");
        assert_eq!(cli.global.actor.as_deref(), Some("widget"), "{argv:?}");
        assert!(
            cli.global
                .reason
                .as_deref()
                .is_some_and(|r| r.ends_with(" (bar widget)")),
            "{argv:?}: --reason"
        );
        let path = command_name(cli.command.as_ref());
        assert_eq!(path, entry["path"], "{argv:?}");
        assert_eq!(entry["mutates"], app::mutates(path), "{argv:?}");
        assert_eq!(cli.global.dry_run, entry["preview"] == true, "{argv:?}");
        assert_eq!(
            cli.global.if_match.as_deref().unwrap_or(""),
            entry["token"].as_str().unwrap(),
            "{argv:?}"
        );
        let template = strings(&shipped[entry["template"].as_str().unwrap()]["argv"]);
        let values = positional_and_option_values(&argv);
        for (slot, value) in entry["slots"].as_object().unwrap() {
            let value = value.as_str().unwrap();
            if slot == "target" {
                assert_eq!(cli.global.target.as_deref(), Some(value), "{argv:?}");
                continue;
            }
            let element = template
                .iter()
                .find(|p| p.ends_with(&format!("{{{slot}}}")));
            let expected = match element.and_then(|p| p.strip_suffix(&format!("={{{slot}}}"))) {
                Some(option) => (option.trim_start_matches('-').to_owned(), value.to_owned()),
                None => (String::new(), value.to_owned()),
            };
            assert!(
                values.contains(&expected),
                "{argv:?}: {{{slot}}} = {value:?} did not arrive as {expected:?} ({values:?})"
            );
        }
    }

    let owned = widget_flags();
    for entry in doc["classified"].as_array().unwrap() {
        let argv = strings(&entry["argv"]);
        let verdict = &entry["verdict"];
        let end = argv.iter().position(|a| a == "--").unwrap_or(argv.len());
        if verdict["ok"] == true {
            let reported = verdict["path"].as_str().unwrap();
            assert_eq!(
                leading_path(&argv[1..]).as_deref(),
                Some(reported),
                "{argv:?}"
            );
            assert_eq!(verdict["mutates"], app::mutates(reported), "{argv:?}");
            assert_eq!(
                verdict["destructive"],
                DESTRUCTIVE.contains(&reported),
                "{argv:?}"
            );
            assert_eq!(
                verdict["binds"],
                BINDING_COMMANDS.contains(&reported),
                "{argv:?}"
            );
            let runnable = strings(&entry["runnable"]);
            let cli = parse(&runnable);
            let path = command_name(cli.command.as_ref());
            assert!(
                path == reported || (reported == "backup" && path == "backup list"),
                "{runnable:?} parses as {path}, not {reported}"
            );
            assert_eq!(cli.global.actor.as_deref(), Some("widget"), "{runnable:?}");
            if entry["refusal"] == "destructive_without_preview" {
                assert!(
                    DESTRUCTIVE.contains(&reported) && !argv[..end].contains(&"--dry-run".into()),
                    "{argv:?}"
                );
                assert!(cli.global.dry_run, "{runnable:?} runs as a preview");
            } else {
                assert_eq!(entry["refusal"], "", "{argv:?}");
                assert!(
                    !DESTRUCTIVE.contains(&reported) || cli.global.dry_run,
                    "{runnable:?}: a destructive run without its preview"
                );
            }
            continue;
        }
        let reason = verdict["reason"].as_str().unwrap_or_default();
        assert!(
            ["widget_flag", "unknown_path", "not_omaddons", "malformed"].contains(&reason),
            "{argv:?}: unknown reason {reason:?}"
        );
        match reason {
            "widget_flag" => assert!(
                argv[1..end]
                    .iter()
                    .any(|a| owned.contains(&a.split('=').next().unwrap_or(a).to_owned())),
                "{argv:?}"
            ),
            "unknown_path" => assert_eq!(leading_path(&argv[1..end]), None, "{argv:?}"),
            "not_omaddons" => assert_ne!(argv[0], "omaddons", "{argv:?}"),
            _ => assert!(argv.len() < 2, "{argv:?}"),
        }
    }
}

#[cfg(test)]
mod support {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use omaddons::cli::Cli;
    use serde_json::Value;

    use super::common::addons::toc;
    use super::common::home::{Home, Run};
    use super::common::http::{Asset, gh_latest, gh_release_v, paths};

    /// The paths whose runs always go through a preview (M3-D9).
    pub const DESTRUCTIVE: &[&str] = &[
        "remove",
        "undo",
        "backup restore",
        "backup prune",
        "targets rm",
    ];

    /// `omarchy/Model.js`.
    pub fn model_js() -> String {
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("omarchy/Model.js")).unwrap()
    }

    /// The JSON literal of `var <name> = …` between `// BEGIN <name>` and `// END <name>`.
    pub fn block(source: &str, name: &str) -> Value {
        let begin = source.find(&format!("// BEGIN {name}"));
        let end = begin.and_then(|b| source[b..].find(&format!("// END {name}")).map(|e| e + b));
        let assignment = format!("var {name} = ");
        let start = begin.zip(end).and_then(|(b, e)| {
            source[b..e]
                .find(&assignment)
                .map(|s| s + b + assignment.len())
        });
        assert!(
            start.is_some(),
            "Model.js has no `{assignment}` between // BEGIN {name} and // END {name}"
        );
        let (start, end) = (start.unwrap_or_default(), end.unwrap_or_default());
        let parsed: Result<Value, _> = serde_json::from_str(source[start..end].trim());
        assert!(parsed.is_ok(), "{name} is not a JSON literal: {parsed:?}");
        parsed.unwrap_or_default()
    }

    /// The template table.
    pub fn templates() -> BTreeMap<String, Value> {
        serde_json::from_value(block(&model_js(), "COMMANDS")).unwrap()
    }

    /// The widget's path registry.
    pub fn cli_paths() -> BTreeMap<String, Value> {
        serde_json::from_value(block(&model_js(), "CLI_PATHS")).unwrap()
    }

    /// `var WIDGET_FLAGS = [...]`: the flags the widget owns.
    pub fn widget_flags() -> Vec<String> {
        let source = model_js();
        let line = source
            .lines()
            .find(|l| l.starts_with("var WIDGET_FLAGS = "))
            .expect("Model.js defines WIDGET_FLAGS on one line");
        serde_json::from_str(line.split_once(" = ").unwrap().1).unwrap()
    }

    pub fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    }

    /// The registry path an argument list (no program name) starts with: the longest
    /// one its first one or two words form before any flag. The classifier's rule,
    /// written again here so the two implementations can disagree.
    pub fn leading_path(args: &[String]) -> Option<String> {
        let words: Vec<&str> = args
            .iter()
            .take_while(|a| !a.is_empty() && !a.starts_with('-'))
            .take(2)
            .map(String::as_str)
            .collect();
        let known = |path: &str| omaddons::app::COMMANDS.iter().any(|c| c.name == path);
        if let [first, second] = words[..] {
            let two = format!("{first} {second}");
            if known(&two) {
                return Some(two);
            }
        }
        words.first().filter(|w| known(w)).map(|w| (*w).to_owned())
    }

    /// `argv` parsed by the binary's own clap.
    pub fn parse(argv: &[String]) -> Cli {
        use clap::Parser;
        let parsed = Cli::try_parse_from(argv);
        assert!(
            parsed.is_ok(),
            "{argv:?} does not parse: {}",
            parsed
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_default()
        );
        parsed.expect("parsed above")
    }

    /// Every value the innermost subcommand of `argv` received: `("", value)` for a
    /// positional, `(long, value)` for an option.
    pub fn positional_and_option_values(argv: &[String]) -> Vec<(String, String)> {
        use clap::CommandFactory;
        let mut command = Cli::command();
        command.build();
        let mut matches = command.clone().try_get_matches_from(argv).unwrap();
        loop {
            let Some((name, sub)) = matches
                .subcommand()
                .map(|(name, sub)| (name.to_owned(), sub.clone()))
            else {
                break;
            };
            command = command.find_subcommand(&name).unwrap().clone();
            matches = sub;
        }
        let mut out = Vec::new();
        for arg in command.get_arguments() {
            let key = if arg.is_positional() {
                String::new()
            } else if let Some(long) = arg.get_long() {
                long.to_owned()
            } else {
                continue;
            };
            if let Ok(Some(raw)) = matches.try_get_raw(arg.get_id().as_str()) {
                out.extend(raw.map(|v| (key.clone(), v.to_string_lossy().into_owned())));
            }
        }
        out
    }

    /// One retail client with `n` addons one release behind: installed in one
    /// transaction, then a scenario where v2 is every addon's latest, with the release
    /// memo aged a day so the next check asks again.
    pub fn behind(n: usize) -> Home {
        let mut home = Home::new(&["wow"]);
        let names: Vec<String> = (0..n).map(|i| format!("addon{i:03}")).collect();
        let base = |name: &str| format!("https://github.com/o/{name}");
        for name in &names {
            gh_release_v(
                &home.http,
                &base(name),
                "v1",
                &Rel::new(name, "v1").assets(),
            );
            gh_latest(&home.http, &base(name), "v1");
        }
        let mut args = vec!["install".to_owned()];
        args.extend(names.iter().map(|name| format!("gh:o/{name}")));
        args.extend(["-t", "retail", "--json"].map(str::to_owned));
        home.exec(&args).expect(0);
        let latest = home.scenario("http-latest");
        for name in &names {
            gh_release_v(&latest, &base(name), "v2", &Rel::new(name, "v2").assets());
            gh_latest(&latest, &base(name), "v2");
        }
        let (mut memo, _) = omaddons::state::memo::load(&paths(&home.env));
        for entry in memo.entries.values_mut() {
            entry.fetched_at -= jiff::SignedDuration::from_hours(24);
        }
        home.seed_memo(&memo);
        home
    }

    /// The numeric value of `var <name> = <expr>` in Model.js, where `<expr>` is a
    /// number or a product of numbers.
    pub fn constant(source: &str, name: &str) -> u64 {
        let line = source
            .lines()
            .find(|l| l.starts_with(&format!("var {name} = ")));
        assert!(line.is_some(), "Model.js defines no {name}");
        let expr = line
            .and_then(|l| l.split_once(" = "))
            .map(|(_, e)| e)
            .unwrap_or_default();
        expr.split('*')
            .map(|f| f.trim().parse::<u64>().unwrap())
            .product()
    }

    /// `$OMADDONS_WIDGET_OUT`, the pipeline's fresh output directory.
    pub fn out_dir() -> PathBuf {
        PathBuf::from(
            std::env::var_os("OMADDONS_WIDGET_OUT")
                .expect("OMADDONS_WIDGET_OUT is set by the widget pipeline"),
        )
    }

    /// The flags the widget puts before every command (`--progress json` for the runs
    /// that report it).
    pub fn standard(progress: bool) -> Vec<String> {
        let mut argv: Vec<String> = ["--json", "--actor", "widget", "--reason"]
            .map(str::to_owned)
            .to_vec();
        argv.push("Fixture (bar widget)".to_owned());
        if progress {
            argv.extend(["--progress".to_owned(), "json".to_owned()]);
        }
        argv
    }

    /// Template `name` with its slots filled, after [`standard`]'s flags. Fixtures only:
    /// `validate_built_argv` never fills a slot itself.
    pub fn fill(
        templates: &BTreeMap<String, Value>,
        name: &str,
        slots: &[(&str, &str)],
    ) -> Vec<String> {
        let template = &templates[name];
        let mut argv = standard(template["mutates"] == true || name == "check");
        for part in template["argv"].as_array().unwrap() {
            let mut part = part.as_str().unwrap().to_owned();
            for (slot, value) in slots {
                part = part.replace(&format!("{{{slot}}}"), value);
            }
            assert!(!part.contains('{'), "{name}: unfilled slot in {part}");
            argv.push(part);
        }
        argv
    }

    /// Writes a run's envelope to `<dir>/<name>.json` and its stderr (NDJSON) to
    /// `<name>.stderr.ndjson`, after checking the exit code and the envelope.
    pub fn write_fixture(dir: &Path, name: &str, run: &Run, code: i32) -> Value {
        let envelope = run.expect(code);
        fs::write(dir.join(format!("{name}.json")), &run.stdout).unwrap();
        if !run.stderr.is_empty() {
            fs::write(dir.join(format!("{name}.stderr.ndjson")), &run.stderr).unwrap();
        }
        envelope
    }

    pub const FOO: &str = "https://github.com/o/foo";
    pub const BAR: &str = "https://github.com/o/bar";
    pub const BAZ: &str = "https://github.com/o/baz";

    /// Retail and Classic Era.
    pub const INTERFACES: &str = "11509, 120105";
    pub const FLAVORS: &[(&str, u32)] = &[("mainline", 120_105), ("classic", 11_509)];

    /// A release's one asset: `<Name>.zip` with `<Name>/<Name>.toc` and a `core.lua`
    /// that differs per tag.
    pub struct Rel {
        file: String,
        toc_path: String,
        toc: String,
        core_path: String,
        core: String,
    }

    impl Rel {
        pub fn new(name: &str, tag: &str) -> Self {
            let title = format!("## Title: {name}");
            let version = format!("## Version: {tag}");
            Self {
                file: format!("{name}.zip"),
                toc_path: format!("{name}/{name}.toc"),
                toc: toc(INTERFACES, &[&title, &version]),
                core_path: format!("{name}/core.lua"),
                core: format!("-- {name} {tag}"),
            }
        }

        pub fn assets(&self) -> [Asset<'_>; 1] {
            [(
                self.file.as_str(),
                FLAVORS,
                vec![
                    (self.toc_path.as_str(), self.toc.as_str()),
                    (self.core_path.as_str(), self.core.as_str()),
                ],
            )]
        }
    }
}

/// Envelopes from the real binary for bun's reducer tests, written to
/// The Issues view's envelopes: on retail, Foo (one release behind) with a Lua runtime
/// error and a custom message naming a character, a report received from another
/// player, and BugGrabber present; era without BugGrabber. `errors` strict, the same
/// file cut short (partial), Foo's raw details, a dismissal with a stale token, and
/// `status` in the `errors` state.
#[cfg(test)]
fn errors_fixtures(dir: &std::path::Path, t: &BTreeMap<String, Value>) {
    use common::addons::{addon_folder, toc};
    use common::buggrabber::{self, BugDb, BugEntry, NOW, TimeKind, write_files};

    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let (old, new) = (Rel::new("Foo", "v1"), Rel::new("Foo", "v2"));
    home.installed_behind(
        support::FOO,
        ("v1", &old.assets()),
        ("v2", &new.assets()),
        &["retail"],
    );
    home.exec(&fill(t, "check", &[])).expect(0);
    let bug_toc = toc("120105", &["## SavedVariables: BugGrabberDB"]);
    addon_folder(
        &home.addons("_retail_"),
        "!BugGrabber",
        &[("!BugGrabber.toc", &bug_toc)],
        &[],
    );
    let at = |file: &str, line| buggrabber::stack_era_c("Foo", file, line);
    let db = BugDb::new(buggrabber::Writer::Modern, 10)
        .entry(BugEntry::new(
            &format!("{} FakeChar pulled too early", at("boss.lua", 3)),
            10,
            (TimeKind::Unix, NOW - 120),
        ))
        .entry(
            BugEntry::new(
                &format!(
                    "{} attempt to index a nil value (field 'x')",
                    at("core.lua", 12)
                ),
                10,
                (TimeKind::Unix, NOW - 60),
            )
            .stack(&[
                format!("{} in function 'Refresh'", at("core.lua", 12)),
                "[C]: in function 'xpcall'".to_owned(),
            ])
            .counter(4),
        )
        .entry(
            BugEntry::new(
                &format!("{} attempt to call a nil value", at("x.lua", 1)),
                10,
                (TimeKind::Unix, NOW - 30),
            )
            .source("FakeSender"),
        );
    let wtf = home.client("_retail_").join("WTF");
    let mut file = db.file();
    write_files(&wtf, std::slice::from_ref(&file));
    let now = [("OMADDONS_NOW", "2026-10-15T18:30:00Z")];
    let errors = home.exec_with(&fill(t, "errors", &[]), &now);
    let errors = write_fixture(dir, "errors", &errors, 0);
    assert_eq!(
        errors["data"]["targets"].as_array().unwrap().len(),
        2,
        "{errors}"
    );
    let raw = fill(t, "errors_raw", &[("target", "retail"), ("addon", "Foo")]);
    write_fixture(dir, "errors-raw", &home.exec_with(&raw, &now), 0);
    let status = write_fixture(
        dir,
        "status-errors",
        &home.exec_with(&fill(t, "status", &[]), &now),
        0,
    );
    assert_eq!(status["data"]["summary"]["state"], "errors", "{status}");
    let mut stale = fill(t, "dismiss_errors", &[("target", "retail")]);
    let at = stale.iter().position(|a| a == "errors").unwrap();
    stale.splice(
        at..at,
        ["--if-match".to_owned(), format!("pc1:{}", "0".repeat(64))],
    );
    let stale = write_fixture(dir, "dismiss-stale", &home.exec_with(&stale, &now), 1);
    assert_eq!(stale["error"]["code"], "plan_stale", "{stale}");
    // The game still writing: a file cut short.
    let cut = file.bytes.len() - 20;
    file.bytes.truncate(cut);
    write_files(&wtf, &[file]);
    write_fixture(
        dir,
        "errors-partial",
        &home.exec_with(&fill(t, "errors", &[]), &now),
        0,
    );
}

/// `$OMADDONS_WIDGET_OUT/fixtures/`: `status` in the `updates`, `idle` and `attention`
/// states, `list`, `search`, the `check` and `update_all` results (with their progress
/// lines), a `remove` preview, a `remove` whose token went stale, and a failure.
#[test]
#[ignore = "run by the widget pipeline, which sets OMADDONS_WIDGET_OUT"]
fn write_widget_fixtures() {
    let dir = out_dir().join("fixtures");
    fs::create_dir_all(&dir).unwrap();
    let t = templates();

    // Updates: Foo and Bar one release behind on retail and era, Baz current on retail.
    let mut home = Home::new(&["wow", "wow_classic_era"]);
    let baz = Rel::new("Baz", "v1");
    gh_release(&home.http, support::BAZ, "v1", &baz.assets());
    gh_latest(&home.http, support::BAZ, "v1");
    home.run(&["install", "gh:o/baz", "-t", "retail"]).expect(0);
    for (base, name) in [(support::FOO, "Foo"), (support::BAR, "Bar")] {
        let (old, new) = (Rel::new(name, "v1"), Rel::new(name, "v2"));
        home.installed_behind(
            base,
            ("v1", &old.assets()),
            ("v2", &new.assets()),
            &["retail", "era"],
        );
    }
    // A folder omaddons does not manage, for `list`.
    let loose = home.addons("_retail_").join("Loose");
    fs::create_dir_all(&loose).unwrap();
    fs::write(
        loose.join("Loose.toc"),
        "## Interface: 120105\n## Title: Loose\n",
    )
    .unwrap();
    write_fixture(&dir, "check", &home.exec(&fill(&t, "check", &[])), 0);
    let status = write_fixture(
        &dir,
        "status-updates",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    assert_eq!(status["data"]["summary"]["state"], "updates", "{status}");
    home.retail_running();
    let status = write_fixture(
        &dir,
        "status-running",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    assert_eq!(
        status["data"]["summary"]["running"][0], "retail",
        "{status}"
    );
    home.nothing_running();
    write_fixture(&dir, "list", &home.exec(&fill(&t, "list", &[])), 0);
    let mut preview = fill(&t, "remove", &[("target", "retail"), ("addon", "Baz")]);
    preview.insert(preview.len() - 5, "--dry-run".to_owned());
    let preview = write_fixture(&dir, "remove-preview", &home.exec(&preview), 0);
    assert!(preview["data"]["precondition"].is_string(), "{preview}");
    let mut stale = fill(&t, "remove", &[("target", "retail"), ("addon", "Baz")]);
    stale.splice(
        stale.len() - 5..stale.len() - 5,
        ["--if-match".to_owned(), format!("pc1:{}", "0".repeat(64))],
    );
    let stale = write_fixture(&dir, "remove-stale", &home.exec(&stale), 1);
    assert_eq!(stale["error"]["code"], "plan_stale", "{stale}");
    write_fixture(&dir, "update", &home.exec(&fill(&t, "update_all", &[])), 0);
    let status = write_fixture(
        &dir,
        "status-after-update",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    // "Undo last" previews the CLI's own undo argv.
    let undo: Vec<String> = status["data"]["last_change"]["undo"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap().to_owned())
        .collect();
    let mut preview = support::standard(true);
    preview.push("--dry-run".to_owned());
    preview.extend(undo);
    let preview = write_fixture(&dir, "undo-preview", &home.exec(&preview), 0);
    assert!(preview["data"]["precondition"].is_string(), "{preview}");
    let failure = write_fixture(
        &dir,
        "failure",
        &home.exec(&fill(&t, "update_target", &[("target", "nosuch")])),
        3,
    );
    assert_eq!(failure["error"]["code"], "target_not_found", "{failure}");

    // The Installed view's markers: Foo edited by hand, Bar's folder gone, Baz pinned
    // behind its latest; and Foo's release notes, which the Updates view's `i` reads.
    let mut home = Home::new(&["wow"]);
    for (base, name) in [
        (support::FOO, "Foo"),
        (support::BAR, "Bar"),
        (support::BAZ, "Baz"),
    ] {
        let (old, new) = (Rel::new(name, "v1"), Rel::new(name, "v2"));
        home.installed_behind(
            base,
            ("v1", &old.assets()),
            ("v2", &new.assets()),
            &["retail"],
        );
    }
    home.exec(&fill(&t, "check", &[])).expect(0);
    home.exec(&fill(&t, "pin", &[("target", "retail"), ("addon", "Baz")]))
        .expect(0);
    let toc = home.addons("_retail_").join("Foo/Foo.toc");
    fs::write(&toc, "## Interface: 120105\n## Title: edited by hand\n").unwrap();
    fs::remove_dir_all(home.addons("_retail_").join("Bar")).unwrap();
    let markers = write_fixture(&dir, "list-markers", &home.exec(&fill(&t, "list", &[])), 0);
    let drift: Vec<(&str, &str, &str)> = markers["data"]["targets"][0]["addons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap(),
                a["drift"]["state"].as_str().unwrap(),
                a["latest"]["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        drift,
        [
            ("Bar", "missing", "update_available"),
            ("Baz", "clean", "pinned"),
            ("Foo", "modified", "update_available"),
        ],
        "{markers}"
    );
    gh_feed(
        &home.http,
        support::FOO,
        &[
            ("v2", "<p>Fixes the <b>pull</b> timer.</p>"),
            ("v1", "<p>First.</p>"),
        ],
        None,
    );
    let changelog = fill(&t, "changelog", &[("target", "retail"), ("addon", "Foo")]);
    write_fixture(&dir, "changelog", &home.exec(&changelog), 0);

    errors_fixtures(&dir, &t);

    // Idle: Baz current on retail, checked just now.
    let home = Home::new(&["wow"]);
    gh_release(&home.http, support::BAZ, "v1", &baz.assets());
    gh_latest(&home.http, support::BAZ, "v1");
    home.run(&["install", "gh:o/baz", "-t", "retail"]).expect(0);
    home.exec(&fill(&t, "check", &[])).expect(0);
    let status = write_fixture(&dir, "status-idle", &home.exec(&fill(&t, "status", &[])), 0);
    assert_eq!(status["data"]["summary"]["state"], "idle", "{status}");

    // Attention: Baz's folder deleted behind omaddons' back (a P1 issue).
    fs::remove_dir_all(home.addons("_retail_").join("Baz")).unwrap();
    let status = write_fixture(
        &dir,
        "status-attention",
        &home.exec(&fill(&t, "status", &[])),
        0,
    );
    assert_eq!(status["data"]["summary"]["state"], "attention", "{status}");

    // Search: Details! and Recount on WoWInterface, Skada on GitHub, ElvUI on Tukui.
    let home = Home::new(&["wow"]);
    let mut details = FileEntry::new(23056, "Details! Damage Meter", &["Details"]);
    details.downloads = 515_870;
    let mut recount = FileEntry::new(7000, "Recount Damage Meter", &["Recount"]);
    recount.downloads = 2_100_000;
    recount.compat = vec![("12.0.5".into(), "Midnight".into())];
    wowi_filelist(&home.http, &[details, recount]);
    tukui_list(&home.http, &[elvui()]);
    github_search(
        &home.http,
        "damage meter",
        &[(
            "Skada-Damage-Meter",
            "Skada",
            "A modular damage meter for World of Warcraft",
            412,
            "2026-09-01T00:00:00Z",
        )],
    );
    let search = fill(
        &t,
        "search",
        &[("target", "retail"), ("query", "damage meter")],
    );
    let search = write_fixture(&dir, "search", &home.exec(&search), 0);
    assert!(
        !search["data"]["results"].as_array().unwrap().is_empty(),
        "{search}"
    );
}
