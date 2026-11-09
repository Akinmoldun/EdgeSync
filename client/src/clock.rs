use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::{Deref, DerefMut};

/// Causal relationship between two vector clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    Dominates,
    Dominated,
    Equal,
    Concurrent,
}

impl fmt::Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Relation::Dominates => write!(f, "Dominates"),
            Relation::Dominated => write!(f, "Dominated"),
            Relation::Equal => write!(f, "Equal"),
            Relation::Concurrent => write!(f, "Concurrent"),
        }
    }
}

/// VectorClock maps device IDs to logical sequence numbers (counters).
/// Internal storage is normalized: absent keys and keys with counter 0 are logically identical,
/// and counters are strictly positive (>= 1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "BTreeMap<String, u64>", into = "BTreeMap<String, u64>")]
pub struct VectorClock {
    counters: BTreeMap<String, u64>,
}

impl VectorClock {
    /// Creates an empty vector clock.
    pub fn new() -> Self {
        Self {
            counters: BTreeMap::new(),
        }
    }

    /// Returns the counter for a given device, defaulting to 0 if not present.
    pub fn get_counter(&self, device_id: &str) -> u64 {
        self.counters.get(device_id).copied().unwrap_or(0)
    }

    /// Sets the counter for a given device ID. If counter is 0, removes the entry.
    pub fn set(&mut self, device_id: impl Into<String>, counter: u64) {
        let key = device_id.into();
        if counter > 0 {
            self.counters.insert(key, counter);
        } else {
            self.counters.remove(&key);
        }
    }

    /// Increments the counter for a given device ID by 1, returning the new counter.
    /// If not present, initializes at 1.
    pub fn increment(&mut self, device_id: &str) -> u64 {
        let next = self.get_counter(device_id) + 1;
        self.counters.insert(device_id.to_string(), next);
        next
    }

    /// Compares this vector clock with another to determine causal ordering.
    ///
    /// - Dominates: for all k, self[k] >= other[k] and exists k where self[k] > other[k]
    /// - Dominated: for all k, other[k] >= self[k] and exists k where other[k] > self[k]
    /// - Equal: for all k, self[k] == other[k]
    /// - Concurrent: exists k1 where self[k1] > other[k1] and exists k2 where other[k2] > self[k2]
    pub fn compare(&self, other: &VectorClock) -> Relation {
        let mut self_greater = false;
        let mut other_greater = false;

        let mut all_keys: BTreeSet<&String> = self.counters.keys().collect();
        all_keys.extend(other.counters.keys());

        for key in all_keys {
            let val_self = self.get_counter(key);
            let val_other = other.get_counter(key);

            if val_self > val_other {
                self_greater = true;
            } else if val_other > val_self {
                other_greater = true;
            }
        }

        if self_greater && !other_greater {
            Relation::Dominates
        } else if other_greater && !self_greater {
            Relation::Dominated
        } else if !self_greater && !other_greater {
            Relation::Equal
        } else {
            Relation::Concurrent
        }
    }

    /// Merges two vector clocks by computing the pairwise maximum for each device ID.
    pub fn merge(&self, other: &VectorClock) -> VectorClock {
        let mut merged = BTreeMap::new();
        let mut all_keys: BTreeSet<&String> = self.counters.keys().collect();
        all_keys.extend(other.counters.keys());

        for key in all_keys {
            let max_val = max(self.get_counter(key), other.get_counter(key));
            if max_val > 0 {
                merged.insert(key.clone(), max_val);
            }
        }

        VectorClock { counters: merged }
    }

    /// Normalizes internal representation by removing any zero-valued entries.
    pub fn normalize(&mut self) {
        self.counters.retain(|_, &mut v| v > 0);
    }

    /// Returns a reference to the underlying counters map.
    pub fn counters(&self) -> &BTreeMap<String, u64> {
        &self.counters
    }

    /// Consumes the vector clock and returns the underlying map.
    pub fn into_inner(self) -> BTreeMap<String, u64> {
        self.counters
    }
}

impl Deref for VectorClock {
    type Target = BTreeMap<String, u64>;

    fn deref(&self) -> &Self::Target {
        &self.counters
    }
}

impl DerefMut for VectorClock {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.counters
    }
}

impl From<BTreeMap<String, u64>> for VectorClock {
    fn from(mut map: BTreeMap<String, u64>) -> Self {
        map.retain(|_, &mut v| v > 0);
        VectorClock { counters: map }
    }
}

impl From<VectorClock> for BTreeMap<String, u64> {
    fn from(vc: VectorClock) -> Self {
        vc.counters
    }
}

impl IntoIterator for VectorClock {
    type Item = (String, u64);
    type IntoIter = std::collections::btree_map::IntoIter<String, u64>;

    fn into_iter(self) -> Self::IntoIter {
        self.counters.into_iter()
    }
}

impl<'a> IntoIterator for &'a VectorClock {
    type Item = (&'a String, &'a u64);
    type IntoIter = std::collections::btree_map::Iter<'a, String, u64>;

    fn into_iter(self) -> Self::IntoIter {
        self.counters.iter()
    }
}

impl FromIterator<(String, u64)> for VectorClock {
    fn from_iter<T: IntoIterator<Item = (String, u64)>>(iter: T) -> Self {
        let mut vc = VectorClock::new();
        for (k, v) in iter {
            vc.set(k, v);
        }
        vc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn test_empty_clocks_equal() {
        let c1 = VectorClock::new();
        let c2 = VectorClock::new();
        assert_eq!(c1.compare(&c2), Relation::Equal);
        assert_eq!(c1.merge(&c2), VectorClock::new());
    }

    #[test]
    fn test_single_device_dominance() {
        let mut c1 = VectorClock::new();
        c1.set("dev_a", 2);

        let mut c2 = VectorClock::new();
        c2.set("dev_a", 1);

        assert_eq!(c1.compare(&c2), Relation::Dominates);
        assert_eq!(c2.compare(&c1), Relation::Dominated);
        assert_eq!(c1.merge(&c2), c1);
    }

    #[test]
    fn test_concurrent_clocks() {
        let mut c1 = VectorClock::new();
        c1.set("dev_a", 2);
        c1.set("dev_b", 1);

        let mut c2 = VectorClock::new();
        c2.set("dev_a", 1);
        c2.set("dev_b", 2);

        assert_eq!(c1.compare(&c2), Relation::Concurrent);
        assert_eq!(c2.compare(&c1), Relation::Concurrent);

        let mut expected = VectorClock::new();
        expected.set("dev_a", 2);
        expected.set("dev_b", 2);

        assert_eq!(c1.merge(&c2), expected);
        assert_eq!(c2.merge(&c1), expected);
    }

    #[test]
    fn test_disjoint_concurrent() {
        let mut c1 = VectorClock::new();
        c1.set("dev_a", 1);

        let mut c2 = VectorClock::new();
        c2.set("dev_b", 1);

        assert_eq!(c1.compare(&c2), Relation::Concurrent);
        assert_eq!(c2.compare(&c1), Relation::Concurrent);

        let mut merged = VectorClock::new();
        merged.set("dev_a", 1);
        merged.set("dev_b", 1);
        assert_eq!(c1.merge(&c2), merged);
    }

    #[test]
    fn test_multi_device_dominance() {
        let mut c1 = VectorClock::new();
        c1.set("dev_a", 3);
        c1.set("dev_b", 2);
        c1.set("dev_c", 1);

        let mut c2 = VectorClock::new();
        c2.set("dev_a", 3);
        c2.set("dev_b", 1);
        c2.set("dev_c", 1);

        assert_eq!(c1.compare(&c2), Relation::Dominates);
        assert_eq!(c2.compare(&c1), Relation::Dominated);
        assert_eq!(c1.merge(&c2), c1);
    }

    #[test]
    fn test_increment_and_normalization() {
        let mut c = VectorClock::new();
        assert_eq!(c.increment("dev_x"), 1);
        assert_eq!(c.increment("dev_x"), 2);
        assert_eq!(c.get_counter("dev_x"), 2);
        assert_eq!(c.get_counter("dev_nonexistent"), 0);

        c.set("dev_y", 0);
        assert_eq!(c.get("dev_y"), None);
    }

    #[test]
    fn test_json_roundtrip() {
        let mut c = VectorClock::new();
        c.set("device-1", 5);
        c.set("device-2", 12);

        let json = serde_json::to_string(&c).expect("serialize vector clock");
        assert_eq!(json, r#"{"device-1":5,"device-2":12}"#);

        let deserialized: VectorClock =
            serde_json::from_str(&json).expect("deserialize vector clock");
        assert_eq!(deserialized, c);
    }

    // ---------------------------------------------------------
    // Property-based testing via proptest (§8 Week 4)
    // ---------------------------------------------------------

    prop_compose! {
        fn arb_vector_clock()(
            map in prop::collection::btree_map("[a-e]", 0..50u64, 0..5)
        ) -> VectorClock {
            VectorClock::from(map)
        }
    }

    prop_compose! {
        fn arb_dominant_pair()(
            base in arb_vector_clock(),
            extra in prop::collection::btree_map("[a-e]", 0..20u64, 0..3),
            strictly_greater_key in "[a-e]",
            increment in 1..20u64,
        ) -> (VectorClock, VectorClock) {
            let b = base;
            let mut a = b.clone();
            for (k, v) in extra {
                let current = a.get_counter(&k);
                a.set(k, current + v);
            }
            let current = a.get_counter(&strictly_greater_key);
            a.set(strictly_greater_key, current + increment);
            (a, b)
        }
    }

    proptest! {
        /// Merging is commutative: A merge B == B merge A
        #[test]
        fn prop_merge_commutative(a in arb_vector_clock(), b in arb_vector_clock()) {
            let ab = a.merge(&b);
            let ba = b.merge(&a);
            prop_assert_eq!(ab, ba);
        }

        /// Merging is associative: A merge (B merge C) == (A merge B) merge C
        #[test]
        fn prop_merge_associative(
            a in arb_vector_clock(),
            b in arb_vector_clock(),
            c in arb_vector_clock(),
        ) {
            let ab_c = (a.merge(&b)).merge(&c);
            let a_bc = a.merge(&(b.merge(&c)));
            prop_assert_eq!(ab_c, a_bc);
        }

        /// If A dominates B, then A merge B == A (on arbitrary generated clocks)
        #[test]
        fn prop_merge_dominance_arbitrary(a in arb_vector_clock(), b in arb_vector_clock()) {
            if a.compare(&b) == Relation::Dominates {
                prop_assert_eq!(a.merge(&b), a.clone());
            }
            if b.compare(&a) == Relation::Dominates {
                prop_assert_eq!(b.merge(&a), b.clone());
            }
            if a.compare(&b) == Relation::Equal {
                prop_assert_eq!(a.merge(&b), a.clone());
                prop_assert_eq!(b.merge(&a), b.clone());
            }
        }

        /// If A dominates B, then A merge B == A (on guaranteed strictly dominant pairs)
        #[test]
        fn prop_merge_dominance_guaranteed((a, b) in arb_dominant_pair()) {
            prop_assert_eq!(a.compare(&b), Relation::Dominates);
            prop_assert_eq!(b.compare(&a), Relation::Dominated);
            prop_assert_eq!(a.merge(&b), a.clone());
            prop_assert_eq!(b.merge(&a), a);
        }
    }
}
