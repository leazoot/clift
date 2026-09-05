//! The objects the relay is holding, and nothing else about them.
//!
//! Everything is in memory and everything expires. There is no disk, no
//! database and no index, and that is a design constraint rather than an
//! omission: a relay that persists is a relay that can be subpoenaed, backed
//! up, or forgotten about with somebody's ciphertext still in it. Restarting
//! the process is a complete erasure, which is a property worth having.
//!
//! Two limits, both enforced here because the socket layer above must not be
//! able to forget them:
//!
//! - **per object** -- a single upload cannot be larger than the configured
//!   ceiling;
//! - **in total** -- all objects together cannot exceed a second ceiling, so a
//!   relay cannot be pushed into swap by somebody uploading many objects that
//!   are each individually fine.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One stored ciphertext.
struct Object {
    bytes: Vec<u8>,
    expires_at: Instant,
}

/// Why a store refused an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The object on its own is over the per-object ceiling.
    TooLarge,
    /// The relay is holding as much as it is willing to hold.
    Full,
}

/// What the store currently holds, for the health endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub objects: usize,
    pub bytes: u64,
}

/// The relay's whole state.
pub struct Store {
    objects: Mutex<HashMap<String, Object>>,
    max_object_bytes: u64,
    max_total_bytes: u64,
}

impl Store {
    #[must_use]
    pub fn new(max_object_bytes: u64, max_total_bytes: u64) -> Self {
        Self {
            objects: Mutex::new(HashMap::new()),
            max_object_bytes,
            max_total_bytes,
        }
    }

    #[must_use]
    pub const fn max_object_bytes(&self) -> u64 {
        self.max_object_bytes
    }

    /// Stores `bytes` under `id` for `ttl`.
    ///
    /// # Errors
    /// Refuses an object over the per-object ceiling, and one that would take
    /// the total over its own.
    pub fn put(&self, id: String, bytes: Vec<u8>, ttl: Duration) -> Result<(), Refusal> {
        let size = bytes.len() as u64;
        if size > self.max_object_bytes {
            return Err(Refusal::TooLarge);
        }
        let mut objects = self.lock();
        Self::drop_expired(&mut objects);
        let held: u64 = objects
            .values()
            .map(|object| object.bytes.len() as u64)
            .sum();
        if held.saturating_add(size) > self.max_total_bytes {
            return Err(Refusal::Full);
        }
        objects.insert(
            id,
            Object {
                bytes,
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(())
    }

    /// Takes an object out, if it is there and has not expired.
    ///
    /// Removal is the point. An object is single use, and the only way to make
    /// that true in the presence of two simultaneous requests is for the store
    /// to hand it to exactly one of them. The caller writes the bytes out and,
    /// if that write fails, calls [`Store::restore`] -- so a dropped connection
    /// does not consume the object, but a delivered one always does.
    #[must_use]
    pub fn take(&self, id: &str) -> Option<Vec<u8>> {
        let mut objects = self.lock();
        Self::drop_expired(&mut objects);
        objects.remove(id).map(|object| object.bytes)
    }

    /// Puts an object back after a delivery that did not complete.
    ///
    /// The remaining lifetime is not restored: the object gets what is left of
    /// the original window, computed from the same clock. A failed delivery is
    /// not a reason to extend how long a secret exists.
    pub fn restore(&self, id: String, bytes: Vec<u8>, expires_at: Instant) {
        if expires_at <= Instant::now() {
            return;
        }
        let mut objects = self.lock();
        objects.insert(id, Object { bytes, expires_at });
    }

    /// When the object under `id` expires, if it is there.
    #[must_use]
    pub fn expiry_of(&self, id: &str) -> Option<Instant> {
        self.lock().get(id).map(|object| object.expires_at)
    }

    /// Removes an object. A miss is not a failure: the caller wanted it gone.
    pub fn remove(&self, id: &str) {
        self.lock().remove(id);
    }

    /// Drops everything past its expiry. Called by the sweeper, and on every
    /// write, so an idle relay still forgets.
    pub fn sweep(&self) {
        Self::drop_expired(&mut self.lock());
    }

    #[must_use]
    pub fn usage(&self) -> Usage {
        let objects = self.lock();
        Usage {
            objects: objects.len(),
            bytes: objects
                .values()
                .map(|object| object.bytes.len() as u64)
                .sum(),
        }
    }

    fn drop_expired(objects: &mut HashMap<String, Object>) {
        let now = Instant::now();
        objects.retain(|_, object| object.expires_at > now);
    }

    /// A poisoned lock means a worker thread panicked while holding it. The
    /// map is a plain collection with no invariant that a panic could have left
    /// half-applied, so carrying on with it is correct -- and refusing every
    /// subsequent request because one panicked is not.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Object>> {
        self.objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new(1024, 4096)
    }

    #[test]
    fn an_object_goes_in_and_comes_out_once() {
        let store = store();
        store
            .put(
                "a".to_string(),
                b"ciphertext".to_vec(),
                Duration::from_secs(60),
            )
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(store.take("a"), Some(b"ciphertext".to_vec()));
        assert_eq!(store.take("a"), None, "the object was served twice");
    }

    #[test]
    fn an_expired_object_is_gone_even_without_a_sweep() {
        let store = store();
        store
            .put("a".to_string(), b"x".to_vec(), Duration::from_millis(1))
            .unwrap_or_else(|error| panic!("{error:?}"));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(store.take("a"), None);
        assert_eq!(store.usage().objects, 0);
    }

    #[test]
    fn an_object_over_the_ceiling_is_refused() {
        let store = store();
        assert_eq!(
            store.put("a".to_string(), vec![0; 2048], Duration::from_secs(60)),
            Err(Refusal::TooLarge)
        );
        assert_eq!(store.usage().objects, 0);
    }

    #[test]
    fn the_relay_refuses_to_hold_more_than_its_total() {
        let store = store();
        for index in 0..4 {
            store
                .put(index.to_string(), vec![0; 1024], Duration::from_secs(60))
                .unwrap_or_else(|error| panic!("{error:?}"));
        }
        assert_eq!(
            store.put("overflow".to_string(), vec![0; 1], Duration::from_secs(60)),
            Err(Refusal::Full)
        );
        assert_eq!(store.usage().bytes, 4096);
    }

    #[test]
    fn a_restored_object_can_be_taken_again_but_not_beyond_its_original_expiry() {
        let store = store();
        let expiry = Instant::now() + Duration::from_secs(60);
        store.restore("a".to_string(), b"x".to_vec(), expiry);
        assert_eq!(store.take("a"), Some(b"x".to_vec()));

        // Already past: restoring is a no-op rather than a resurrection.
        store.restore("b".to_string(), b"y".to_vec(), Instant::now());
        assert_eq!(store.take("b"), None);
    }

    #[test]
    fn removing_something_that_is_not_there_is_not_a_failure() {
        let store = store();
        store.remove("absent");
        assert_eq!(store.usage().objects, 0);
    }

    /// Two threads racing for one object: exactly one may win.
    #[test]
    fn only_one_of_two_simultaneous_takes_gets_the_object() {
        let store = std::sync::Arc::new(store());
        store
            .put("a".to_string(), b"once".to_vec(), Duration::from_secs(60))
            .unwrap_or_else(|error| panic!("{error:?}"));

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = std::sync::Arc::clone(&store);
            let barrier = std::sync::Arc::clone(&barrier);
            let winners = std::sync::Arc::clone(&winners);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if store.take("a").is_some() {
                    winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }));
        }
        for handle in handles {
            let _ = handle.join();
        }
        assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
