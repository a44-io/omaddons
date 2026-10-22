//! Zip builders for tests: well-formed archives through the `zip` crate, and raw zip
//! records for hostile shapes the crate will not write (symlink and device modes,
//! headers that lie about sizes, names with `..`, drive letters or control bytes).

use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// A deflated zip of `(name, contents)`; names ending in `/` are directories.
pub fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, contents) in entries {
        if name.ends_with('/') {
            zip.add_directory(*name, options).unwrap();
        } else {
            zip.start_file(*name, options).unwrap();
            zip.write_all(contents).unwrap();
        }
    }
    zip.finish().unwrap().into_inner()
}

/// [`zip_of`] with text contents.
pub fn zip_text(entries: &[(&str, &str)]) -> Vec<u8> {
    let entries: Vec<(&str, &[u8])> = entries.iter().map(|(n, c)| (*n, c.as_bytes())).collect();
    zip_of(&entries)
}

/// One raw entry: the bytes as stored, and every header field a test may want to lie in.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: Vec<u8>,
    /// The payload as stored (deflated when `method` is 8).
    pub payload: Vec<u8>,
    pub method: u16,
    pub crc: u32,
    /// The uncompressed size the headers declare.
    pub declared_size: u32,
    /// Unix mode in the high 16 bits of the external attributes, made by Unix.
    pub unix_mode: Option<u32>,
}

impl RawEntry {
    /// A stored regular file.
    pub fn stored(name: &str, contents: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            payload: contents.to_vec(),
            method: 0,
            crc: crc32(contents),
            declared_size: u32::try_from(contents.len()).unwrap(),
            unix_mode: Some(0o100_644),
        }
    }

    /// A deflated regular file.
    pub fn deflated(name: &str, contents: &[u8]) -> Self {
        let (payload, crc) = deflate(contents);
        Self {
            name: name.as_bytes().to_vec(),
            payload,
            method: 8,
            crc,
            declared_size: u32::try_from(contents.len()).unwrap(),
            unix_mode: Some(0o100_644),
        }
    }

    pub fn mode(mut self, mode: u32) -> Self {
        self.unix_mode = Some(mode);
        self
    }

    pub fn declared_size(mut self, size: u32) -> Self {
        self.declared_size = size;
        self
    }

    pub fn raw_name(mut self, name: &[u8]) -> Self {
        self.name = name.to_vec();
        self
    }
}

/// A zip of raw entries: local headers, then the central directory, then its end record.
pub fn raw_zip(entries: &[RawEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let offset = u32::try_from(out.len()).unwrap();
        let compressed = u32::try_from(e.payload.len()).unwrap();
        let name_len = u16::try_from(e.name.len()).unwrap();
        // Local file header.
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(20u16.to_le_bytes()); // version needed
        out.extend(0u16.to_le_bytes()); // flags
        out.extend(e.method.to_le_bytes());
        out.extend(0u32.to_le_bytes()); // time, date
        out.extend(e.crc.to_le_bytes());
        out.extend(compressed.to_le_bytes());
        out.extend(e.declared_size.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend(0u16.to_le_bytes()); // extra length
        out.extend(&e.name);
        out.extend(&e.payload);
        // Central directory header.
        let made_by: u16 = if e.unix_mode.is_some() {
            (3 << 8) | 20
        } else {
            20
        };
        let external = e.unix_mode.map_or(0, |m| m << 16);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(made_by.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(0u16.to_le_bytes());
        central.extend(e.method.to_le_bytes());
        central.extend(0u32.to_le_bytes());
        central.extend(e.crc.to_le_bytes());
        central.extend(compressed.to_le_bytes());
        central.extend(e.declared_size.to_le_bytes());
        central.extend(name_len.to_le_bytes());
        central.extend(0u16.to_le_bytes()); // extra
        central.extend(0u16.to_le_bytes()); // comment
        central.extend(0u16.to_le_bytes()); // disk
        central.extend(0u16.to_le_bytes()); // internal attributes
        central.extend(external.to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(&e.name);
    }
    let count = u16::try_from(entries.len()).unwrap();
    let central_offset = u32::try_from(out.len()).unwrap();
    let central_len = u32::try_from(central.len()).unwrap();
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(central_len.to_le_bytes());
    out.extend(central_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// `contents` deflated by the zip crate, and its CRC-32.
fn deflate(contents: &[u8]) -> (Vec<u8>, u32) {
    let zip = zip_of(&[("x", contents)]);
    let mut archive = ZipArchive::new(Cursor::new(zip)).unwrap();
    let mut entry = archive.by_index_raw(0).unwrap();
    let crc = entry.crc32();
    let mut payload = Vec::new();
    entry.read_to_end(&mut payload).unwrap();
    (payload, crc)
}

/// CRC-32 (IEEE), via a stored zip entry.
fn crc32(contents: &[u8]) -> u32 {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("x", options).unwrap();
    zip.write_all(contents).unwrap();
    let bytes = zip.finish().unwrap().into_inner();
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    archive.by_index_raw(0).unwrap().crc32()
}

// ---- tar.zst: `backup export`'s archives, and hostile ones -----------------------------

/// One tar entry as raw header fields, so a test can write what the `tar` crate would
/// refuse to: hard links, devices, sparse entries, traversal, duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarEntry {
    pub name: Vec<u8>,
    /// The header's type flag (`b'0'` file, `b'5'` directory, `b'2'` symlink, …).
    pub kind: u8,
    pub mode: u32,
    pub body: Vec<u8>,
    pub link: Vec<u8>,
}

impl TarEntry {
    pub fn file(name: &str, body: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            kind: b'0',
            mode: 0o644,
            body: body.to_vec(),
            link: Vec::new(),
        }
    }

    pub fn dir(name: &str) -> Self {
        Self {
            name: format!("{}/", name.trim_end_matches('/')).into_bytes(),
            kind: b'5',
            mode: 0o755,
            body: Vec::new(),
            link: Vec::new(),
        }
    }

    pub fn symlink(name: &str, target: &str) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            kind: b'2',
            mode: 0o777,
            body: Vec::new(),
            link: target.as_bytes().to_vec(),
        }
    }

    /// Any type flag, with no body (a hard link `b'1'`, a device `b'3'`, …).
    pub fn typed(name: &str, kind: u8, link: &str) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            kind,
            mode: 0o644,
            body: Vec::new(),
            link: link.as_bytes().to_vec(),
        }
    }

    /// The name as text.
    pub fn path(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

/// The entries of a `.tar.zst`, raw (GNU long names folded into the entry they name).
pub fn tar_entries(zst: &[u8]) -> Vec<TarEntry> {
    let decoder = zstd::stream::read::Decoder::new(zst).unwrap();
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let mut body = Vec::new();
            e.read_to_end(&mut body).unwrap();
            let h = e.header();
            TarEntry {
                name: e.path_bytes().into_owned(),
                kind: h.entry_type().as_byte(),
                mode: h.mode().unwrap(),
                body,
                link: e
                    .link_name_bytes()
                    .map(|l| l.into_owned())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// `entries` as a `.tar.zst`: GNU headers, a GNU long-name record before each name of
/// 100 bytes or more, zstd level 3.
pub fn tar_zst(entries: &[TarEntry]) -> Vec<u8> {
    zstd::stream::encode_all(&tar_of(entries)[..], 3).unwrap()
}

/// `entries` as an uncompressed tar.
pub fn tar_of(entries: &[TarEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        if e.name.len() >= 100 {
            let mut name = e.name.clone();
            name.push(0);
            push_entry(&mut out, b"././@LongLink", b'L', 0o644, &name, b"");
        }
        let short = &e.name[..e.name.len().min(99)];
        push_entry(&mut out, short, e.kind, e.mode, &e.body, &e.link);
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

/// A PAX extended header (`x`) with `records` (key, value), before the next entry.
pub fn pax(records: &[(&str, &[u8])]) -> TarEntry {
    let mut body = Vec::new();
    for (key, value) in records {
        let rest = 1 + key.len() + 1 + value.len() + 1;
        let mut len = rest + 1;
        while len.to_string().len() + rest != len {
            len = len.to_string().len() + rest;
        }
        body.extend_from_slice(format!("{len} {key}=").as_bytes());
        body.extend_from_slice(value);
        body.push(b'\n');
    }
    TarEntry {
        name: b"././@PaxHeader".to_vec(),
        kind: b'x',
        mode: 0o644,
        body,
        link: Vec::new(),
    }
}

/// One header block (GNU magic, octal fields, its checksum) and the body, padded.
fn push_entry(out: &mut Vec<u8>, name: &[u8], kind: u8, mode: u32, body: &[u8], link: &[u8]) {
    let mut h = [0u8; 512];
    let octal = |field: &mut [u8], value: u64| {
        let width = field.len() - 1;
        let digits = format!("{value:0width$o}");
        field[..width].copy_from_slice(&digits.as_bytes()[digits.len() - width..]);
    };
    h[..name.len()].copy_from_slice(name);
    octal(&mut h[100..108], u64::from(mode));
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], body.len() as u64);
    octal(&mut h[136..148], 1_700_000_000);
    h[156] = kind;
    let link = &link[..link.len().min(100)];
    h[157..157 + link.len()].copy_from_slice(link);
    h[257..265].copy_from_slice(b"ustar  \0");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    let check = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(check.as_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(body);
    let pad = (512 - body.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// A zstd frame whose header declares a 2 GiB window: no decoder honoring a smaller
/// bound may allocate it.
pub fn zstd_huge_window() -> Vec<u8> {
    // Magic, a frame header descriptor with no content size, a window descriptor of
    // exponent 21 (2^31 bytes), then one empty last raw block.
    vec![0x28, 0xB5, 0x2F, 0xFD, 0x00, 21 << 3, 0x01, 0x00, 0x00]
}
