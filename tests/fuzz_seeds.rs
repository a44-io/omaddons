//! The committed fuzz seeds under `fuzz/seeds/<target>/`: written from the test builders
//! and fixtures by an ignored generator, and checked here so every fuzz target keeps a
//! source file and at least three seeds, one of them malformed.
//! `OMADDONS_WRITE_SEEDS=1 cargo test --test fuzz_seeds -- --ignored write`
//!
//! e2e suite: the `journal` fuzz property on every committed journal seed, each seed's
//! diagnosis logged.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::archives::{RawEntry, raw_zip, zip_text};
use common::buggrabber::{self, Writer};
use common::env::TestEnv;
use common::product_db::ProductDb;
use common::tlog::{TestLog, tlog};
use common::trees::BattlenetRoot;
use omaddons::engine::simulate::{fuzz_diagnosis, fuzz_disk_property, fuzz_property};
use omaddons::model::TxId;
use omaddons::state::paths::Paths;

/// Every target of `fuzz/Cargo.toml`.
const TARGETS: &[&str] = &[
    "bpsv",
    "product_db",
    "lutris",
    "heroic",
    "steam_vdf",
    "steam_shortcuts",
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
    "atom_feed",
    "savedvariables",
    "wowace_listing",
    "backup_tar",
    "journal",
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

/// The `journal` target's property on every committed seed, in every gate: the parse and
/// in-memory arms, and the disk arm on a scratch home whose journal is the seed.
#[test]
fn journal_seeds_satisfy_the_fuzz_property() {
    let log: TestLog = tlog!();
    let env = TestEnv::new();
    let root = env.root();
    let paths = Paths::with_roots(root, root.join("c"), root.join("s"), root.join("k"));
    fs::create_dir_all(paths.journal_dir()).unwrap();
    let journal = paths.journal(&TxId::new());
    let mut seeds: Vec<PathBuf> = fs::read_dir(fuzz_dir().join("seeds/journal"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    seeds.sort();
    assert!(seeds.len() >= 3, "{seeds:?}");
    for seed in seeds {
        let name = seed.file_name().unwrap().to_string_lossy().into_owned();
        log.step(&name);
        let bytes = fs::read(&seed).unwrap();
        log.json("diagnosis", &fuzz_diagnosis(&bytes));
        fuzz_property(&bytes);
        fs::write(&journal, &bytes).unwrap();
        fuzz_disk_property(&paths);
    }
}

#[test]
#[ignore = "writes fuzz/seeds: OMADDONS_WRITE_SEEDS=1"]
fn write_fuzz_seeds() {
    // A generator run on purpose, not a checkpoint check: unset, it has nothing to do.
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
    write(
        "product_db",
        "multi-product.db",
        &common::product_db::multi_product().bytes(),
    );

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
        "heroic",
        "config.json",
        br#"{"defaultSettings": {"winePrefix": "~/Games/Heroic/Prefixes/shared", "defaultWinePrefixDir": "~/Games/Heroic/Prefixes", "defaultWinePrefix": "/g/old"}, "version": "v0"}"#,
    );
    write(
        "heroic",
        "library.json",
        br#"{"games": [{"runner": "sideload", "app_name": "x1", "title": "WoW", "install": {"executable": "/g/wow/Wow.exe", "platform": "Windows", "is_dlc": false}, "folder_name": "/g/wow"}, {"install": {"platform": "Browser"}}], "installed": []}"#,
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
    let shortcuts = common::launchers::shortcuts_vdf(&[
        (
            0x8000_0001,
            Path::new("/h/Games/World of Warcraft/_retail_/Wow.exe"),
            Path::new("/h/Games/World of Warcraft/_retail_/"),
        ),
        (
            3_000_000_000,
            Path::new("/usr/bin/supertuxkart"),
            Path::new("/usr/bin/"),
        ),
    ]);
    write("steam_shortcuts", "shortcuts.vdf", &shortcuts);
    write(
        "steam_shortcuts",
        "lowercase-and-every-type.vdf",
        b"\x00shortcuts\x00\x000\x00\x03f\x00\x00\x00\x80\x3f\x05w\x00a\x00\x00\x00\x07u\x00\x01\x02\x03\x04\x05\x06\x07\x08\x01exe\x00\"Wow.exe\"\x00\x01startdir\x00\"/g/wow\"\x00\x0B\x0B",
    );
    write(
        "steam_shortcuts",
        "malformed-truncated.vdf",
        &shortcuts[..shortcuts.len() / 2],
    );
    write(
        "bottles",
        "bottle.yml",
        b"Arch: win64\nName: wow\nPath: wow\nRunner: soda-9.0-1\nExternal_Programs:\n  x:\n    Path: /g/other.exe\n",
    );
    write(
        "bottles",
        "custom-path-bottle.yml",
        b"Arch: win64\r\nName: wow\r\nPath: ~/Games/bottles/wow\r\nCustom_Path: true\r\n",
    );
    write(
        "bottles",
        "placeholder.yml",
        b"Path: /mnt/games/Bottles/wow2\n",
    );
    write(
        "bottles",
        "data.yml",
        b"custom_bottles_path: '~/Bottles'\nnotes:\n  path: /x\n",
    );
    write(
        "bottles",
        "malformed.yml",
        b"Path: 'unclosed\nPath:\n\xff\xfe",
    );

    // toc: the parser fixtures and a hostile one.
    let tocs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toc/parser");
    for name in [
        "modern.toc",
        "unreplaced.toc",
        "wotlk.toc",
        "conditional.toc",
        "elvui-shaped.toc",
        "libraries-shaped.toc",
        "gw2-shaped.toc",
    ] {
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

    // atom_feed: a recorded commits feed, a releases feed with HTML notes, a broken one.
    let recorded = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/fixtures/http/catalog/github.com/get-shagu-pfUI-commits-master.atom-7c40e418.body",
    );
    write("atom_feed", "commits.atom", &fs::read(recorded).unwrap());
    let feeds = env.root().join("feeds");
    common::http::gh_feed(
        &feeds,
        "https://github.com/o/r",
        &[
            (
                "v2.0",
                "<h2>Fixes</h2><ul><li>Faster <b>loading</b> &amp; more</li></ul>",
            ),
            ("v1.0", "<p>First</p>"),
        ],
        None,
    );
    write(
        "atom_feed",
        "releases.atom",
        &fs::read(feeds.join("github.com/feed-o-r.body")).unwrap(),
    );
    write(
        "atom_feed",
        "malformed.atom",
        b"<feed><entry><id>tag:github.com,2008:Grit::Commit/abc</id><content type=\"html\">&lt;b&gt;unclosed<entry><title><![CDATA[x</title></entry",
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

    // wowi_filelist: entries as the API writes them, an empty list, a broken body. The
    // date is fixed, so the committed seed never changes with the clock.
    let list: Vec<serde_json::Value> = [
        common::wowi::FileEntry::new(5995, "BugSack", &["BugSack", "__MACOSX", "bugsack"]),
        common::wowi::FileEntry::new(5996, "!BugGrabber", &["!BugGrabber", ""]),
    ]
    .iter()
    .map(|entry| {
        let mut entry = entry.clone();
        entry.date_ms = 1_760_300_000_000;
        entry.json()
    })
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

    // wowace_listing: mod_dav_svn's two index formats and the PROPFIND tag list, as
    // documented; BugGrabber's .pkgmeta and one that needs the packager; a document cut
    // short, and names a listing refuses.
    write("wowace_listing", "html-index", WOWACE_HTML.as_bytes());
    write("wowace_listing", "xml-index", WOWACE_XML.as_bytes());
    write(
        "wowace_listing",
        "propfind-tags",
        WOWACE_PROPFIND.as_bytes(),
    );
    write(
        "wowace_listing",
        "pkgmeta-bug-grabber",
        b"package-as: \"!BugGrabber\"\n",
    );
    write(
        "wowace_listing",
        "pkgmeta-externals",
        b"package-as: Foo\nexternals:\n  Libs/LibStub: https://repos.wowace.com/wow/libstub/trunk\nignore:\n  - README.md\nmove-folders:\n  Foo/Modules: FooModules\n",
    );
    write(
        "wowace_listing",
        "malformed-truncated",
        &WOWACE_PROPFIND.as_bytes()[..WOWACE_PROPFIND.len() / 2],
    );
    write(
        "wowace_listing",
        "malformed-names",
        b"<title>wow - Revision 9: /x</title><ul><li><a href=\"../\">..</a></li><li><a href=\"a%2Fb\">x</a></li><li><a href=\"Core.lua\">a</a></li><li><a href=\"core.lua\">b</a></li><li><a href=\"%01\">c</a></li></ul>\n--@debug@\n@project-revision@ a@b",
    );

    // backup_tar: a real export (a verified backup with a read-only directory and a
    // symlink), the same cut short, one with a name that leaves WTF/, and a frame asking
    // for a 2 GiB decoder window.
    let home = common::home::Home::new(&["wow"]);
    common::wtf::Wtf::at(home.client("_retail_"))
        .config_wtf("SET locale \"enUS\"\n")
        .saved_variables("Foo", 64)
        .file("Account/FAKEACCT/Sealed/Kept.lua", b"Kept = 1")
        .mode("Account/FAKEACCT/Sealed", 0o500)
        .build();
    let created = home
        .run(&["backup", "create", "-t", "retail", "--verify"])
        .expect(0);
    let id = created["data"]["targets"][0]["backup"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = home.env.root().join("export.tar.zst");
    home.run(&["backup", "export", &id, &out.display().to_string()])
        .expect(0);
    let export = fs::read(&out).unwrap();
    write("backup_tar", "export", &export);
    write(
        "backup_tar",
        "malformed-truncated",
        &export[..export.len() / 2],
    );
    let mut entries = common::archives::tar_entries(&export);
    let last = entries.len() - 1;
    entries.insert(last, common::archives::TarEntry::file("WTF/../x", b"x"));
    write(
        "backup_tar",
        "malformed-traversal",
        &common::archives::tar_zst(&entries),
    );
    write(
        "backup_tar",
        "malformed-window",
        &common::archives::zstd_huge_window(),
    );

    // desired_toml: the documented example, an empty file, a broken one.
    write("desired_toml", "example.toml", DESIRED_EXAMPLE.as_bytes());
    write("desired_toml", "empty.toml", b"");
    write(
        "desired_toml",
        "malformed.toml",
        b"schema_version = \"1.0\"\n[family.mainline\naddons = [\"DBM\", 3\n[[family.mainline.addon]]\nname = \"\"\n",
    );

    // savedvariables: BugGrabber in both writer formats, r152's chunks, a DB embedded in
    // another addon's file, bare keys as a hand edit writes them, a file cut inside a
    // string, and tables nested past any sane depth.
    let modern = buggrabber::modern_int(Writer::Modern).render();
    write("savedvariables", "modern-crlf", &modern);
    write(
        "savedvariables",
        "legacy-tabs",
        &buggrabber::modern_int(Writer::Legacy).render(),
    );
    write(
        "savedvariables",
        "chunked-r152",
        &buggrabber::r152_chunked(Writer::Legacy).render(),
    );
    write(
        "savedvariables",
        "embedded-in-addon",
        &buggrabber::embedded_bdui(Writer::Modern)[0].bytes,
    );
    write(
        "savedvariables",
        "bare-keys",
        b"\r\nBugGrabberDB = {\r\nsession = 3,\r\nerrors = {\r\n{\r\nmessage = \"x:1: boom\",\r\ntime = 1792089000,\r\n},\r\n},\r\n}\r\n",
    );
    let quote = modern
        .windows(15)
        .position(|w| w == b"[\"message\"] = \"")
        .unwrap();
    write(
        "savedvariables",
        "malformed-unterminated",
        &modern[..quote + 40],
    );
    let mut deep = b"\r\nDeepDB = ".to_vec();
    deep.extend("{\r\n".repeat(4096).bytes());
    deep.extend("},\r\n".repeat(4095).bytes());
    deep.extend(b"}\r\n");
    write("savedvariables", "malformed-deep", &deep);

    // journal: the journals of real transactions the binary was killed in (an update cut
    // before its third rename, after `swapped` and after `committed`; a whole and a
    // partial settings restore), the test root renamed `/sim`; a newer major, a torn tail
    // and a malformed middle line made from the first.
    let journal_of = |s: &common::engine::Scene| {
        let journals = s.journals();
        assert_eq!(journals.len(), 1, "{journals:?}");
        let text = fs::read_to_string(&journals[0]).unwrap();
        let root = s.home.env.root();
        let canonical = fs::canonicalize(root).unwrap();
        text.replace(&canonical.display().to_string(), "/sim")
            .replace(&root.display().to_string(), "/sim")
    };
    // An update of Foo from v1 to v2, killed at `failpoint`.
    let crashed_update = |failpoint: &str| {
        let s = common::engine::Scene::new().with_saved_variables(common::engine::FOLDERS);
        s.install("gh:o/foo", "v1");
        let run = s.run(&s.install_args("gh:o/foo", "v2"), failpoint);
        assert_ne!(run.code, 0, "{failpoint} killed it");
        journal_of(&s)
    };
    // A client with Foo's and Bar's SavedVariables, backed up.
    let restore_scene = || {
        let s = common::engine::Scene::new();
        let wtf = common::wtf::Wtf::at(s.home.client("_retail_"))
            .config_wtf("SET locale \"enUS\"\n")
            .saved_variables("Foo", 300)
            .saved_variables("Bar", 100)
            .build();
        let created = s.run(&s.args(&["backup", "create"]), "").expect(0);
        let id = created["data"]["targets"][0]["backup"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        (s, wtf, id)
    };
    // What the game does to Foo's SavedVariables.
    let play = |wtf: &Path| {
        for rel in common::trees::tree_entries(wtf).into_keys() {
            if rel.ends_with("/Foo.lua") {
                let path = wtf.join(rel);
                let mut text = fs::read_to_string(&path).unwrap();
                text.push_str("FooDB.played = true\n");
                fs::write(&path, text).unwrap();
            }
        }
    };
    let update = crashed_update("engine.swap.before_rename=abort@3");
    write("journal", "update_prefix_2.jsonl", update.as_bytes());
    write(
        "journal",
        "swapped.jsonl",
        crashed_update("engine.swapped=abort").as_bytes(),
    );
    write(
        "journal",
        "committed.jsonl",
        crashed_update("engine.committed=abort").as_bytes(),
    );
    // A whole restore killed after its exchange.
    let (s, wtf, id) = restore_scene();
    play(&wtf);
    let run = s.run(
        &s.args(&["backup", "restore", &id]),
        "engine.restore.exchanged=abort",
    );
    assert_ne!(run.code, 0, "killed after the exchange");
    let whole = journal_of(&s);
    // A partial one (Foo's SavedVariables) killed there too, whose files the game then
    // wrote, and whose recovery was killed once its preservation intent was journaled.
    let (s, wtf, id) = restore_scene();
    play(&wtf);
    let restore = s.args(&["backup", "restore", &id, "--addon", "Foo"]);
    let run = s.run(&restore, "engine.restore.exchanged=abort");
    assert_ne!(run.code, 0, "killed after the file exchanges");
    play(&wtf);
    let run = s.run(&restore, "engine.preserve.intent=abort");
    assert_ne!(run.code, 0, "recovery killed once its intent was journaled");
    let partial = journal_of(&s);
    assert!(partial.contains("\"type\":\"preserve\""), "{partial}");
    write("journal", "whole_restore_exchanged.jsonl", whole.as_bytes());
    write(
        "journal",
        "partial_restore_with_intent.jsonl",
        partial.as_bytes(),
    );
    let newer = update.replacen(
        "\"schema_version\":\"1.1\"",
        "\"schema_version\":\"9.0\"",
        1,
    );
    assert_ne!(newer, update, "the prepared line names its version");
    write("journal", "newer_major.jsonl", newer.as_bytes());
    let body = update.strip_suffix('\n').unwrap();
    let last = body.rfind('\n').unwrap() + 1;
    let torn = &update[..last + (body.len() - last) / 2];
    write("journal", "torn_tail.jsonl", torn.as_bytes());
    let first = update.find('\n').unwrap() + 1;
    let malformed = format!("{}not json\n{}", &update[..first], &update[first..]);
    write(
        "journal",
        "malformed_middle_line.jsonl",
        malformed.as_bytes(),
    );
}

/// mod_dav_svn's HTML index of BugGrabber v12.1.0, as documented (C2 records the real one).
const WOWACE_HTML: &str = r#"<html><head><title>wow - Revision 401: /bug-grabber/tags/v12.1.0</title></head>
<body>
 <h2>wow - Revision 401: /bug-grabber/tags/v12.1.0</h2>
 <ul>
  <li><a href="../">..</a></li>
  <li><a href=".pkgmeta">.pkgmeta</a></li>
  <li><a href="%21BugGrabber.toc">!BugGrabber.toc</a></li>
  <li><a href="BugGrabber.lua">BugGrabber.lua</a></li>
  <li><a href="load.xml">load.xml</a></li>
 </ul>
 <hr noshade><em>Powered by <a href="http://subversion.apache.org/">Apache Subversion</a> version 1.9.3 (r1718519).</em>
</body></html>
"#;

/// The XML index of the same directory.
const WOWACE_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<?xml-stylesheet type="text/xsl" href="/svnindex.xsl"?>
<svn version="1.9.3 (r1718519)" href="http://subversion.apache.org/">
  <index rev="401" path="/bug-grabber/tags/v12.1.0" base="wow">
    <updir href="../"/>
    <file name=".pkgmeta" href=".pkgmeta"/>
    <file name="!BugGrabber.toc" href="%21BugGrabber.toc"/>
    <file name="BugGrabber.lua" href="BugGrabber.lua"/>
    <file name="load.xml" href="load.xml"/>
  </index>
</svn>
"#;

/// The `PROPFIND Depth: 1` multistatus of BugGrabber's `tags/`: the collection, two tags.
const WOWACE_PROPFIND: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:ns0="DAV:">
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://subversion.tigris.org/xmlns/dav/">
<D:href>/wow/bug-grabber/tags/</D:href>
<D:propstat><D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:version-name>401</lp1:version-name>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response>
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://subversion.tigris.org/xmlns/dav/">
<D:href>/wow/bug-grabber/tags/v12.0.9/</D:href>
<D:propstat><D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:version-name>369</lp1:version-name>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response>
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://subversion.tigris.org/xmlns/dav/">
<D:href>/wow/bug-grabber/tags/v12.1.0/</D:href>
<D:propstat><D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:version-name>401</lp1:version-name>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
</D:response>
</D:multistatus>
"#;

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
