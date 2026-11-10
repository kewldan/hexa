//! Value freezing (PROC-04): a background thread keeps writing fixed bytes to
//! addresses on an interval, like Cheat Engine's "freeze".

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::source::MemorySource;

/// Shortest allowed interval.
pub const MIN_INTERVAL: Duration = Duration::from_millis(1);
/// Interval used by the UI by default.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(100);

/// One frozen address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenValue {
    pub id: u64,
    pub address: u64,
    pub bytes: Vec<u8>,
    pub label: Option<String>,
    pub enabled: bool,
    /// Successful writes so far.
    pub writes: u64,
    /// Error of the last failed write (cleared by the next success).
    pub last_error: Option<String>,
}

struct Shared {
    source: Arc<dyn MemorySource>,
    entries: Mutex<Vec<FrozenValue>>,
    interval: Mutex<Duration>,
    stop: Mutex<bool>,
    wake: Condvar,
    next_id: AtomicU64,
}

impl Shared {
    fn tick(&self) {
        let jobs: Vec<(u64, u64, Vec<u8>)> = self
            .entries
            .lock()
            .iter()
            .filter(|e| e.enabled)
            .map(|e| (e.id, e.address, e.bytes.clone()))
            .collect();
        for (id, address, bytes) in jobs {
            let result = self.source.write(address, &bytes);
            let mut entries = self.entries.lock();
            if let Some(e) = entries.iter_mut().find(|e| e.id == id) {
                match result {
                    Ok(()) => {
                        e.writes += 1;
                        e.last_error = None;
                    }
                    Err(err) => e.last_error = Some(err.to_string()),
                }
            }
        }
    }

    fn run(&self) {
        loop {
            {
                let mut stop = self.stop.lock();
                if *stop {
                    return;
                }
                let interval = *self.interval.lock();
                self.wake.wait_for(&mut stop, interval);
                if *stop {
                    return;
                }
            }
            self.tick();
        }
    }
}

/// Keeps values frozen from a background thread (PROC-04). Dropping the
/// freezer stops the thread.
pub struct Freezer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for Freezer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Freezer")
            .field("source", &self.shared.source.name())
            .field("interval", &*self.shared.interval.lock())
            .field("entries", &self.shared.entries.lock().len())
            .finish_non_exhaustive()
    }
}

impl Freezer {
    /// Starts the background thread writing every `interval`.
    pub fn start(source: Arc<dyn MemorySource>, interval: Duration) -> Result<Self> {
        if !source.writable() {
            return Err(Error::unsupported("the memory source is not writable"));
        }
        let shared = Arc::new(Shared {
            source,
            entries: Mutex::new(Vec::new()),
            interval: Mutex::new(interval.max(MIN_INTERVAL)),
            stop: Mutex::new(false),
            wake: Condvar::new(),
            next_id: AtomicU64::new(1),
        });
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("hexa-freezer".into())
            .spawn(move || worker.run())?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Freezes `bytes` at `address`. The value is written once immediately;
    /// a failing first write is returned and nothing is added.
    pub fn freeze(&self, address: u64, bytes: Vec<u8>, label: Option<String>) -> Result<u64> {
        if bytes.is_empty() {
            return Err(Error::invalid("nothing to freeze"));
        }
        self.shared.source.write(address, &bytes)?;
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.entries.lock().push(FrozenValue {
            id,
            address,
            bytes,
            label,
            enabled: true,
            writes: 1,
            last_error: None,
        });
        Ok(id)
    }

    /// Stops freezing one value. Returns whether it existed.
    pub fn unfreeze(&self, id: u64) -> bool {
        let mut entries = self.shared.entries.lock();
        let before = entries.len();
        entries.retain(|e| e.id != id);
        entries.len() != before
    }

    /// Pauses or resumes one value.
    pub fn set_enabled(&self, id: u64, enabled: bool) -> bool {
        self.update(id, |e| e.enabled = enabled)
    }

    /// Replaces the frozen bytes.
    pub fn set_value(&self, id: u64, bytes: Vec<u8>) -> bool {
        !bytes.is_empty() && self.update(id, |e| e.bytes = bytes)
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut FrozenValue)) -> bool {
        let mut entries = self.shared.entries.lock();
        entries.iter_mut().find(|e| e.id == id).map(f).is_some()
    }

    /// All frozen values with their write statistics.
    pub fn list(&self) -> Vec<FrozenValue> {
        self.shared.entries.lock().clone()
    }

    /// Unfreezes everything (the thread keeps running).
    pub fn clear(&self) {
        self.shared.entries.lock().clear();
    }

    pub fn interval(&self) -> Duration {
        *self.shared.interval.lock()
    }

    pub fn set_interval(&self, interval: Duration) {
        *self.shared.interval.lock() = interval.max(MIN_INTERVAL);
        self.shared.wake.notify_all();
    }

    /// Stops the background thread and waits for it.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        *self.shared.stop.lock() = true;
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Freezer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::{MemoryRegion, Protection};
    use crate::source::{Access, MemorySnapshot, ProcessMemory};
    use std::time::Instant;

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    #[test]
    fn freezes_snapshot_value() {
        let mut s = MemorySnapshot::new("s", 8);
        s.add_region(MemoryRegion::new(0x1000, 0, Protection::RW), vec![0; 64])
            .unwrap();
        let s: Arc<MemorySnapshot> = Arc::new(s);
        let f = Freezer::start(s.clone(), Duration::from_millis(2)).unwrap();
        let id = f
            .freeze(0x1008, vec![0xAA, 0xBB], Some("hp".into()))
            .unwrap();
        s.write(0x1008, &[0, 0]).unwrap();
        assert!(wait_until(|| {
            let mut b = [0u8; 2];
            s.read(0x1008, &mut b).unwrap();
            b == [0xAA, 0xBB]
        }));
        assert!(f.freeze(0x9000, vec![1], None).is_err());
        let listed = f.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label.as_deref(), Some("hp"));
        assert!(f.set_enabled(id, false));
        assert!(f.unfreeze(id));
        assert!(!f.unfreeze(id));
        f.stop();
    }

    #[test]
    fn freezes_own_memory() {
        let value = std::sync::atomic::AtomicU32::new(1);
        let addr = (&raw const value) as u64;
        let mem = Arc::new(ProcessMemory::open_self(Access::ReadWrite).unwrap());
        let f = Freezer::start(mem, Duration::from_millis(1)).unwrap();
        f.freeze(addr, 42u32.to_ne_bytes().to_vec(), None).unwrap();
        value.store(7, std::sync::atomic::Ordering::SeqCst);
        assert!(wait_until(|| value
            .load(std::sync::atomic::Ordering::SeqCst)
            == 42));
        f.clear();
        drop(f);
    }
}
