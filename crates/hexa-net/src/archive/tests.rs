//! FILE-15 tests over archives built on the fly.

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;
use tar::{EntryType, Header};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::*;

const LONG: &str = "deeply/nested/directory/with/a/name/that/is/definitely/longer/than/one/hundred/bytes/payload.bin";

fn all(p: &dyn Provider) -> Vec<u8> {
    let mut buf = vec![0; p.len() as usize];
    p.read_at(0, &mut buf).unwrap();
    buf
}

fn make_zip(path: &Path) {
    let mut w = ZipWriter::new(File::create(path).unwrap());
    w.set_comment("archive comment").unwrap();
    w.add_directory("dir/", SimpleFileOptions::default())
        .unwrap();
    w.start_file(
        "dir/stored.txt",
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
    )
    .unwrap();
    w.write_all(b"stored content").unwrap();
    w.start_file(
        "deflated.bin",
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            // Unix permissions only mean something for a Unix entry; on
            // Windows the writer would otherwise mark it as DOS.
            .system(zip::System::Unix)
            .unix_permissions(0o640),
    )
    .unwrap();
    w.write_all(&vec![7u8; 10_000]).unwrap();
    w.start_file(
        "bz.bin",
        SimpleFileOptions::default().compression_method(CompressionMethod::Bzip2),
    )
    .unwrap();
    w.write_all(b"bzip2 bytes").unwrap();
    w.finish().unwrap();
}

#[test]
fn zip_list_read_open_and_repack() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.zip");
    make_zip(&path);
    let a = Archive::open(&path).unwrap();
    assert_eq!(a.kind(), ArchiveKind::Zip);
    let names: Vec<_> = a.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["dir/", "dir/stored.txt", "deflated.bin", "bz.bin"]);
    assert!(a.entries()[0].is_dir);
    let stored = a.entry("dir/stored.txt").unwrap();
    assert!(stored.data_offset.is_some());
    assert_eq!(a.entry("deflated.bin").unwrap().data_offset, None);
    assert_eq!(a.read("deflated.bin").unwrap(), vec![7u8; 10_000]);
    assert!(matches!(a.read("nope"), Err(Error::EntryNotFound(_))));
    assert!(a.read("dir/").is_err());

    let p = a.open_entry("dir/stored.txt").unwrap();
    assert_eq!(all(&*p), b"stored content");
    assert_eq!(p.name(), "a.zip!/dir/stored.txt");
    assert!(!p.capabilities().writable);
    let p = a.open_entry("deflated.bin").unwrap();
    assert_eq!(p.len(), 10_000);

    let mut small = a.clone();
    small.set_max_entry_size(100);
    assert!(matches!(
        small.read("deflated.bin"),
        Err(Error::TooLarge { .. })
    ));

    // Replace the deflated entry; the others must be copied raw.
    let out = dir.path().join("b.zip");
    a.repack("deflated.bin", b"new data", &out).unwrap();
    let b = Archive::open(&out).unwrap();
    assert_eq!(b.read("deflated.bin").unwrap(), b"new data");
    assert_eq!(b.read("dir/stored.txt").unwrap(), b"stored content");
    assert_eq!(b.read("bz.bin").unwrap(), b"bzip2 bytes");
    assert_eq!(b.entry("bz.bin").unwrap().method, "Bzip2", "raw copied");
    let mut za = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
    assert_eq!(za.comment(), b"archive comment");
    assert_eq!(
        za.by_name("deflated.bin")
            .unwrap()
            .unix_mode()
            .map(|m| m & 0o777),
        Some(0o640)
    );

    // In place, stored stays stored.
    a.repack("dir/stored.txt", b"replaced!", &path).unwrap();
    let c = Archive::open(&path).unwrap();
    assert_eq!(c.entry("dir/stored.txt").unwrap().method, "Stored");
    assert_eq!(all(&*c.open_entry("dir/stored.txt").unwrap()), b"replaced!");
    assert_eq!(c.read("deflated.bin").unwrap(), vec![7u8; 10_000]);
    let leftovers = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".hexa-repack")
        })
        .count();
    assert_eq!(leftovers, 0, "temporary files are renamed away");
}

fn pax_header(records: &[(&str, &str)]) -> (Header, Vec<u8>) {
    let mut body = Vec::new();
    for (k, v) in records {
        let rec = format!(" {k}={v}\n");
        let mut len = rec.len() + 1;
        while len.to_string().len() + rec.len() != len {
            len = len.to_string().len() + rec.len();
        }
        body.extend_from_slice(format!("{len}{rec}").as_bytes());
    }
    let mut h = Header::new_ustar();
    h.set_path("PaxHeaders/x").unwrap();
    h.set_entry_type(EntryType::XHeader);
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    (h, body)
}

fn tar_bytes() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = Header::new_gnu();
    h.set_entry_type(EntryType::Directory);
    h.set_mode(0o755);
    h.set_size(0);
    b.append_data(&mut h, "top/", &[][..]).unwrap();

    let mut h = Header::new_gnu();
    h.set_mode(0o600);
    h.set_mtime(1_700_000_000);
    h.set_size(5);
    b.append_data(&mut h, "top/small.txt", &b"small"[..])
        .unwrap();

    // GNU long name.
    let mut h = Header::new_gnu();
    h.set_mode(0o644);
    h.set_size(3);
    b.append_data(&mut h, LONG, &b"abc"[..]).unwrap();

    // PAX path + size records.
    let (ph, body) = pax_header(&[("path", "pax/named.dat"), ("size", "4")]);
    b.append(&ph, body.as_slice()).unwrap();
    let mut h = Header::new_ustar();
    h.set_path("short").unwrap();
    h.set_mode(0o644);
    h.set_size(4);
    h.set_cksum();
    b.append(&h, &b"wxyz"[..]).unwrap();

    b.into_inner().unwrap()
}

#[test]
fn tar_list_read_open_and_repack() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.tar");
    std::fs::write(&path, tar_bytes()).unwrap();
    let a = Archive::open(&path).unwrap();
    assert_eq!(a.kind(), ArchiveKind::Tar);
    let names: Vec<_> = a.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["top/", "top/small.txt", LONG, "pax/named.dat"]);
    assert!(a.entries()[0].is_dir);
    assert_eq!(
        a.entry("top/small.txt").unwrap().modified,
        Some(1_700_000_000)
    );
    assert_eq!(a.read(LONG).unwrap(), b"abc");
    assert_eq!(a.read("pax/named.dat").unwrap(), b"wxyz");
    let p = a.open_entry("top/small.txt").unwrap();
    assert!(a.entry("top/small.txt").unwrap().data_offset.is_some());
    assert_eq!(all(&*p), b"small");

    let out = dir.path().join("b.tar");
    a.repack(LONG, b"a much longer replacement payload", &out)
        .unwrap();
    let b = Archive::open(&out).unwrap();
    assert_eq!(b.entries().len(), 4);
    assert_eq!(b.read(LONG).unwrap(), b"a much longer replacement payload");
    assert_eq!(b.read("top/small.txt").unwrap(), b"small");
    assert_eq!(b.read("pax/named.dat").unwrap(), b"wxyz");

    // The PAX size record follows the new length.
    b.repack("pax/named.dat", b"0123456789", &out).unwrap();
    let c = Archive::open(&out).unwrap();
    assert_eq!(c.entry("pax/named.dat").unwrap().size, 10);
    assert_eq!(c.read("pax/named.dat").unwrap(), b"0123456789");
    // The standard `tar` reader agrees.
    let mut ar = tar::Archive::new(File::open(&out).unwrap());
    let mut seen = Vec::new();
    for e in ar.entries().unwrap() {
        let mut e = e.unwrap();
        let name = e.path().unwrap().to_string_lossy().into_owned();
        let mut body = Vec::new();
        e.read_to_end(&mut body).unwrap();
        seen.push((name, body));
    }
    assert_eq!(
        seen[2],
        (
            LONG.to_owned(),
            b"a much longer replacement payload".to_vec()
        )
    );
    assert_eq!(
        seen[3],
        ("pax/named.dat".to_owned(), b"0123456789".to_vec())
    );
}

#[test]
fn tar_gz_and_gz() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.tgz");
    let mut enc = flate2::GzBuilder::new()
        .filename("a.tar")
        .mtime(1234)
        .write(File::create(&path).unwrap(), Compression::default());
    enc.write_all(&tar_bytes()).unwrap();
    enc.finish().unwrap();
    let a = Archive::open(&path).unwrap();
    assert_eq!(a.kind(), ArchiveKind::TarGz);
    assert_eq!(a.entries().len(), 4);
    assert_eq!(a.entry("top/small.txt").unwrap().data_offset, None);
    assert_eq!(all(&*a.open_entry("top/small.txt").unwrap()), b"small");
    a.repack("top/small.txt", b"SMALLER?", &path).unwrap();
    let b = Archive::open(&path).unwrap();
    assert_eq!(b.read("top/small.txt").unwrap(), b"SMALLER?");
    assert_eq!(b.read(LONG).unwrap(), b"abc");
    assert_eq!(gzfmt::meta(&path).unwrap().mtime, 1234, "gzip header kept");

    let gz = dir.path().join("log.txt.gz");
    let mut enc = flate2::GzBuilder::new()
        .filename("server.log")
        .mtime(99)
        .write(File::create(&gz).unwrap(), Compression::default());
    enc.write_all(b"line one\nline two\n").unwrap();
    enc.finish().unwrap();
    let g = Archive::open(&gz).unwrap();
    assert_eq!(g.kind(), ArchiveKind::Gzip);
    let e = &g.entries()[0];
    assert_eq!(
        (e.name.as_str(), e.size, e.modified),
        ("server.log", 18, Some(99))
    );
    assert_eq!(g.read("server.log").unwrap(), b"line one\nline two\n");
    g.repack("server.log", b"rewritten", &gz).unwrap();
    let g = Archive::open(&gz).unwrap();
    assert_eq!(g.entries()[0].name, "server.log");
    assert_eq!(all(&*g.open_entry("server.log").unwrap()), b"rewritten");

    // Without a stored name the file name minus `.gz` is used.
    let bare = dir.path().join("blob.bin.gz");
    let mut enc = GzEncoder::new(File::create(&bare).unwrap(), Compression::fast());
    enc.write_all(b"x").unwrap();
    enc.finish().unwrap();
    assert_eq!(Archive::open(&bare).unwrap().entries()[0].name, "blob.bin");
}

#[test]
fn seven_zip_list_read_and_repack() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a.7z");
    {
        let mut w = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
        w.push_archive_entry::<&[u8]>(sevenz_rust2::ArchiveEntry::new_directory("d"), None)
            .unwrap();
        w.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file("d/one.txt"),
            Some(&b"first file"[..]),
        )
        .unwrap();
        w.push_archive_entry(
            sevenz_rust2::ArchiveEntry::new_file("two.bin"),
            Some(&vec![42u8; 5000][..]),
        )
        .unwrap();
        w.push_archive_entry(sevenz_rust2::ArchiveEntry::new_file("empty"), None::<&[u8]>)
            .unwrap();
        w.finish().unwrap();
    }
    let a = Archive::open(&path).unwrap();
    assert_eq!(a.kind(), ArchiveKind::SevenZip);
    let names: Vec<_> = a.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["d", "d/one.txt", "two.bin", "empty"]);
    assert_eq!(a.read("two.bin").unwrap(), vec![42u8; 5000]);
    assert_eq!(all(&*a.open_entry("d/one.txt").unwrap()), b"first file");

    a.repack("d/one.txt", b"changed", &path).unwrap();
    let b = Archive::open(&path).unwrap();
    assert_eq!(b.read("d/one.txt").unwrap(), b"changed");
    assert_eq!(b.read("two.bin").unwrap(), vec![42u8; 5000]);
    assert!(b.entry("d").unwrap().is_dir);
    assert_eq!(b.read("empty").unwrap(), b"");
}

#[test]
fn detection() {
    let dir = TempDir::new().unwrap();
    let plain = dir.path().join("plain.bin");
    std::fs::write(&plain, b"just bytes").unwrap();
    assert_eq!(ArchiveKind::detect(&plain).unwrap(), None);
    assert!(matches!(Archive::open(&plain), Err(Error::Unsupported(_))));
    let tar = dir.path().join("noext");
    std::fs::write(&tar, tar_bytes()).unwrap();
    assert_eq!(ArchiveKind::detect(&tar).unwrap(), Some(ArchiveKind::Tar));
}
