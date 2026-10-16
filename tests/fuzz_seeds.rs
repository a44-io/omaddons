//! The committed fuzz seeds under `fuzz/seeds/<target>/`: written from the test builders
//! and fixtures by an ignored generator, and checked here so every fuzz target keeps a
//! source file and at least three seeds, one of them malformed.
//! `OMADDONS_WRITE_SEEDS=1 cargo test --test fuzz_seeds -- --ignored write`

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::archives::{RawEntry, raw_zip, zip_text};
use common::env::TestEnv;
use common::product_db::ProductDb;
use common::trees::BattlenetRoot;

/// Every target of `fuzz/Cargo.toml`.
const TARGETS: &[&str] = &[
    "bpsv",
    "product_db",
    "lutris",
    "heroic",
    "steam_vdf",
    "bottles",
    "toc",
    "archive",
    "ref_parse",
    "desired_toml",
    "wowi_filelist",
    "html_text",
    "bbcode_text",
    "wowi_details",
    "tukui_addons",
];

fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz")
}

#[test]
fn every_fuzz_target_has_a_source_and_seeds() {
    let manifest = fs::read_to_string(fuzz_dir().join("Cargo.toml")).unwrap();
    for target in TARGETS {
        assert!(
            manifest.contains(&format!("name = \"{target}\"")),
            "{target} is a [[bin]] of fuzz/Cargo.toml"
        );
        assert!(
            fuzz_dir()
                .join(format!("fuzz_targets/{target}.rs"))
                .is_file()
        );
        let seeds: Vec<String> = fs::read_dir(fuzz_dir().join("seeds").join(target))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(seeds.len() >= 3, "{target}: {seeds:?}");
        assert!(
            seeds.iter().any(|s| s.starts_with("malformed")),
            "{target} has a malformed seed: {seeds:?}"
        );
    }
    let bins = manifest.matches("[[bin]]").count();
    assert_eq!(bins, TARGETS.len(), "TARGETS lists every fuzz target");
}

#[test]
#[ignore = "writes fuzz/seeds: OMADDONS_WRITE_SEEDS=1"]
fn write_fuzz_seeds() {
    if std::env::var("OMADDONS_WRITE_SEEDS").as_deref() != Ok("1") {
        return;
    }
    let seeds = fuzz_dir().join("seeds");
    let write = |target: &str, name: &str, bytes: &[u8]| {
        let dir = seeds.join(target);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), bytes).unwrap();
    };
    let env = TestEnv::new();

    // bpsv: the current and the older .build.info layouts, a .flavor.info.
    let root = BattlenetRoot::at(env.root().join("bnet"))
        .product("wow")
        .product("wowf")
        .build();
    write(
        "bpsv",
        "build-info",
        &fs::read(root.join(".build.info")).unwrap(),
    );
    write(
        "bpsv",
        "flavor-info",
        &fs::read(root.join("_retail_/.flavor.info")).unwrap(),
    );
    let old = BattlenetRoot::at(env.root().join("old"))
        .product("wow_classic_era")
        .old_build_info_layout()
        .build();
    write(
        "bpsv",
        "old-build-info",
        &fs::read(old.join(".build.info")).unwrap(),
    );
    write(
        "bpsv",
        "malformed",
        b"Branch!STRING:0|Active!DEC:1\nus|1|extra\n|\n## seqn = 1\n\xff",
    );

    // product_db: the recorded database, a synthetic one, a truncated one.
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/product_db");
    write(
        "product_db",
        "retail.db",
        &fs::read(fixtures.join("retail.db")).unwrap(),
    );
    let db = ProductDb::new()
        .with_entry_for("agent", "C:/ProgramData/Battle.net/Agent")
        .install(
            "wow",
            "C:/Program Files (x86)/World of Warcraft",
            "_retail_",
            "12.1.5.70391",
        )
        .install(
            "wowf",
            "D:/Games/World of Warcraft",
            "_forever_",
            "1.60.1.70245",
        )
        .bytes();
    write("product_db", "two-installs.db", &db);
    write("product_db", "malformed-truncated.db", &db[..db.len() / 2]);

    // Launcher configs.
    write(
        "lutris",
        "native.yml",
        b"game:\n  exe: /g/World of Warcraft/_retail_/Wow.exe\n  prefix: '~/Games/wow'  # the Wine prefix\nname: wow\nrunner: wine\nslug: wow\n",
    );
    write(
        "lutris",
        "crlf.yml",
        b"game:\r\n  exe: \"drive_c/WoW 335/Wow.exe\"\r\n  prefix: \"$HOME/Games/wotlk\"\r\n  working_dir: /g/wotlk\r\nsystem:\r\n  prefix_command: gamemoderun\r\n",
    );
    write(
        "lutris",
        "malformed.yml",
        b"game:\n  exe: '/g/unclosed\n  prefix\xff\xfe:\n\0",
    );
    write(
        "heroic",
        "game.json",
        br#"{"wow": {"winePrefix": "/g/wow-pfx", "wineVersion": {"bin": "/usr/bin/wine", "name": "Wine - System", "type": "wine"}, "language": "en"}, "version": "v0"}"#,
    );
    write(
        "heroic",
        "nested.json",
        br#"{"a": {"winePrefix": "~/Games/Heroic/Prefixes/a", "x": [{"winePrefix": "/g/deep"}, 1, null]}, "b": {"winePrefix": ""}}"#,
    );
    write(
        "heroic",
        "malformed-truncated.json",
        br#"{"wow": {"winePrefix": "/g/wo"#,
    );
    write(
        "steam_vdf",
        "libraryfolders.vdf",
        b"\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/h/.local/share/Steam\"\n\t\t\"label\"\t\t\"\"\n\t\t\"apps\"\n\t\t{\n\t\t\t\"228980\"\t\t\"102931551\"\n\t\t}\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"/mnt/games/SteamLibrary\"\n\t}\n}\n",
    );
    write(
        "steam_vdf",
        "escaped.vdf",
        b"\"libraryfolders\"\r\n{\r\n\t\"0\"\r\n\t{\r\n\t\t\"PATH\" \"/mnt/a \\\"b\\\" \\\\c\"\r\n\t}\r\n}\r\n",
    );
    write(
        "steam_vdf",
        "malformed.vdf",
        b"\"libraryfolders\"\n{\n\t\"path\"\"/x\n\t\"path\"\t\"\xff",
    );
    write(
        "bottles",
        "bottle.yml",
        b"Arch: win64\nName: wow\nPath: wow\nRunner: soda-9.0-1\nExternal_Programs:\n  x:\n    Path: /g/other.exe\n",
    );
    write(
        "bottles",
        "custom-path.yml",
        b"Name: wow\r\nCustom_Path: true\r\nPath: ~/Games/bottles/wow\r\n",
    );
    write(
        "bottles",
        "malformed.yml",
        b"Path: 'unclosed\nPath:\n\xff\xfe",
    );

    // toc: the parser fixtures and a hostile one.
    let tocs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toc/parser");
    for name in ["modern.toc", "unreplaced.toc", "wotlk.toc"] {
        write("toc", name, &fs::read(tocs.join(name)).unwrap());
    }
    write(
        "toc",
        "malformed.toc",
        b"\xef\xbb\xbf## Interface: 120105, 50504,,x\r\n## Title: |cff00ff00\xff\n#@no-lib-strip@\nLibs\\..\\..\\x.lua\n\0",
    );

    // archive: an addon, a repository-wrapped pair, a path escape, not a zip.
    write(
        "archive",
        "addon.zip",
        &zip_text(&[
            ("Foo/Foo.toc", "## Interface: 120105\n"),
            ("Foo/Foo.lua", "print(1)\n"),
        ]),
    );
    write(
        "archive",
        "wrapped.zip",
        &zip_text(&[
            ("Repo-main/A/A.toc", "## Interface: 30300\n"),
            ("Repo-main/B/B-Classic.toc", "## Interface: 11509\n"),
            ("Repo-main/README.md", "x\n"),
        ]),
    );
    write(
        "archive",
        "malformed-escape.zip",
        &raw_zip(&[
            RawEntry::stored("Foo/Foo.toc", b"## Interface: 120105\n"),
            RawEntry::stored("Foo/../../evil.lua", b"x"),
            RawEntry::deflated("Foo/big.lua", &[b'a'; 4096]).declared_size(16),
        ]),
    );
    write(
        "archive",
        "malformed-not-a-zip.zip",
        b"PK\x03\x04 but nothing else",
    );

    // ref_parse: canonical refs, pasted URLs, garbage.
    write("ref_parse", "gh", b"gh:WeakAuras/WeakAuras2");
    write(
        "ref_parse",
        "release-url",
        b"https://github.com/WeakAuras/WeakAuras2/releases/tag/5.19.0",
    );
    write(
        "ref_parse",
        "git",
        b"git:https://github.com/fondlez/GroupBulletinBoard#main",
    );
    write("ref_parse", "url", b"url:https://example.org/addon.zip");
    write("ref_parse", "malformed", b"gh:/\xff#@../..");

    // html_text and bbcode_text: two bytes of max_chars (4000), then the text.
    let text_fixture = |name: &str| {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/text")
                .join(name),
        )
        .unwrap()
    };
    let with_max = |body: &[u8]| -> Vec<u8> {
        let mut seed = 4000u16.to_le_bytes().to_vec();
        seed.extend_from_slice(body);
        seed
    };
    write(
        "html_text",
        "weakauras",
        &with_max(&text_fixture("weakauras_release.html")),
    );
    write(
        "html_text",
        "handmade",
        &with_max(&text_fixture("handmade.html")),
    );
    write(
        "html_text",
        "malformed",
        &with_max(b"<a href=\"never closed>text &#xFFFFFFFF; &#27;<script>x"),
    );
    let details: serde_json::Value =
        serde_json::from_slice(&text_fixture("bugsack_filedetails.json")).unwrap();
    let field = |key: &str| details[key].as_str().unwrap().as_bytes().to_vec();
    write("bbcode_text", "changelog", &with_max(&field("UIChangeLog")));
    write(
        "bbcode_text",
        "description",
        &with_max(&field("UIDescription")),
    );
    write(
        "bbcode_text",
        "malformed",
        &with_max(b"[url=\"unterminated [b][[i]i]] [/list [*][COLOR=red\r\n"),
    );

    // wowi_details: BugSack's real entry, synthetic ones, the API's answer when no id
    // exists, a broken body.
    write(
        "wowi_details",
        "bugsack.json",
        &serde_json::to_vec(&[&details]).unwrap(),
    );
    let synthetic = [
        common::wowi::details(
            5995,
            "BugSack",
            "v12.1.7",
            "683278e9d6e1cbc1514c3066d3d347b2",
        ),
        common::wowi::details(7032, "Clique", "", ""),
    ];
    write(
        "wowi_details",
        "synthetic.json",
        &serde_json::to_vec(&synthetic).unwrap(),
    );
    write(
        "wowi_details",
        "unknown.json",
        br#"{"ERROR":"No AddOn found."}"#,
    );
    write(
        "wowi_details",
        "malformed.json",
        br#"[{"UID":-1,"UIMD5":"ABCDEF0123456789abcdef012345678Z","UIDate":1e30,"UIChangeLog":"[url=<\u0000b]x"},{"UID":"7"},"#,
    );

    // tukui_addons: the list as the API writes it, one project, a broken body.
    let tukui = common::tukui::entry(
        -1,
        "tukui",
        "Tukui",
        "20.463",
        &["11.1.7", "5.5.0", "1.15.7"],
        &["Tukui"],
    );
    write(
        "tukui_addons",
        "list.json",
        &serde_json::to_vec(&[common::tukui::elvui(), tukui]).unwrap(),
    );
    write(
        "tukui_addons",
        "addon.json",
        &serde_json::to_vec(&common::tukui::elvui()).unwrap(),
    );
    write(
        "tukui_addons",
        "malformed.json",
        br#"[{"id":"x","slug":"Elv UI","patch":["12..1",null,7],"url":"http://x"},{"slug":"a","version":1e400},"#,
    );

    // wowi_filelist: entries as the API writes them, an empty list, a broken body.
    let list: Vec<serde_json::Value> = [
        common::wowi::FileEntry::new(5995, "BugSack", &["BugSack", "__MACOSX", "bugsack"]),
        common::wowi::FileEntry::new(5996, "!BugGrabber", &["!BugGrabber", ""]),
    ]
    .iter()
    .map(common::wowi::FileEntry::json)
    .collect();
    write(
        "wowi_filelist",
        "list.json",
        &serde_json::to_vec(&list).unwrap(),
    );
    write("wowi_filelist", "empty.json", b"[]");
    write(
        "wowi_filelist",
        "malformed.json",
        br#"[{"UID":"x","UIDir":[1,null,"a/b"]},{"UID":3,"UIDir":null,"UIDate":"soon"},"#,
    );

    // desired_toml: the documented example, an empty file, a broken one.
    write("desired_toml", "example.toml", DESIRED_EXAMPLE.as_bytes());
    write("desired_toml", "empty.toml", b"");
    write(
        "desired_toml",
        "malformed.toml",
        b"schema_version = \"1.0\"\n[family.mainline\naddons = [\"DBM\", 3\n[[family.mainline.addon]]\nname = \"\"\n",
    );
}

/// The `addons.toml` example of `src/state/desired.rs`.
const DESIRED_EXAMPLE: &str = r#"# Desired addon sets for omaddons. `omaddons sync` makes each client match this file.
schema_version = "1.0"

[family.mainline]                       # every live mainline client
addons = ["WeakAuras", "DBM", "wowi:5995", "LittleWigs"]

[[family.mainline.addon]]               # table form when options are needed
name = "BigWigs"
ref = "gh:BigWigsMods/BigWigs"
dependency = true                       # a lock for LittleWigs' dependency, not a request
version = "v428"

[[family.mainline.addon]]
name = "Plater"
version = "Plater-v661"
pinned = true

[family.vanilla]
addons = ["Questie", "DBM"]

[target."wotlk"]                        # one client by id; the only way to address test clients
addons = ["git:https://github.com/fondlez/GroupBulletinBoard#main"]
"#;
