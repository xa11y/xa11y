//! The bounded handle table every backend keeps.
//!
//! `ElementData::handle` is an opaque number a provider maps to its native
//! object (a UIA COM element, a retained `AXUIElementRef`, an AT-SPI bus name
//! and path). Without a bound that map grows for as long as the provider
//! lives, which in a long-running test session or MCP server is forever.
//!
//! Handles are minted from a counter and never reused once evicted, so
//! insertion order is enough: an evicted handle can never resolve to a newer
//! object. A lookup miss is therefore
//! [`StaleReason::Evicted`](crate::StaleReason::Evicted), never a
//! re-resolution. A backend may hand the *same* resident handle out again for
//! the same native object (Linux keys handles by bus name and path); it
//! learns which handles left the table from
//! [`BoundedHandleCache::insert_evicting`], so its reverse map never names an
//! evicted handle.
//!
//! Not public API (`#[doc(hidden)]` at the re-export): shared by the backend
//! crates only.

use std::collections::{HashMap, VecDeque};

/// How many native objects a backend keeps reachable at once. Several large
/// tree reads' worth, while still bounded.
pub const MAX_CACHED_ELEMENTS: usize = 65_536;

/// Insertion-order-bounded handle table with refresh-on-update.
///
/// Updating an existing key refreshes its age, which lets repeated
/// enumeration keep a still-live entry resident while old ones age out.
///
/// A refresh is O(1) amortized: the age queue holds `(handle, stamp)` pairs
/// and only the pair whose stamp matches the entry's is live, so a refresh
/// pushes a new pair instead of searching the queue for the old one. Dead
/// pairs are skipped at eviction and compacted away once the queue holds
/// twice the capacity.
#[derive(Debug)]
pub struct BoundedHandleCache<T> {
    entries: HashMap<u64, (T, u64)>,
    order: VecDeque<(u64, u64)>,
    capacity: usize,
    next_stamp: u64,
}

impl<T> BoundedHandleCache<T> {
    /// A table holding at most `capacity` entries (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
            next_stamp: 0,
        }
    }

    /// Insert or refresh `handle`, evicting the oldest entries past capacity.
    pub fn insert(&mut self, handle: u64, value: T) {
        // The caller keeps no reverse map, so the evicted entries are just
        // dropped.
        drop(self.insert_evicting(handle, value));
    }

    /// [`insert`](Self::insert), returning the `(handle, entry)` pairs it
    /// evicted, oldest first. Never includes `handle` itself.
    pub fn insert_evicting(&mut self, handle: u64, value: T) -> Vec<(u64, T)> {
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        self.entries.insert(handle, (value, stamp));
        self.order.push_back((handle, stamp));
        let mut evicted = Vec::new();
        while self.entries.len() > self.capacity {
            let Some((oldest, oldest_stamp)) = self.order.pop_front() else {
                break;
            };
            let live = self
                .entries
                .get(&oldest)
                .is_some_and(|(_, stamp)| *stamp == oldest_stamp);
            if live {
                if let Some((value, _)) = self.entries.remove(&oldest) {
                    evicted.push((oldest, value));
                }
            }
        }
        if self.order.len() > 2 * self.capacity {
            let entries = &self.entries;
            self.order.retain(|(handle, stamp)| {
                entries.get(handle).is_some_and(|(_, live)| live == stamp)
            });
        }
        evicted
    }

    /// The entry for `handle`, if the table still holds it.
    pub fn get(&self, handle: &u64) -> Option<&T> {
        self.entries.get(handle).map(|(value, _)| value)
    }

    /// Mutable access to the entry for `handle`.
    pub fn get_mut(&mut self, handle: &u64) -> Option<&mut T> {
        self.entries.get_mut(handle).map(|(value, _)| value)
    }

    /// Drop every entry (a backend whose native ids were invalidated
    /// wholesale, such as an AT-SPI bus restart).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Every `(handle, entry)` pair, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &T)> {
        self.entries
            .iter()
            .map(|(handle, (value, _))| (handle, value))
    }

    /// How many entries the table holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A table of [`MAX_CACHED_ELEMENTS`] filled from `(handle, value)` pairs,
/// in order (test fixtures that prefill handles).
impl<T> FromIterator<(u64, T)> for BoundedHandleCache<T> {
    fn from_iter<I: IntoIterator<Item = (u64, T)>>(iter: I) -> Self {
        let mut cache = Self::new(MAX_CACHED_ELEMENTS);
        for (handle, value) in iter {
            cache.insert(handle, value);
        }
        cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_oldest_and_refreshes_on_update() {
        let mut cache = BoundedHandleCache::new(2);
        cache.insert(1, "a");
        cache.insert(2, "b");
        cache.insert(1, "a2"); // refresh 1: 2 is now oldest
        cache.insert(3, "c");
        assert_eq!(cache.get(&1), Some(&"a2"));
        assert_eq!(cache.get(&2), None);
        assert_eq!(cache.get(&3), Some(&"c"));
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn insert_evicting_reports_exactly_the_evicted_entries() {
        let mut cache = BoundedHandleCache::new(2);
        assert!(cache.insert_evicting(1, "a").is_empty());
        assert!(cache.insert_evicting(2, "b").is_empty());
        // Refreshing a resident handle evicts nothing.
        assert!(cache.insert_evicting(1, "a2").is_empty());
        assert_eq!(cache.insert_evicting(3, "c"), vec![(2, "b")]);
        assert_eq!(cache.insert_evicting(4, "d"), vec![(1, "a2")]);
        assert_eq!(cache.len(), 2);
    }

    /// Repeated refreshes of resident handles neither evict them nor grow
    /// the age queue without bound.
    #[test]
    fn refreshes_keep_the_age_queue_bounded() {
        let mut cache = BoundedHandleCache::new(4);
        for round in 0..1000 {
            for handle in 0..4 {
                assert!(cache.insert_evicting(handle, round).is_empty());
            }
            assert!(cache.order.len() <= 2 * cache.capacity);
        }
        assert_eq!(cache.len(), 4);
        // The least recently refreshed handle is still the first to go.
        assert_eq!(cache.insert_evicting(9, 0), vec![(0, 999)]);
    }
}
