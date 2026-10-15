//! A `product.db` writer in WowUp's layout, the one a real Agent database was verified
//! to use: repeated top-level installs (field 1), each with uid (1), product code (2),
//! settings (3: install path 1, region 2, flavor folder 13) and cached state
//! (4 → base state 1: installed 1, version 7). The reader is shape-based, so tests may
//! also rely on field numbers not mattering.

use std::path::{Path, PathBuf};

/// The installs to encode, in order.
#[derive(Debug, Clone, Default)]
pub struct ProductDb {
    entries: Vec<Vec<u8>>,
}

impl ProductDb {
    pub fn new() -> Self {
        Self::default()
    }

    /// A WoW install, e.g. `("wow", "C:/Program Files (x86)/World of Warcraft",
    /// "_retail_", "12.1.0.69933")`.
    pub fn install(mut self, code: &str, path: &str, folder: &str, version: &str) -> Self {
        self.entries.push(entry(code, path, Some(folder), version));
        self
    }

    /// Non-WoW noise such as the Agent itself (`"agent"`, `"C:/ProgramData/Battle.net"`).
    pub fn with_entry_for(mut self, code: &str, path: &str) -> Self {
        self.entries.push(entry(code, path, None, "2.41.1.9824"));
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in &self.entries {
            out.extend(message(1, entry));
        }
        out.extend(uint(6, 4_072_387));
        out
    }

    /// Writes `<prefix>/drive_c/ProgramData/Battle.net/Agent/product.db`.
    pub fn write(&self, prefix: &Path) -> PathBuf {
        let dir = prefix.join("drive_c/ProgramData/Battle.net/Agent");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("product.db");
        std::fs::write(&path, self.bytes()).unwrap();
        path
    }
}

fn entry(code: &str, path: &str, folder: Option<&str>, version: &str) -> Vec<u8> {
    let mut settings = [
        string(1, path),
        string(2, "us"),
        uint(3, 2),
        string(6, "enUS"),
    ]
    .concat();
    if let Some(folder) = folder {
        settings.extend(string(13, folder));
    }
    let base = [uint(1, 1), uint(2, 1), string(7, version)].concat();
    [
        string(1, code),
        string(2, code),
        message(3, &settings),
        message(4, &message(1, &base)),
    ]
    .concat()
}

fn varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let low = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

fn message(number: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint(u64::from(number) << 3 | 2, &mut out);
    varint(body.len() as u64, &mut out);
    out.extend_from_slice(body);
    out
}

fn string(number: u32, s: &str) -> Vec<u8> {
    message(number, s.as_bytes())
}

fn uint(number: u32, n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint(u64::from(number) << 3, &mut out);
    varint(n, &mut out);
    out
}
