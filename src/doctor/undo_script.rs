//! A fix run's `undo.sh` (M4-D32): a POSIX script that runs `doctor undo` for that run
//! under the `HOME` and XDG directories the run used, with the binary that made it, as the
//! widget stamps its context. Written once, never printed: it holds the user's paths.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::runtime::DoctorRunId;
use crate::model::TxId;
use crate::omarchy::render::Context;

/// The script's name in the run directory.
pub const UNDO_SCRIPT: &str = "undo.sh";

/// The script's bytes: the five variables in `ctx.as_env()`'s order, then the `exec`.
pub fn undo_script(binary: &Path, ctx: &Context, run: &DoctorRunId, tx: TxId) -> Vec<u8> {
    let run = run.as_str();
    let mut out = format!(
        "#!/bin/sh\n# omaddons: undoes doctor run {run} (repair tx {tx}). Generated once by omaddons doctor fix.\n# It runs `omaddons doctor undo` for this run under the HOME and XDG directories the run used;\n# extra arguments are passed on (for example --dry-run).\n"
    )
    .into_bytes();
    for (name, value) in ctx.as_env() {
        out.extend_from_slice(name.as_bytes());
        out.push(b'=');
        out.extend(quoted(value));
        out.extend_from_slice(format!("; export {name}\n").as_bytes());
    }
    out.extend_from_slice(b"exec ");
    out.extend(quoted(binary));
    out.extend_from_slice(b" doctor undo ");
    out.extend(quoted(Path::new(run)));
    out.extend_from_slice(b" \"$@\"\n");
    out
}

/// `path`'s bytes in single quotes, each `'` written `'\''`: any byte is quoted exactly,
/// UTF-8 or not.
fn quoted(path: &Path) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &byte in path.as_os_str().as_bytes() {
        if byte == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(byte);
        }
    }
    out.push(b'\'');
    out
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    const RUN: &str = "2026-10-30T18-03-40Z__01M8S7XQKZ0000000000000ABC";
    const TX: &str = "01JB8Q2VA00000000000000009";

    fn context(home: &Path) -> Context {
        Context {
            home: home.to_path_buf(),
            xdg_config_home: home.join(".config"),
            xdg_state_home: home.join(".local/state"),
            xdg_cache_home: home.join(".cache"),
            xdg_data_home: home.join(".local/share"),
        }
    }

    fn script(home: &Path) -> Vec<u8> {
        undo_script(
            Path::new("/usr/local/bin/omaddons"),
            &context(home),
            &DoctorRunId::parse(RUN).unwrap(),
            TX.parse().unwrap(),
        )
    }

    #[test]
    fn undo_script_snapshot() {
        let text = String::from_utf8(script(Path::new("/home/user"))).unwrap();
        insta::assert_snapshot!("undo_script", text);
    }

    /// A home whose name holds a space, a `'`, a newline and a byte that is not UTF-8.
    fn awkward(base: &Path) -> PathBuf {
        let name = OsStr::from_bytes(b"a b'c\nd\xff");
        base.join(name)
    }

    #[test]
    fn undo_script_quotes_every_byte() {
        let dir = tempfile::tempdir().unwrap();
        let home = awkward(dir.path());
        let ctx = context(&home);
        let generated = script(&home);
        // Everything before the `exec` line (the comments and the five assignments), so
        // the `exec` is never run.
        let exec = generated.windows(6).position(|w| w == b"\nexec ").unwrap();
        let mut probe = generated[..=exec].to_vec();
        probe.extend_from_slice(
            b"printf '%s\\0' \"$HOME\" \"$XDG_CONFIG_HOME\" \"$XDG_STATE_HOME\" \"$XDG_CACHE_HOME\" \"$XDG_DATA_HOME\"\n",
        );
        let path = dir.path().join("probe.sh");
        std::fs::write(&path, &probe).unwrap();
        let out = Command::new("/bin/sh")
            .arg(&path)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let mut want: Vec<u8> = Vec::new();
        for (_, value) in ctx.as_env() {
            want.extend_from_slice(value.as_os_str().as_bytes());
            want.push(0);
        }
        assert_eq!(out.stdout, want);
    }

    #[test]
    fn undo_script_passes_sh_n() {
        let dir = tempfile::tempdir().unwrap();
        for home in [PathBuf::from("/home/user"), awkward(dir.path())] {
            let path = dir.path().join("undo.sh");
            std::fs::write(&path, script(&home)).unwrap();
            let out = Command::new("/bin/sh")
                .arg("-n")
                .arg(&path)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}: {out:?}", home.display());
        }
    }
}
