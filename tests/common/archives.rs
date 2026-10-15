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
