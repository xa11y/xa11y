//! The bounded handle table every backend keeps.
//!
//! `ElementData::handle` is an opaque number a provider maps to its native
//! object (a UIA COM element, a retained `AXUIElementRef`, an AT-SPI bus name
//! and path). Without a bound that map grows for as long as the provider
//! lives, which in a long-running test session or MCP server is forever.
//!
//! Handles are minted from a counter and never reused, so insertion order is
//! enough: an evicted handle can never resolve to a newer object. A lookup
//! miss is therefore [`StaleReason::Evicted`](crate::StaleReason::Evicted),
//! never a re-resolution.
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
#[derive(Debug)]
pub struct BoundedHandleCache<T> {
    entries: HashMap<u64, T>,
    order: VecDeque<u64>,
    capacity: usize,
}

impl<T> BoundedHandleCache<T> {
    /// A table holding at most `capacity` entries (at least one).
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Insert or refresh `handle`, evicting the oldest entries past capacity.
    pub fn insert(&mut self, handle: u64, value: T) {
        if self.entries.insert(handle, value).is_some() {
            self.order.retain(|candidate| *candidate != handle);
        }
        self.order.push_back(handle);
        while self.entries.len() > self.capacity {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.entries.remove(&oldest);
                }
                None => break,
            }
        }
    }

    /// The entry for `handle`, if the table still holds it.
    pub fn get(&self, handle: &u64) -> Option<&T> {
        self.entries.get(handle)
    }

    /// Mutable access to the entry for `handle`.
    pub fn get_mut(&mut self, handle: &u64) -> Option<&mut T> {
        self.entries.get_mut(handle)
    }

    /// Drop every entry (a backend whose native ids were invalidated
    /// wholesale, such as an AT-SPI bus restart).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Every `(handle, entry)` pair, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &T)> {
        self.entries.iter()
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
}
