//! A directory made a mount point of its own: bind-mounted over itself in an
//! unprivileged user and mount namespace (`unshare -r -m`). It keeps its filesystem's
//! device number, so only its mount id tells it apart. Where the kernel refuses
//! unprivileged user namespaces none can be made, and callers skip, saying so.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use super::home::{Home, Run};

/// Whether this machine lets an unprivileged process bind-mount `dir` over itself in a
/// user and mount namespace.
pub fn can_bind_mount(dir: &Path) -> bool {
    Command::new("unshare")
        .args(["-r", "-m", "mount", "--bind"])
        .arg(dir)
        .arg(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A command that bind-mounts `dir` over itself in a new user and mount namespace, then
/// runs `program` there; the caller adds its arguments and environment.
pub fn under_bind(dir: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new("unshare");
    cmd.args([
        "-r",
        "-m",
        "sh",
        "-c",
        "mount --bind \"$0\" \"$0\" && exec \"$@\"",
    ])
    .arg(dir)
    .arg(program);
    cmd
}

/// `omaddons <args>` verbatim, with this home's environment, while `dir` is a mount point
/// of its own; `None` where unprivileged bind mounts are refused.
pub fn bound(home: &Home, dir: &Path, args: &[String]) -> Option<Run> {
    if !can_bind_mount(dir) {
        return None;
    }
    let inner = home.std_command(args, &[]);
    let mut cmd = under_bind(dir, inner.get_program());
    cmd.args(inner.get_args());
    for (key, value) in inner.get_envs() {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    let output = cmd.output().unwrap();
    Some(Run {
        argv: args.to_vec(),
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    })
}
