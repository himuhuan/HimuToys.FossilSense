use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Arc, Mutex};

use crate::call_service::CallReadHandle;
use crate::project_context::ProjectContextIndex;
use crate::query::NameTable;
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

    pub fn exact_name_hits_scoped_for_family(
        &self,
        name: &str,
        limit: usize,
        scope: Option<&crate::query::CompletionScope>,
        semantic_family: crate::semantic_model::SemanticFamily,
    ) -> Vec<crate::query::RankedNameHit> {
        self.names
            .exact_name_hits_scoped_for_family(name, limit, scope, semantic_family)
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
            semantic_family: crate::config::SemanticFamily::CFamily,
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

    #[test]
    #[ignore = "diagnostic production pooled-recall component benchmark; set FOSSILSENSE_BENCH_DB"]
    fn benchmark_large_declaration_index_production_pooled_recall_component() {
        let db = std::env::var_os("FOSSILSENSE_BENCH_DB")
            .map(std::path::PathBuf::from)
            .expect("set FOSSILSENSE_BENCH_DB to a current schema benchmark database");
        let store = crate::store::IndexStore::open_readonly(&db).expect("benchmark database");
        let names =
            crate::query::NameTable::build_from_declaration_view(&store.declaration_view(), None)
                .expect("stream declaration names");
        let index = SemanticDeclarationIndex::build(names, 0);
        let quotas = crate::query::CompletionRecallQuotas::default_for_completion_limit(100);

        let mut samples = Vec::new();
        let mut inspected = Vec::new();
        let mut fuzzy_posting_inspected = Vec::new();
        let mut fuzzy_sample_inspected = Vec::new();
        let mut selection_inspected = Vec::new();
        for prefix in ["i", "in", "init", "d", "de", "dev", "c", "cmd"] {
            let (oracle_hits, _, oracle_metrics) = index
                .name_table()
                .search_completion_recall_pooled_bounded(crate::query::CompletionRecallQuery {
                    query: prefix,
                    quotas,
                    scope: None,
                    active_project: None,
                    prior_pool: None,
                    semantic_family: Some(crate::semantic_model::SemanticFamily::CFamily),
                    cancellation: None,
                    candidate_budget: usize::MAX,
                });
            assert!(!oracle_metrics.truncated);
            let (bounded_hits, _, bounded_metrics) = index
                .name_table()
                .search_completion_recall_pooled_bounded(crate::query::CompletionRecallQuery {
                    query: prefix,
                    quotas,
                    scope: None,
                    active_project: None,
                    prior_pool: None,
                    semantic_family: Some(crate::semantic_model::SemanticFamily::CFamily),
                    cancellation: None,
                    candidate_budget: crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET,
                });
            let oracle_top: std::collections::HashSet<_> =
                oracle_hits.iter().take(100).map(|hit| hit.id).collect();
            let overlap = bounded_hits
                .iter()
                .take(100)
                .filter(|hit| oracle_top.contains(&hit.id))
                .count();
            println!("completion_quality_{prefix}_top100_overlap: {overlap}");
            println!(
                "completion_quality_{prefix}_bounded_hits: {}",
                bounded_hits.len()
            );
            println!(
                "completion_quality_{prefix}_oracle_hits: {}",
                oracle_hits.len()
            );
            assert_eq!(
                overlap,
                oracle_top.len(),
                "bounded top-100 prefix quality diverged for {prefix}"
            );
            assert!(
                bounded_metrics.selection_entries_inspected
                    <= crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET.saturating_mul(6),
                "post-recall selection escaped the fixed channel bound: {bounded_metrics:?}"
            );
            for _ in 0..5 {
                let started = std::time::Instant::now();
                let (hits, pool, metrics) = index
                    .name_table()
                    .search_completion_recall_pooled_bounded(crate::query::CompletionRecallQuery {
                        query: prefix,
                        quotas,
                        scope: None,
                        active_project: None,
                        prior_pool: None,
                        semantic_family: Some(crate::semantic_model::SemanticFamily::CFamily),
                        cancellation: None,
                        candidate_budget: crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET,
                    });
                samples.push(started.elapsed().as_micros());
                inspected.push(metrics.entries_inspected);
                fuzzy_posting_inspected.push(metrics.fuzzy_posting_entries_inspected);
                fuzzy_sample_inspected.push(metrics.fuzzy_sample_entries_inspected);
                selection_inspected.push(metrics.selection_entries_inspected);
                assert!(!metrics.cancelled);
                assert!(metrics.truncated);
                assert!(
                    metrics.entries_inspected <= crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET
                );
                std::hint::black_box((hits, pool));
            }
        }
        for (query, target_name) in [
            ("dbdtn", "device_bind_driver_to_node"),
            ("ugdbn", "uclass_get_device_by_name"),
            ("ogn", "ofnode_get_name"),
            ("bif", "board_init_f"),
        ] {
            let (oracle_hits, _, oracle_metrics) = index
                .name_table()
                .search_completion_recall_pooled_bounded(crate::query::CompletionRecallQuery {
                    query,
                    quotas,
                    scope: None,
                    active_project: None,
                    prior_pool: None,
                    semantic_family: Some(crate::semantic_model::SemanticFamily::CFamily),
                    cancellation: None,
                    candidate_budget: usize::MAX,
                });
            let (bounded_hits, _, bounded_metrics) = index
                .name_table()
                .search_completion_recall_pooled_bounded(crate::query::CompletionRecallQuery {
                    query,
                    quotas,
                    scope: None,
                    active_project: None,
                    prior_pool: None,
                    semantic_family: Some(crate::semantic_model::SemanticFamily::CFamily),
                    cancellation: None,
                    candidate_budget: crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET,
                });
            assert!(!oracle_metrics.truncated);
            assert!(
                !oracle_hits.is_empty(),
                "fuzzy oracle has no hits for {query}"
            );
            let quality_limit = oracle_hits.len().min(100);
            let oracle_top: std::collections::HashSet<_> = oracle_hits
                .iter()
                .take(quality_limit)
                .map(|hit| hit.id)
                .collect();
            let overlap = bounded_hits
                .iter()
                .take(quality_limit)
                .filter(|hit| oracle_top.contains(&hit.id))
                .count();
            println!("completion_fuzzy_quality_{query}_limit: {quality_limit}");
            println!("completion_fuzzy_quality_{query}_overlap: {overlap}");
            let oracle_indexed_quality: std::collections::HashSet<_> = oracle_hits
                .iter()
                .filter(|hit| hit.base_match >= 400)
                .take(100)
                .map(|hit| hit.id)
                .collect();
            let indexed_quality_overlap = bounded_hits
                .iter()
                .filter(|hit| hit.base_match >= 400)
                .take(100)
                .filter(|hit| oracle_indexed_quality.contains(&hit.id))
                .count();
            let target_rank = bounded_hits
                .iter()
                .position(|hit| hit.name == target_name)
                .map(|rank| rank + 1);
            let oracle_target_rank = oracle_hits
                .iter()
                .position(|hit| hit.name == target_name)
                .map(|rank| rank + 1);
            println!(
                "completion_fuzzy_quality_{query}_indexed_oracle: {}",
                oracle_indexed_quality.len()
            );
            println!("completion_fuzzy_quality_{query}_indexed_overlap: {indexed_quality_overlap}");
            println!("completion_fuzzy_quality_{query}_oracle_target_rank: {oracle_target_rank:?}");
            println!("completion_fuzzy_quality_{query}_target_rank: {target_rank:?}");
            println!(
                "completion_fuzzy_quality_{query}_entries_inspected: {}",
                bounded_metrics.entries_inspected
            );
            assert_eq!(
                indexed_quality_overlap,
                oracle_indexed_quality.len(),
                "bounded indexed fuzzy tier diverged for {query}"
            );
            assert_eq!(
                target_rank.is_some(),
                oracle_target_rank.is_some(),
                "bounded target presence diverged from the full-scan oracle for {query}"
            );
        }
        samples.sort_unstable();
        inspected.sort_unstable();
        fuzzy_posting_inspected.sort_unstable();
        fuzzy_sample_inspected.sort_unstable();
        selection_inspected.sort_unstable();
        let p50 = samples[samples.len() / 2];
        let p95 = samples[samples.len() * 95 / 100];
        let inspected_p50 = inspected[inspected.len() / 2];
        let inspected_max = inspected[inspected.len() - 1];
        let fuzzy_posting_p50 = fuzzy_posting_inspected[fuzzy_posting_inspected.len() / 2];
        let fuzzy_posting_max = fuzzy_posting_inspected[fuzzy_posting_inspected.len() - 1];
        let fuzzy_sample_p50 = fuzzy_sample_inspected[fuzzy_sample_inspected.len() / 2];
        let fuzzy_sample_max = fuzzy_sample_inspected[fuzzy_sample_inspected.len() - 1];
        let selection_p50 = selection_inspected[selection_inspected.len() / 2];
        let selection_max = selection_inspected[selection_inspected.len() - 1];
        let stats = index.payload_cache_stats();
        assert_eq!(
            stats.sql_reads, 0,
            "production pooled recall must not hydrate SQLite payloads"
        );
        println!("declarations: {}", index.len());
        println!("completion_production_cold_p50_us: {p50}");
        println!("completion_production_cold_p95_us: {p95}");
        println!("completion_production_entries_inspected_p50: {inspected_p50}");
        println!("completion_production_entries_inspected_max: {inspected_max}");
        println!("completion_production_fuzzy_posting_inspected_p50: {fuzzy_posting_p50}");
        println!("completion_production_fuzzy_posting_inspected_max: {fuzzy_posting_max}");
        println!("completion_production_fuzzy_sample_inspected_p50: {fuzzy_sample_p50}");
        println!("completion_production_fuzzy_sample_inspected_max: {fuzzy_sample_max}");
        println!("completion_production_selection_inspected_p50: {selection_p50}");
        println!("completion_production_selection_inspected_max: {selection_max}");
        println!(
            "completion_production_candidate_budget: {}",
            crate::query::COMPLETION_RECALL_CANDIDATE_BUDGET
        );
        println!("completion_production_truncated: true");
        println!("completion_production_sql_reads: {}", stats.sql_reads);
    }
}
