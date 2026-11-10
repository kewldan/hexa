//! NFR-08: random edit sequences must match a naive `Vec<u8>` model.
#![no_main]

use hexa_core::{Document, EditMode};
use libfuzzer_sys::fuzz_target;

fn take(data: &mut &[u8]) -> u8 {
    let Some((&b, rest)) = data.split_first() else { return 0 };
    *data = rest;
    b
}

fn take16(data: &mut &[u8]) -> usize {
    usize::from(take(data)) << 8 | usize::from(take(data))
}

fuzz_target!(|input: &[u8]| {
    let mut data = input;
    let mut model: Vec<u8> = (0..take(&mut data)).collect();
    let mut doc = Document::from_bytes("fuzz", model.clone());
    doc.set_mode(EditMode::Insert).unwrap();
    let mut states = std::collections::HashMap::new();
    let current = |d: &Document| d.history().iter().find(|h| h.current).unwrap().id;
    states.insert(current(&doc), model.clone());
    while !data.is_empty() {
        let len = model.len();
        match take(&mut data) % 8 {
            0 | 1 => {
                let at = take16(&mut data) % (len + 1);
                let n = usize::from(take(&mut data) % 16);
                let bytes: Vec<u8> = (0..n).map(|_| take(&mut data)).collect();
                doc.insert(at as u64, &bytes).unwrap();
                model.splice(at..at, bytes);
            }
            2 => {
                let at = take16(&mut data) % (len + 1);
                let n = usize::from(take(&mut data) % 16);
                let bytes: Vec<u8> = (0..n).map(|_| take(&mut data)).collect();
                doc.overwrite(at as u64, &bytes).unwrap();
                let end = (at + bytes.len()).min(len);
                model.splice(at..end, bytes);
            }
            3 => {
                let at = take16(&mut data) % (len + 1);
                let end = (at + usize::from(take(&mut data))).min(len);
                doc.delete(at as u64, end as u64).unwrap();
                model.drain(at..end);
            }
            4 => {
                let at = take16(&mut data) % (len + 1);
                let end = (at + usize::from(take(&mut data))).min(len);
                let to = take16(&mut data) % (len + 1);
                let clip = doc.copy(at as u64, end as u64).unwrap();
                doc.insert_tree(to as u64, &clip, "paste").unwrap();
                let chunk = model[at..end].to_vec();
                model.splice(to..to, chunk);
            }
            5 => {
                let at = take16(&mut data) % (len + 1);
                let b = take(&mut data);
                let amend = take(&mut data) % 2 == 0 && at < len;
                doc.type_byte(at as u64, b, amend).unwrap();
                if amend {
                    model[at] = b;
                } else {
                    model.insert(at, b);
                }
            }
            6 => {
                doc.undo();
                model = states[&current(&doc)].clone();
            }
            _ => {
                doc.redo();
                model = states[&current(&doc)].clone();
            }
        }
        states.insert(current(&doc), model.clone());
        assert_eq!(doc.len(), model.len() as u64);
    }
    assert_eq!(doc.read_vec(0, usize::MAX).unwrap(), model);
});
