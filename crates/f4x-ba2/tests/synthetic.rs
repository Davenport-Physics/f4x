//! Synthetic archives built in code; nothing here reads game files.

use std::io::Write;

use f4x_ba2::*;

const HEADER_LEN: usize = 24;
const GNRL_RECORD_LEN: usize = 36;
const DX10_RECORD_LEN: usize = 24;
const DX10_CHUNK_LEN: usize = 24;

struct GnrlFile<'a> {
    name: &'a str,
    data: &'a [u8],
    compress: bool,
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn build_gnrl(version: u32, files: &[GnrlFile<'_>], with_names: bool) -> Vec<u8> {
    let blobs: Vec<Vec<u8>> =
        files.iter().map(|f| if f.compress { zlib(f.data) } else { f.data.to_vec() }).collect();
    let mut data_offset = (HEADER_LEN + files.len() * GNRL_RECORD_LEN) as u64;
    let name_table_offset = data_offset + blobs.iter().map(|b| b.len() as u64).sum::<u64>();

    let mut out = Vec::new();
    out.extend_from_slice(b"BTDX");
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(b"GNRL");
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    out.extend_from_slice(&(if with_names { name_table_offset } else { 0 }).to_le_bytes());
    for (i, (file, blob)) in files.iter().zip(&blobs).enumerate() {
        out.extend_from_slice(&(0x1000 + i as u32).to_le_bytes());
        let ext = file.name.rsplit('.').next().unwrap().as_bytes();
        let mut ext4 = [0u8; 4];
        ext4[..ext.len()].copy_from_slice(ext);
        out.extend_from_slice(&ext4);
        out.extend_from_slice(&0xD1D1_0000u32.to_le_bytes());
        out.extend_from_slice(&0x0010_0100u32.to_le_bytes());
        out.extend_from_slice(&data_offset.to_le_bytes());
        let packed = if file.compress { blob.len() as u32 } else { 0 };
        out.extend_from_slice(&packed.to_le_bytes());
        out.extend_from_slice(&(file.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&RECORD_SENTINEL.to_le_bytes());
        data_offset += blob.len() as u64;
    }
    for blob in &blobs {
        out.extend_from_slice(blob);
    }
    if with_names {
        for file in files {
            out.extend_from_slice(&(file.name.len() as u16).to_le_bytes());
            out.extend_from_slice(file.name.as_bytes());
        }
    }
    out
}

fn sample_files() -> Vec<GnrlFile<'static>> {
    vec![
        GnrlFile { name: "scripts\\Form.pex", data: b"stored bytes", compress: false },
        GnrlFile { name: "Scripts\\Sub\\Actor.pex", data: &[7u8; 5000], compress: true },
        GnrlFile { name: "misc\\empty.txt", data: b"", compress: false },
    ]
}

#[test]
fn gnrl_lists_and_extracts() {
    for version in SUPPORTED_VERSIONS {
        let archive = Archive::from_bytes(build_gnrl(version, &sample_files(), true)).unwrap();
        assert_eq!(archive.header().version, version);
        assert_eq!(archive.header().kind, ArchiveKind::General);
        assert_eq!(archive.entries().len(), 3);

        let names: Vec<&str> = archive.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["scripts\\Form.pex", "Scripts\\Sub\\Actor.pex", "misc\\empty.txt"]);

        let form = &archive.entries()[0];
        assert!(!form.is_compressed());
        assert_eq!(form.extension_str(), "pex");
        assert_eq!(archive.extract(form).unwrap(), b"stored bytes");

        let actor = archive.find("scripts/sub/ACTOR.PEX").unwrap();
        assert!(actor.is_compressed());
        assert_eq!(actor.size(), 5000);
        assert!(actor.stored_size() < 5000);
        assert_eq!(archive.extract(actor).unwrap(), vec![7u8; 5000]);

        assert_eq!(archive.extract(&archive.entries()[2]).unwrap(), b"");
    }
}

#[test]
fn gnrl_from_disk_matches_memory() {
    let bytes = build_gnrl(1, &sample_files(), true);
    // Cargo's per-package scratch dir under target/, never the game dir or /tmp.
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("f4x-ba2-synthetic.ba2");
    std::fs::write(&path, &bytes).unwrap();
    let on_disk = Archive::open(&path).unwrap();
    let in_memory = Archive::from_bytes(bytes).unwrap();
    assert_eq!(on_disk.entries(), in_memory.entries());
    for entry in on_disk.entries() {
        assert_eq!(on_disk.extract(entry).unwrap(), in_memory.extract(entry).unwrap());
    }
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn missing_name_table_leaves_names_empty() {
    let archive = Archive::from_bytes(build_gnrl(1, &sample_files(), false)).unwrap();
    assert!(archive.entries().iter().all(|e| e.name.is_empty()));
    assert_eq!(archive.extract(&archive.entries()[1]).unwrap(), vec![7u8; 5000]);
}

#[test]
fn rejects_bad_headers() {
    let good = build_gnrl(1, &sample_files(), true);

    let mut bad = good.clone();
    bad[0] = b'X';
    assert!(matches!(Archive::from_bytes(bad), Err(Error::BadMagic(_))));

    let mut starfield = good.clone();
    starfield[4] = 2;
    assert!(matches!(Archive::from_bytes(starfield), Err(Error::UnsupportedVersion(2))));

    let mut console = good.clone();
    console[8..12].copy_from_slice(b"GNMF");
    assert!(matches!(Archive::from_bytes(console), Err(Error::UnsupportedKind(_))));

    assert!(matches!(Archive::from_bytes(good[..40].to_vec()), Err(Error::Truncated { .. })));

    // Absurd counts fail cleanly instead of allocating.
    let mut huge = good.clone();
    huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(Archive::from_bytes(huge.clone()), Err(Error::Truncated { .. })));
    huge[8..12].copy_from_slice(b"DX10");
    assert!(Archive::from_bytes(huge).is_err());
}

#[test]
fn corrupt_zlib_and_size_mismatch_are_errors() {
    let mut bytes = build_gnrl(1, &sample_files(), true);
    // Record 1 (compressed): bump unpacked_size so decompression no longer matches.
    let size_at = HEADER_LEN + GNRL_RECORD_LEN + 28;
    bytes[size_at..size_at + 4].copy_from_slice(&5001u32.to_le_bytes());
    let archive = Archive::from_bytes(bytes.clone()).unwrap();
    assert!(matches!(
        archive.extract(&archive.entries()[1]),
        Err(Error::SizeMismatch { expected: 5001, actual: 5000, .. })
    ));

    // Corrupt the zlib header byte of the compressed blob.
    let EntryData::General(record) = archive.entries()[1].data else { unreachable!() };
    bytes[record.offset as usize] = 0xFF;
    let archive = Archive::from_bytes(bytes).unwrap();
    assert!(matches!(archive.extract(&archive.entries()[1]), Err(Error::Decompress { .. })));
}

#[test]
fn dx10_lists_chunks() {
    let mut out = Vec::new();
    let chunk_data_at = (HEADER_LEN + DX10_RECORD_LEN + 2 * DX10_CHUNK_LEN) as u64;
    let name = b"textures\\a.dds";
    out.extend_from_slice(b"BTDX");
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(b"DX10");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(chunk_data_at + 300).to_le_bytes());
    // Record: hashes, unknown, chunk count, chunk record size, height, width, mips, format, flags, tile.
    out.extend_from_slice(&0xAAAA_AAAAu32.to_le_bytes());
    out.extend_from_slice(b"dds\0");
    out.extend_from_slice(&0xBBBB_BBBBu32.to_le_bytes());
    out.extend_from_slice(&[0, 2]);
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&512u16.to_le_bytes());
    out.extend_from_slice(&256u16.to_le_bytes());
    out.extend_from_slice(&[10, 71, 1, 8]);
    for (offset, packed, unpacked, start, end) in
        [(chunk_data_at, 100u32, 200u32, 0u16, 0u16), (chunk_data_at + 100, 0, 200, 1, 9)]
    {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&packed.to_le_bytes());
        out.extend_from_slice(&unpacked.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&end.to_le_bytes());
        out.extend_from_slice(&RECORD_SENTINEL.to_le_bytes());
    }
    out.resize(out.len() + 300, 0);
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(name);

    let archive = Archive::from_bytes(out).unwrap();
    assert_eq!(archive.header().kind, ArchiveKind::Texture);
    let entry = &archive.entries()[0];
    assert_eq!(entry.name, "textures\\a.dds");
    let EntryData::Texture(tex) = &entry.data else { panic!("expected texture record") };
    assert_eq!(
        (tex.width, tex.height, tex.mip_count, tex.dxgi_format, tex.flags, tex.tile_mode),
        (256, 512, 10, 71, 1, 8)
    );
    assert_eq!(tex.chunks.len(), 2);
    assert_eq!((tex.chunks[1].start_mip, tex.chunks[1].end_mip), (1, 9));
    assert_eq!(entry.size(), 400);
    assert_eq!(entry.stored_size(), 300);
    assert!(entry.is_compressed());
    assert!(matches!(archive.extract(entry), Err(Error::TextureExtraction(_))));
}
