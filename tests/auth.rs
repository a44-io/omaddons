//! `omaddons auth github` end to end, in an isolated home: `set` from standard input
//! (mode 0600 in a 0700 directory), `status` offline and online (one request, to
//! `/rate_limit`), `clear`, the order env, then config, then file (`GITHUB_TOKEN`
//! never), `credential_file_mode`, a rejected token, `--from-gh` with a fake `gh` and
//! without one, and the token never in any output, log or state file.

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::exec::write_executable;
use common::home::Home;
use common::http::fixture;
use common::netlog::net_log;
use serde_json::{Value, json};
use support::{argv, contains_anywhere, credentials, envelope, fake_gh, mode};

// ubs:ignore — a canary to find leaks, not a credential
const TOKEN: &str = "ghp_CANARYc4n4ry0123456789";

#[cfg(test)]
mod support {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::common::exec::write_executable;
    use super::common::home::{Home, Run};

    pub fn argv(args: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        out.push("--json".to_owned());
        out
    }

    pub fn envelope(run: &Run) -> Value {
        let parsed = serde_json::from_str(&run.stdout);
        assert!(parsed.is_ok(), "{}", run.context());
        parsed.unwrap()
    }

    pub fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    pub fn credentials(home: &Home) -> PathBuf {
        home.env.config().join("omaddons/credentials")
    }

    /// A fake `gh` in a directory of its own (the only thing on PATH), printing `token`
    /// for `gh auth token` and logging its argv beside it.
    pub fn fake_gh(home: &Home, token: &str) -> PathBuf {
        let dir = home.env.root().join("fake-bin");
        fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        write_executable(
            &gh,
            &format!(
                "#!/bin/sh\necho \"$@\" >> \"{}\"\nprintf '%s\\n' '{token}'\n",
                dir.join("argv.log").display()
            ),
        );
        dir
    }

    /// The files under `dir` whose bytes contain `needle`.
    pub fn contains_anywhere(dir: &Path, needle: &str) -> Vec<String> {
        let mut found = Vec::new();
        if let Ok(items) = fs::read_dir(dir) {
            for item in items.flatten() {
                let path = item.path();
                if path.is_dir() {
                    found.extend(contains_anywhere(&path, needle));
                } else if fs::read(&path)
                    .is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle))
                {
                    found.push(path.display().to_string());
                }
            }
        }
        found
    }
}

#[test]
fn set_status_and_clear() {
    let home = Home::new(&["wow"]);
    let set = home.exec_input(
        &argv(&["auth", "github", "set"]),
        &[],
        format!("{TOKEN}\n").as_bytes(),
    );
    let e = envelope(&set);
    assert_eq!(set.code, 0, "{}", set.context());
    assert_eq!(
        e["data"],
        json!({"configured": true, "source": "file", "rate_limit": null, "dry_run": false})
    );
    let file = credentials(&home).join("github");
    assert_eq!(fs::read_to_string(&file).unwrap(), TOKEN);
    assert_eq!((mode(&file), mode(&credentials(&home))), (0o600, 0o700));

    // Offline: no request, no budget.
    let before = net_log(&home).len();
    let status = home.run(&["auth", "github", "--offline"]).expect(0);
    assert_eq!(
        status["data"],
        json!({"configured": true, "source": "file", "rate_limit": null, "dry_run": false})
    );
    assert_eq!(net_log(&home).len(), before);

    // Online: one request, to /rate_limit.
    fixture(
        &home.http,
        "rate-limit",
        "https://api.github.com/rate_limit",
        200,
        &[],
        Some(
            r#"{"resources":{"core":{"limit":5000,"remaining":4987,"reset":1792000000,"used":13}}}"#,
        ),
    );
    let status = home.run(&["auth", "github", "status"]).expect(0);
    assert_eq!(
        status["data"]["rate_limit"],
        json!({"limit": 5000, "remaining": 4987, "reset_at": "2026-10-14T17:46:40Z"})
    );
    let log = net_log(&home);
    let new: Vec<(&str, &str, &str)> = log.entries[before..]
        .iter()
        .map(|e| (e.host.as_str(), e.path.as_str(), e.purpose.as_str()))
        .collect();
    assert_eq!(new, [("api.github.com", "/rate_limit", "rate-limit")]);

    // Clear: an empty file, no token.
    let clear = home.run(&["auth", "github", "clear"]).expect(0);
    assert_eq!(
        clear["data"],
        json!({"configured": false, "source": "none", "rate_limit": null, "dry_run": false})
    );
    assert_eq!(fs::read(&file).unwrap(), b"");
    assert_eq!(mode(&file), 0o600);
}

/// A change to nothing records none: clearing no token, or storing the one stored,
/// writes nothing and adds no history line, though it still recovers first.
#[cfg(debug_assertions)]
#[test]
fn a_change_to_nothing_records_nothing() {
    let home = Home::new(&["wow"]);
    let set = |token: &str| {
        let r = home.exec_input(
            &argv(&["auth", "github", "set"]),
            &[],
            format!("{token}\n").as_bytes(),
        );
        assert_eq!(r.code, 0, "{}", r.context());
    };
    let clear = || home.run(&["auth", "github", "clear"]).expect(0);
    clear();
    assert!(
        !credentials(&home).exists(),
        "nothing to clear: nothing made"
    );
    clear();
    set(TOKEN);
    let file = credentials(&home).join("github");
    let stored = fs::metadata(&file).unwrap().modified().unwrap();
    set(TOKEN);
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), stored);
    set("ghp_OTHERc4n4ry0123456789");
    clear();
    let history = home.run(&["history"]).expect(0);
    let auth: Vec<String> = history["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        // ubs:ignore — a history kind, not a secret
        .filter(|e| e["kind"] == "auth")
        .map(|e| e["argv"][3].as_str().unwrap_or_default().to_owned())
        .collect();
    // Newest first.
    assert_eq!(auth, ["clear", "set", "set"], "{history}");

    // A no-op clear still takes the lock and recovers an interrupted install first.
    let src = home.env.root().join("src");
    let foo = common::addons::addon_folder(
        &src,
        "Foo",
        &[("Foo.toc", &common::addons::toc("120105", &[]))],
        &[],
    );
    let crashed = home.exec_with(
        &argv(&["install", &foo.display().to_string(), "-t", "retail"]),
        &[("OMADDONS_FAILPOINTS", "engine.journal.prepared=abort")],
    );
    assert_eq!(crashed.code, -1, "{}", crashed.context());
    let again = home.run(&["auth", "github", "clear"]).expect(0);
    let codes: Vec<&Value> = again["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["code"])
        .collect();
    assert!(codes.contains(&&json!("journal_recovered")), "{again}");
    let history = home.run(&["history"]).expect(0);
    let lines = history["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        // ubs:ignore — a history kind, not a secret
        .filter(|e| e["kind"] == "auth")
        .count();
    assert_eq!(lines, 3, "{history}");
}

#[test]
fn env_then_config_then_file_and_never_github_token() {
    let home = Home::new(&["wow"]);
    let source = |extra: &[(&str, &str)]| {
        let run = home.exec_with(&argv(&["auth", "github", "--offline"]), extra);
        envelope(&run)["data"]["source"].clone()
    };
    assert_eq!(source(&[("GITHUB_TOKEN", "ghp_ignored")]), "none");
    home.exec_input(&argv(&["auth", "github", "set"]), &[], b"ghp_file\n");
    assert_eq!(source(&[]), "file");
    fs::write(
        home.env.config().join("omaddons/config.toml"),
        "[sources.github]\ncredential = { env = \"MY_GH_TOKEN\" }\n",
    )
    .unwrap();
    assert_eq!(source(&[("MY_GH_TOKEN", "ghp_config")]), "config");
    assert_eq!(
        source(&[
            ("MY_GH_TOKEN", "ghp_config"),
            ("OMADDONS_GITHUB_TOKEN", "ghp_env")
        ]),
        "env"
    );
    // A configured credential that cannot be had never falls through to the file.
    let run = home.exec_with(&argv(&["auth", "github", "--offline"]), &[]);
    assert_eq!(run.code, 1, "{}", run.context());
    assert_eq!(envelope(&run)["error"]["code"], "auth_required");
}

/// `status` says where the token comes from, as `auth github --offline` does (the
/// widget's settings sheet, omad-r0to): no request, no credential command run, never the
/// token itself; a configured credential it cannot have is configured: false.
#[test]
fn status_says_where_the_token_comes_from_and_runs_nothing() {
    let home = Home::new(&["wow"]);
    let github = |extra: &[(&str, &str)]| {
        let run = home.exec_with(&argv(&["status", "--cached"]), extra);
        assert_eq!(run.code, 0, "{}", run.context());
        assert!(
            !run.stdout.contains(TOKEN) && !run.stderr.contains(TOKEN),
            "{}",
            run.context()
        );
        envelope(&run)["data"]["github"].clone()
    };
    assert_eq!(
        github(&[]),
        json!({ "configured": false, "source": "none" })
    );
    home.exec_input(
        &argv(&["auth", "github", "set"]),
        &[],
        format!("{TOKEN}\n").as_bytes(),
    );
    assert_eq!(github(&[]), json!({ "configured": true, "source": "file" }));
    fs::write(
        home.env.config().join("omaddons/config.toml"),
        "[sources.github]\ncredential = { env = \"MY_GH_TOKEN\" }\n",
    )
    .unwrap();
    assert_eq!(
        github(&[("MY_GH_TOKEN", TOKEN)]),
        json!({ "configured": true, "source": "config" })
    );
    // A configured credential it cannot have: never the file behind it, never an error.
    assert_eq!(
        github(&[]),
        json!({ "configured": false, "source": "config" })
    );
    assert_eq!(
        github(&[("OMADDONS_GITHUB_TOKEN", TOKEN)]),
        json!({ "configured": true, "source": "env" })
    );
    // A command credential counts as configured, and never runs for status.
    let ran = home.env.root().join("cred-ran.log");
    let cred = home.env.root().join("cred");
    write_executable(
        &cred,
        &format!("#!/bin/sh\necho x >> '{}'\necho {TOKEN}\n", ran.display()),
    );
    fs::write(
        home.env.config().join("omaddons/config.toml"),
        format!(
            "[sources.github]\ncredential = {{ command = [\"{}\"] }}\n",
            cred.display()
        ),
    )
    .unwrap();
    assert_eq!(
        github(&[]),
        json!({ "configured": true, "source": "config" })
    );
    let full = home.exec(&argv(&["status"]));
    assert_eq!(
        envelope(&full)["data"]["github"],
        json!({ "configured": true, "source": "config" })
    );
    assert!(!ran.exists(), "the credential command never ran");
    assert!(net_log(&home).is_empty(), "no request");
}

/// A `command` credential runs only when a REST request needs it: never offline, and
/// never for `set` or `clear`; online, `status` runs it once.
#[test]
fn a_credential_command_never_runs_offline_or_for_set_and_clear() {
    let home = Home::new(&["wow"]);
    let ran = home.env.root().join("cred-ran.log");
    let cred = home.env.root().join("cred");
    write_executable(
        &cred,
        &format!("#!/bin/sh\necho x >> '{}'\necho {TOKEN}\n", ran.display()),
    );
    fs::create_dir_all(home.env.config().join("omaddons")).unwrap();
    fs::write(
        home.env.config().join("omaddons/config.toml"),
        format!(
            "[sources.github]\ncredential = {{ command = [\"{}\"] }}\n",
            cred.display()
        ),
    )
    .unwrap();

    let run = home.exec(&argv(&["auth", "github", "--offline"]));
    let status = envelope(&run);
    assert_eq!(
        (&status["data"]["configured"], &status["data"]["source"]),
        (&json!(true), &json!("config")),
        "{}",
        run.context()
    );
    home.exec_input(&argv(&["auth", "github", "set"]), &[], b"ghp_stored\n");
    home.run(&["auth", "github", "clear"]).expect(0);
    let rest = home.exec(&argv(&[
        "install",
        "gh:o/r",
        "--channel",
        "beta",
        "-t",
        "retail",
        "--dry-run",
        "--offline",
    ]));
    assert_eq!(
        envelope(&rest)["error"]["code"],
        "offline",
        "{}",
        rest.context()
    );
    assert!(
        envelope(&rest)["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("offline: not requesting https://api.github.com/"),
        "{}",
        rest.context()
    );
    assert!(
        !ran.exists(),
        "the command ran: {:?}",
        fs::read_to_string(&ran)
    );

    // Online, status's one request needs it: it runs, once.
    fixture(
        &home.http,
        "rate-limit",
        "https://api.github.com/rate_limit",
        200,
        &[],
        Some("{\"resources\":{\"core\":{\"limit\":5000,\"remaining\":4999,\"reset\":1791824100}}}"),
    );
    let run = home.exec(&argv(&["auth", "github"]));
    assert_eq!(run.code, 0, "{}", run.context());
    assert_eq!(fs::read_to_string(&ran).unwrap().lines().count(), 1);
}

#[test]
fn a_file_others_can_read_warns() {
    let home = Home::new(&["wow"]);
    home.exec_input(&argv(&["auth", "github", "set"]), &[], b"ghp_file\n");
    let file = credentials(&home).join("github");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    let status = home.run(&["auth", "github", "--offline"]).expect(0);
    let codes: Vec<&Value> = status["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| &w["code"])
        .collect();
    assert_eq!(codes, [&json!("credential_file_mode")]);
    // Every command that resolves the token says so.
    let listed = home.run(&["list"]).expect(0);
    assert!(
        listed.to_string().contains("credential_file_mode"),
        "{listed}"
    );
}

#[test]
fn a_rejected_token_says_how_to_clear_it() {
    let home = Home::new(&["wow"]);
    home.exec_input(&argv(&["auth", "github", "set"]), &[], b"ghp_revoked\n");
    fixture(
        &home.http,
        "rate-limit-401",
        "https://api.github.com/rate_limit",
        401,
        &[],
        Some(r#"{"message":"Bad credentials"}"#),
    );
    let r = home.run(&["auth", "github"]);
    let e = r.expect(1);
    assert_eq!(
        (&e["error"]["code"], &e["error"]["fix_argv"]),
        (
            &json!("auth_rejected"),
            &json!(["omaddons", "auth", "github", "clear"])
        ),
        "{}",
        r.context()
    );
}

#[test]
fn from_gh_asks_gh_once_and_only_on_request() {
    let home = Home::new(&["wow"]);
    // No gh on PATH.
    let empty = home.env.root().join("empty-bin");
    fs::create_dir_all(&empty).unwrap();
    let path = empty.display().to_string();
    let r = home.exec_with(&argv(&["auth", "github", "--from-gh"]), &[("PATH", &path)]);
    assert_eq!(r.code, 1, "{}", r.context());
    let e = envelope(&r);
    assert_eq!(e["error"]["code"], "auth_required");
    assert!(
        e["error"]["fix"]
            .as_str()
            .unwrap()
            .contains("auth github set")
    );

    // A fake gh: its token is stored as set stores it.
    let bin = fake_gh(&home, "ghp_from_gh");
    let path = bin.display().to_string();
    let r = home.exec_with(&argv(&["auth", "github", "--from-gh"]), &[("PATH", &path)]);
    assert_eq!(r.code, 0, "{}", r.context());
    let file = credentials(&home).join("github");
    assert_eq!(fs::read_to_string(&file).unwrap(), "ghp_from_gh");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(
        fs::read_to_string(bin.join("argv.log")).unwrap(),
        "auth token\n"
    );
    // Status never asks gh.
    home.exec_with(&argv(&["auth", "github", "--offline"]), &[("PATH", &path)]);
    assert_eq!(
        fs::read_to_string(bin.join("argv.log")).unwrap(),
        "auth token\n"
    );

    // --from-gh goes with set only.
    let r = home.run(&["auth", "github", "status", "--from-gh"]);
    assert_eq!(r.expect(2)["error"]["code"], "usage");
}

#[test]
fn the_token_is_never_shown_logged_or_stored_elsewhere() {
    let home = Home::new(&["wow"]);
    fixture(
        &home.http,
        "rate-limit",
        "https://api.github.com/rate_limit",
        200,
        &[],
        Some(r#"{"resources":{"core":{"limit":5000,"remaining":4999,"reset":1792000000}}}"#),
    );
    let mut outputs = Vec::new();
    let set = home.exec_input(
        &argv(&["auth", "github", "set", "-v"]),
        &[],
        format!("{TOKEN}\n").as_bytes(),
    );
    outputs.push(set);
    for args in [
        &["auth", "github", "-v"][..],
        &["auth", "github", "--offline"],
        &["list", "-v"],
        &["status", "-v"],
    ] {
        outputs.push(home.exec_with(&argv(args), &[]));
    }
    let human = home.exec(&["auth".to_owned(), "github".to_owned()]);
    outputs.push(human);
    for run in &outputs {
        assert!(
            !run.stdout.contains(TOKEN) && !run.stderr.contains(TOKEN),
            "{}",
            run.context()
        );
    }
    // Only the credentials file holds it.
    let mut holders = contains_anywhere(&home.env.config(), TOKEN);
    holders.extend(contains_anywhere(&home.env.state(), TOKEN));
    holders.extend(contains_anywhere(&home.env.cache(), TOKEN));
    assert_eq!(
        holders,
        [credentials(&home).join("github").display().to_string()]
    );
}

/// `--dry-run` writes nothing, says so in `data`, and describes the token as the write
/// would leave it.
#[test]
fn a_dry_run_set_or_clear_writes_nothing_and_says_what_would_be() {
    let home = Home::new(&["wow"]);
    let token = format!("{TOKEN}\n");
    let state = |run: &common::home::Run| {
        let e = envelope(run);
        (
            e["data"]["dry_run"].clone(),
            e["data"]["configured"].clone(),
            e["data"]["source"].clone(),
        )
    };
    let set = argv(&["auth", "github", "set", "--dry-run"]);
    let run = home.exec_input(&set, &[], token.as_bytes());
    assert_eq!(
        state(&run),
        (json!(true), json!(true), json!("file")),
        "{}",
        run.context()
    );
    assert!(!credentials(&home).exists(), "nothing written");

    home.exec_input(&argv(&["auth", "github", "set"]), &[], token.as_bytes());
    let file = credentials(&home).join("github");
    let before = fs::read(&file).unwrap();
    let run = home.exec(&argv(&["auth", "github", "clear", "--dry-run"]));
    assert_eq!(
        state(&run),
        (json!(true), json!(false), json!("none")),
        "{}",
        run.context()
    );
    assert_eq!(fs::read(&file).unwrap(), before, "the file is unchanged");
    let human = home.exec(&["auth", "github", "clear", "--dry-run"].map(str::to_owned));
    assert!(
        human.stdout.starts_with("would clear "),
        "{}",
        human.context()
    );
}
