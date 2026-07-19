use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Arc, Mutex};

use crate::call_service::CallReadHandle;
use crate::project_context::ProjectContextIndex;
use crate::query::{CompletionScope, NameTable, RankedNameHit};
use crate::store::views::{DeclarationNameRow, DeclarationReadRow};
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

/// Immutable, generation-scoped declaration read model. The existing compact
/// name matcher is the only always-resident workspace-wide structure. Every
/// entry ID is a canonical declaration ID; semantic consumers hydrate the same
/// typed declaration payload by ID so completion recall cannot become a second
/// Hover/navigation truth source.
#[derive(Clone)]
pub struct SemanticDeclarationIndex {
    names: Arc<NameTable>,
    accounted_core_bytes: usize,
    total_budget_bytes: usize,
    payloads: Arc<DeclarationPayloadCache>,
}

impl SemanticDeclarationIndex {
    #[cfg(test)]
    pub(crate) fn from_name_table_for_test(names: NameTable) -> Self {
        Self::build(names, 0)
    }

    /// `total_budget_bytes` covers the resident recall index plus hydrated
    /// payloads. Process/runtime and the other engine snapshot components stay
    /// outside this semantic-index budget and are guarded by the LSP benchmark.
    pub fn build(names: NameTable, total_budget_bytes: usize) -> Self {
        let accounted_core_bytes = names.accounted_bytes();
        let payload_budget_bytes = total_budget_bytes.saturating_sub(accounted_core_bytes);
        Self {
            names: Arc::new(names),
            accounted_core_bytes,
            total_budget_bytes,
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
        self.names.len()
    }

    pub fn accounted_core_bytes(&self) -> usize {
        self.accounted_core_bytes
    }

    pub fn payload_budget_bytes(&self) -> usize {
        self.payloads.budget_bytes
    }

    pub fn total_budget_bytes(&self) -> usize {
        self.total_budget_bytes
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

    pub fn exact_name_hits_scoped(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&CompletionScope>,
    ) -> Vec<RankedNameHit> {
        self.names.exact_name_hits_scoped(name, limit, scope)
    }

    pub fn with_updated_paths(
        &self,
        paths: &HashSet<String>,
        rows: Vec<DeclarationNameRow>,
        project_context: Option<&ProjectContextIndex>,
        total_budget_bytes: usize,
    ) -> Self {
        let names = self
            .names
            .with_updated_declaration_name_rows_with_project_context(paths, rows, project_context);
        Self::build(names, total_budget_bytes)
    }

    pub fn with_project_context(&self, project_context: Option<&ProjectContextIndex>) -> Self {
        Self::build(
            self.names.with_project_context(project_context),
            self.total_budget_bytes,
        )
    }

    pub fn needs_compaction(&self) -> bool {
        self.names.needs_compaction()
    }

    pub fn compacted(&self) -> Self {
        Self::build(self.names.compacted(), self.total_budget_bytes)
    }
}

fn declaration_payload_bytes(row: &DeclarationReadRow) -> usize {
    let fact = &row.fact;
    let key = &fact.identity.logical_key;
    size_of::<DeclarationReadRow>()
        .saturating_add(size_of::<CachedPayload>())
        .saturating_add(size_of::<i64>())
        .saturating_add(size_of::<usize>().saturating_mul(2))
        .saturating_add(1)
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
    use crate::semantic_model::{SemanticDeclarationKind, SemanticDeclarationRole};
    use crate::store::views::DeclarationNameRow;

    use super::SemanticDeclarationIndex;

    fn row(id: i64, name: &str, path: &str) -> DeclarationNameRow {
        DeclarationNameRow {
            id,
            name: name.into(),
            declaration_kind: SemanticDeclarationKind::Function,
            role: SemanticDeclarationRole::Declaration,
            path: path.into(),
            external: false,
            directly_included: false,
        }
    }

    #[test]
    fn dirty_path_replacement_keeps_recall_ids_generation_aligned() {
        let names = crate::query::NameTable::build_from_declaration_name_rows_with_project_context(
            vec![row(1, "before", "a.c")],
            None,
        );
        let index = SemanticDeclarationIndex::build(names, 0);
        let changed = std::collections::HashSet::from(["a.c".to_string()]);
        let updated = index.with_updated_paths(&changed, vec![row(2, "after", "a.c")], None, 0);

        assert!(updated.name_table().search_ranked("before", 10).is_empty());
        assert_eq!(updated.name_table().search_ranked("after", 10)[0].id, 2);
    }

    #[test]
    fn total_budget_pays_for_resident_recall_before_payload_cache() {
        let names = || {
            crate::query::NameTable::build_from_declaration_name_rows_with_project_context(
                vec![row(1, "answer", "answer.c")],
                None,
            )
        };
        let core_only = SemanticDeclarationIndex::build(names(), 0);
        let core_bytes = core_only.accounted_core_bytes();
        assert!(core_bytes > 0);
        assert_eq!(core_only.payload_budget_bytes(), 0);

        let below_core = SemanticDeclarationIndex::build(names(), core_bytes - 1);
        assert_eq!(below_core.payload_budget_bytes(), 0);

        let payload_bytes = 1_024;
        let with_payload =
            SemanticDeclarationIndex::build(names(), core_bytes.saturating_add(payload_bytes));
        assert_eq!(
            with_payload.total_budget_bytes(),
            core_bytes + payload_bytes
        );
        assert_eq!(with_payload.payload_budget_bytes(), payload_bytes);
    }

    #[test]
    #[ignore = "diagnostic large-workspace declaration completion benchmark; set FOSSILSENSE_BENCH_DB"]
    fn benchmark_large_declaration_index_completion_hot_path() {
        let db = std::env::var_os("FOSSILSENSE_BENCH_DB")
            .map(std::path::PathBuf::from)
            .expect("set FOSSILSENSE_BENCH_DB to a current schema benchmark database");
        let store = crate::store::IndexStore::open_readonly(&db).expect("benchmark database");
        let build_started = std::time::Instant::now();
        let names =
            crate::query::NameTable::build_from_declaration_view(&store.declaration_view(), None)
                .expect("stream declaration names");
        let index = SemanticDeclarationIndex::build(names, 0);
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
        println!(
            "declaration_payload_budget_bytes: {}",
            index.payload_budget_bytes()
        );
        println!("declaration_index_build_ms: {build_ms}");
        println!("completion_hot_p50_us: {p50}");
        println!("completion_hot_p95_us: {p95}");
        println!("completion_hot_sql_reads: {}", stats.sql_reads);
    }
}
