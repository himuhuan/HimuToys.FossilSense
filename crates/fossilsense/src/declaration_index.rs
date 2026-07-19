use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Arc, Mutex};

use crate::call_service::CallReadHandle;
use crate::project_context::ProjectContextIndex;
use crate::query::{CompletionScope, NameTable, RankedNameHit};
use crate::store::views::{DeclarationCoreRow, DeclarationReadRow};
use anyhow::Result;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeclarationPayloadCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub sql_reads: u64,
    pub evictions: u64,
    pub bytes: usize,
    pub entries: usize,
}

struct CachedPayload {
    row: Arc<DeclarationReadRow>,
    bytes: usize,
    last_used: u64,
}

struct DeclarationPayloadCacheState {
    entries: HashMap<i64, CachedPayload>,
    bytes: usize,
    clock: u64,
    stats: DeclarationPayloadCacheStats,
}

struct DeclarationPayloadCache {
    budget_bytes: usize,
    state: Mutex<DeclarationPayloadCacheState>,
}

impl DeclarationPayloadCache {
    fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            state: Mutex::new(DeclarationPayloadCacheState {
                entries: HashMap::new(),
                bytes: 0,
                clock: 0,
                stats: DeclarationPayloadCacheStats::default(),
            }),
        }
    }

    fn get(&self, id: i64) -> Option<Arc<DeclarationReadRow>> {
        let mut state = self
            .state
            .lock()
            .expect("declaration payload cache poisoned");
        state.clock = state.clock.wrapping_add(1).max(1);
        let clock = state.clock;
        let row = state.entries.get_mut(&id).map(|entry| {
            entry.last_used = clock;
            entry.row.clone()
        });
        if row.is_some() {
            state.stats.hits += 1;
        } else {
            state.stats.misses += 1;
        }
        row
    }

    fn record_sql_read(&self) {
        self.state
            .lock()
            .expect("declaration payload cache poisoned")
            .stats
            .sql_reads += 1;
    }

    fn insert(&self, row: DeclarationReadRow) -> Arc<DeclarationReadRow> {
        let row = Arc::new(row);
        let bytes = declaration_payload_bytes(&row);
        if self.budget_bytes == 0 || bytes > self.budget_bytes {
            return row;
        }
        let mut state = self
            .state
            .lock()
            .expect("declaration payload cache poisoned");
        state.clock = state.clock.wrapping_add(1).max(1);
        while state.bytes.saturating_add(bytes) > self.budget_bytes {
            let Some((&victim, _)) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
            else {
                break;
            };
            if let Some(removed) = state.entries.remove(&victim) {
                state.bytes = state.bytes.saturating_sub(removed.bytes);
                state.stats.evictions += 1;
            }
        }
        let clock = state.clock;
        if let Some(replaced) = state.entries.insert(
            row.id,
            CachedPayload {
                row: row.clone(),
                bytes,
                last_used: clock,
            },
        ) {
            state.bytes = state.bytes.saturating_sub(replaced.bytes);
        }
        state.bytes = state.bytes.saturating_add(bytes);
        row
    }

    fn stats(&self) -> DeclarationPayloadCacheStats {
        let state = self
            .state
            .lock()
            .expect("declaration payload cache poisoned");
        DeclarationPayloadCacheStats {
            bytes: state.bytes,
            entries: state.entries.len(),
            ..state.stats
        }
    }
}

#[derive(Debug)]
struct DeclarationCoreSegment {
    rows: Vec<DeclarationCoreRow>,
    path_counts: HashMap<Arc<str>, usize>,
    accounted_bytes: usize,
}

impl DeclarationCoreSegment {
    fn from_rows(mut rows: Vec<DeclarationCoreRow>) -> Self {
        rows.sort_unstable_by_key(|row| row.id);
        let mut path_counts = HashMap::<Arc<str>, usize>::new();
        let mut accounted_bytes = rows.len().saturating_mul(size_of::<DeclarationCoreRow>());
        for row in &rows {
            *path_counts.entry(Arc::from(row.path.as_str())).or_default() += 1;
            accounted_bytes = accounted_bytes
                .saturating_add(row.name.len())
                .saturating_add(row.path.len())
                .saturating_add(row.locator_fingerprint.len())
                .saturating_add(row.backing_kind.len())
                .saturating_add(row.revision_hash.len())
                .saturating_add(row.logical_key_digest.len());
        }
        Self {
            rows,
            path_counts,
            accounted_bytes,
        }
    }

    fn find(&self, id: i64) -> Option<&DeclarationCoreRow> {
        self.rows
            .binary_search_by_key(&id, |row| row.id)
            .ok()
            .map(|index| &self.rows[index])
    }

    fn path_count(&self, path: &str) -> usize {
        self.path_counts.get(path).copied().unwrap_or_default()
    }
}

/// Immutable, generation-scoped declaration read model. The existing compact
/// name matcher is retained as an implementation detail, but every entry ID is
/// now a canonical declaration ID and every semantic consumer can recover the
/// exact declaration core without translating through `symbol_facts`.
#[derive(Clone)]
pub struct SemanticDeclarationIndex {
    names: Arc<NameTable>,
    base: Arc<DeclarationCoreSegment>,
    deltas: Arc<Vec<Arc<DeclarationCoreSegment>>>,
    /// Path -> active delta segment. `None` is a deletion tombstone.
    path_overrides: Arc<HashMap<Arc<str>, Option<usize>>>,
    active_len: usize,
    accounted_core_bytes: usize,
    payloads: Arc<DeclarationPayloadCache>,
}

impl SemanticDeclarationIndex {
    const EAGER_PAYLOAD_CORE_MULTIPLIER: usize = 4;

    #[cfg(test)]
    pub(crate) fn from_name_table_for_test(names: NameTable) -> Self {
        let active_len = names.len();
        let base = Arc::new(DeclarationCoreSegment::from_rows(Vec::new()));
        Self {
            names: Arc::new(names),
            base,
            deltas: Arc::new(Vec::new()),
            path_overrides: Arc::new(HashMap::new()),
            active_len,
            accounted_core_bytes: 0,
            payloads: Arc::new(DeclarationPayloadCache::new(0)),
        }
    }

    pub fn build(
        rows: Vec<DeclarationCoreRow>,
        project_context: Option<&ProjectContextIndex>,
        payload_budget_bytes: usize,
    ) -> Self {
        let names = NameTable::build_from_declaration_rows_with_project_context(
            rows.clone(),
            project_context,
        );
        let base = Arc::new(DeclarationCoreSegment::from_rows(rows));
        let accounted_core_bytes = base.accounted_bytes;
        let active_len = base.rows.len();
        Self {
            names: Arc::new(names),
            base,
            deltas: Arc::new(Vec::new()),
            path_overrides: Arc::new(HashMap::new()),
            active_len,
            accounted_core_bytes,
            payloads: Arc::new(DeclarationPayloadCache::new(payload_budget_bytes)),
        }
    }

    pub fn name_table(&self) -> &NameTable {
        &self.names
    }

    pub fn name_table_arc(&self) -> Arc<NameTable> {
        self.names.clone()
    }

    pub fn len(&self) -> usize {
        self.active_len
    }

    pub fn accounted_core_bytes(&self) -> usize {
        self.accounted_core_bytes
    }

    pub fn should_preload_all_payloads(&self) -> bool {
        Self::budget_prefers_eager_payloads(self.payloads.budget_bytes, self.accounted_core_bytes)
    }

    pub fn budget_prefers_eager_payloads(
        payload_budget_bytes: usize,
        accounted_core_bytes: usize,
    ) -> bool {
        payload_budget_bytes > 0
            && payload_budget_bytes
                >= accounted_core_bytes.saturating_mul(Self::EAGER_PAYLOAD_CORE_MULTIPLIER)
    }

    pub fn core_by_id(&self, id: i64) -> Option<&DeclarationCoreRow> {
        for (delta_index, segment) in self.deltas.iter().enumerate().rev() {
            if let Some(row) = segment.find(id) {
                return self
                    .path_is_active_delta(&row.path, delta_index)
                    .then_some(row);
            }
        }
        let row = self.base.find(id)?;
        (!self.path_overrides.contains_key(row.path.as_str())).then_some(row)
    }

    pub fn payloads_by_ids(
        &self,
        read_handle: &CallReadHandle,
        ids: &[i64],
    ) -> Result<Vec<Arc<DeclarationReadRow>>> {
        let mut output = HashMap::with_capacity(ids.len());
        let mut missing = Vec::new();
        for &id in ids {
            if let Some(row) = self.payloads.get(id) {
                output.insert(id, row);
            } else {
                missing.push(id);
            }
        }
        if !missing.is_empty() {
            self.payloads.record_sql_read();
            for row in read_handle.read(|store| store.declaration_view().by_ids(&missing))? {
                output.insert(row.id, self.payloads.insert(row));
            }
        }
        Ok(ids
            .iter()
            .filter_map(|id| output.get(id).cloned())
            .collect())
    }

    pub fn payload_cache_stats(&self) -> DeclarationPayloadCacheStats {
        self.payloads.stats()
    }

    pub fn preload_payloads(&self, rows: Vec<DeclarationReadRow>) {
        for row in rows {
            self.payloads.insert(row);
        }
    }

    pub fn exact_name_cores_scoped(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<(RankedNameHit, &DeclarationCoreRow)> {
        self.names
            .exact_name_hits_scoped(name, limit, scope)
            .into_iter()
            .filter_map(|hit| self.core_by_id(hit.id).map(|core| (hit, core)))
            .collect()
    }

    pub fn with_updated_paths(
        &self,
        paths: &HashSet<String>,
        rows: Vec<DeclarationCoreRow>,
        project_context: Option<&ProjectContextIndex>,
        payload_budget_bytes: usize,
    ) -> Self {
        let names = self
            .names
            .with_updated_declaration_rows_with_project_context(
                paths,
                rows.clone(),
                project_context,
            );
        let fresh = Arc::new(DeclarationCoreSegment::from_rows(rows));
        let mut deltas = self.deltas.as_ref().clone();
        let delta_index = deltas.len();
        let mut overrides = self.path_overrides.as_ref().clone();
        let mut active_len = self.active_len;
        for path in paths {
            let old_count = match overrides.get(path.as_str()) {
                Some(Some(previous_delta)) => self.deltas[*previous_delta].path_count(path),
                Some(None) => 0,
                None => self.base.path_count(path),
            };
            let fresh_count = fresh.path_count(path);
            active_len = active_len.saturating_sub(old_count) + fresh_count;
            overrides.insert(
                Arc::from(path.as_str()),
                (fresh_count > 0).then_some(delta_index),
            );
        }
        let accounted_core_bytes = self
            .accounted_core_bytes
            .saturating_add(fresh.accounted_bytes);
        deltas.push(fresh);
        Self {
            names: Arc::new(names),
            base: self.base.clone(),
            deltas: Arc::new(deltas),
            path_overrides: Arc::new(overrides),
            active_len,
            accounted_core_bytes,
            payloads: Arc::new(DeclarationPayloadCache::new(payload_budget_bytes)),
        }
    }

    pub fn with_project_context(&self, project_context: Option<&ProjectContextIndex>) -> Self {
        Self {
            names: Arc::new(self.names.with_project_context(project_context)),
            base: self.base.clone(),
            deltas: self.deltas.clone(),
            path_overrides: self.path_overrides.clone(),
            active_len: self.active_len,
            accounted_core_bytes: self.accounted_core_bytes,
            payloads: self.payloads.clone(),
        }
    }

    pub fn needs_compaction(&self) -> bool {
        self.names.needs_compaction()
    }

    pub fn compacted(&self) -> Self {
        let rows: Vec<_> = self
            .base
            .rows
            .iter()
            .chain(self.deltas.iter().flat_map(|segment| segment.rows.iter()))
            .filter(|row| self.core_by_id(row.id).is_some())
            .cloned()
            .collect();
        let base = Arc::new(DeclarationCoreSegment::from_rows(rows));
        Self {
            names: Arc::new(self.names.compacted()),
            active_len: base.rows.len(),
            accounted_core_bytes: base.accounted_bytes,
            base,
            deltas: Arc::new(Vec::new()),
            path_overrides: Arc::new(HashMap::new()),
            payloads: self.payloads.clone(),
        }
    }

    fn path_is_active_delta(&self, path: &str, delta_index: usize) -> bool {
        matches!(self.path_overrides.get(path), Some(Some(active)) if *active == delta_index)
    }
}

fn declaration_payload_bytes(row: &DeclarationReadRow) -> usize {
    let fact = &row.fact;
    let key = &fact.identity.logical_key;
    size_of::<DeclarationReadRow>()
        .saturating_add(fact.name.len())
        .saturating_add(fact.qualified_name.len())
        .saturating_add(fact.path.len())
        .saturating_add(fact.canonical_signature.as_deref().map_or(0, str::len))
        .saturating_add(fact.owner.as_deref().map_or(0, str::len))
        .saturating_add(fact.guard.as_deref().map_or(0, str::len))
        .saturating_add(fact.identity.locator.workspace_id.len())
        .saturating_add(fact.identity.locator.path.len())
        .saturating_add(fact.identity.locator.fingerprint.len())
        .saturating_add(key.qualified_name.len())
        .saturating_add(key.owner.as_deref().map_or(0, str::len))
        .saturating_add(key.canonical_signature.as_deref().map_or(0, str::len))
        .saturating_add(key.linkage_domain.len())
        .saturating_add(key.guard_fingerprint.as_deref().map_or(0, str::len))
        .saturating_add(row.logical_key_digest.len())
        .saturating_add(row.backing_kind.len())
        .saturating_add(row.revision_hash.len())
}

#[cfg(test)]
mod tests {
    use crate::call_model::{SourcePosition, SourceRange};
    use crate::semantic_model::{
        SemanticDeclarationKind, SemanticDeclarationRole, SemanticFactFidelity,
    };
    use crate::store::views::DeclarationCoreRow;

    use super::SemanticDeclarationIndex;

    fn row(id: i64, name: &str, path: &str) -> DeclarationCoreRow {
        let range = SourceRange {
            start: SourcePosition {
                line: 0,
                character: 0,
            },
            end: SourcePosition {
                line: 0,
                character: name.len() as u32,
            },
            start_byte: 0,
            end_byte: name.len(),
        };
        DeclarationCoreRow {
            id,
            name: name.into(),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Declaration,
            fact_fidelity: SemanticFactFidelity::Authoritative,
            name_range: range,
            declaration_range: range,
            logical_key_digest: vec![id as u8; 12],
            locator_fingerprint: format!("{id:024x}"),
            backing_kind: "none".into(),
            backing_id: None,
            path: path.into(),
            external: false,
            directly_included: false,
            revision_id: id,
            revision_size: 0,
            revision_mtime_ns: 0,
            revision_hash: String::new(),
        }
    }

    #[test]
    fn dirty_path_replacement_keeps_name_and_core_generations_aligned() {
        let index = SemanticDeclarationIndex::build(vec![row(1, "before", "a.c")], None, 0);
        let changed = std::collections::HashSet::from(["a.c".to_string()]);
        let updated = index.with_updated_paths(&changed, vec![row(2, "after", "a.c")], None, 0);

        assert!(updated.core_by_id(1).is_none());
        assert_eq!(
            updated.core_by_id(2).map(|core| core.name.as_str()),
            Some("after")
        );
        assert!(updated.name_table().search_ranked("before", 10).is_empty());
        assert_eq!(updated.name_table().search_ranked("after", 10)[0].id, 2);
    }

    #[test]
    #[ignore = "diagnostic large-workspace declaration completion benchmark; set FOSSILSENSE_BENCH_DB"]
    fn benchmark_large_declaration_index_completion_hot_path() {
        let db = std::env::var_os("FOSSILSENSE_BENCH_DB")
            .map(std::path::PathBuf::from)
            .expect("set FOSSILSENSE_BENCH_DB to a current schema benchmark database");
        let store = crate::store::IndexStore::open_readonly(&db).expect("benchmark database");
        let mut rows = Vec::new();
        let build_started = std::time::Instant::now();
        store
            .declaration_view()
            .visit_core_rows(|row| {
                rows.push(row);
                Ok(())
            })
            .expect("stream declaration cores");
        let index = SemanticDeclarationIndex::build(rows, None, 0);
        let build_ms = build_started.elapsed().as_millis();

        let mut samples = Vec::new();
        for prefix in ["i", "in", "init", "d", "de", "dev", "c", "cmd"] {
            for _ in 0..100 {
                let started = std::time::Instant::now();
                std::hint::black_box(index.name_table().search_ranked(prefix, 100));
                samples.push(started.elapsed().as_micros());
            }
        }
        samples.sort_unstable();
        let p50 = samples[samples.len() / 2];
        let p95 = samples[samples.len() * 95 / 100];
        let stats = index.payload_cache_stats();
        assert_eq!(
            stats.sql_reads, 0,
            "completion core lookup must not read SQLite"
        );
        println!("declarations: {}", index.len());
        println!("declaration_core_bytes: {}", index.accounted_core_bytes());
        println!("declaration_index_build_ms: {build_ms}");
        println!("completion_hot_p50_us: {p50}");
        println!("completion_hot_p95_us: {p95}");
        println!("completion_hot_sql_reads: {}", stats.sql_reads);
    }
}
