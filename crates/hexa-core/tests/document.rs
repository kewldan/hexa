//! Document behaviour: editing, history, change tracking and saving.

use std::fs;
use std::io::Write;
use std::path::Path;

use hexa_core::document::{ChangeKind, Document, EditMode, Fill, OpenOptions};
use hexa_core::save::{self, SaveOptions, SaveStrategy};
use hexa_core::task::{Cancel, no_progress};
use hexa_core::{Error, Tree};
use proptest::prelude::*;

fn content(doc: &Document) -> Vec<u8> {
    doc.read_vec(0, usize::MAX).unwrap()
}

fn tree_bytes(t: &Tree) -> Vec<u8> {
    t.read_vec(0, usize::MAX).unwrap()
}

fn write_file(dir: &Path, name: &str, data: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, data).unwrap();
    p
}

fn opts(strategy: SaveStrategy) -> SaveOptions {
    SaveOptions {
        strategy,
        ..SaveOptions::default()
    }
}

#[test]
fn typing_coalesces_and_nibbles_amend() {
    let mut doc = Document::from_bytes("t", b"abcdef".to_vec());
    doc.type_byte(1, b'X', false).unwrap();
    doc.type_byte(2, b'Y', false).unwrap();
    doc.type_byte(3, b'Z', false).unwrap();
    assert_eq!(content(&doc), b"aXYZef");
    assert_eq!(doc.history().len(), 2, "one coalesced entry");
    doc.undo();
    assert_eq!(content(&doc), b"abcdef");
    doc.redo();
    assert_eq!(content(&doc), b"aXYZef");

    doc.set_mode(EditMode::Insert).unwrap();
    doc.type_byte(0, 0x10, false).unwrap(); // high nibble inserts
    doc.type_byte(0, 0x1F, true).unwrap(); // low nibble amends
    doc.type_byte(1, 0x20, false).unwrap();
    assert_eq!(content(&doc), b"\x1F\x20aXYZef");
    doc.undo();
    assert_eq!(content(&doc), b"aXYZef");
}

#[test]
fn typing_at_end_extends_unless_fixed() {
    let mut doc = Document::from_bytes("t", b"ab".to_vec());
    doc.type_byte(2, b'c', false).unwrap();
    assert_eq!(content(&doc), b"abc");
    doc.set_fixed_size(true);
    assert!(matches!(
        doc.type_byte(3, b'd', false),
        Err(Error::FixedSize)
    ));
    doc.set_mode(EditMode::ReadOnly).unwrap();
    assert!(matches!(doc.overwrite(0, b"z"), Err(Error::ReadOnly)));
}

#[test]
fn transactions_are_one_entry() {
    let mut doc = Document::from_bytes("t", vec![0; 16]);
    doc.transaction("Replace all", |d| {
        d.overwrite(0, b"\x01")?;
        d.insert(4, b"\x02\x02")?;
        d.delete(10, 12)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(doc.history().len(), 2);
    assert_eq!(doc.len(), 16);
    doc.undo();
    assert_eq!(content(&doc), vec![0; 16]);
}

#[test]
fn copy_paste_shares_pieces() {
    let mut doc = Document::from_bytes("t", (0..=255).collect());
    let clip = doc.copy(10, 20).unwrap();
    doc.insert_tree(0, &clip, "Paste").unwrap();
    doc.overwrite_tree(250, &clip, "Paste over").unwrap();
    let c = content(&doc);
    assert_eq!(&c[..10], &(10..20).collect::<Vec<u8>>()[..]);
    assert_eq!(c.len(), 266);
    assert_eq!(&c[250..260], &(10..20).collect::<Vec<u8>>()[..]);
}

#[test]
fn fills_are_lazy_and_correct() {
    let mut doc = Document::new_sized(1 << 40, &Fill::Byte { value: 0xAA }).unwrap();
    assert_eq!(doc.len(), 1 << 40);
    doc.fill(
        100,
        108,
        &Fill::Pattern {
            bytes: vec![1, 2, 3],
        },
    )
    .unwrap();
    assert_eq!(
        doc.read_vec(99, 10).unwrap(),
        [0xAA, 1, 2, 3, 1, 2, 3, 1, 2, 0xAA]
    );
    doc.fill(
        0,
        8,
        &Fill::Sequence {
            start: 1,
            step: 1,
            width: 2,
            big_endian: true,
        },
    )
    .unwrap();
    assert_eq!(doc.read_vec(0, 8).unwrap(), [0, 1, 0, 2, 0, 3, 0, 4]);
    assert!(doc.piece_count() < 10);
}

#[test]
fn transforms() {
    let mut doc = Document::from_bytes("t", vec![1, 2, 3, 4, 5, 6, 7, 8]);
    doc.byte_swap(0, 8, 4).unwrap();
    assert_eq!(content(&doc), [4, 3, 2, 1, 8, 7, 6, 5]);
    doc.reverse(0, 4).unwrap();
    assert_eq!(content(&doc), [1, 2, 3, 4, 8, 7, 6, 5]);
    doc.move_block(0, 2, 8).unwrap();
    assert_eq!(content(&doc), [3, 4, 8, 7, 6, 5, 1, 2]);
    doc.resize(4, &Fill::Byte { value: 0 }).unwrap();
    doc.resize(6, &Fill::Byte { value: 9 }).unwrap();
    assert_eq!(content(&doc), [3, 4, 8, 7, 9, 9]);
}

#[test]
fn change_spans() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "a.bin", &[0u8; 64]);
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    assert_eq!(doc.changes_in(0, 64), [] as [hexa_core::ChangeSpan; 0]);

    doc.overwrite(4, &[1, 1]).unwrap();
    doc.set_mode(EditMode::Insert).unwrap();
    doc.insert(10, &[2, 2, 2]).unwrap();
    doc.delete(20, 25).unwrap();
    let spans = doc.changes_in(0, doc.len());
    let kinds: Vec<_> = spans.iter().map(|s| (s.start, s.end, s.kind)).collect();
    assert_eq!(
        kinds,
        vec![
            (4, 6, ChangeKind::Modified),
            (10, 13, ChangeKind::Inserted),
            (20, 20, ChangeKind::Deleted),
        ]
    );
    // Windowed query.
    let w = doc.changes_in(11, 12);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].kind, ChangeKind::Inserted);
}

#[test]
fn atomic_save_and_undo_after_save() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "a.bin", b"hello world");
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    doc.set_mode(EditMode::Insert).unwrap();
    doc.insert(5, b",").unwrap();
    let report = doc
        .save(&opts(SaveStrategy::Auto), &no_progress, &Cancel::new())
        .unwrap();
    assert_eq!(report.strategy, SaveStrategy::Atomic);
    assert_eq!(fs::read(&p).unwrap(), b"hello, world");
    assert!(!doc.is_modified());

    // CORE-12: undo still works and reads the pre-save bytes.
    doc.undo();
    assert_eq!(content(&doc), b"hello world");
    assert!(doc.is_modified());
    doc.save(&SaveOptions::default(), &no_progress, &Cancel::new())
        .unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"hello world");
    doc.redo();
    assert_eq!(content(&doc), b"hello, world");
}

#[cfg(unix)]
#[test]
fn atomic_save_keeps_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "x.sh", b"#!/bin/sh\n");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o751)).unwrap();
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    doc.set_mode(EditMode::Insert).unwrap();
    doc.insert(10, b"true\n").unwrap();
    doc.save(&opts(SaveStrategy::Atomic), &no_progress, &Cancel::new())
        .unwrap();
    let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o751);
}

#[test]
fn in_place_save_chain_keeps_every_history_state() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "a.bin", b"0123456789");
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    let s0 = content(&doc);
    doc.overwrite(2, b"AB").unwrap();
    let s1 = content(&doc);
    let r = doc
        .save(&SaveOptions::default(), &no_progress, &Cancel::new())
        .unwrap();
    assert_eq!(r.strategy, SaveStrategy::InPlace);
    assert_eq!(r.bytes_written, 2);
    doc.overwrite(6, b"CD").unwrap();
    doc.overwrite(2, b"X").unwrap();
    let s2 = content(&doc);
    doc.save(&SaveOptions::default(), &no_progress, &Cancel::new())
        .unwrap();
    assert_eq!(fs::read(&p).unwrap(), s2);
    assert!(!save::has_pending_journal(&p));

    doc.undo();
    doc.undo();
    assert_eq!(content(&doc), s1);
    doc.undo();
    assert_eq!(content(&doc), s0);
    doc.redo();
    doc.redo();
    doc.redo();
    assert_eq!(content(&doc), s2);
}

#[test]
fn in_place_save_with_shifts_and_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let p = write_file(dir.path(), "big.bin", &data);
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    doc.set_mode(EditMode::Insert).unwrap();
    // Swap two big blocks (creates a dependency cycle) and shift everything.
    let a = doc.copy(0, 1_200_000).unwrap();
    let b = doc.copy(1_800_000, 3_000_000).unwrap();
    doc.transaction("swap", |d| {
        d.replace_tree(1_800_000, 3_000_000, &a, "a")?;
        d.replace_tree(0, 1_200_000, &b, "b")?;
        d.insert(5, b"shift")?;
        d.delete(2_000_000, 2_000_100)?;
        Ok(())
    })
    .unwrap();
    let expected = content(&doc);
    doc.save(&opts(SaveStrategy::InPlace), &no_progress, &Cancel::new())
        .unwrap();
    assert_eq!(fs::read(&p).unwrap(), expected);
    doc.undo();
    assert_eq!(content(&doc), data);
}

#[test]
fn cancelled_in_place_save_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..4_000_000u32).map(|i| (i % 253) as u8).collect();
    let p = write_file(dir.path(), "c.bin", &data);
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    doc.set_mode(EditMode::Insert).unwrap();
    doc.insert(0, b"x").unwrap();
    let cancel = Cancel::new();
    let c2 = cancel.clone();
    let progress = move |done: u64, _total: u64| {
        if done > 1_000_000 {
            c2.cancel();
        }
    };
    let r = doc.save(&opts(SaveStrategy::InPlace), &progress, &cancel);
    assert!(matches!(r, Err(Error::Cancelled)));
    assert_eq!(fs::read(&p).unwrap(), data);
    assert!(!save::has_pending_journal(&p));
    assert!(doc.is_modified());
}

#[test]
fn journal_rollback_restores_original() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "j.bin", b"abcdefgh");
    // Hand-made journal: two records, file was later truncated to 4 bytes.
    let mut j = b"HXJRNL01".to_vec();
    j.extend_from_slice(&8u64.to_le_bytes());
    for (target, bytes) in [(0u64, b"ab".as_slice()), (4, b"efgh".as_slice())] {
        j.extend_from_slice(&target.to_le_bytes());
        j.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        j.extend_from_slice(bytes);
    }
    j.extend_from_slice(&[1, 2, 3]); // torn record
    fs::write(save::journal_path(&p), j).unwrap();
    fs::write(&p, b"XYcd").unwrap();
    assert!(save::has_pending_journal(&p));
    save::rollback_journal(&p).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"abcdefgh");
    assert!(!save::has_pending_journal(&p));
}

#[test]
fn external_changes_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "e.bin", b"1234");
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    doc.overwrite(0, b"9").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&p, b"12345").unwrap();
    assert!(doc.disk_changed());
    let r = doc.save(&SaveOptions::default(), &no_progress, &Cancel::new());
    assert!(matches!(r, Err(Error::ExternallyModified(_))));
    let forced = SaveOptions {
        force: true,
        ..SaveOptions::default()
    };
    doc.save(&forced, &no_progress, &Cancel::new()).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"9234");
}

#[test]
fn backup_and_save_variants() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "b.bin", b"original");
    let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
    doc.overwrite(0, b"O").unwrap();
    let o = SaveOptions {
        backup: true,
        ..SaveOptions::default()
    };
    doc.save(&o, &no_progress, &Cancel::new()).unwrap();
    assert_eq!(fs::read(save::backup_path(&p)).unwrap(), b"original");

    let copy = dir.path().join("copy.bin");
    doc.save_copy(&copy, &no_progress, &Cancel::new()).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), b"Original");
    let part = dir.path().join("part.bin");
    doc.save_range(1, 4, &part, &no_progress, &Cancel::new())
        .unwrap();
    assert_eq!(fs::read(&part).unwrap(), b"rig");

    let new = dir.path().join("new.bin");
    doc.overwrite(1, b"R").unwrap();
    doc.save_as(&new, &no_progress, &Cancel::new()).unwrap();
    assert_eq!(doc.path(), Some(new.as_path()));
    assert_eq!(doc.name(), "new.bin");
    assert_eq!(fs::read(&new).unwrap(), b"ORiginal");
    assert_eq!(fs::read(&p).unwrap(), b"Original");
}

#[test]
fn new_document_needs_save_as() {
    let mut doc = Document::new_empty();
    doc.type_byte(0, 1, false).unwrap();
    assert!(matches!(
        doc.save(&SaveOptions::default(), &no_progress, &Cancel::new()),
        Err(Error::NoPath)
    ));
}

#[test]
fn follow_growth_appends_tail() {
    let dir = tempfile::tempdir().unwrap();
    let p = write_file(dir.path(), "log.txt", b"line1\n");
    let mut doc = Document::open(
        &p,
        &OpenOptions {
            mode: EditMode::ReadOnly,
            ..OpenOptions::default()
        },
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"line2\n")
        .unwrap();
    assert_eq!(doc.follow_growth().unwrap(), 6);
    assert_eq!(content(&doc), b"line1\nline2\n");
}

fn current_id(doc: &Document) -> usize {
    doc.history().iter().find(|h| h.current).unwrap().id
}

#[derive(Debug, Clone)]
enum Op {
    Overwrite(u32, Vec<u8>),
    Insert(u32, Vec<u8>),
    Delete(u32, u16),
    Move(u32, u32, u32),
    Save(bool),
    Undo,
    Redo,
}

fn op() -> impl Strategy<Value = Op> {
    let data = proptest::collection::vec(any::<u8>(), 1..300);
    prop_oneof![
        3 => (any::<u32>(), data.clone()).prop_map(|(a, d)| Op::Overwrite(a, d)),
        3 => (any::<u32>(), data).prop_map(|(a, d)| Op::Insert(a, d)),
        2 => (any::<u32>(), any::<u16>()).prop_map(|(a, n)| Op::Delete(a, n)),
        1 => (any::<u32>(), any::<u32>(), any::<u32>()).prop_map(|(a, b, c)| Op::Move(a, b, c)),
        2 => any::<bool>().prop_map(Op::Save),
        1 => Just(Op::Undo),
        1 => Just(Op::Redo),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// NFR-08: editing and saving (both strategies, repeatedly) always matches a
    /// plain in-memory model, the file always matches the last saved state, and
    /// every history state stays readable.
    #[test]
    fn edits_and_saves_match_model(init in proptest::collection::vec(any::<u8>(), 0..5000),
                                   ops in proptest::collection::vec(op(), 1..40)) {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(dir.path(), "m.bin", &init);
        let mut doc = Document::open(&p, &OpenOptions::default()).unwrap();
        doc.set_mode(EditMode::Insert).unwrap();
        let mut model = init.clone();
        let mut on_disk = init.clone();
        let mut snapshots: Vec<(Tree, Vec<u8>)> = vec![(doc.snapshot(), model.clone())];
        let mut states = std::collections::HashMap::new();
        states.insert(current_id(&doc), model.clone());
        for op in ops {
            let len = model.len();
            match op {
                Op::Overwrite(a, d) => {
                    let at = a as usize % (len + 1);
                    doc.overwrite(at as u64, &d).unwrap();
                    let end = (at + d.len()).min(len);
                    model.splice(at..end, d);
                }
                Op::Insert(a, d) => {
                    let at = a as usize % (len + 1);
                    doc.insert(at as u64, &d).unwrap();
                    model.splice(at..at, d);
                }
                Op::Delete(a, n) => {
                    let at = a as usize % (len + 1);
                    let end = (at + n as usize).min(len);
                    doc.delete(at as u64, end as u64).unwrap();
                    model.drain(at..end);
                }
                Op::Move(a, b, c) => {
                    if len == 0 { continue; }
                    let s = a as usize % len;
                    let e = s + 1 + b as usize % (len - s);
                    let to = c as usize % (len + 1);
                    doc.move_block(s as u64, e as u64, to as u64).unwrap();
                    if !(to >= s && to <= e) {
                        let block: Vec<u8> = model.drain(s..e).collect();
                        let at = if to > e { to - (e - s) } else { to };
                        model.splice(at..at, block);
                    }
                }
                Op::Save(in_place) => {
                    let strategy = if in_place { SaveStrategy::InPlace } else { SaveStrategy::Atomic };
                    doc.save(&opts(strategy), &no_progress, &Cancel::new()).unwrap();
                    on_disk = model.clone();
                    prop_assert_eq!(fs::read(&p).unwrap(), on_disk.clone());
                }
                Op::Undo => {
                    doc.undo();
                    model = states[&current_id(&doc)].clone();
                }
                Op::Redo => {
                    doc.redo();
                    model = states[&current_id(&doc)].clone();
                }
            }
            states.insert(current_id(&doc), model.clone());
            prop_assert_eq!(content(&doc), model.clone());
            snapshots.push((doc.snapshot(), model.clone()));
        }
        prop_assert_eq!(fs::read(&p).unwrap(), on_disk);
        for (t, m) in snapshots {
            prop_assert_eq!(tree_bytes(&t), m);
        }
    }
}
