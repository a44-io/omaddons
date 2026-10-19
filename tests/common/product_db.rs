//! A `product.db` writer in the Agent's layout, verified on 2026-10-18 (see
//! `src/discover/product_db.rs`): repeated top-level installs (field 1), each with uid
//! (1), product code (2), settings (3: install path 1, region 2, game subfolder 13) and
//! cached state (4 → base state 1: installed 1, version 7). Optionally, what else the
//! Agent writes at the top level: install handshakes (field 2: product 1, uid 2,
//! settings 3), download settings (5), a sequence number (6) and uids (7).

use std::path::{Path, PathBuf};

/// The installs to encode, in order, then the other top-level fields.
#[derive(Debug, Clone, Default)]
pub struct ProductDb {
    entries: Vec<Vec<u8>>,
    others: Vec<Vec<u8>>,
}

impl ProductDb {
    pub fn new() -> Self {
        Self::default()
    }

    /// A WoW install whose uid is its product code, e.g. `("wow", "C:/Program Files
    /// (x86)/World of Warcraft", "_retail_", "12.1.0.69933")`.
    pub fn install(self, code: &str, path: &str, folder: &str, version: &str) -> Self {
        self.install_as(code, code, path, folder, version)
    }

    /// A WoW install whose uid differs from its product code, as Anniversary's
    /// (`wow_classic_anniversary`, `wow_anniversary`).
    pub fn install_as(
        mut self,
        uid: &str,
        code: &str,
        path: &str,
        folder: &str,
        version: &str,
    ) -> Self {
        self.entries
            .push(entry(uid, code, path, Some(folder), version));
        self
    }

    /// Non-WoW noise such as the Agent itself (`"agent"`, `"C:/ProgramData/Battle.net"`).
    pub fn with_entry_for(mut self, code: &str, path: &str) -> Self {
        self.entries
            .push(entry(code, code, path, None, "2.41.1.9824"));
        self
    }

    /// An install in progress: a top-level handshake (field 2), product before uid.
    pub fn handshake(mut self, product: &str, uid: &str, path: &str) -> Self {
        let body = [
            string(1, product),
            string(2, uid),
            message(3, &string(1, path)),
        ]
        .concat();
        self.others.push(message(2, &body));
        self
    }

    /// The Agent's other top-level fields: download settings (5), a sequence number (6)
    /// and the priority uid list (7).
    pub fn agent_fields(mut self, uids: &[&str]) -> Self {
        self.others
            .push(message(5, &[uint(1, 0), uint(2, 1_000_000)].concat()));
        self.others.push(uint(6, 7));
        for uid in uids {
            self.others.push(string(7, uid));
        }
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in &self.entries {
            out.extend(message(1, entry));
        }
        for other in &self.others {
            out.extend(other);
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

/// A synthetic database with every kind of top-level field: five WoW installs (one on
/// `D:`, Anniversary's uid not its code), the Agent's own install, a handshake for a
/// Titan install in progress, and the Agent's other fields. The seed
/// `fuzz/seeds/product_db/multi-product.db`, which the reader's unit test decodes.
pub fn multi_product() -> ProductDb {
    let c = "C:/Program Files (x86)/World of Warcraft";
    ProductDb::new()
        .with_entry_for("agent", "C:/ProgramData/Battle.net/Agent")
        .install("wow", c, "_retail_", "12.1.5.70391")
        .install("wowf", c, "_forever_", "1.60.1.70245")
        .install("wow_classic", c, "_classic_", "5.5.4.63000")
        .install(
            "wow_classic_era",
            "D:\\Games\\World of Warcraft",
            "_classic_era_",
            "1.15.9.65000",
        )
        .install_as(
            "wow_classic_anniversary",
            "wow_anniversary",
            c,
            "_anniversary_",
            "2.5.6.70006",
        )
        .handshake("wow_classic_titan", "wow_classic_titan", c)
        .agent_fields(&["wow", "wow_classic_anniversary", "agent"])
}

fn entry(uid: &str, code: &str, path: &str, folder: Option<&str>, version: &str) -> Vec<u8> {
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
        string(1, uid),
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
