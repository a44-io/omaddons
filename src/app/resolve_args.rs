//! What a user's argument names, as every command reads it: a canonical ref, a pasted
//! URL, a path, an installed addon's name or folder, or a catalog name or alias. One
//! resolver, so `install`, `info`, `sync` and `adopt --ref` mean the same thing by the
//! same string.

use std::collections::BTreeMap;
use std::path::Path;

use super::world::World;
use crate::cli::Context;
use crate::error::{Error, ErrorCode, Result, Suggestion};
use crate::model::Ref;
use crate::resolve::catalog::Catalog;
use crate::rules::Family;
use crate::sources::refs::{CatalogMatch, InstalledMatch, ParsedRef, RefContext, parse_user_ref};
use crate::state::desired::Entry;
use crate::state::records::TargetRecord;
use crate::vocabulary::TargetVocabulary;

/// Parses `request` against the installed addons of `records` (by target id; name or
/// folder, ignoring ASCII case) and `catalog` (name or alias); a relative path resolves
/// against `cwd`, and `git_hosts` are the extra hosts `git:` refs may name.
pub fn parse_ref(
    git_hosts: &[String],
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    cwd: &Path,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let all: Vec<(&str, &TargetRecord)> = records.iter().map(|(k, v)| (k.as_str(), v)).collect();
    parse_among(git_hosts, &all, catalog, cwd, request)
}

/// `error`, from parsing the argument `request` of the command `argv`, with each
/// `omaddons install <ref>` fix the parser suggests (the https form, each ambiguity
/// candidate) aimed at the command that ran: `argv` with `request` replaced by `<ref>`,
/// every other argument (`-t` included) kept.
pub fn aimed_at(error: Error, argv: &[String], request: &str) -> Error {
    aim(error, |r| crate::cli::fix_argv_replacing(argv, request, r))
}

/// [`aimed_at`] for an argument given as the value of the option `flag` (`adopt`'s
/// `--ref`): that value replaced, never another argument spelled the same.
pub fn aimed_at_option(error: Error, argv: &[String], flag: &str) -> Error {
    aim(error, |r| crate::cli::fix_argv_with_value(argv, flag, r))
}

/// `error` with each `omaddons install <ref>` fix replaced by `aimed(<ref>)`.
fn aim(mut error: Error, aimed: impl Fn(&str) -> Option<Vec<String>>) -> Error {
    let aim = |s: &mut Suggestion| {
        let aimed = s.argv().and_then(|a| match a {
            [_, command, r] if command == "install" => aimed(r),
            _ => None,
        });
        if let Some(aimed) = aimed {
            *s = Suggestion::command(aimed);
        }
    };
    aim(&mut error.fix);
    for alternative in &mut error.alternatives {
        aim(alternative);
    }
    error
}

/// What a desired-state entry names on the target `target_id`: its `ref`, else its
/// `name`, read as `install` reads an argument, except that an installed name is looked
/// up in that target's record only, and a relative path resolves against `base_dir`
/// (the directory of the desired-state file, so a file means the same thing wherever it
/// is used). Only the ref: a release link's tag is not a pin (`version` and `pinned`
/// are). Errors are `ref_invalid`, `ref_ambiguous` and `not_found`, as for `install`.
pub fn resolve_entry(
    entry: &Entry,
    target: (&str, Family),
    records: &BTreeMap<String, TargetRecord>,
    catalog: &Catalog,
    git_hosts: &[String],
    base_dir: &Path,
) -> Result<Ref> {
    let (target_id, family) = target;
    let own: Vec<(&str, &TargetRecord)> = records
        .iter()
        .filter(|(id, _)| id.eq_ignore_ascii_case(target_id))
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let request = entry.request();
    parse_among(git_hosts, &own, catalog, base_dir, request)
        .and_then(|p| for_family(&p, catalog, family, request))
        .map(|p| p.r#ref)
}

/// What `parsed` names on a client of `family`: itself, unless it is a catalog name with
/// variants (M3-D16), which names the variant declaring `family`. On a client no variant
/// covers, the catalog has no entry: `not_found`, as for a name it does not know, never
/// another family's build.
pub fn for_family(
    parsed: &ParsedRef,
    catalog: &Catalog,
    family: Family,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let Some(name) = &parsed.catalog_variants else {
        return Ok(parsed.clone());
    };
    match catalog.resolve(name, Some(family)) {
        Some(entry) => Ok(ParsedRef {
            r#ref: entry.r#ref.clone(),
            ..parsed.clone()
        }),
        None => Err(Error::new(
            ErrorCode::NotFound,
            format!(
                "no {name} build for this client is known ({} clients)",
                family.as_str()
            ),
            Suggestion::command(["omaddons", "search", request]),
        )
        .details(serde_json::json!({ "name": name, "family": family }))),
    }
}

fn parse_among(
    git_hosts: &[String],
    records: &[(&str, &TargetRecord)],
    catalog: &Catalog,
    cwd: &Path,
    request: &str,
) -> std::result::Result<ParsedRef, Error> {
    let installed = |name: &str| -> Vec<InstalledMatch> {
        let mut found: Vec<InstalledMatch> = Vec::new();
        for &(target_id, record) in records {
            for addon in record.addons.values() {
                let by_folder = addon
                    .folders
                    .iter()
                    .any(|f| f.as_str().eq_ignore_ascii_case(name));
                if (addon.name.eq_ignore_ascii_case(name) || by_folder)
                    && !found.iter().any(|m| m.r#ref == addon.r#ref)
                {
                    found.push(InstalledMatch {
                        name: addon.name.clone(),
                        r#ref: addon.r#ref.clone(),
                        target_id: target_id.to_owned(),
                    });
                }
            }
        }
        found
    };
    // A name with variants answers with its default variant; each client then resolves
    // its own ([`for_family`]).
    let in_catalog = |name: &str| {
        let variants = if catalog.has_variants(name) {
            catalog
                .variants(name)
                .into_iter()
                .map(|e| e.r#ref.clone())
                .collect()
        } else {
            Vec::new()
        };
        catalog.resolve(name, None).map(|e| CatalogMatch {
            name: e.name.clone(),
            r#ref: e.r#ref.clone(),
            variants,
        })
    };
    let ref_ctx = RefContext {
        cwd: cwd.to_path_buf(),
        git_hosts,
        installed: &installed,
        catalog: &in_catalog,
    };
    parse_user_ref(request, &ref_ctx)
}

/// The catalog's name for `r`: a catalog addon is recorded under it, however it was
/// named.
pub fn catalog_name(catalog: &Catalog, r: &Ref) -> Option<String> {
    catalog
        .entries()
        .iter()
        .find(|e| e.r#ref == *r)
        .map(|e| e.name.clone())
}

/// M4-D12 hook 3. A positional of `install`, `update` or `remove` that names a client
/// and nothing the command reads as an addon makes the whole invocation a `usage`
/// error whose fix moves it into `-t` and keeps the user's verb. Called before
/// planning, recovery or any commit, so the wrong invocation changes nothing.
/// `Ok(())` when `-t` is given, when no positional is a misplaced client, or when the
/// corrected command would not parse.
pub fn misplaced_clients(
    ctx: &Context,
    world: &World,
    catalog: &Catalog,
    positionals: &[String],
    names_an_addon: &dyn Fn(&str) -> bool,
) -> Result<()> {
    // A client already given: a client word among the names fails alone, as an unknown
    // name, rather than override the user's -t.
    if ctx.target.is_some() || positionals.is_empty() {
        return Ok(());
    }
    // Every id -t would resolve, and the world's own rules: the preflight agrees with -t.
    let registered = world.registry.targets.iter().map(|e| e.id.as_str());
    let discovered = world.resolved.targets.iter().map(|d| d.target.id.as_str());
    let vocabulary = TargetVocabulary::new(&world.table, registered.chain(discovered));
    let misplaced = misplaced_words(positionals, &vocabulary, catalog, names_an_addon);
    if misplaced.is_empty() {
        return Ok(());
    }
    misplaced_error(&ctx.argv, positionals, &misplaced).map_or(Ok(()), Err)
}

/// The indices of the `positionals` that name a client and no addon: neither one
/// `names_an_addon` claims nor a catalog name or alias (the addon wins).
pub(crate) fn misplaced_words(
    positionals: &[String],
    vocabulary: &TargetVocabulary,
    catalog: &Catalog,
    names_an_addon: &dyn Fn(&str) -> bool,
) -> Vec<usize> {
    positionals
        .iter()
        .enumerate()
        .filter(|(_, word)| {
            vocabulary.classify(word).is_some()
                && !names_an_addon(word)
                && !catalog.entries().iter().any(|e| e.answers_to(word))
        })
        .map(|(k, _)| k)
        .collect()
}

/// The usage error for the misplaced `positionals` of `argv` (indices `misplaced`, in
/// order): the first moved into -t in place, every other one dropped from the fix and
/// offered as an alternative of its own, since -t takes one client. `None` when a word
/// is not where `argv` has it (after `--`, never moved) or the fix would not parse (the
/// client was the only name: the command then fails as before, having changed nothing).
pub(crate) fn misplaced_error(
    argv: &[String],
    positionals: &[String],
    misplaced: &[usize],
) -> Option<Error> {
    let at = crate::cli::positional_indices(argv);
    let mut indices = Vec::new();
    for &k in misplaced {
        let index = *at.get(k)?;
        // ubs:ignore — comparing command-line words, not secrets
        if argv.get(index) != positionals.get(k) {
            return None;
        }
        indices.push(index);
    }
    let (&first, others) = indices.split_first()?;
    let fix = moved_alone(argv, &indices, first);
    if !crate::cli::parses(&Suggestion::command(fix.clone())) {
        return None;
    }
    let words: Vec<String> = indices.iter().map(|&i| format!("'{}'", argv[i])).collect();
    let message = match words.as_slice() {
        [one] => format!("{one} is a client, not an addon; the client goes in -t"),
        [rest @ .., last] => format!(
            "{} and {last} are clients, not addons; the client goes in -t, one per command",
            rest.join(", ")
        ),
        [] => return None,
    };
    let path = crate::cli::command_path(argv);
    let mut error = crate::cli::misplaced_client_error(message, fix, &path, &argv[first]);
    for &other in others {
        let alternative = Suggestion::command(moved_alone(argv, &indices, other));
        if crate::cli::parses(&alternative) {
            error = error.alternative(alternative);
        }
    }
    Some(error)
}

/// `argv` with the client at `keep` moved into -t in place and every other index of
/// `misplaced` dropped.
fn moved_alone(argv: &[String], misplaced: &[usize], keep: usize) -> Vec<String> {
    let dropped = |i: usize| i != keep && misplaced.contains(&i);
    let trimmed: Vec<String> = argv
        .iter()
        .enumerate()
        .filter(|(i, _)| !dropped(*i))
        .map(|(_, a)| a.clone())
        .collect();
    let shift = misplaced
        .iter()
        .filter(|&&i| i < keep && dropped(i))
        .count();
    crate::cli::with_target_in_place(&trimmed, keep - shift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UsageFix;
    use crate::rules::RulesTable;
    use crate::state::paths::Paths;

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("omaddons")
            .chain(words.iter().copied())
            .map(str::to_owned)
            .collect()
    }

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    /// The positionals clap gives the command `argv` names, for an argv without `--`.
    fn positionals(argv: &[String]) -> Vec<String> {
        crate::cli::positional_indices(argv)
            .into_iter()
            .map(|i| argv[i].clone())
            .collect()
    }

    /// The hook's error for `argv`, with the built-in words and catalog.
    fn hooked(argv: &[String]) -> Option<Error> {
        let positionals = positionals(argv);
        let catalog = Catalog::builtin().unwrap();
        let vocabulary = TargetVocabulary::builtin();
        let misplaced = misplaced_words(&positionals, &vocabulary, &catalog, &|_| false);
        misplaced_error(argv, &positionals, &misplaced)
    }

    fn shown(error: &Error) -> String {
        let alternatives: Vec<_> = error.alternatives.iter().map(Suggestion::argv).collect();
        format!(
            "message {:?}, fix {:?}, alternatives {alternatives:?}, details {}, usage_fix {:?}",
            error.message,
            error.fix.argv(),
            error.details,
            error.usage_fix
        )
    }

    #[test]
    fn misplaced_words_finds_client_words_that_name_no_addon() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
        std::fs::create_dir_all(paths.catalog_d()).unwrap();
        std::fs::write(
            paths.catalog_d().join("hardcore.toml"),
            "schema_version = \"1.0\"\n[[addon]]\nname = \"Hardcore\"\naliases = [\"hcx\"]\nref = \"gh:o/hardcore\"\nfamilies = [\"vanilla\"]\nfolders = [\"Hardcore\"]\n",
        )
        .unwrap();
        let (catalog, warnings) = Catalog::load(&paths);
        assert!(warnings.is_empty(), "{warnings:?}");
        let installed = |w: &str| ["dbm", "classic"].iter().any(|a| a.eq_ignore_ascii_case(w));
        let table = RulesTable::builtin();
        let turtle = TargetVocabulary::new(&table, ["turtle"]);
        let all = TargetVocabulary::new(&table, ["all"]);
        let cases: &[(&str, &[&str], &TargetVocabulary, &[usize])] = &[
            ("client_after_a_name", &["dbm", "retail"], &turtle, &[1]),
            ("client_alone", &["retail"], &turtle, &[0]),
            ("case_ignored", &["Retail"], &turtle, &[0]),
            ("two_clients", &["dbm", "retail", "era"], &turtle, &[1, 2]),
            ("registered_id", &["turtle"], &turtle, &[0]),
            ("each_kind: mode", &["sod"], &turtle, &[0]),
            ("each_kind: mode alias", &["mop"], &turtle, &[0]),
            (
                "each_kind: product code",
                &["wow_classic_era"],
                &turtle,
                &[0],
            ),
            ("each_kind: flavor folder", &["_retail_"], &turtle, &[0]),
            ("each_kind: family", &["wrath-legacy"], &turtle, &[0]),
            ("each_kind: family row id", &["wotlk"], &turtle, &[0]),
            ("installed_addon_wins", &["classic"], &turtle, &[]),
            ("catalog_name_wins", &["hardcore"], &turtle, &[]),
            ("catalog_name_wins: any case", &["HARDCORE"], &turtle, &[]),
            ("catalog_name_wins: alias", &["hcx"], &turtle, &[]),
            ("builtin_catalog_name", &["weakauras"], &turtle, &[]),
            ("builtin_catalog_name: dbm", &["dbm"], &turtle, &[]),
            ("no_client_words", &["nosuch", "questie"], &turtle, &[]),
            ("all_is_no_client_word", &["all"], &turtle, &[]),
            ("all_as_a_target_id", &["all"], &all, &[0]),
            ("nothing", &[], &turtle, &[]),
        ];
        for (case, words, vocabulary, want) in cases {
            let got = misplaced_words(&owned(words), vocabulary, &catalog, &installed);
            assert_eq!(got, *want, "{case}: {words:?}");
        }
    }

    /// (case, argv, fix, details.target, details.command).
    type Moved<'a> = (&'a str, &'a [&'a str], &'a [&'a str], &'a str, &'a str);

    #[test]
    fn misplaced_error_moves_the_client_into_dash_t_in_place() {
        let cases: &[Moved<'_>] = &[
            (
                "alone",
                &["update", "retail"],
                &["update", "-t", "retail"],
                "retail",
                "update",
            ),
            (
                "after a name",
                &["update", "dbm", "retail"],
                &["update", "dbm", "-t", "retail"],
                "retail",
                "update",
            ),
            (
                "after two names",
                &["remove", "dbm", "questie", "retail"],
                &["remove", "dbm", "questie", "-t", "retail"],
                "retail",
                "remove",
            ),
            (
                "an alias, a presentation flag dropped",
                &["rm", "dbm", "retail", "--json"],
                &["rm", "dbm", "-t", "retail"],
                "retail",
                "remove",
            ),
            (
                "a flag after it kept",
                &["install", "questie", "era", "--dry-run"],
                &["install", "questie", "-t", "era", "--dry-run"],
                "era",
                "install",
            ),
            (
                "a global flag's value kept",
                &["upgrade", "dbm", "retail", "--reason", "x"],
                &["upgrade", "dbm", "-t", "retail", "--reason", "x"],
                "retail",
                "update",
            ),
            (
                "--if-match kept",
                &["remove", "dbm", "retail", "--if-match", "pc1:x"],
                &["remove", "dbm", "-t", "retail", "--if-match", "pc1:x"],
                "retail",
                "remove",
            ),
            (
                "a presentation flag before the verb dropped",
                &["--json", "remove", "dbm", "retail"],
                &["remove", "dbm", "-t", "retail"],
                "retail",
                "remove",
            ),
        ];
        for (case, words, fix, target, command) in cases {
            let typed = argv(words);
            let got = hooked(&typed);
            let message = format!("'{target}' is a client, not an addon; the client goes in -t");
            let want = crate::cli::misplaced_client_error(
                message,
                argv(fix),
                &owned(&command.split(' ').collect::<Vec<_>>()),
                target,
            );
            let got = got.unwrap_or_else(|| panic!("{case}: {typed:?} gets no fix")); // ubs:ignore
            assert_eq!(
                got,
                want,
                "{case}: {typed:?}\n got  {}\n want {}",
                shown(&got),
                shown(&want)
            );
            assert!(!got.retryable, "{case}");
            assert_eq!(got.usage_fix, UsageFix::UserVerb, "{case}");
            assert_eq!(got.code, ErrorCode::Usage, "{case}");
        }
    }

    #[test]
    fn misplaced_error_declines_what_it_cannot_fix() {
        for (case, words) in [
            ("the only name of remove", &["remove", "retail"][..]),
            ("the only name of install", &["install", "retail"]),
            ("the only names", &["remove", "retail", "era"]),
        ] {
            let typed = argv(words);
            let got = hooked(&typed);
            assert!(
                got.is_none(),
                "{case}: {typed:?} got {:?}",
                got.map(|e| shown(&e))
            );
        }
        // After `--`, by the user's marking literal: clap's positionals hold it, argv's
        // walk does not.
        let after = argv(&["update", "dbm", "--", "retail"]);
        let got = misplaced_error(&after, &owned(&["dbm", "retail"]), &[1]);
        assert!(got.is_none(), "after --: {:?}", got.map(|e| shown(&e)));
        // Positionals that are not argv's.
        let typed = argv(&["update", "dbm", "retail"]);
        let got = misplaced_error(&typed, &owned(&["dbm", "era"]), &[1]);
        assert!(got.is_none(), "disagreeing: {:?}", got.map(|e| shown(&e)));
    }

    #[test]
    fn several_misplaced_clients_move_the_first_and_offer_the_others() {
        let help = Suggestion::command(["omaddons", "help", "update"]);
        for (words, fix, others, message) in [
            (
                &["update", "retail", "dbm", "era"][..],
                &["update", "-t", "retail", "dbm"][..],
                &[&["update", "dbm", "-t", "era"][..]][..],
                "'retail' and 'era' are clients, not addons; the client goes in -t, one per command",
            ),
            (
                &["update", "sod", "hc", "era"],
                &["update", "-t", "sod"],
                &[&["update", "-t", "hc"][..], &["update", "-t", "era"]],
                "'sod', 'hc' and 'era' are clients, not addons; the client goes in -t, one per command",
            ),
        ] {
            let typed = argv(words);
            let got = hooked(&typed).unwrap_or_else(|| panic!("{typed:?} gets no fix")); // ubs:ignore
            let ctx = format!("{typed:?}: {}", shown(&got));
            assert_eq!(got.fix.argv(), Some(argv(fix).as_slice()), "{ctx}");
            let mut alternatives = vec![help.clone()];
            alternatives.extend(others.iter().map(|o| Suggestion::command(argv(o))));
            assert_eq!(got.alternatives, alternatives, "{ctx}");
            assert_eq!(got.message, message, "{ctx}");
            let first = &words[1];
            assert_eq!(
                got.details,
                serde_json::json!({ "target": first, "command": "update" }),
                "{ctx}"
            );
            assert_eq!(got.usage_fix, UsageFix::UserVerb, "{ctx}");
        }
    }
}
