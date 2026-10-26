//! Synthetic WoW installs: Wine prefixes, Battle.net roots (every vision §4.1 product),
//! custom legacy roots, and `tree_hash` for "discovery never writes" assertions.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::env::TestEnv;
use super::pe::pe_stub;

/// One Battle.net product of the vision §4.1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogProduct {
    pub code: &'static str,
    pub folder: &'static str,
    pub exe: &'static str,
    /// Always four parts, so `.build.info` rows look real.
    pub version: &'static str,
}

/// The vision §4.1 products (October 2026).
#[rustfmt::skip]
pub const PRODUCTS: &[CatalogProduct] = &[
    CatalogProduct { code: "wow",                 folder: "_retail_",          exe: "WoW.exe",         version: "12.1.5.70391" },
    CatalogProduct { code: "wowt",                folder: "_ptr_",             exe: "WoWT.exe",        version: "12.1.0.69933" },
    CatalogProduct { code: "wowxptr",             folder: "_xptr_",            exe: "WoWT.exe",        version: "12.1.5.70402" },
    CatalogProduct { code: "wow_beta",            folder: "_beta_",            exe: "WoWB.exe",        version: "12.0.1.68765" },
    CatalogProduct { code: "wow_classic",         folder: "_classic_",         exe: "WoWClassic.exe",  version: "5.5.4.63854" },
    CatalogProduct { code: "wow_classic_ptr",     folder: "_classic_ptr_",     exe: "WoWClassicT.exe", version: "5.5.4.64012" },
    CatalogProduct { code: "wow_classic_beta",    folder: "_classic_beta_",    exe: "WowB.exe",        version: "1.60.1.70374" },
    CatalogProduct { code: "wow_classic_era",     folder: "_classic_era_",     exe: "WoWClassic.exe",  version: "1.15.9.64520" },
    CatalogProduct { code: "wow_classic_era_ptr", folder: "_classic_era_ptr_", exe: "WowClassicT.exe", version: "2.5.6.64781" },
    CatalogProduct { code: "wow_anniversary",     folder: "_anniversary_",     exe: "WoWClassic.exe",  version: "2.5.6.64702" },
    CatalogProduct { code: "wow_classic_titan",   folder: "_classic_titan_",   exe: "WoWClassic.exe",  version: "3.80.2.63211" },
    CatalogProduct { code: "wowf",                folder: "_forever_",         exe: "Wow.exe",         version: "1.60.1.70245" },
];

/// The catalog row for `code`.
pub fn product(code: &str) -> &'static CatalogProduct {
    PRODUCTS
        .iter()
        .find(|p| p.code == code)
        .expect("a product of the vision §4.1 table")
}

/// The five legacy client versions with their build numbers.
pub const LEGACY_VERSIONS: &[(u16, u16, u16, u16)] = &[
    (1, 12, 1, 5875),
    (2, 4, 3, 8606),
    (3, 3, 5, 12340),
    (4, 3, 4, 15595),
    (5, 4, 8, 18414),
];

/// The vision's current `.build.info` header (15 columns).
pub const BUILD_INFO_HEADER: &str = "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|CDN Key!HEX:16|Install Key!HEX:16|IM Size!DEC:4|CDN Path!STRING:0|CDN Hosts!STRING:0|CDN Servers!STRING:0|Tags!STRING:0|Armadillo!STRING:0|Last Activated!STRING:0|Version!STRING:0|KeyRing!HEX:16|Product!STRING:0";
/// An older layout, before `KeyRing` and `Product` existed.
pub const OLD_BUILD_INFO_HEADER: &str = "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|CDN Key!HEX:16|Install Key!HEX:16|IM Size!DEC:4|CDN Path!STRING:0|CDN Hosts!STRING:0|CDN Servers!STRING:0|Tags!STRING:0|Armadillo!STRING:0|Last Activated!STRING:0|Version!STRING:0";
const TAGS: &str = "Windows x86_64 US? acct-USA? geoip-US? enUS speech?:Windows x86_64 US? acct-USA? geoip-US? enUS text?";

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// `(major, minor, patch, build)` of a dotted version; missing or oversized parts are 0.
fn version_parts(version: &str) -> (u16, u16, u16, u16) {
    let part = |i: usize| {
        version
            .split('.')
            .nth(i)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0)
    };
    (part(0), part(1), part(2), part(3))
}

// ---------------------------------------------------------------------------------------
// Wine prefixes.

/// A Wine prefix at `<home>/Games/<name>` with `drive_c/` and the `c:` and `z:` drives.
pub struct Prefix {
    path: PathBuf,
}

impl Prefix {
    pub fn new(env: &TestEnv, name: &str) -> Self {
        Self::at(env.home().join("Games").join(name))
    }

    /// A prefix at any path.
    pub fn at(path: PathBuf) -> Self {
        fs::create_dir_all(path.join("drive_c")).unwrap();
        fs::create_dir_all(path.join("dosdevices")).unwrap();
        symlink("../drive_c", path.join("dosdevices/c:")).unwrap();
        symlink("/", path.join("dosdevices/z:")).unwrap();
        Self { path }
    }

    /// Maps drive `letter` to `target` (any directory, e.g. outside `drive_c`).
    pub fn with_drive(self, letter: char, target: &Path) -> Self {
        fs::create_dir_all(target).unwrap();
        symlink(target, self.path.join(format!("dosdevices/{letter}:"))).unwrap();
        self
    }

    /// Creates directories under `drive_c` (e.g. `windows`, `users/x`, `ProgramData`).
    pub fn with_dirs(self, dirs: &[&str]) -> Self {
        for dir in dirs {
            fs::create_dir_all(self.drive_c().join(dir)).unwrap();
        }
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn drive_c(&self) -> PathBuf {
        self.path.join("drive_c")
    }

    /// The default Battle.net WoW root inside this prefix.
    pub fn wow_root(&self) -> PathBuf {
        self.drive_c().join("Program Files (x86)/World of Warcraft")
    }
}

// ---------------------------------------------------------------------------------------
// Battle.net roots.

/// A Battle.net root: `.build.info` and one flavor folder per product.
#[derive(Debug, Clone)]
pub struct BattlenetRoot {
    root: PathBuf,
    products: Vec<&'static CatalogProduct>,
    old_layout: bool,
    without_flavor_info: BTreeSet<String>,
    unknown: Vec<(String, Option<String>, Option<String>)>,
    last_addon_version: BTreeMap<String, u32>,
    partial: BTreeSet<String>,
    exe_stub: bool,
}

impl BattlenetRoot {
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            products: Vec::new(),
            old_layout: false,
            without_flavor_info: BTreeSet::new(),
            unknown: Vec::new(),
            last_addon_version: BTreeMap::new(),
            partial: BTreeSet::new(),
            exe_stub: true,
        }
    }

    /// Adds a product of the catalog by code.
    pub fn product(mut self, code: &str) -> Self {
        self.products.push(product(code));
        self
    }

    /// Adds every product of the catalog.
    pub fn all_products(mut self) -> Self {
        self.products.extend(PRODUCTS.iter());
        self
    }

    /// `.build.info` without the `KeyRing` and `Product` columns (one row only makes
    /// sense then).
    pub fn old_build_info_layout(mut self) -> Self {
        self.old_layout = true;
        self
    }

    pub fn without_flavor_info(mut self, folder: &str) -> Self {
        self.without_flavor_info.insert(folder.to_owned());
        self
    }

    /// A folder the catalog does not know, with an optional `.flavor.info` product and
    /// an optional `.build.info` version for that product.
    pub fn unknown_folder(
        mut self,
        folder: &str,
        product: Option<&str>,
        version: Option<&str>,
    ) -> Self {
        self.unknown.push((
            folder.to_owned(),
            product.map(str::to_owned),
            version.map(str::to_owned),
        ));
        self
    }

    /// `SET lastAddonVersion "<n>"` in the folder's `WTF/Config.wtf`.
    pub fn last_addon_version(mut self, folder: &str, interface: u32) -> Self {
        self.last_addon_version.insert(folder.to_owned(), interface);
        self
    }

    /// A half-installed folder: no exe and no `Interface/`.
    pub fn partial(mut self, folder: &str) -> Self {
        self.partial.insert(folder.to_owned());
        self
    }

    /// `false` writes empty exe files instead of PE stubs.
    pub fn exe_stub(mut self, stub: bool) -> Self {
        self.exe_stub = stub;
        self
    }

    /// Writes the root and returns its path.
    pub fn build(self) -> PathBuf {
        let mut rows = Vec::new();
        for p in &self.products {
            rows.push((p.code.to_owned(), p.version.to_owned()));
        }
        for (_, product, version) in &self.unknown {
            if let (Some(product), Some(version)) = (product, version) {
                rows.push((product.clone(), version.clone()));
            }
        }
        write(&self.root.join(".build.info"), self.build_info(&rows));

        for p in &self.products {
            self.flavor_folder(p.folder, Some(p.code), Some(p.exe), p.version);
        }
        for (folder, product, version) in &self.unknown {
            let exe = "Wow.exe";
            self.flavor_folder(
                folder,
                product.as_deref(),
                Some(exe),
                version.as_deref().unwrap_or(""),
            );
        }
        self.root
    }

    fn build_info(&self, rows: &[(String, String)]) -> String {
        let header = if self.old_layout {
            OLD_BUILD_INFO_HEADER
        } else {
            BUILD_INFO_HEADER
        };
        let mut text = format!("{header}\n");
        for (i, (product, version)) in rows.iter().enumerate() {
            let common = format!(
                "us|1|{i:032x}|{:032x}|{:032x}|8|tpr/wow|us.cdn.blizzard.com level3.blizzard.com|http://us.cdn.blizzard.com/?maxhosts=4|{TAGS}||2026-10-01T12:00:00Z|{version}",
                i + 100,
                i + 200
            );
            if self.old_layout {
                text.push_str(&format!("{common}\n"));
            } else {
                text.push_str(&format!(
                    "{common}|3ca57fe7319a297346440e4d2a03a0cd|{product}\n"
                ));
            }
        }
        text
    }

    fn flavor_folder(&self, folder: &str, product: Option<&str>, exe: Option<&str>, version: &str) {
        let dir = self.root.join(folder);
        fs::create_dir_all(&dir).unwrap();
        if let Some(product) = product
            && !self.without_flavor_info.contains(folder)
        {
            write(
                &dir.join(".flavor.info"),
                format!("Product Flavor!STRING:0\n{product}\n"),
            );
        }
        if !self.partial.contains(folder) {
            if let Some(exe) = exe {
                let (major, minor, patch, _) = version_parts(version);
                let bytes = if self.exe_stub {
                    pe_stub(major, minor, patch, 0)
                } else {
                    Vec::new()
                };
                write(&dir.join(exe), bytes);
            }
            fs::create_dir_all(dir.join("Interface/AddOns")).unwrap();
        }
        fs::create_dir_all(dir.join("WTF")).unwrap();
        if let Some(interface) = self.last_addon_version.get(folder) {
            write(
                &dir.join("WTF/Config.wtf"),
                format!("SET locale \"enUS\"\nSET lastAddonVersion \"{interface}\"\n"),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Custom (legacy) roots.

/// The Blizzard addon folders of a 3.3.5 client (23, as on the development machine).
const BLIZZARD_FOLDERS: &[&str] = &[
    "Blizzard_AchievementUI",
    "Blizzard_ArenaUI",
    "Blizzard_AuctionUI",
    "Blizzard_BarbershopUI",
    "Blizzard_BattlefieldMinimap",
    "Blizzard_BindingUI",
    "Blizzard_Calendar",
    "Blizzard_CombatLog",
    "Blizzard_CombatText",
    "Blizzard_DebugTools",
    "Blizzard_GlyphUI",
    "Blizzard_GMChatUI",
    "Blizzard_GMSurveyUI",
    "Blizzard_GuildBankUI",
    "Blizzard_InspectUI",
    "Blizzard_ItemSocketingUI",
    "Blizzard_MacroUI",
    "Blizzard_RaidUI",
    "Blizzard_TalentUI",
    "Blizzard_TimeManager",
    "Blizzard_TokenUI",
    "Blizzard_TradeSkillUI",
    "Blizzard_TrainerUI",
];

/// A custom client root: `Wow.exe` beside `Interface/`, `WTF/` and `Data/`.
#[derive(Debug, Clone)]
pub struct CustomRoot {
    root: PathBuf,
    version: (u16, u16, u16, u16),
    exe: String,
    realmlist: Option<String>,
    last_addon_version: Option<u32>,
    blizzard_folders: usize,
}

impl CustomRoot {
    /// A 3.3.5a (12340) client with realmlist `127.0.0.1`.
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            version: (3, 3, 5, 12340),
            exe: "Wow.exe".to_owned(),
            realmlist: Some("127.0.0.1".to_owned()),
            last_addon_version: None,
            blizzard_folders: 0,
        }
    }

    pub fn version(mut self, major: u16, minor: u16, patch: u16, build: u16) -> Self {
        self.version = (major, minor, patch, build);
        self
    }

    /// The exe's file name (default `Wow.exe`).
    pub fn exe(mut self, name: &str) -> Self {
        self.exe = name.to_owned();
        self
    }

    pub fn realmlist(mut self, host: &str) -> Self {
        self.realmlist = Some(host.to_owned());
        self
    }

    pub fn no_realmlist(mut self) -> Self {
        self.realmlist = None;
        self
    }

    pub fn last_addon_version(mut self, interface: u32) -> Self {
        self.last_addon_version = Some(interface);
        self
    }

    /// `n` `Blizzard_*` folders with plain TOCs in `Interface/AddOns`.
    pub fn blizzard_folders(mut self, n: usize) -> Self {
        self.blizzard_folders = n;
        self
    }

    /// The legacy interface: `major*10000 + minor*100`.
    pub fn interface(&self) -> u32 {
        u32::from(self.version.0) * 10_000 + u32::from(self.version.1) * 100
    }

    /// Writes the root and returns its path.
    pub fn build(self) -> PathBuf {
        let (major, minor, patch, build) = self.version;
        write(
            &self.root.join(&self.exe),
            pe_stub(major, minor, patch, build),
        );
        let addons = self.root.join("Interface/AddOns");
        fs::create_dir_all(&addons).unwrap();
        let mut config = "SET locale \"enUS\"\n".to_owned();
        if let Some(interface) = self.last_addon_version {
            config.push_str(&format!("SET lastAddonVersion \"{interface}\"\n"));
        }
        write(&self.root.join("WTF/Config.wtf"), config);
        if let Some(host) = &self.realmlist {
            write(
                &self.root.join("Data/enUS/realmlist.wtf"),
                format!("set realmlist {host}\n"),
            );
        } else {
            fs::create_dir_all(self.root.join("Data/enUS")).unwrap();
        }
        for i in 0..self.blizzard_folders {
            let name = BLIZZARD_FOLDERS
                .get(i)
                .map_or_else(|| format!("Blizzard_Extra{i}"), |n| (*n).to_owned());
            write(
                &addons.join(&name).join(format!("{name}.toc")),
                format!(
                    "## Interface: {}\n## Title: {name}\n## Secure: 1\n{name}.lua\n",
                    self.interface()
                ),
            );
        }
        self.root
    }
}

// ---------------------------------------------------------------------------------------
// Tree hashing.

/// SHA-256 over the sorted relative paths under `dir`, each entry's kind and permission
/// bits, every file's bytes and every symlink's target (symlinks are not followed).
pub fn tree_hash(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let mut entries = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path.clone());
            }
            let mode = meta.permissions().mode() & 0o7777;
            entries.push((path, meta.file_type(), mode));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash = Sha256::new();
    for (path, kind, mode) in entries {
        let rel = path.strip_prefix(dir).unwrap();
        hash.update(rel.as_os_str().as_encoded_bytes());
        hash.update([0]);
        hash.update(mode.to_le_bytes());
        if kind.is_symlink() {
            hash.update(b"L");
            hash.update(fs::read_link(&path).unwrap().as_os_str().as_encoded_bytes());
        } else if kind.is_dir() {
            hash.update(b"D");
        } else {
            let bytes = fs::read(&path).unwrap();
            hash.update(b"F");
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
        }
        hash.update([0]);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What a tree entry is, as [`tree_hash`] tells entries apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// One entry of [`tree_entries`]: what [`tree_hash`] hashes of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// The permission bits, `mode & 0o7777`.
    pub mode: u32,
    /// A file's length; 0 for a directory or a symlink.
    pub len: u64,
    /// A file's SHA-256.
    pub sha256: Option<String>,
    /// A symlink's target.
    pub target: Option<String>,
}

/// Every entry under `dir`, by relative path: the same walk, kinds and modes as
/// [`tree_hash`], so the two differ together, but able to say which path differs.
/// Symlinks are not followed.
pub fn tree_entries(dir: &Path) -> BTreeMap<String, Entry> {
    use std::os::unix::fs::PermissionsExt;
    let mut entries = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let mode = meta.permissions().mode() & 0o7777;
            let entry = if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap().display().to_string();
                Entry {
                    kind: EntryKind::Symlink,
                    mode,
                    len: 0,
                    sha256: None,
                    target: Some(target),
                }
            } else if meta.is_dir() {
                stack.push(path.clone());
                Entry {
                    kind: EntryKind::Dir,
                    mode,
                    len: 0,
                    sha256: None,
                    target: None,
                }
            } else {
                let bytes = fs::read(&path).unwrap();
                let digest = Sha256::digest(&bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                Entry {
                    kind: EntryKind::File,
                    mode,
                    len: bytes.len() as u64,
                    sha256: Some(digest),
                    target: None,
                }
            };
            let rel = path.strip_prefix(dir).unwrap().display().to_string();
            entries.insert(rel, entry);
        }
    }
    entries
}

/// How two [`tree_entries`] differ: paths only `after` has, paths only `before` has, and
/// paths both have that differ, each with what changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// `(path, what)`: `what` joins `kind`, `content`, `target` and `mode` with `+`, in
    /// that order; a path whose kind changed says `kind` alone.
    pub changed: Vec<(String, String)>,
    pub total: usize,
}

impl TreeDiff {
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The first `n` differing paths in path order: `+ <path>`, `- <path>` or
    /// `~ <path> (<what>)`.
    pub fn first(&self, n: usize) -> Vec<String> {
        let mut lines: Vec<(&str, String)> = Vec::with_capacity(self.total);
        lines.extend(self.added.iter().map(|p| (p.as_str(), format!("+ {p}"))));
        lines.extend(self.removed.iter().map(|p| (p.as_str(), format!("- {p}"))));
        lines.extend(
            self.changed
                .iter()
                .map(|(p, what)| (p.as_str(), format!("~ {p} ({what})"))),
        );
        lines.sort_by(|a, b| a.0.cmp(b.0));
        lines.into_iter().take(n).map(|(_, line)| line).collect()
    }
}

/// What changed from `before` to `after` (two [`tree_entries`]).
pub fn tree_diff(before: &BTreeMap<String, Entry>, after: &BTreeMap<String, Entry>) -> TreeDiff {
    let mut diff = TreeDiff::default();
    for (path, now) in after {
        match before.get(path) {
            None => diff.added.push(path.clone()),
            Some(was) if was != now => diff.changed.push((path.clone(), what_changed(was, now))),
            Some(_) => {}
        }
    }
    diff.removed = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .cloned()
        .collect();
    diff.total = diff.added.len() + diff.removed.len() + diff.changed.len();
    diff
}

/// `kind` alone when the kind changed (the rest does not compare across kinds), else
/// `content`, `target` and `mode` as they differ, joined with `+`.
fn what_changed(was: &Entry, now: &Entry) -> String {
    if was.kind != now.kind {
        return "kind".to_owned();
    }
    let mut what = Vec::new();
    if was.len != now.len || was.sha256 != now.sha256 {
        what.push("content");
    }
    if was.target != now.target {
        what.push("target");
    }
    if was.mode != now.mode {
        what.push("mode");
    }
    what.join("+")
}

/// Every path under `dir` with its kind, length and modification time, sorted: a cheap
/// "nothing was written" check for real installs, where hashing gigabytes would take
/// minutes. Symlinks are not followed.
pub fn tree_stat(dir: &Path) -> Vec<(PathBuf, bool, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path.clone());
            }
            out.push((path, meta.is_dir(), meta.len(), meta.modified().unwrap()));
        }
    }
    out.sort();
    out
}

/// Every path under `dir`, relative and sorted, for failure messages.
pub fn listing(dir: &Path) -> String {
    let mut paths = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                stack.push(path.clone());
            }
            paths.push(path.strip_prefix(dir).unwrap().display().to_string());
        }
    }
    paths.sort();
    paths.join("\n")
}
