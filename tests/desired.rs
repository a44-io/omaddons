//! `addons.toml` entries resolved per target the way `install` reads its arguments
//! (`app::resolve_args::resolve_entry`): catalog names and aliases (with `catalog.d`
//! overrides), canonical refs, pasted links, names installed on that target only, and
//! relative paths against the file's own directory.

use crate::common;

use std::collections::BTreeMap;
use std::fs;

use common::addons::toc;
use common::archives::zip_text;
use common::home::Home;
use common::http::paths;
use omaddons::app::resolve_args::resolve_entry;
use omaddons::discover::DiscoverySource;
use omaddons::discover::battlenet::{BattlenetHints, discover_root};
use omaddons::discover::registry::reconcile;
use omaddons::error::{ErrorCode, WarningCode};
use omaddons::model::Ref;
use omaddons::resolve::catalog::Catalog;
use omaddons::rules::{Family, RulesTable};
use omaddons::state::desired::{Entry, parse, validate};
use omaddons::state::registry::load_registry;

#[test]
fn entries_resolve_like_install_arguments_on_their_own_target() {
    let home = Home::new(&["wow", "wow_classic"]);
    // Foo is installed on retail only.
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("foo.zip");
    let foo = toc("120105, 50504", &["## Version: 1"]);
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", foo.as_str())])).unwrap();
    let zip = zip.display().to_string();
    let r = home.run(&[
        "install",
        "gh:o/foo",
        "--file",
        &zip,
        "-t",
        "retail",
        "--offline",
    ]);
    r.expect(0);
    // A catalog override with an alias.
    let catalog_d = home.env.config().join("omaddons/catalog.d");
    fs::create_dir_all(&catalog_d).unwrap();
    fs::write(
        catalog_d.join("50-bar.toml"),
        "schema_version = \"1.0\"\n\n[[addon]]\nname = \"Bar\"\naliases = [\"bar-alias\"]\nref = \"gh:o/bar\"\nfolders = [\"Bar\"]\n",
    )
    .unwrap();
    // The desired-state file lives elsewhere, next to a local addon.
    let base = home.env.root().join("dotfiles");
    fs::create_dir_all(base.join("MyAddon")).unwrap();
    fs::write(base.join("MyAddon/MyAddon.toc"), toc("120105", &[])).unwrap();

    let paths = paths(&home.env);
    let (catalog, warnings) = Catalog::load(&paths);
    assert!(warnings.is_empty(), "{warnings:?}");
    let records = BTreeMap::from([("retail".to_owned(), common::state::record(&home, "retail"))]);
    let resolve = |request: &str, target: &str| {
        resolve_entry(
            &Entry::named(request),
            (target, Family::Mainline),
            &records,
            &catalog,
            &[],
            &base,
        )
    };
    let gh = |repo: &str| -> Ref { format!("gh:o/{repo}").parse().unwrap() };

    // A catalog alias, on any target, records or not.
    assert_eq!(resolve("bar-alias", "classic").unwrap(), gh("bar"));
    assert_eq!(resolve("BAR", "retail").unwrap(), gh("bar"));
    // A canonical ref, a pasted link; a release link's tag is not kept.
    assert_eq!(resolve("gh:o/baz", "classic").unwrap(), gh("baz"));
    assert_eq!(
        resolve("https://github.com/o/qux", "retail").unwrap(),
        gh("qux")
    );
    assert_eq!(
        resolve("https://github.com/o/qux/releases/tag/v1.2", "retail").unwrap(),
        gh("qux")
    );
    // An installed name, on that target only (ids ignore case).
    assert_eq!(resolve("foo", "retail").unwrap(), gh("foo"));
    assert_eq!(resolve("Foo", "RETAIL").unwrap(), gh("foo"));
    let elsewhere = resolve("foo", "classic").unwrap_err();
    assert_eq!(elsewhere.code, ErrorCode::NotFound, "{elsewhere:?}");
    // A relative path: against the file's directory, whatever the working directory.
    // (Compared without `.` components, which install keeps today: omad-ex35.)
    let local = |request: &str| match resolve(request, "retail").unwrap() {
        Ref::File { path } => path.components().collect::<std::path::PathBuf>(),
        other => std::path::PathBuf::from(other.to_string()),
    };
    assert_eq!(local("./MyAddon"), base.join("MyAddon"));
    assert_eq!(local("file:MyAddon"), base.join("MyAddon"));
    // A table entry resolves its ref before its name.
    let mut table = Entry::named("foo");
    table.r#ref = Some("gh:o/other".into());
    let resolved = resolve_entry(
        &table,
        ("retail", Family::Mainline),
        &records,
        &catalog,
        &[],
        &base,
    )
    .unwrap();
    assert_eq!(resolved, gh("other"));
}

#[test]
fn validate_knows_registered_discovered_and_missing_targets() {
    let home = Home::new(&["wow", "wow_classic", "wow_classic_era"]);
    let dl = home.env.root().join("dl");
    fs::create_dir_all(&dl).unwrap();
    let zip = dl.join("foo.zip");
    let foo = toc("120105, 50504", &[]);
    fs::write(&zip, zip_text(&[("Foo/Foo.toc", foo.as_str())])).unwrap();
    let zip = zip.display().to_string();
    // Retail and classic are registered; classic then goes missing; era is only
    // discovered.
    for target in ["retail", "classic"] {
        let r = home.run(&[
            "install",
            "gh:o/foo",
            "--file",
            &zip,
            "-t",
            target,
            "--offline",
        ]);
        r.expect(0);
    }
    fs::remove_dir_all(home.client("_classic_")).unwrap();

    let paths = paths(&home.env);
    let table = RulesTable::builtin();
    let (found, _) = discover_root(
        &home.root,
        &table,
        &BattlenetHints::default(),
        "battlenet",
        &DiscoverySource::Scan,
    );
    let registry = load_registry(&paths).unwrap().value;
    let resolved = reconcile(&registry, &found);
    assert_eq!(resolved.missing.len(), 1, "{:?}", resolved.missing);
    let known = resolved.known_ids();
    let era = resolved
        .targets
        .iter()
        .find(|d| d.target.key.is_none())
        .map(|d| d.target.id.clone())
        .unwrap();

    let text = format!(
        "[target.retail]\n\n[target.CLASSIC]\n\n[target.\"{era}\"]\n\n[target.elsewhere]\n"
    );
    let (state, _) = parse(&text, &paths.addons_toml()).unwrap();
    let warnings = validate(&state, &known);
    let named: Vec<&str> = warnings.iter().map(|w| w.message.as_str()).collect();
    assert_eq!(warnings.len(), 1, "{named:?} (known: {known:?})");
    assert_eq!(warnings[0].code, WarningCode::DesiredUnknownTarget);
    assert!(named[0].contains("\"elsewhere\""), "{named:?}");
}

#[test]
fn a_file_parses_into_entries_that_resolve() {
    let home = Home::new(&["wow"]);
    let text = "[family.mainline]\naddons = [\"bigwigs\", \"gh:o/foo\"]\n";
    let path = home.env.config().join("omaddons/addons.toml");
    let (state, warnings) = parse(text, &path).unwrap();
    assert!(warnings.is_empty());
    let (catalog, _) = Catalog::load(&paths(&home.env));
    let base = path.parent().unwrap();
    let resolved: Vec<Ref> = state.families[&omaddons::rules::Family::Mainline]
        .entries
        .iter()
        .map(|e| {
            let target = ("retail", Family::Mainline);
            resolve_entry(e, target, &BTreeMap::new(), &catalog, &[], base).unwrap()
        })
        .collect();
    assert_eq!(
        resolved,
        [
            "gh:BigWigsMods/BigWigs".parse::<Ref>().unwrap(),
            "gh:o/foo".parse().unwrap()
        ]
    );
}
