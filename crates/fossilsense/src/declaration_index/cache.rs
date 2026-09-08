//! Generation-owned declaration cache. All list links are checked arena indices.
use std::collections::HashMap;
use std::mem::size_of;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::{
    declaration_payload_bytes, DeclarationPayloadCacheShrink, DeclarationPayloadCacheStats,
};
use crate::store::views::DeclarationReadRow;

const MAX_EVICTIONS: usize = 32;
const ARENA_GROWTH: usize = 8;

struct CachedPayload {
    id: i64,
    row: Arc<DeclarationReadRow>,
    bytes: usize,
    previous: Option<usize>,
    next: Option<usize>,
    #[cfg(test)]
    drop_probe: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Default)]
struct Slot {
    value: Option<CachedPayload>,
    next_free: Option<usize>,
}

#[derive(Default)]
struct Storage {
    entries: HashMap<i64, usize>,
    allocated_map_bytes: usize,
    slots: Vec<Slot>,
    free: Option<usize>,
    head: Option<usize>,
    tail: Option<usize>,
    #[cfg(test)]
    drop_probe: Option<Arc<dyn Fn() + Send + Sync>>,
}

pub(super) struct DeclarationPayloadCacheState {
    storage: Storage,
    bytes: usize,
    pub(super) effective_budget_bytes: usize,
    stats: DeclarationPayloadCacheStats,
}

pub(super) struct DeclarationPayloadCache {
    configured_budget_bytes: usize,
    pub(super) state: Mutex<DeclarationPayloadCacheState>,
}

fn map_bytes(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    // Account allocated buckets and control bytes, including the control group.
    let buckets = capacity
        .saturating_add(1)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX);
    buckets
        .saturating_mul(size_of::<(i64, usize)>() + 1)
        .saturating_add(16)
}

impl Storage {
    fn metadata_bytes(&self) -> usize {
        self.allocated_map_bytes
            .saturating_add(self.slots.capacity().saturating_mul(size_of::<Slot>()))
    }

    fn projected_metadata_bytes(&self) -> usize {
        // HashMap::capacity may fall as tombstones accumulate without any
        // bucket allocation being freed. Only actual growth updates our ledger.
        let projected_map_bytes = if self.entries.len() < self.entries.capacity() {
            self.allocated_map_bytes
        } else {
            self.allocated_map_bytes.saturating_mul(2).max(map_bytes(3))
        };
        let arena_capacity = if self.free.is_some() || self.slots.len() < self.slots.capacity() {
            self.slots.capacity()
        } else {
            self.slots.len().saturating_add(ARENA_GROWTH)
        };
        projected_map_bytes.saturating_add(arena_capacity.saturating_mul(size_of::<Slot>()))
    }

    fn detach(&mut self, slot: usize) {
        let entry = self.slots[slot].value.as_ref().expect("occupied LRU slot");
        let (previous, next) = (entry.previous, entry.next);
        if let Some(previous) = previous {
            self.slots[previous]
                .value
                .as_mut()
                .expect("occupied previous slot")
                .next = next;
        } else {
            self.head = next;
        }
        if let Some(next) = next {
            self.slots[next]
                .value
                .as_mut()
                .expect("occupied next slot")
                .previous = previous;
        } else {
            self.tail = previous;
        }
    }

    fn attach_head(&mut self, slot: usize) {
        let head = self.head;
        let entry = self.slots[slot].value.as_mut().expect("occupied LRU slot");
        entry.previous = None;
        entry.next = head;
        if let Some(head) = head {
            self.slots[head]
                .value
                .as_mut()
                .expect("occupied head")
                .previous = Some(slot);
        } else {
            self.tail = Some(slot);
        }
        self.head = Some(slot);
    }

    fn touch(&mut self, slot: usize) -> Arc<DeclarationReadRow> {
        if self.head != Some(slot) {
            self.detach(slot);
            self.attach_head(slot);
        }
        self.slots[slot]
            .value
            .as_ref()
            .expect("occupied cache slot")
            .row
            .clone()
    }

    fn remove_tail(&mut self) -> Option<CachedPayload> {
        let slot = self.tail?;
        self.detach(slot);
        let removed = self.slots[slot].value.take().expect("occupied tail");
        self.entries.remove(&removed.id);
        self.slots[slot].next_free = self.free;
        self.free = Some(slot);
        Some(removed)
    }

    fn insert(&mut self, row: Arc<DeclarationReadRow>, bytes: usize) {
        if self.entries.len() == self.entries.capacity() {
            self.entries.reserve(1);
            self.allocated_map_bytes = self
                .allocated_map_bytes
                .max(map_bytes(self.entries.capacity()));
        }
        let slot = if let Some(slot) = self.free {
            self.free = self.slots[slot].next_free.take();
            slot
        } else {
            if self.slots.len() == self.slots.capacity() {
                self.slots.reserve_exact(ARENA_GROWTH);
            }
            self.slots.push(Slot::default());
            self.slots.len() - 1
        };
        let id = row.id;
        self.slots[slot].value = Some(CachedPayload {
            id,
            row,
            bytes,
            previous: None,
            next: None,
            #[cfg(test)]
            drop_probe: self.drop_probe.clone(),
        });
        self.entries.insert(id, slot);
        self.attach_head(slot);
    }
}

impl DeclarationPayloadCache {
    pub(super) fn new(budget_bytes: usize) -> Self {
        Self {
            configured_budget_bytes: budget_bytes,
            state: Mutex::new(DeclarationPayloadCacheState {
                storage: Storage::default(),
                bytes: 0,
                effective_budget_bytes: budget_bytes,
                stats: DeclarationPayloadCacheStats::default(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, DeclarationPayloadCacheState> {
        let started = Instant::now();
        let mut state = self
            .state
            .lock()
            .expect("declaration payload cache poisoned");
        state.stats.lock_wait_ns = state
            .stats
            .lock_wait_ns
            .saturating_add(started.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        state
    }

    pub(super) fn get(&self, id: i64) -> Option<Arc<DeclarationReadRow>> {
        let mut state = self.lock();
        if let Some(slot) = state.storage.entries.get(&id).copied() {
            state.stats.hits = state.stats.hits.saturating_add(1);
            Some(state.storage.touch(slot))
        } else {
            state.stats.misses = state.stats.misses.saturating_add(1);
            None
        }
    }

    pub(super) fn record_sql_read(&self) {
        let mut state = self.lock();
        state.stats.sql_reads = state.stats.sql_reads.saturating_add(1);
    }

    pub(super) fn insert(&self, row: DeclarationReadRow) -> Arc<DeclarationReadRow> {
        let row = Arc::new(row);
        let bytes = declaration_payload_bytes(&row);
        // This list is bounded and allocated before locking. Removed payloads
        // remain owned here until after the mutex guard is explicitly dropped.
        let mut removed = Vec::with_capacity(MAX_EVICTIONS);
        let mut state = self.lock();
        if let Some(slot) = state.storage.entries.get(&row.id).copied() {
            let existing = state.storage.touch(slot);
            drop(state);
            return existing;
        }
        let budget = state.effective_budget_bytes;
        if budget == 0 || bytes.saturating_add(state.storage.metadata_bytes()) > budget {
            state.stats.admission_skips = state.stats.admission_skips.saturating_add(1);
            drop(state);
            return row;
        }
        while state
            .bytes
            .saturating_add(bytes)
            .saturating_add(state.storage.projected_metadata_bytes())
            > budget
            && removed.len() < MAX_EVICTIONS
        {
            let Some(victim) = state.storage.remove_tail() else {
                break;
            };
            state.bytes = state.bytes.saturating_sub(victim.bytes);
            state.stats.evictions = state.stats.evictions.saturating_add(1);
            state.stats.victim_steps = state.stats.victim_steps.saturating_add(1);
            removed.push(victim);
        }
        if state
            .bytes
            .saturating_add(bytes)
            .saturating_add(state.storage.projected_metadata_bytes())
            <= budget
        {
            state.storage.insert(row.clone(), bytes);
            state.bytes = state.bytes.saturating_add(bytes);
            debug_assert!(state.bytes.saturating_add(state.storage.metadata_bytes()) <= budget);
        } else {
            state.stats.admission_skips = state.stats.admission_skips.saturating_add(1);
            if removed.len() == MAX_EVICTIONS {
                state.stats.eviction_limit_skips =
                    state.stats.eviction_limit_skips.saturating_add(1);
            }
        }
        drop(state);
        drop(removed);
        row
    }

    pub(super) fn configured_budget_bytes(&self) -> usize {
        self.configured_budget_bytes
    }
    pub(super) fn effective_budget_bytes(&self) -> usize {
        self.lock().effective_budget_bytes
    }

    pub(super) fn suspend_for_full_publication(&self) -> DeclarationPayloadCacheShrink {
        let mut state = self.lock();
        let before = state.effective_budget_bytes;
        state.effective_budget_bytes = 0;
        let removed = std::mem::take(&mut state.storage);
        let removed_entries = removed.entries.len();
        let removed_bytes =
            std::mem::take(&mut state.bytes).saturating_add(removed.metadata_bytes());
        state.stats.publication_shrink_entries = state
            .stats
            .publication_shrink_entries
            .saturating_add(removed_entries);
        state.stats.publication_shrink_bytes = state
            .stats
            .publication_shrink_bytes
            .saturating_add(removed_bytes);
        drop(state);
        drop(removed);
        DeclarationPayloadCacheShrink {
            configured_budget_bytes: self.configured_budget_bytes,
            effective_budget_before_bytes: before,
            removed_entries,
            removed_bytes,
        }
    }

    pub(super) fn restore_configured_budget(&self) {
        self.lock().effective_budget_bytes = self.configured_budget_bytes;
    }

    pub(super) fn stats(&self) -> DeclarationPayloadCacheStats {
        let state = self.lock();
        let metadata_bytes = state.storage.metadata_bytes();
        DeclarationPayloadCacheStats {
            bytes: state.bytes.saturating_add(metadata_bytes),
            payload_bytes: state.bytes,
            metadata_bytes,
            entries: state.storage.entries.len(),
            configured_budget_bytes: self.configured_budget_bytes,
            effective_budget_bytes: state.effective_budget_bytes,
            ..state.stats
        }
    }
}

#[cfg(test)]
impl Drop for CachedPayload {
    fn drop(&mut self) {
        if let Some(probe) = &self.drop_probe {
            probe();
        }
    }
}

#[cfg(test)]
impl DeclarationPayloadCache {
    pub(super) fn set_drop_probe(&self, probe: Arc<dyn Fn() + Send + Sync>) {
        self.lock().storage.drop_probe = Some(probe);
    }

    pub(super) fn validate_for_test(&self) {
        use std::collections::HashSet;
        let state = self.lock();
        let storage = &state.storage;
        let mut seen = HashSet::new();
        let mut previous = None;
        let mut next = storage.head;
        let mut bytes = 0usize;
        while let Some(slot) = next {
            assert!(seen.insert(slot), "cycle or duplicate LRU node");
            let entry = storage.slots[slot].value.as_ref().expect("occupied node");
            assert_eq!(entry.previous, previous);
            assert_eq!(storage.entries.get(&entry.id), Some(&slot));
            previous = Some(slot);
            next = entry.next;
            bytes += entry.bytes;
        }
        assert_eq!(previous, storage.tail);
        assert_eq!(seen.len(), storage.entries.len());
        assert_eq!(bytes, state.bytes);
        let mut reverse = Vec::new();
        let mut cursor = storage.tail;
        while let Some(slot) = cursor {
            reverse.push(slot);
            assert!(reverse.len() <= seen.len());
            cursor = storage.slots[slot].value.as_ref().unwrap().previous;
        }
        assert_eq!(reverse.len(), seen.len());
        let mut free = storage.free;
        while let Some(slot) = free {
            assert!(seen.insert(slot), "free slot overlaps the live list");
            assert!(storage.slots[slot].value.is_none());
            free = storage.slots[slot].next_free;
        }
        assert_eq!(seen.len(), storage.slots.len());
        assert!(
            state.bytes.saturating_add(storage.metadata_bytes()) <= state.effective_budget_bytes
        );
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    #[test]
    fn allocated_hash_metadata_survives_mass_removal() {
        let mut storage = Storage::default();
        storage.entries.reserve(100_000);
        storage.allocated_map_bytes = map_bytes(storage.entries.capacity());
        for id in 0..100_000 {
            storage.entries.insert(id, id as usize);
        }
        let allocated = storage.metadata_bytes();
        for id in 0..100_000 {
            storage.entries.remove(&id);
            assert_eq!(storage.metadata_bytes(), allocated);
        }
        assert_eq!(std::mem::take(&mut storage).metadata_bytes(), allocated);
        assert_eq!(storage.metadata_bytes(), 0);
    }
}
