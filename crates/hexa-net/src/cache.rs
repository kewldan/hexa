//! Paged LRU read cache shared by the remote providers (CORE-06 applied to
//! CORE-02 / FILE-16 sources).
//!
//! Remote reads are expensive (one round trip each), so missing pages that are
//! contiguous are fetched with a single request and then split into pages.

use std::io;
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Default page size of remote caches: larger than local pages because every
/// miss costs a network round trip.
pub const DEFAULT_REMOTE_PAGE_SIZE: usize = 256 * 1024;
/// Default memory budget of a remote cache.
pub const DEFAULT_REMOTE_CACHE_BUDGET: usize = 64 * 1024 * 1024;

/// Page cache geometry of a remote provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CacheOptions {
    /// Bytes per page (clamped to 4 KiB … 8 MiB).
    pub page_size: usize,
    /// Memory budget; at least four pages are always kept.
    pub budget: usize,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_REMOTE_PAGE_SIZE,
            budget: DEFAULT_REMOTE_CACHE_BUDGET,
        }
    }
}

/// LRU cache of fixed-size pages of a remote object.
#[derive(Debug)]
pub(crate) struct PageCache {
    page_size: usize,
    pages: Mutex<LruCache<u64, Arc<[u8]>>>,
}

impl PageCache {
    pub(crate) fn new(opts: CacheOptions) -> Self {
        let page_size = opts.page_size.clamp(4096, 8 << 20);
        let count = (opts.budget / page_size).max(4);
        Self {
            page_size,
            pages: Mutex::new(LruCache::new(NonZeroUsize::new(count).expect("count >= 4"))),
        }
    }

    pub(crate) fn page_size(&self) -> usize {
        self.page_size
    }

    /// Reads `buf.len()` bytes at `offset` of an object of `total` bytes.
    /// `fetch(offset, buf)` must fill `buf` exactly from the backing store.
    pub(crate) fn read(
        &self,
        total: u64,
        offset: u64,
        buf: &mut [u8],
        fetch: &dyn Fn(u64, &mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        check_range(offset, buf.len(), total)?;
        if buf.is_empty() {
            return Ok(());
        }
        let ps = self.page_size as u64;
        // Bulk reads (hashing, export) go straight through so they do not
        // evict what the user is looking at.
        if buf.len() >= 4 * self.page_size {
            return fetch(offset, buf);
        }
        let first = offset / ps;
        let last = (offset + buf.len() as u64 - 1) / ps;
        let mut pages: Vec<Option<Arc<[u8]>>> = {
            let mut cache = self.pages.lock();
            (first..=last).map(|i| cache.get(&i).cloned()).collect()
        };
        // Fetch each run of missing pages with one request.
        let mut i = 0;
        while i < pages.len() {
            if pages[i].is_some() {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < pages.len() && pages[j].is_none() {
                j += 1;
            }
            let start = (first + i as u64) * ps;
            let end = ((first + j as u64) * ps).min(total);
            let mut run = vec![0u8; (end - start) as usize];
            fetch(start, &mut run)?;
            let mut cache = self.pages.lock();
            for (k, chunk) in run.chunks(self.page_size).enumerate() {
                let page: Arc<[u8]> = chunk.into();
                cache.put(first + (i + k) as u64, page.clone());
                pages[i + k] = Some(page);
            }
            i = j;
        }
        let mut done = 0usize;
        for (k, page) in pages.iter().enumerate() {
            let page = page.as_ref().expect("all pages fetched");
            let page_start = (first + k as u64) * ps;
            let pos = offset + done as u64;
            let in_page = (pos - page_start) as usize;
            let n = (page.len() - in_page).min(buf.len() - done);
            if n == 0 && done < buf.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short page"));
            }
            buf[done..done + n].copy_from_slice(&page[in_page..in_page + n]);
            done += n;
        }
        Ok(())
    }

    /// Drops every page.
    pub(crate) fn clear(&self) {
        self.pages.lock().clear();
    }

    /// Drops the pages intersecting `[offset, offset + len)`.
    pub(crate) fn forget(&self, offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        let ps = self.page_size as u64;
        let mut cache = self.pages.lock();
        for i in offset / ps..=(offset + len - 1) / ps {
            cache.pop(&i);
        }
    }
}

/// Errors when `[offset, offset + len)` is not inside `[0, total)`.
pub(crate) fn check_range(offset: u64, len: usize, total: u64) -> io::Result<()> {
    match offset.checked_add(len as u64) {
        Some(end) if end <= total => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("read {offset}+{len} past end {total}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn fetches_runs_once_and_caches() {
        let data: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        let calls = AtomicUsize::new(0);
        let fetch = |off: u64, buf: &mut [u8]| {
            calls.fetch_add(1, Ordering::SeqCst);
            buf.copy_from_slice(&data[off as usize..off as usize + buf.len()]);
            Ok(())
        };
        let cache = PageCache::new(CacheOptions {
            page_size: 4096,
            budget: 64 * 4096,
        });
        let total = data.len() as u64;
        let mut buf = vec![0; 9000];
        cache.read(total, 100, &mut buf, &fetch).unwrap();
        assert_eq!(buf, data[100..9100]);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "three pages, one request");
        cache.read(total, 4096, &mut buf[..10], &fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "served from cache");
        let mut tail = vec![0; 20];
        cache.read(total, total - 20, &mut tail, &fetch).unwrap();
        assert_eq!(tail, data[data.len() - 20..]);
        assert!(cache.read(total, total - 5, &mut tail, &fetch).is_err());
        cache.forget(4096, 1);
        cache.read(total, 4096, &mut buf[..10], &fetch).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }
}
