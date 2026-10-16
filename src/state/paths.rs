//! Where every file lives (vision §5.6), computed and never created: creation goes
//! through fsx. Nothing here writes. [`Paths`] reads only the environment;
//! [`ClientPaths`] also looks up how a client spells `Interface` and `AddOns` on disk,
//! ignoring case as the client does.
//!
//! Every path derives from `HOME` and `XDG_{CONFIG,STATE,CACHE}_HOME` (DEC-6). The XDG
//! rules match etcetera's XDG base strategy: a variable is used only when it holds an
//! absolute path, otherwise the default under `HOME` applies. Unlike etcetera, a missing
//! `HOME` is an error rather than a fallback to the passwd entry, so a test that forgets
//! to set `HOME` can never reach the real home directory.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::discover::{find_child, find_children};
use crate::error::{Error, Result};
use crate::model::{PlanId, TargetKey, TxId};

const APP: &str = "omaddons";

/// The per-user directories: `<config>/omaddons`, `<state>/omaddons`, `<cache>/omaddons`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
    /// `<config>` itself, for paths of other tools under it (the Omarchy plugin).
    config_home: PathBuf,
    /// `<data>` (`$XDG_DATA_HOME`), where launchers keep their configs; omaddons
    /// writes nothing there.
    data_home: PathBuf,
    config: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}

impl Paths {
    /// From the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    /// From an environment lookup, so tests need not mutate the process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        let home = absolute(lookup("HOME")).ok_or_else(|| {
            Error::internal(
                "HOME is not set to an absolute path; omaddons derives every path from it",
            )
        })?;
        let xdg = |name: &str, default: &str| {
            absolute(lookup(name)).unwrap_or_else(|| home.join(default))
        };
        let (config, state, cache) = (
            xdg("XDG_CONFIG_HOME", ".config"),
            xdg("XDG_STATE_HOME", ".local/state"),
            xdg("XDG_CACHE_HOME", ".cache"),
        );
        let data = xdg("XDG_DATA_HOME", ".local/share");
        Ok(Self {
            data_home: data,
            ..Self::with_roots(home, config, state, cache)
        })
    }

    /// From explicit base directories (`<config>`, `<state>`, `<cache>`, without the
    /// `omaddons` component); `<data>` is its default under `home`.
    pub fn with_roots(
        home: impl Into<PathBuf>,
        config: impl AsRef<Path>,
        state: impl AsRef<Path>,
        cache: impl AsRef<Path>,
    ) -> Self {
        let home = home.into();
        Self {
            data_home: home.join(".local/share"),
            home,
            config_home: config.as_ref().to_path_buf(),
            config: config.as_ref().join(APP),
            state: state.as_ref().join(APP),
            cache: cache.as_ref().join(APP),
        }
    }

    /// `<home>`: discovery looks under `Games/`, `.wine/` and the launcher locations.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// `<config>` (`$XDG_CONFIG_HOME`).
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    /// `<data>` (`$XDG_DATA_HOME`).
    pub fn data_home(&self) -> &Path {
        &self.data_home
    }

    /// `<config>/omaddons`
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// `<state>/omaddons`
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// `<cache>/omaddons`
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    // ---- <config>/omaddons -------------------------------------------------------------

    /// User configuration.
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// Optional key files, mode 0600.
    pub fn credentials_dir(&self) -> PathBuf {
        self.config.join("credentials")
    }

    /// The key file of one source; `source` is a plain name such as `curseforge`.
    pub fn credential_file(&self, source: &str) -> PathBuf {
        debug_assert!(is_plain_name(source), "credential source {source:?}");
        self.credentials_dir().join(source)
    }

    /// Desired state for `sync` (M2).
    pub fn addons_toml(&self) -> PathBuf {
        self.config.join("addons.toml")
    }

    /// Catalog overrides (`*.toml`).
    pub fn catalog_d(&self) -> PathBuf {
        self.config.join("catalog.d")
    }

    // ---- Other tools' locations ------------------------------------------------------

    /// The installed Omarchy widget (M3): `<config>/omarchy/plugins/omaddons`.
    pub fn omarchy_plugin_dir(&self) -> PathBuf {
        self.config_home.join("omarchy/plugins/omaddons")
    }

    /// The widget's hidden staging sibling, invisible to the plugin registry.
    pub fn omarchy_plugin_stage(&self) -> PathBuf {
        self.config_home.join("omarchy/plugins/.omaddons.stage")
    }

    /// The default agent skill location (M4): `<home>/.claude/skills/omaddons`.
    pub fn skill_dir(&self) -> PathBuf {
        self.home.join(".claude/skills/omaddons")
    }

    // ---- <state>/omaddons --------------------------------------------------------------

    /// Registered targets.
    pub fn registry_file(&self) -> PathBuf {
        self.state.join("registry.json")
    }

    pub fn targets_dir(&self) -> PathBuf {
        self.state.join("targets")
    }

    /// Managed addons and `generation` of one target.
    pub fn target_record(&self, key: &TargetKey) -> PathBuf {
        self.targets_dir().join(format!("{key}.json"))
    }

    /// The per-file index of one target.
    pub fn files_index(&self, key: &TargetKey) -> PathBuf {
        self.targets_dir().join(format!("{key}.files.json"))
    }

    /// Append-only transaction log.
    pub fn history_file(&self) -> PathBuf {
        self.state.join("history.jsonl")
    }

    /// Derived summary plus per-target generations; the widget's doorbell.
    pub fn status_file(&self) -> PathBuf {
        self.state.join("status.json")
    }

    pub fn journal_dir(&self) -> PathBuf {
        self.state.join("journal")
    }

    /// One in-flight journal, JSON Lines with one fsync'd append per record.
    pub fn journal(&self, tx: &TxId) -> PathBuf {
        self.journal_dir().join(format!("{tx}.jsonl"))
    }

    pub fn inflight_dir(&self) -> PathBuf {
        self.state.join("inflight")
    }

    /// One transaction's liveness flock.
    pub fn inflight_lock(&self, tx: &TxId) -> PathBuf {
        self.inflight_dir().join(format!("{tx}.lock"))
    }

    pub fn state_snapshots_dir(&self) -> PathBuf {
        self.state.join("snapshots")
    }

    /// Pre-transaction copies of state files.
    pub fn state_snapshot(&self, tx: &TxId) -> PathBuf {
        self.state_snapshots_dir().join(tx.to_string())
    }

    pub fn plans_dir(&self) -> PathBuf {
        self.state.join("plans")
    }

    /// One stored plan (M4).
    pub fn plan(&self, id: &PlanId) -> PathBuf {
        self.plans_dir().join(format!("{id}.json"))
    }

    /// Doctor run artifacts (M4).
    pub fn doctor_runs_dir(&self) -> PathBuf {
        self.state.join("doctor").join("runs")
    }

    /// Request log.
    pub fn net_log(&self) -> PathBuf {
        self.state.join("net.log")
    }

    /// The single previous request log kept by rotation.
    pub fn net_log_previous(&self) -> PathBuf {
        self.state.join("net.log.1")
    }

    /// The global flock. The file is immutable: never renamed, truncated or deleted.
    pub fn lock_file(&self) -> PathBuf {
        self.state.join("lock")
    }

    /// Where `prune()` stages state-side victims before deleting them.
    pub fn state_trash(&self) -> PathBuf {
        self.state.join("trash")
    }

    // ---- <cache>/omaddons --------------------------------------------------------------

    pub fn blobs_dir(&self) -> PathBuf {
        self.cache.join("blobs")
    }

    /// A content-addressed zip; `sha256` is lowercase hex.
    pub fn blob(&self, sha256: &str) -> PathBuf {
        debug_assert!(
            sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "blob name {sha256:?}"
        );
        self.blobs_dir().join(sha256)
    }

    /// HTTP validators (ETag, Last-Modified).
    pub fn http_cache_dir(&self) -> PathBuf {
        self.cache.join("http")
    }

    /// The WoWInterface file list (M2).
    pub fn wowi_cache_dir(&self) -> PathBuf {
        self.cache.join("wowi")
    }

    /// The slim WoWInterface project index.
    pub fn wowi_index(&self) -> PathBuf {
        self.wowi_cache_dir().join("index.json")
    }

    /// The discovery cache.
    pub fn discovery_cache(&self) -> PathBuf {
        self.cache.join("discovery.json")
    }

    /// The release memo: what each fetch learned, for answering without the network.
    pub fn release_memo(&self) -> PathBuf {
        self.cache.join("releases.json")
    }

    /// What `export --write` wrote: each file's hash, to protect hand edits.
    pub fn export_memo(&self) -> PathBuf {
        self.cache.join("export.json")
    }

    /// One transaction's temp dir (git clones, partial downloads).
    pub fn cache_tmp(&self, tx: &TxId) -> PathBuf {
        self.cache.join("tmp").join(tx.to_string())
    }

    /// Where `prune()` stages cache victims before deleting them.
    pub fn cache_trash(&self) -> PathBuf {
        self.cache.join("trash")
    }
}

/// Paths inside one client dir (the directory holding `Interface/` and `WTF/`).
///
/// `Interface/.omaddons/` shares a filesystem with `AddOns/` and `WTF/`, so renames
/// and reflinks between them are atomic, and the client does not scan it (D7).
/// Components are in canonical case; discovery passes the on-disk `client_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPaths {
    client_dir: PathBuf,
}

impl ClientPaths {
    pub fn new(client_dir: impl Into<PathBuf>) -> Self {
        Self {
            client_dir: client_dir.into(),
        }
    }

    pub fn client_dir(&self) -> &Path {
        &self.client_dir
    }

    /// `<client dir>/Interface`, spelled as on disk: Wine resolves names ignoring case,
    /// so an existing `interface/` is the client's `Interface/` (the marker does the
    /// same). `Interface` when there is none yet.
    pub fn interface(&self) -> PathBuf {
        find_child(&self.client_dir, "Interface")
            .unwrap_or_else(|| self.client_dir.join("Interface"))
    }

    /// `<client dir>/Interface/AddOns`, each part spelled as on disk. A client creates
    /// it on its first launch, so it may not exist yet.
    pub fn addons(&self) -> PathBuf {
        let interface = self.interface();
        find_child(&interface, "AddOns").unwrap_or_else(|| interface.join("AddOns"))
    }

    /// Two or more spellings of `Interface`, or else of `Interface/AddOns`, that differ
    /// only in case (`Interface/AddOns` beside `interface/addons`); empty when there is
    /// at most one. The game reads only one of them.
    pub fn addons_case_variants(&self) -> Vec<PathBuf> {
        let interfaces = find_children(&self.client_dir, "Interface");
        if interfaces.len() > 1 {
            return interfaces;
        }
        let addons = find_children(&self.interface(), "AddOns");
        if addons.len() > 1 {
            return addons;
        }
        Vec::new()
    }

    /// `<client dir>/Interface/.omaddons`
    pub fn meta_dir(&self) -> PathBuf {
        self.interface().join(".omaddons")
    }

    /// The target marker holding the target's key.
    pub fn marker(&self) -> PathBuf {
        self.meta_dir().join("target.json")
    }

    /// Same-filesystem staging for one transaction.
    pub fn stage(&self, tx: &TxId) -> PathBuf {
        self.meta_dir().join("stage").join(tx.to_string())
    }

    /// AddOns and SavedVariables snapshots of one transaction.
    pub fn snapshots(&self, tx: &TxId) -> PathBuf {
        self.meta_dir().join("snapshots").join(tx.to_string())
    }

    /// Where `prune()` stages client-side victims before deleting them.
    pub fn trash(&self) -> PathBuf {
        self.meta_dir().join("trash")
    }

    /// `WTF/` backups (M3).
    pub fn backups(&self) -> PathBuf {
        self.meta_dir().join("backups")
    }

    /// `<client dir>/WTF`
    pub fn wtf(&self) -> PathBuf {
        self.client_dir.join("WTF")
    }
}

/// `Some` only for a non-empty absolute path.
fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const TX: &str = "01JB8Q2VA00000000000000000";
    const KEY: &str = "01JB8Q2V9W0000000000000000";

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let vars: BTreeMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn paths() -> Paths {
        Paths::with_roots("/h", "/c", "/s", "/k")
    }

    #[test]
    fn every_documented_path() {
        let p = paths();
        let tx: TxId = TX.parse().unwrap();
        let key: TargetKey = KEY.parse().unwrap();
        let plan: PlanId = TX.parse().unwrap();
        let sha = "ab".repeat(32);
        let cases: Vec<(PathBuf, String)> = vec![
            (p.home().to_path_buf(), "/h".into()),
            (p.config_dir().to_path_buf(), "/c/omaddons".into()),
            (p.state_dir().to_path_buf(), "/s/omaddons".into()),
            (p.cache_dir().to_path_buf(), "/k/omaddons".into()),
            (p.config_file(), "/c/omaddons/config.toml".into()),
            (p.credentials_dir(), "/c/omaddons/credentials".into()),
            (
                p.credential_file("curseforge"),
                "/c/omaddons/credentials/curseforge".into(),
            ),
            (p.addons_toml(), "/c/omaddons/addons.toml".into()),
            (p.catalog_d(), "/c/omaddons/catalog.d".into()),
            (p.omarchy_plugin_dir(), "/c/omarchy/plugins/omaddons".into()),
            (
                p.omarchy_plugin_stage(),
                "/c/omarchy/plugins/.omaddons.stage".into(),
            ),
            (p.skill_dir(), "/h/.claude/skills/omaddons".into()),
            (p.registry_file(), "/s/omaddons/registry.json".into()),
            (p.targets_dir(), "/s/omaddons/targets".into()),
            (
                p.target_record(&key),
                format!("/s/omaddons/targets/{KEY}.json"),
            ),
            (
                p.files_index(&key),
                format!("/s/omaddons/targets/{KEY}.files.json"),
            ),
            (p.history_file(), "/s/omaddons/history.jsonl".into()),
            (p.status_file(), "/s/omaddons/status.json".into()),
            (p.journal_dir(), "/s/omaddons/journal".into()),
            (p.journal(&tx), format!("/s/omaddons/journal/{TX}.jsonl")),
            (p.inflight_dir(), "/s/omaddons/inflight".into()),
            (
                p.inflight_lock(&tx),
                format!("/s/omaddons/inflight/{TX}.lock"),
            ),
            (p.state_snapshots_dir(), "/s/omaddons/snapshots".into()),
            (p.state_snapshot(&tx), format!("/s/omaddons/snapshots/{TX}")),
            (p.plans_dir(), "/s/omaddons/plans".into()),
            (p.plan(&plan), format!("/s/omaddons/plans/{TX}.json")),
            (p.doctor_runs_dir(), "/s/omaddons/doctor/runs".into()),
            (p.net_log(), "/s/omaddons/net.log".into()),
            (p.net_log_previous(), "/s/omaddons/net.log.1".into()),
            (p.lock_file(), "/s/omaddons/lock".into()),
            (p.state_trash(), "/s/omaddons/trash".into()),
            (p.blobs_dir(), "/k/omaddons/blobs".into()),
            (p.blob(&sha), format!("/k/omaddons/blobs/{sha}")),
            (p.http_cache_dir(), "/k/omaddons/http".into()),
            (p.wowi_cache_dir(), "/k/omaddons/wowi".into()),
            (p.wowi_index(), "/k/omaddons/wowi/index.json".into()),
            (p.discovery_cache(), "/k/omaddons/discovery.json".into()),
            (p.cache_tmp(&tx), format!("/k/omaddons/tmp/{TX}")),
            (p.cache_trash(), "/k/omaddons/trash".into()),
        ];
        for (actual, expected) in cases {
            assert_eq!(actual, Path::new(&expected));
        }
    }

    #[test]
    fn client_paths_for_flavor_folders_and_custom_roots() {
        let tx: TxId = TX.parse().unwrap();
        for root in [
            "/wow/World of Warcraft/_retail_",
            "/games/wow-335/drive_c/WoW",
        ] {
            let c = ClientPaths::new(root);
            let cases = [
                (c.client_dir().to_path_buf(), root.to_owned()),
                (c.interface(), format!("{root}/Interface")),
                (c.addons(), format!("{root}/Interface/AddOns")),
                (c.meta_dir(), format!("{root}/Interface/.omaddons")),
                (
                    c.marker(),
                    format!("{root}/Interface/.omaddons/target.json"),
                ),
                (
                    c.stage(&tx),
                    format!("{root}/Interface/.omaddons/stage/{TX}"),
                ),
                (
                    c.snapshots(&tx),
                    format!("{root}/Interface/.omaddons/snapshots/{TX}"),
                ),
                (c.trash(), format!("{root}/Interface/.omaddons/trash")),
                (c.backups(), format!("{root}/Interface/.omaddons/backups")),
                (c.wtf(), format!("{root}/WTF")),
            ];
            for (actual, expected) in cases {
                assert_eq!(actual, Path::new(&expected));
            }
        }
    }

    #[test]
    fn client_paths_follow_the_spelling_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let c = ClientPaths::new(root);
        assert_eq!(c.addons(), root.join("Interface/AddOns"), "nothing yet");
        assert!(c.addons_case_variants().is_empty());

        std::fs::create_dir_all(root.join("interface/addons")).unwrap();
        assert_eq!(c.interface(), root.join("interface"));
        assert_eq!(c.addons(), root.join("interface/addons"));
        assert_eq!(c.marker(), root.join("interface/.omaddons/target.json"));
        assert!(c.addons_case_variants().is_empty(), "one spelling is fine");

        std::fs::create_dir_all(root.join("interface/AddOns")).unwrap();
        assert_eq!(
            c.addons_case_variants(),
            [root.join("interface/AddOns"), root.join("interface/addons")]
        );
        std::fs::create_dir_all(root.join("Interface")).unwrap();
        assert_eq!(
            c.interface(),
            root.join("Interface"),
            "the exact name first"
        );
        assert_eq!(
            c.addons_case_variants(),
            [root.join("Interface"), root.join("interface")]
        );
    }

    #[test]
    fn xdg_variables_override_the_home_defaults() {
        let set = Paths::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", "/xc"),
            ("XDG_STATE_HOME", "/xs"),
            ("XDG_CACHE_HOME", "/xk"),
        ]))
        .unwrap();
        assert_eq!(set, Paths::with_roots("/h", "/xc", "/xs", "/xk"));
        assert_eq!(
            (set.config_home(), set.data_home()),
            (Path::new("/xc"), Path::new("/h/.local/share"))
        );
        let data = Paths::from_lookup(lookup(&[("HOME", "/h"), ("XDG_DATA_HOME", "/xd")])).unwrap();
        assert_eq!(data.data_home(), Path::new("/xd"));

        let defaults = Paths::with_roots("/h", "/h/.config", "/h/.local/state", "/h/.cache");
        let unset = Paths::from_lookup(lookup(&[("HOME", "/h")])).unwrap();
        assert_eq!(unset, defaults);
        let empty_or_relative = Paths::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("XDG_CONFIG_HOME", ""),
            ("XDG_STATE_HOME", "relative/state"),
            ("XDG_CACHE_HOME", ""),
        ]))
        .unwrap();
        assert_eq!(empty_or_relative, defaults);
    }

    #[test]
    fn missing_home_is_an_internal_error() {
        for vars in [&[][..], &[("HOME", "")][..], &[("HOME", "relative")][..]] {
            let err = Paths::from_lookup(lookup(vars)).unwrap_err();
            assert_eq!(err.code, crate::error::ErrorCode::Internal, "{vars:?}");
            assert!(!err.fix.display().is_empty());
            assert!(err.message.contains("HOME"), "{}", err.message);
        }
    }

    #[test]
    fn from_env_agrees_with_etcetera() {
        use etcetera::BaseStrategy;
        let Ok(xdg) = etcetera::base_strategy::Xdg::new() else {
            eprintln!("skipped: no home directory in this environment");
            return;
        };
        let Ok(paths) = Paths::from_env() else {
            eprintln!("skipped: HOME is unset (etcetera fell back to passwd)");
            return;
        };
        assert_eq!(paths.home(), xdg.home_dir());
        assert_eq!(paths.config_dir(), xdg.config_dir().join(APP));
        assert_eq!(
            Some(paths.state_dir().to_path_buf()),
            xdg.state_dir().map(|d| d.join(APP))
        );
        assert_eq!(paths.cache_dir(), xdg.cache_dir().join(APP));
    }

    #[test]
    fn this_module_never_touches_the_filesystem() {
        let source = include_str!("paths.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        for call in [
            "create_dir",
            "canonicalize",
            ".exists(",
            "fs::",
            "metadata(",
            "read_dir(",
        ] {
            assert!(!code.contains(call), "src/state/paths.rs calls {call}");
        }
    }
}
