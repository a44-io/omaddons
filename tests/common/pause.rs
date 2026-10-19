//! `pause` failpoints from the test's side: a child armed with `<name>=pause` stops at
//! that point, says so with `<dir>/<name>.reached`, and waits for `<dir>/<name>.resume`.
//! The test waits until the child [`Pause::reached`] a step, acts, then
//! [`Pause::resume`]s it, so the interleaving a test names is the one that runs, however
//! loaded the machine is.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// One child's marker directory: two children can pause at the same name.
pub struct Pause {
    dir: PathBuf,
    /// `dir` as a string, for the child's environment.
    var: String,
}

impl Pause {
    /// A fresh marker directory under `root`.
    pub fn new(root: &Path) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = root.join(format!("pause-{}", NEXT.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&dir).unwrap();
        let var = dir.display().to_string();
        Self { dir, var }
    }

    /// The variable that points the child's `pause` failpoints here.
    pub fn env(&self) -> (&'static str, &str) {
        ("OMADDONS_FAILPOINT_DIR", &self.var)
    }

    /// Waits (30 s at most) until the child stops at `name`.
    pub fn reached(&self, name: &str) {
        let marker = self.dir.join(format!("{name}.reached"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "the child never reached {name}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Lets the child go on from `name`; later hits of `name` pass straight through.
    pub fn resume(&self, name: &str) {
        fs::write(self.dir.join(format!("{name}.resume")), "").unwrap();
    }
}
