//! A minimal PE32 image with a `VS_VERSIONINFO` resource, enough for `pelite` to read
//! a legacy `Wow.exe` version.
//!
//! Layout (file offsets): DOS header with `e_lfanew` 0x40; `PE\0\0`; the file header
//! (i386, one section); the PE32 optional header (file alignment 0x200, section
//! alignment 0x1000, 16 data directories, the resource directory set); one `.rsrc`
//! section at file offset 0x200 / RVA 0x1000 holding `RT_VERSION → 1 → 0x0409 →` a data
//! entry for the `VS_VERSIONINFO` block.

const FILE_ALIGN: usize = 0x200;
const SECTION_RVA: u32 = 0x1000;
/// Offset of the optional header: `e_lfanew` (0x40) + signature (4) + file header (20).
const OPTIONAL: usize = 0x58;
const OPTIONAL_SIZE: usize = 224;

/// A PE whose file and product versions are both `major.minor.patch.build`.
pub fn pe_stub(major: u16, minor: u16, patch: u16, build: u16) -> Vec<u8> {
    let v = [major, minor, patch, build];
    image(v, v)
}

/// A PE whose file version is zero and whose product version is set, to exercise the
/// product-version fallback.
pub fn pe_stub_product_only(major: u16, minor: u16, patch: u16, build: u16) -> Vec<u8> {
    image([0; 4], [major, minor, patch, build])
}

/// A PE with distinct file and product versions, like a real 3.3.5a `Wow.exe`
/// (file 3.3.5.12340, product 3.3.0.0).
pub fn pe_stub_with(file: [u16; 4], product: [u16; 4]) -> Vec<u8> {
    image(file, product)
}

/// The `(file, product)` versions `pelite` reads from a PE, each as
/// `[major, minor, patch, build]`; panics when it cannot.
pub fn pe_versions(bytes: &[u8]) -> ([u16; 4], [u16; 4]) {
    use pelite::pe32::{Pe, PeFile};
    let file = PeFile::from_bytes(bytes).unwrap();
    let info = file.resources().unwrap().version_info().unwrap();
    let fixed = info.fixed().unwrap();
    let parts = |v: pelite::image::VS_VERSION| [v.Major, v.Minor, v.Patch, v.Build];
    (parts(fixed.dwFileVersion), parts(fixed.dwProductVersion))
}

fn image(file: [u16; 4], product: [u16; 4]) -> Vec<u8> {
    let rsrc = resources(&version_info(file, product));
    let raw_size = rsrc.len().div_ceil(FILE_ALIGN) * FILE_ALIGN;
    let virtual_size = rsrc.len().div_ceil(0x1000) * 0x1000;
    let mut img = vec![0u8; FILE_ALIGN + raw_size];

    img[..2].copy_from_slice(b"MZ");
    put32(&mut img, 0x3c, 0x40);
    img[0x40..0x44].copy_from_slice(b"PE\0\0");
    let file_header = 0x44;
    put16(&mut img, file_header, 0x014c); // i386
    put16(&mut img, file_header + 2, 1); // one section
    put16(&mut img, file_header + 16, OPTIONAL_SIZE as u16);
    put16(&mut img, file_header + 18, 0x0102); // executable, 32-bit

    let o = OPTIONAL;
    put16(&mut img, o, 0x10b); // PE32
    put32(&mut img, o + 8, raw_size as u32); // SizeOfInitializedData
    put32(&mut img, o + 28, 0x0040_0000); // ImageBase
    put32(&mut img, o + 32, 0x1000); // SectionAlignment
    put32(&mut img, o + 36, FILE_ALIGN as u32); // FileAlignment
    put16(&mut img, o + 48, 4); // MajorSubsystemVersion
    put32(&mut img, o + 56, SECTION_RVA + virtual_size as u32); // SizeOfImage
    put32(&mut img, o + 60, FILE_ALIGN as u32); // SizeOfHeaders
    put16(&mut img, o + 68, 2); // Windows GUI
    put32(&mut img, o + 72, 0x0010_0000); // stack reserve
    put32(&mut img, o + 76, 0x1000); // stack commit
    put32(&mut img, o + 80, 0x0010_0000); // heap reserve
    put32(&mut img, o + 84, 0x1000); // heap commit
    put32(&mut img, o + 92, 16); // NumberOfRvaAndSizes
    let resource_dir = o + 96 + 2 * 8;
    put32(&mut img, resource_dir, SECTION_RVA);
    put32(&mut img, resource_dir + 4, rsrc.len() as u32);

    let s = o + OPTIONAL_SIZE;
    img[s..s + 8].copy_from_slice(b".rsrc\0\0\0");
    put32(&mut img, s + 8, rsrc.len() as u32); // VirtualSize
    put32(&mut img, s + 12, SECTION_RVA);
    put32(&mut img, s + 16, raw_size as u32);
    put32(&mut img, s + 20, FILE_ALIGN as u32); // PointerToRawData
    put32(&mut img, s + 36, 0x4000_0040); // initialized data, readable

    img[FILE_ALIGN..FILE_ALIGN + rsrc.len()].copy_from_slice(&rsrc);
    img
}

/// The resource tree: three one-entry directories and a data entry, then the data.
fn resources(data: &[u8]) -> Vec<u8> {
    const DATA_AT: usize = 0x58;
    let mut r = vec![0u8; DATA_AT];
    let directory = |r: &mut Vec<u8>, at: usize, id: u32, target: u32| {
        put16(r, at + 14, 1); // NumberOfIdEntries
        put32(r, at + 16, id);
        put32(r, at + 20, target);
    };
    directory(&mut r, 0x00, 16, 0x8000_0018); // RT_VERSION → subdirectory
    directory(&mut r, 0x18, 1, 0x8000_0030); // ID 1 → subdirectory
    directory(&mut r, 0x30, 0x0409, 0x48); // en-US → data entry
    put32(&mut r, 0x48, SECTION_RVA + DATA_AT as u32); // OffsetToData is an RVA
    put32(&mut r, 0x4c, data.len() as u32);
    r.extend_from_slice(data);
    r
}

/// `VS_VERSIONINFO` with a `VS_FIXEDFILEINFO` and no children.
fn version_info(file: [u16; 4], product: [u16; 4]) -> Vec<u8> {
    let ms = |v: [u16; 4]| u32::from(v[0]) << 16 | u32::from(v[1]);
    let ls = |v: [u16; 4]| u32::from(v[2]) << 16 | u32::from(v[3]);
    let mut b = Vec::new();
    b.extend_from_slice(&0u16.to_le_bytes()); // wLength, set below
    b.extend_from_slice(&52u16.to_le_bytes()); // wValueLength
    b.extend_from_slice(&0u16.to_le_bytes()); // wType: binary
    for unit in "VS_VERSION_INFO\0".encode_utf16() {
        b.extend_from_slice(&unit.to_le_bytes());
    }
    while b.len() % 4 != 0 {
        b.push(0);
    }
    for word in [
        0xfeef_04bd, // dwSignature
        0x0001_0000, // dwStrucVersion
        ms(file),
        ls(file),
        ms(product),
        ls(product),
        0x3f,        // dwFileFlagsMask
        0,           // dwFileFlags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ] {
        b.extend_from_slice(&u32::to_le_bytes(word));
    }
    let len = b.len() as u16;
    b[..2].copy_from_slice(&len.to_le_bytes());
    b
}

fn put16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
