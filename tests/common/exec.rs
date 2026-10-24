//! Scripts a test writes and then runs (stub tools, credential commands). A file this
//! multi-threaded test process opens for writing is inherited by a sibling thread's
//! `fork` until that child `exec`s, and running the file meanwhile fails with `ETXTBSY`
//! ("Text file busy"). So a child process writes the script, and no descriptor of this
//! one ever holds it.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `contents` to `path` as an executable (mode 0755), through `sh`.
pub fn write_executable(path: &Path, contents: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "writing {}: {status}", path.display());
}
