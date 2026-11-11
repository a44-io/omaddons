//! Which build this is (M4-D19): `tool_version` stays `0.1.0` for every build until M5,
//! so a token, a stored plan or `capabilities` names the build by its id.

pub mod inputs;

/// This build's id: the first 16 hex digits of a SHA-256 over the explicit input
/// manifest ([`inputs::BUILD_ID_INPUTS`]) and the toolchain, compiled in by `build.rs`.
/// Never read from the environment at run time, and never covering `skills/` (M4-D7).
pub const BUILD_ID: &str = env!("OMADDONS_BUILD_ID");

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use super::inputs::{
        BUILD_ID_EXCLUDED, BUILD_ID_INPUTS, Input, Toolchain, input_digest, rerun_paths,
        unclassified,
    };
    use super::*;

    #[test]
    fn build_id_is_sixteen_lowercase_hex_digits() {
        assert_eq!(BUILD_ID.len(), 16, "{BUILD_ID:?}");
        assert!(
            BUILD_ID
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{BUILD_ID:?}"
        );
    }

    #[test]
    fn every_top_level_path_is_an_input_or_excluded() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let names: Vec<String> = fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let missing = unclassified(names.iter().map(String::as_str));
        assert!(
            missing.is_empty(),
            "unclassified top-level paths: {missing:?}: add each to BUILD_ID_INPUTS or BUILD_ID_EXCLUDED with its reason (src/build_info/inputs.rs)"
        );
    }

    #[test]
    fn a_planted_unlisted_path_is_reported() {
        let names = [
            "src",
            "README.md",
            "new-build-input.txt",
            "lcov.info",
            "default.profraw",
            ".cargo",
        ];
        assert_eq!(unclassified(names), ["new-build-input.txt"]);
    }

    #[test]
    fn no_name_is_both_an_input_and_excluded() {
        for input in BUILD_ID_INPUTS {
            let top = input.top_level();
            assert!(
                BUILD_ID_EXCLUDED.iter().all(|(name, _)| *name != top),
                "{top} is both an input and excluded"
            );
        }
        for (name, why) in BUILD_ID_EXCLUDED {
            assert!(!why.is_empty(), "{name} is excluded without a reason");
        }
    }

    #[test]
    fn skills_never_feeds_the_build_id() {
        assert!(BUILD_ID_EXCLUDED.iter().any(|(name, _)| *name == "skills"));
        for input in BUILD_ID_INPUTS {
            assert!(
                !input.path().starts_with("skills"),
                "{input:?} feeds skills/ into the build id (M4-D7)"
            );
        }
    }

    fn toolchain() -> Toolchain {
        Toolchain {
            rustc_version: "rustc 1.97.1 (00000000 2026-01-01)".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            profile: [
                ("PROFILE", "release"),
                ("OPT_LEVEL", "3"),
                ("DEBUG", "false"),
                ("debug_assertions", "0"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .to_vec(),
            features: Vec::new(),
            rustflags: String::new(),
        }
    }

    /// Every input and a few excluded paths, written in a fixed order (reversed when
    /// `reversed`), with a symlink under `omarchy/`.
    fn tree(root: &Path, reversed: bool) {
        let mut files = vec![
            ("Cargo.toml", "[package]\n"),
            ("Cargo.lock", "version = 4\n"),
            ("build.rs", "fn main() {}\n"),
            ("rust-toolchain.toml", "[toolchain]\n"),
            ("src/lib.rs", "pub mod x;\n"),
            ("src/x.rs", "pub fn x() {}\n"),
            ("src/nested/y.rs", "pub fn y() {}\n"),
            ("crates/facade/src/lib.rs", "pub trait T {}\n"),
            ("catalog/catalog.toml", "schema_version = \"1.0\"\n"),
            ("omarchy/Model.js", "export {}\n"),
            ("skills/omaddons/SKILL.md", "# omaddons\n"),
            ("tests/t.rs", "#[test] fn t() {}\n"),
            ("README.md", "# omaddons\n"),
        ];
        if reversed {
            files.reverse();
        }
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        std::os::unix::fs::symlink("Model.js", root.join("omarchy/link.js")).unwrap();
    }

    fn digest(root: &Path) -> String {
        input_digest(root, &toolchain()).unwrap()
    }

    #[test]
    fn the_input_digest_is_content_and_order_free() {
        type Change = fn(&Path);
        let cases: [(&str, Change, bool); 11] = [
            ("unchanged", |_| {}, false),
            (
                "touch src/x.rs",
                |root| {
                    let file = fs::File::options()
                        .append(true)
                        .open(root.join("src/x.rs"))
                        .unwrap();
                    let later = SystemTime::now() + Duration::from_secs(3600);
                    file.set_modified(later).unwrap();
                },
                false,
            ),
            (
                "one byte of src/x.rs",
                |root| fs::write(root.join("src/x.rs"), "pub fn x() {}\r").unwrap(),
                true,
            ),
            (
                "edit under crates/",
                |root| {
                    fs::write(root.join("crates/facade/src/lib.rs"), "pub trait U {}\n").unwrap()
                },
                true,
            ),
            (
                "new file under catalog/",
                |root| fs::write(root.join("catalog/extra.toml"), "").unwrap(),
                true,
            ),
            (
                "a symlink under omarchy/ retargeted",
                |root| {
                    fs::remove_file(root.join("omarchy/link.js")).unwrap();
                    std::os::unix::fs::symlink("Other.js", root.join("omarchy/link.js")).unwrap();
                },
                true,
            ),
            (
                "edit under skills/",
                |root| fs::write(root.join("skills/omaddons/SKILL.md"), "# new\n").unwrap(),
                false,
            ),
            (
                "edit under tests/",
                |root| fs::write(root.join("tests/t.rs"), "// new\n").unwrap(),
                false,
            ),
            (
                "edit README.md",
                |root| fs::write(root.join("README.md"), "# new\n").unwrap(),
                false,
            ),
            (
                ".cargo/config.toml created",
                |root| {
                    fs::create_dir_all(root.join(".cargo")).unwrap();
                    fs::write(root.join(".cargo/config.toml"), "[build]\n").unwrap();
                },
                true,
            ),
            (
                "Cargo.lock edited",
                |root| fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap(),
                true,
            ),
        ];
        for (case, change, moves) in cases {
            let tmp = tempfile::tempdir().unwrap();
            tree(tmp.path(), false);
            let before = digest(tmp.path());
            change(tmp.path());
            let after = digest(tmp.path());
            // ubs:ignore — comparing content digests, not secrets
            assert_eq!(before != after, moves, "{case}: digest {before} -> {after}");
        }
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        tree(a.path(), false);
        tree(b.path(), true);
        assert_eq!(
            digest(a.path()),
            digest(b.path()),
            "same files created in another order"
        );
    }

    #[test]
    fn a_missing_input_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        tree(tmp.path(), false);
        fs::remove_file(tmp.path().join("Cargo.lock")).unwrap();
        assert!(input_digest(tmp.path(), &toolchain()).is_err());
    }

    #[test]
    fn toolchain_fields_move_the_digest() {
        let tmp = tempfile::tempdir().unwrap();
        tree(tmp.path(), false);
        let base = toolchain();
        let of = |t: &Toolchain| input_digest(tmp.path(), t).unwrap();
        let before = of(&base);
        let profile = |key: &str, value: &str| {
            let mut t = toolchain();
            for (k, v) in &mut t.profile {
                if k == key {
                    *v = value.to_owned();
                }
            }
            t
        };
        let mut cases: Vec<(&str, Toolchain)> = vec![
            ("PROFILE", profile("PROFILE", "debug")),
            ("OPT_LEVEL", profile("OPT_LEVEL", "0")),
            ("DEBUG", profile("DEBUG", "true")),
            ("debug_assertions", profile("debug_assertions", "1")),
        ];
        let mut t = toolchain();
        t.rustc_version = "rustc 1.98.0 (11111111 2026-02-01)".into();
        cases.push(("rustc_version", t));
        let mut t = toolchain();
        t.target = "aarch64-unknown-linux-gnu".into();
        cases.push(("target", t));
        let mut t = toolchain();
        t.features = vec!["default".into()];
        cases.push(("a feature added", t));
        let mut t = toolchain();
        t.rustflags = "--cfg\x1fextra".into();
        cases.push(("rustflags", t));
        for (case, changed) in &cases {
            let after = of(changed);
            assert_ne!(before, after, "{case}: digest {before} -> {after}");
        }
        let mut ab = toolchain();
        ab.features = vec!["a".into(), "b".into()];
        let mut ba = toolchain();
        ba.features = vec!["b".into(), "a".into()];
        assert_eq!(of(&ab), of(&ba), "feature order does not move it");
    }

    #[test]
    fn rerun_paths_watch_every_input() {
        let tmp = tempfile::tempdir().unwrap();
        tree(tmp.path(), false);
        let always: Vec<&str> = BUILD_ID_INPUTS
            .iter()
            .filter(|i| !matches!(i, Input::OptionalFile(_)))
            .map(|i| i.path())
            .collect();
        assert_eq!(
            rerun_paths(tmp.path()),
            always,
            "without .cargo/config.toml"
        );
        fs::create_dir_all(tmp.path().join(".cargo")).unwrap();
        fs::write(tmp.path().join(".cargo/config.toml"), "").unwrap();
        let every: Vec<&str> = BUILD_ID_INPUTS.iter().map(|i| i.path()).collect();
        assert_eq!(rerun_paths(tmp.path()), every, "with .cargo/config.toml");
    }
}
