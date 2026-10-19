//! `omaddons config [get <k> | set <k> <v> | path]`: read and edit
//! `<config>/omaddons/config.toml`. `config set` edits only the named value (comments
//! and order survive), validates the whole result, and writes under the global lock;
//! `--dry-run` writes nothing. `config path` never loads the file, so it works even
//! when the file is broken. Credentials are shown as their references, never resolved.

use std::io::Write;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Serialize;

use crate::cli::{Context, fix_argv};
use crate::error::{Error, ErrorCode, Result, Suggestion, Warning};
use crate::fsx::{
    Content, GlobalLock, Op, ScopeTable, WriteSession, Writer, ensure_dir_all, mutate,
};
use crate::limits::EffectiveLimits;
use crate::output::{Outcome, Style};
use crate::politeness::DEFAULT_CHECK_INTERVAL;
use crate::state::config::{Config, KEYS, closest_key, lookup, set_value, spec_for};
use crate::state::paths::Paths;

/// `config`'s actions.
#[derive(Debug, Clone, clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: Option<ConfigAction>,
}

/// What `config` does besides listing.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ConfigAction {
    /// Show one setting
    Get {
        /// A dotted key, e.g. limits.snapshot_max_bytes
        key: String,
    },
    /// Change one setting, keeping the file's comments and order
    Set {
        /// A dotted key, e.g. sources.github.enabled
        key: String,
        /// The value: a string, number or boolean, or a TOML array or inline table
        value: String,
    },
    /// Show the config file's path
    Path,
}

impl ConfigAction {
    /// The envelope `command`.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Get { .. } => "config get",
            Self::Set { .. } => "config set",
            Self::Path => "config path",
        }
    }
}

/// One setting.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Setting {
    pub key: String,
    pub value: serde_json::Value,
    /// `file` or `default`.
    pub source: &'static str,
}

/// Bare `config`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ConfigData {
    pub path: PathBuf,
    pub exists: bool,
    /// Sorted by key; patterned keys appear only when set.
    pub settings: Vec<Setting>,
}

/// `config get`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct GetData {
    pub key: String,
    pub value: serde_json::Value,
    pub source: &'static str,
    pub default: serde_json::Value,
}

/// `config set`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SetData {
    pub key: String,
    pub value: serde_json::Value,
    pub previous: serde_json::Value,
    pub changed: bool,
    pub dry_run: bool,
}

/// `config path`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PathData {
    pub path: PathBuf,
    pub exists: bool,
}

/// A `config` outcome.
pub struct Done<D> {
    command: &'static str,
    data: D,
    warnings: Vec<Warning>,
    render: fn(&D, &mut dyn Write) -> std::io::Result<()>,
}

impl<D: Serialize + JsonSchema> Outcome for Done<D> {
    type Data = D;

    fn command(&self) -> &'static str {
        self.command
    }

    fn data(&self) -> &D {
        &self.data
    }

    fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    fn render_human(&self, w: &mut dyn Write, _: &Style) -> std::io::Result<()> {
        (self.render)(&self.data, w)
    }
}

/// The documented default of a key; `null` when it depends on something else.
fn default_value(key: &str) -> serde_json::Value {
    let limits = EffectiveLimits::default();
    match key {
        "extra_roots" | "git_hosts" => serde_json::json!([]),
        "default_channel" => serde_json::json!("stable"),
        "running_policy" => serde_json::json!("safe"),
        "auto_backup" => serde_json::json!("off"),
        "check_interval_hours" => serde_json::json!(DEFAULT_CHECK_INTERVAL.as_secs() / 3600),
        "limits.archive_max_entries" => serde_json::json!(limits.archive_max_entries),
        "limits.archive_max_uncompressed" => serde_json::json!(limits.archive_max_uncompressed),
        "limits.archive_max_ratio" => serde_json::json!(limits.archive_max_ratio),
        "limits.download_max_bytes" => serde_json::json!(limits.download_max_bytes),
        "limits.tx_retention_count" => serde_json::json!(limits.tx_retention.keep_last),
        "limits.tx_retention_days" => {
            serde_json::json!(limits.tx_retention.window.as_secs() / 86_400)
        }
        "limits.snapshot_max_bytes" => serde_json::json!(limits.snapshot_max_bytes),
        "limits.blob_cache_max_bytes" => serde_json::json!(limits.blob_cache_max_bytes),
        _ => serde_json::Value::Null,
    }
}

fn json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// The file's raw table and the loaded config (with its warnings).
fn read(paths: &Paths) -> Result<(toml::Table, Config, Vec<Warning>, bool)> {
    let loaded = Config::load(paths)?;
    let raw = loaded
        .text
        .as_deref()
        .and_then(|t| toml::from_str(t).ok())
        .unwrap_or_default();
    Ok((raw, loaded.config, loaded.warnings, loaded.exists))
}

/// Bare `config`.
pub fn list() -> Result<Done<ConfigData>> {
    let paths = Paths::from_env()?;
    let (raw, _, warnings, exists) = read(&paths)?;
    let mut settings: Vec<Setting> = KEYS
        .iter()
        .filter(|spec| !spec.key.contains('*'))
        .map(|spec| match lookup(&raw, spec.key) {
            Some(v) => Setting {
                key: spec.key.to_owned(),
                value: json(v),
                source: "file",
            },
            None => Setting {
                key: spec.key.to_owned(),
                value: default_value(spec.key),
                source: "default",
            },
        })
        .collect();
    for (section, value) in &raw {
        let Some(table) = value
            .as_table()
            .filter(|_| matches!(section.as_str(), "sources" | "targets" | "client_rules"))
        else {
            continue;
        };
        for (name, inner) in table {
            for (field, v) in inner.as_table().into_iter().flatten() {
                let key = format!("{section}.{name}.{field}");
                if spec_for(&key).is_some() {
                    settings.push(Setting {
                        key,
                        value: json(v),
                        source: "file",
                    });
                }
            }
        }
    }
    settings.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(Done {
        command: "config",
        data: ConfigData {
            path: paths.config_file(),
            exists,
            settings,
        },
        warnings,
        render: |data, w| {
            writeln!(
                w,
                "# {}{}",
                data.path.display(),
                if data.exists {
                    ""
                } else {
                    " (not created yet)"
                }
            )?;
            for s in &data.settings {
                let note = match s.source {
                    "default" => "  # default",
                    _ => "",
                };
                writeln!(w, "{} = {}{note}", s.key, s.value)?;
            }
            Ok(())
        },
    })
}

/// `usage` for an unknown key: the same `config <action>` with the closest key, `rest`
/// (a value) after it, else the listing of every key.
fn unknown_key(key: &str, action: &str, rest: &[&str]) -> Error {
    let fix = closest_key(key).map_or_else(
        || Suggestion::command(["omaddons", "config"]),
        |c| {
            let mut argv = vec!["omaddons".to_owned(), "config".into(), action.into(), c];
            argv.extend(rest.iter().map(|a| (*a).to_owned()));
            Suggestion::command(argv)
        },
    );
    Error::new(ErrorCode::Usage, format!("{key} is not a config key"), fix)
}

/// `config get <key>`.
pub fn get(key: &str) -> Result<Done<GetData>> {
    if spec_for(key).is_none() {
        return Err(unknown_key(key, "get", &[]));
    }
    let paths = Paths::from_env()?;
    let (raw, _, warnings, _) = read(&paths)?;
    let default = default_value(key);
    let (value, source) =
        lookup(&raw, key).map_or_else(|| (default.clone(), "default"), |v| (json(v), "file"));
    Ok(Done {
        command: "config get",
        data: GetData {
            key: key.to_owned(),
            value,
            source,
            default,
        },
        warnings,
        render: |data, w| writeln!(w, "{} = {}", data.key, data.value),
    })
}

/// `config set <key> <value>`.
pub fn set(ctx: &Context, key: &str, raw_value: &str) -> Result<Done<SetData>> {
    if spec_for(key).is_none() {
        return Err(unknown_key(key, "set", &[raw_value]));
    }
    let paths = Paths::from_env()?;
    let guard = if ctx.dry_run {
        None
    } else {
        Some(
            GlobalLock::acquire(&paths, "config set").map_err(|mut err| {
                if err.code == ErrorCode::Locked {
                    err.fix = Suggestion::command(fix_argv(&ctx.argv));
                }
                err
            })?,
        )
    };
    let path = paths.config_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // ubs:ignore — an io::ErrorKind, not a secret
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(Error::io(
                &path,
                &err,
                Suggestion::text(format!("check {}", path.display())),
            ));
        }
    };
    // What the file says about itself (an unknown key) stands whether or not the value
    // is accepted; a file that does not parse is set_value's error.
    if let Ok((_, warnings)) = Config::parse(&text, &path) {
        ctx.notes.warn_all(&warnings);
    }
    let before: toml::Table = toml::from_str(&text).unwrap_or_default();
    let updated = set_value(&text, &path, key, raw_value)?;
    let after: toml::Table = toml::from_str(&updated).unwrap_or_default();
    let previous = lookup(&before, key).map_or(serde_json::Value::Null, json);
    let value = lookup(&after, key).map_or(serde_json::Value::Null, json);
    let changed = updated != text;
    if changed && !ctx.dry_run {
        let scopes = ScopeTable::new(&paths, &[], None)?;
        let mut s = WriteSession::untracked(Writer::Config, &scopes, "config.toml");
        ensure_dir_all(&mut s, paths.config_dir())?;
        let op = Op::WriteFile {
            content: Content::Bytes(updated.as_bytes()),
            mode: Some(0o644),
        };
        mutate(&mut s, &path, op)?;
    }
    drop(guard);
    Ok(Done {
        command: "config set",
        data: SetData {
            key: key.to_owned(),
            value,
            previous,
            changed,
            dry_run: ctx.dry_run,
        },
        warnings: Vec::new(),
        render: |data, w| {
            let verb = match (data.dry_run, data.changed) {
                (_, false) => "unchanged:",
                (true, true) => "would set",
                (false, true) => "set",
            };
            writeln!(w, "{verb} {} = {}", data.key, data.value)
        },
    })
}

/// `config path`: never loads the file.
pub fn path() -> Result<Done<PathData>> {
    let paths = Paths::from_env()?;
    let path = paths.config_file();
    Ok(Done {
        command: "config path",
        data: PathData {
            exists: path.is_file(),
            path,
        },
        warnings: Vec::new(),
        render: |data, w| writeln!(w, "{}", data.path.display()),
    })
}
