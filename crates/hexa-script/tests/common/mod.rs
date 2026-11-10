//! In-memory [`ScriptHost`] for tests: one or more documents backed by `Vec<u8>`.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use hexa_script::{
    ByteRange, DocId, DocumentInfo, FindQuery, HostResult, LogLevel, QueryKind, ScriptHost,
};

#[derive(Debug, Default, Clone)]
pub struct Doc {
    pub name: String,
    pub data: Vec<u8>,
    pub selection: Option<ByteRange>,
    pub cursor: u64,
    pub undo: Vec<Vec<u8>>,
    pub redo: Vec<Vec<u8>>,
    pub saved: bool,
}

#[derive(Debug, Default)]
pub struct State {
    pub docs: Vec<Doc>,
    pub active: Option<DocId>,
    pub commands: Vec<(String, serde_json::Value)>,
    pub logs: Vec<(LogLevel, String)>,
    pub printed: Vec<String>,
    pub prompts: VecDeque<Option<String>>,
    pub messages: Vec<String>,
}

#[derive(Debug, Default)]
pub struct MemoryHost {
    pub state: Mutex<State>,
}

impl MemoryHost {
    pub fn with_data(data: &[u8]) -> Arc<Self> {
        let host = Self::default();
        host.add_doc("test.bin", data);
        Arc::new(host)
    }

    pub fn add_doc(&self, name: &str, data: &[u8]) -> DocId {
        let mut st = self.lock();
        st.docs.push(Doc {
            name: name.to_owned(),
            data: data.to_vec(),
            ..Doc::default()
        });
        let id = st.docs.len() as DocId;
        st.active.get_or_insert(id);
        id
    }

    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn data(&self, doc: DocId) -> Vec<u8> {
        self.lock().docs[(doc - 1) as usize].data.clone()
    }

    fn with_doc<T>(&self, doc: DocId, f: impl FnOnce(&mut Doc) -> HostResult<T>) -> HostResult<T> {
        let mut st = self.lock();
        let d = st
            .docs
            .get_mut((doc as usize).wrapping_sub(1))
            .ok_or_else(|| format!("no document {doc}"))?;
        f(d)
    }

    fn edit(&self, doc: DocId, f: impl FnOnce(&mut Vec<u8>) -> HostResult<()>) -> HostResult<()> {
        self.with_doc(doc, |d| {
            let before = d.data.clone();
            f(&mut d.data)?;
            d.undo.push(before);
            d.redo.clear();
            Ok(())
        })
    }
}

fn check_range(len: usize, offset: u64, count: u64) -> HostResult<(usize, usize)> {
    let start = offset as usize;
    let end = start.checked_add(count as usize).ok_or("overflow")?;
    if end > len {
        return Err(format!("range {offset}+{count} beyond end {len}"));
    }
    Ok((start, end))
}

fn matches_at(data: &[u8], pos: usize, needle: &[u8], ignore_case: bool) -> bool {
    data.get(pos..pos + needle.len()).is_some_and(|w| {
        if ignore_case {
            w.eq_ignore_ascii_case(needle)
        } else {
            w == needle
        }
    })
}

impl ScriptHost for MemoryHost {
    fn documents(&self) -> Vec<DocumentInfo> {
        self.lock()
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| DocumentInfo {
                id: i as DocId + 1,
                name: d.name.clone(),
                path: None,
                len: d.data.len() as u64,
                read_only: false,
                modified: !d.undo.is_empty() && !d.saved,
            })
            .collect()
    }

    fn active_document(&self) -> Option<DocId> {
        self.lock().active
    }

    fn open_file(&self, path: &str, _read_only: bool) -> HostResult<DocId> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        Ok(self.add_doc(path, &data))
    }

    fn len(&self, doc: DocId) -> HostResult<u64> {
        self.with_doc(doc, |d| Ok(d.data.len() as u64))
    }

    fn read(&self, doc: DocId, offset: u64, len: u64) -> HostResult<Vec<u8>> {
        self.with_doc(doc, |d| {
            let start = (offset as usize).min(d.data.len());
            let end = start.saturating_add(len as usize).min(d.data.len());
            Ok(d.data[start..end].to_vec())
        })
    }

    fn write(&self, doc: DocId, offset: u64, data: &[u8]) -> HostResult<()> {
        self.edit(doc, |buf| {
            let start = offset as usize;
            if start > buf.len() {
                return Err("write beyond end".into());
            }
            let end = start + data.len();
            if end > buf.len() {
                buf.resize(end, 0);
            }
            buf[start..end].copy_from_slice(data);
            Ok(())
        })
    }

    fn insert(&self, doc: DocId, offset: u64, data: &[u8]) -> HostResult<()> {
        self.edit(doc, |buf| {
            let at = offset as usize;
            if at > buf.len() {
                return Err("insert beyond end".into());
            }
            buf.splice(at..at, data.iter().copied());
            Ok(())
        })
    }

    fn delete(&self, doc: DocId, offset: u64, len: u64) -> HostResult<()> {
        self.edit(doc, |buf| {
            let (s, e) = check_range(buf.len(), offset, len)?;
            buf.drain(s..e);
            Ok(())
        })
    }

    fn replace(&self, doc: DocId, offset: u64, len: u64, data: &[u8]) -> HostResult<()> {
        self.edit(doc, |buf| {
            let (s, e) = check_range(buf.len(), offset, len)?;
            buf.splice(s..e, data.iter().copied());
            Ok(())
        })
    }

    fn selection(&self, doc: DocId) -> HostResult<Option<ByteRange>> {
        self.with_doc(doc, |d| Ok(d.selection))
    }

    fn set_selection(&self, doc: DocId, selection: Option<ByteRange>) -> HostResult<()> {
        self.with_doc(doc, |d| {
            d.selection = selection;
            Ok(())
        })
    }

    fn cursor(&self, doc: DocId) -> HostResult<u64> {
        self.with_doc(doc, |d| Ok(d.cursor))
    }

    fn set_cursor(&self, doc: DocId, offset: u64) -> HostResult<()> {
        self.with_doc(doc, |d| {
            d.cursor = offset;
            Ok(())
        })
    }

    fn goto(&self, doc: DocId, offset: u64) -> HostResult<()> {
        self.with_doc(doc, |d| {
            if offset > d.data.len() as u64 {
                return Err(format!("offset {offset} beyond end"));
            }
            d.cursor = offset;
            Ok(())
        })
    }

    fn find_next(&self, doc: DocId, query: &FindQuery, from: u64) -> HostResult<Option<ByteRange>> {
        let needle = query.to_bytes()?;
        let ic = query.ignore_case && query.kind == QueryKind::Text;
        self.with_doc(doc, |d| {
            Ok((from as usize..d.data.len())
                .find(|&p| matches_at(&d.data, p, &needle, ic))
                .map(|p| ByteRange::new(p as u64, needle.len() as u64)))
        })
    }

    fn find_all(&self, doc: DocId, query: &FindQuery, limit: usize) -> HostResult<Vec<ByteRange>> {
        let needle = query.to_bytes()?;
        let ic = query.ignore_case && query.kind == QueryKind::Text;
        self.with_doc(doc, |d| {
            Ok((0..d.data.len())
                .filter(|&p| matches_at(&d.data, p, &needle, ic))
                .take(limit)
                .map(|p| ByteRange::new(p as u64, needle.len() as u64))
                .collect())
        })
    }

    fn hash(&self, doc: DocId, range: ByteRange, algorithm: &str) -> HostResult<String> {
        self.with_doc(doc, |d| {
            let (s, e) = check_range(d.data.len(), range.offset, range.len)?;
            let bytes = &d.data[s..e];
            match algorithm {
                "sha256" => Ok(hexa_script::sha256_hex(bytes)),
                "sum8" => Ok(format!(
                    "{:02x}",
                    bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))
                )),
                other => Err(format!("unknown hash algorithm {other:?}")),
            }
        })
    }

    fn inspect(&self, doc: DocId, offset: u64, value_type: &str) -> HostResult<String> {
        self.with_doc(doc, |d| {
            let at = |n: usize| -> HostResult<&[u8]> {
                let (s, e) = check_range(d.data.len(), offset, n as u64)?;
                Ok(&d.data[s..e])
            };
            match value_type {
                "u8" => Ok(at(1)?[0].to_string()),
                "u16le" => Ok(u16::from_le_bytes(at(2)?.try_into().unwrap()).to_string()),
                "u32le" => Ok(u32::from_le_bytes(at(4)?.try_into().unwrap()).to_string()),
                "u32be" => Ok(u32::from_be_bytes(at(4)?.try_into().unwrap()).to_string()),
                other => Err(format!("unknown type {other:?}")),
            }
        })
    }

    fn apply_template(&self, doc: DocId, name: &str, offset: u64) -> HostResult<String> {
        if name != "header" {
            return Err(format!("no template {name:?}"));
        }
        let magic = self.read(doc, offset, 4)?;
        Ok(serde_json::json!({
            "name": "header",
            "offset": offset,
            "fields": [{"name": "magic", "value": hex::encode(magic)}],
            "children": [],
        })
        .to_string())
    }

    fn transform(&self, doc: DocId, recipe_json: &str, range: ByteRange) -> HostResult<Vec<u8>> {
        let recipe: serde_json::Value =
            serde_json::from_str(recipe_json).map_err(|e| e.to_string())?;
        let input = self.read(doc, range.offset, range.len)?;
        match recipe["op"].as_str() {
            Some("xor") => {
                let key = recipe["key"].as_u64().ok_or("missing key")? as u8;
                Ok(input.iter().map(|b| b ^ key).collect())
            }
            Some("reverse") => Ok(input.into_iter().rev().collect()),
            other => Err(format!("unknown op {other:?}")),
        }
    }

    fn undo(&self, doc: DocId) -> HostResult<bool> {
        self.with_doc(doc, |d| match d.undo.pop() {
            Some(prev) => {
                let cur = std::mem::replace(&mut d.data, prev);
                d.redo.push(cur);
                Ok(true)
            }
            None => Ok(false),
        })
    }

    fn redo(&self, doc: DocId) -> HostResult<bool> {
        self.with_doc(doc, |d| match d.redo.pop() {
            Some(next) => {
                let cur = std::mem::replace(&mut d.data, next);
                d.undo.push(cur);
                Ok(true)
            }
            None => Ok(false),
        })
    }

    fn save(&self, doc: DocId) -> HostResult<()> {
        self.with_doc(doc, |d| {
            d.saved = true;
            Ok(())
        })
    }

    fn run_command(&self, command: &str, args: &serde_json::Value) -> HostResult<()> {
        if command == "fail" {
            return Err("command failed".into());
        }
        self.lock()
            .commands
            .push((command.to_owned(), args.clone()));
        Ok(())
    }

    fn log(&self, level: LogLevel, message: &str) {
        self.lock().logs.push((level, message.to_owned()));
    }

    fn print(&self, text: &str) {
        self.lock().printed.push(text.to_owned());
    }

    fn prompt(&self, _title: &str, message: &str, default: &str) -> Option<String> {
        let mut st = self.lock();
        st.messages.push(format!("prompt:{message}"));
        st.prompts
            .pop_front()
            .unwrap_or_else(|| Some(default.to_owned()))
    }

    fn confirm(&self, _title: &str, message: &str) -> bool {
        self.lock().messages.push(format!("confirm:{message}"));
        true
    }

    fn message(&self, title: &str, message: &str) {
        self.lock().messages.push(format!("{title}:{message}"));
    }
}
