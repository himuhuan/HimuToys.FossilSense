use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use crate::declaration_read_handle::{
    CallReadHandle, DeclarationReadFailure, DeclarationReadFailureReason, DeclarationReadIdentity,
};
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
    /// Retained payload and allocated cache metadata, excluding caller-held rows.
    pub bytes: usize,
    pub payload_bytes: usize,
    pub metadata_bytes: usize,
    pub admission_skips: u64,
    pub eviction_limit_skips: u64,
    pub victim_steps: u64,
    pub lock_wait_ns: u64,
    pub entries: usize,
    pub configured_budget_bytes: usize,
    pub effective_budget_bytes: usize,
    pub publication_shrink_entries: usize,
    pub publication_shrink_bytes: usize,
}

mod cache;
use cache::DeclarationPayloadCache;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeclarationPayloadCacheShrink {
    pub(crate) configured_budget_bytes: usize,
    pub(crate) effective_budget_before_bytes: usize,
    pub(crate) removed_entries: usize,
    pub(crate) removed_bytes: usize,
}

/// Owns the temporary cache suspension used only while a complete replacement
/// snapshot is built. Dropping without `commit` restores the prior generation's
/// configured budget; a successful publication deliberately leaves it cold.
pub(crate) struct DeclarationPayloadCachePublicationLease {
    cache: Arc<DeclarationPayloadCache>,
    shrink: DeclarationPayloadCacheShrink,
    committed: bool,
}

impl DeclarationPayloadCachePublicationLease {
    pub(crate) fn shrink(&self) -> DeclarationPayloadCacheShrink {
        self.shrink
    }

    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for DeclarationPayloadCachePublicationLease {
    fn drop(&mut self) {
        if !self.committed {
            self.cache.restore_configured_budget();
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
    read_identity: Option<DeclarationReadIdentity>,
}

impl SemanticDeclarationIndex {
    #[cfg(test)]
    pub(crate) fn with_payload_budget_for_test(&self, budget: usize) -> Self {
        Self {
            names: self.names.clone(),
            accounted_core_bytes: self.accounted_core_bytes,
            total_budget_bytes: self.accounted_core_bytes.saturating_add(budget),
            payloads: Arc::new(DeclarationPayloadCache::new(budget)),
            read_identity: self.read_identity.clone(),
        }
    }
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
            read_identity: None,
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
        self.payloads.configured_budget_bytes()
    }

    pub(crate) fn effective_payload_budget_bytes(&self) -> usize {
        self.payloads.effective_budget_bytes()
    }

    pub(crate) fn suspend_payload_cache_for_full_publication(
        &self,
    ) -> DeclarationPayloadCachePublicationLease {
        let shrink = self.payloads.suspend_for_full_publication();
        DeclarationPayloadCachePublicationLease {
            cache: self.payloads.clone(),
            shrink,
            committed: false,
        }
    }

    pub fn total_budget_bytes(&self) -> usize {
        self.total_budget_bytes
    }

    pub(crate) fn read_identity(&self) -> Option<&DeclarationReadIdentity> {
        self.read_identity.as_ref()
    }

    pub(crate) fn bind_identity(mut self, identity: &DeclarationReadIdentity) -> Result<Self> {
        if self
            .read_identity
            .as_ref()
            .is_some_and(|current| current != identity)
        {
            return Err(anyhow::Error::new(DeclarationReadFailure::new(
                DeclarationReadFailureReason::IdentityMismatch,
                "declaration index is already bound to another read identity",
            )));
        }
        self.read_identity = Some(identity.clone());
        Ok(self)
    }

    pub(crate) fn payloads_by_ids_bound(
        &self,
        read_handle: &CallReadHandle,
        ids: &[i64],
    ) -> Result<Vec<Arc<DeclarationReadRow>>> {
        if self.read_identity.as_ref() != Some(read_handle.identity()) {
            return Err(anyhow::Error::new(DeclarationReadFailure::new(
                DeclarationReadFailureReason::IdentityMismatch,
                "declaration payload cache and read handle identities differ",
            )));
        }
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
        let mut rebuilt = Self::build(
            self.names.with_project_context(project_context),
            self.total_budget_bytes,
        );
        rebuilt.read_identity.clone_from(&self.read_identity);
        rebuilt
    }

    pub fn needs_compaction(&self) -> bool {
        self.names.needs_compaction()
    }

    pub(crate) fn compacted_with_cancellation(
        &self,
        cancellation: &crate::build_coordinator::BuildCancellation,
    ) -> Option<Self> {
        self.names
            .compacted_with_cancellation(cancellation)
            .map(|names| {
                let mut rebuilt = Self::build(names, self.total_budget_bytes);
                rebuilt.read_identity.clone_from(&self.read_identity);
                rebuilt
            })
    }
}

fn declaration_payload_bytes(row: &DeclarationReadRow) -> usize {
    let fact = &row.fact;
    let key = &fact.identity.logical_key;
    size_of::<DeclarationReadRow>()
        .saturating_add(size_of::<usize>().saturating_mul(2))
        .saturating_add(fact.name.capacity())
        .saturating_add(fact.qualified_name.capacity())
        .saturating_add(fact.path.capacity())
        .saturating_add(
            fact.canonical_signature
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(fact.owner.as_ref().map_or(0, String::capacity))
        .saturating_add(fact.guard.as_ref().map_or(0, String::capacity))
        .saturating_add(fact.identity.locator.workspace_id.capacity())
        .saturating_add(fact.identity.locator.path.capacity())
        .saturating_add(fact.identity.locator.fingerprint.capacity())
        .saturating_add(key.qualified_name.capacity())
        .saturating_add(key.owner.as_ref().map_or(0, String::capacity))
        .saturating_add(key.canonical_signature.as_ref().map_or(0, String::capacity))
        .saturating_add(key.linkage_domain.capacity())
        .saturating_add(key.guard_fingerprint.as_ref().map_or(0, String::capacity))
        .saturating_add(row.logical_key_digest.capacity())
        .saturating_add(row.backing_kind.capacity())
        .saturating_add(row.revision_hash.capacity())
        .saturating_add(match &fact.linkage {
            crate::call_model::LinkageDomain::Internal(path)
            | crate::call_model::LinkageDomain::Package(path) => path.capacity(),
            crate::call_model::LinkageDomain::External
            | crate::call_model::LinkageDomain::Unknown => 0,
        })
        .saturating_add(match &fact.backing {
            crate::semantic_model::DeclarationBacking::CallableAnchor { fingerprint }
            | crate::semantic_model::DeclarationBacking::TypeAlias { fingerprint } => {
                fingerprint.capacity()
            }
            crate::semantic_model::DeclarationBacking::Record { record_key } => {
                record_key.capacity()
            }
            crate::semantic_model::DeclarationBacking::SourceRange { .. }
            | crate::semantic_model::DeclarationBacking::None => 0,
        })
        .saturating_add(match &fact.declarator_shape {
            Some(
                crate::semantic_model::DeclaratorShape::Pointer { qualifiers }
                | crate::semantic_model::DeclaratorShape::Qualified { qualifiers },
            ) => qualifiers.iter().fold(
                qualifiers.capacity().saturating_mul(size_of::<String>()),
                |bytes, value| bytes.saturating_add(value.capacity()),
            ),
            Some(crate::semantic_model::DeclaratorShape::Array { extent_text }) => {
                extent_text.capacity()
            }
            Some(
                crate::semantic_model::DeclaratorShape::Function { signature }
                | crate::semantic_model::DeclaratorShape::FunctionPointer { signature },
            ) => signature.capacity(),
            Some(
                crate::semantic_model::DeclaratorShape::Identity
                | crate::semantic_model::DeclaratorShape::Unsupported,
            )
            | None => 0,
        })
}

#[cfg(test)]
mod tests {
    use crate::call_model::{LinkageDomain, SourcePosition, SourceRange};
    use crate::semantic_model::{
        DeclarationBacking, DeclarationFact, DeclarationIdentity, DeclarationLocator,
        LanguageFidelity, LogicalEntityKey, SemanticDeclarationKind, SemanticDeclarationRole,
        SemanticFactFidelity, SemanticFactProvenance, SemanticLanguage,
    };
    use crate::store::views::{DeclarationNameRow, DeclarationReadRow};

    use super::{declaration_payload_bytes, DeclarationPayloadCache, SemanticDeclarationIndex};

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

    fn source_range() -> SourceRange {
        SourceRange {
            start: SourcePosition {
                line: 0,
                character: 0,
            },
            end: SourcePosition {
                line: 0,
                character: 0,
            },
            start_byte: 0,
            end_byte: 0,
        }
    }

    fn read_row(id: i64) -> DeclarationReadRow {
        let range = source_range();
        let name = format!("cached_{id}");
        DeclarationReadRow {
            id,
            fact: DeclarationFact {
                tag_kind: None,
                identity: DeclarationIdentity {
                    locator: DeclarationLocator {
                        workspace_id: "workspace".into(),
                        path: "src/cache.c".into(),
                        range,
                        fingerprint: format!("fingerprint-{id}"),
                    },
                    logical_key: LogicalEntityKey {
                        qualified_name: name.clone(),
                        declaration_kind: SemanticDeclarationKind::Function,
                        owner: None,
                        canonical_signature: None,
                        linkage_domain: "external".into(),
                        guard_fingerprint: None,
                    },
                    language: SemanticLanguage::C,
                    language_fidelity: LanguageFidelity::Explicit,
                    provenance: SemanticFactProvenance::Ast,
                    fact_fidelity: SemanticFactFidelity::Authoritative,
                    role: SemanticDeclarationRole::Definition,
                },
                name: name.clone(),
                qualified_name: name,
                declaration_kind: SemanticDeclarationKind::Function,
                role: SemanticDeclarationRole::Definition,
                path: "src/cache.c".into(),
                name_range: range,
                declaration_range: range,
                canonical_signature: None,
                declarator_shape: None,
                has_initializer: None,
                owner: None,
                linkage: LinkageDomain::External,
                guard: None,
                backing: DeclarationBacking::None,
            },
            logical_key_digest: vec![id as u8],
            backing_kind: "none".into(),
            backing_id: None,
            external: false,
            directly_included: false,
            revision_id: 1,
            revision_size: 0,
            revision_mtime_ns: 0,
            revision_hash: "revision".into(),
        }
    }

    #[test]
    fn cache_insert_evicts_at_most_32_rows() {
        let cache = DeclarationPayloadCache::new(256 * 1024);
        for id in 0..100 {
            cache.insert(read_row(id));
        }
        let before = cache.stats();
        cache.state.lock().unwrap().effective_budget_bytes = before.bytes;
        let mut large = read_row(1000);
        large.fact.canonical_signature = Some("x".repeat(before.bytes * 3 / 4));
        let returned = cache.insert(large);
        assert_eq!(returned.id, 1000);
        assert!(
            cache.stats().evictions - before.evictions <= 32,
            "one insert must not evict an unbounded number of small rows"
        );
        assert!(
            cache.get(1000).is_none(),
            "an oversized admission must return the row without caching it"
        );
    }

    #[test]
    fn cache_duplicate_id_does_not_evict_unrelated_rows() {
        let cache = DeclarationPayloadCache::new(256 * 1024);
        for id in 0..100 {
            cache.insert(read_row(id));
        }
        let before = cache.stats();
        cache.state.lock().unwrap().effective_budget_bytes = before.bytes;
        let returned = cache.insert(read_row(99));
        assert_eq!(returned.id, 99);
        assert_eq!(cache.stats().evictions, before.evictions);
        assert_eq!(cache.stats().entries, before.entries);
        assert!(cache.get(0).is_some());
    }

    #[test]
    fn cache_victim_work_stays_constant_as_capacity_grows() {
        for count in [10, 1_000, 100_000] {
            let cache = DeclarationPayloadCache::new(512 * 1024 * 1024);
            for id in 0..count {
                cache.insert(read_row(id));
            }
            let before = cache.stats();
            cache.state.lock().unwrap().effective_budget_bytes = before.bytes;
            cache.get(0).unwrap();
            cache.insert(read_row(count));
            let after = cache.stats();
            assert!(
                (1..=2).contains(&(after.victim_steps - before.victim_steps)),
                "{count}: {after:?}"
            );
            assert!(cache.get(0).is_some(), "recent hit remains resident");
            assert!(
                cache.get(1).is_none(),
                "least recently used is removed directly"
            );
            assert!(after.bytes <= after.effective_budget_bytes);
            cache.validate_for_test();
        }
    }

    #[test]
    fn cache_metadata_capacity_and_payload_spare_capacity_are_charged() {
        let mut spare = read_row(1);
        spare.fact.name.reserve(4096);
        spare.logical_key_digest.reserve(1024);
        assert!(
            declaration_payload_bytes(&spare) >= declaration_payload_bytes(&read_row(1)) + 5000
        );
        let cache = DeclarationPayloadCache::new(declaration_payload_bytes(&read_row(1)));
        cache.insert(read_row(1));
        assert_eq!(
            cache.stats().entries,
            0,
            "a payload-only budget cannot allocate cache metadata"
        );
        let cache = DeclarationPayloadCache::new(64 * 1024);
        for id in 0..1000 {
            cache.insert(read_row(id));
            cache.validate_for_test();
        }
        let before = cache.stats();
        assert!(before.metadata_bytes > 0);
        assert_eq!(before.bytes, before.payload_bytes + before.metadata_bytes);
        let mut huge = read_row(1001);
        huge.fact.canonical_signature = Some("x".repeat(64 * 1024));
        cache.insert(huge);
        assert_eq!(cache.stats().evictions, before.evictions);
        let zero = DeclarationPayloadCache::new(0);
        assert_eq!(zero.insert(read_row(2)).id, 2);
        assert_eq!(zero.stats().bytes, 0);
        assert_eq!(zero.stats().evictions, 0);
    }

    #[test]
    fn cache_concurrent_same_id_has_one_node_and_releases_victims_outside_lock() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = Arc::new(DeclarationPayloadCache::new(16 * 1024));
        let drops = Arc::new(AtomicUsize::new(0));
        let weak = Arc::downgrade(&cache);
        let observed = drops.clone();
        cache.set_drop_probe(Arc::new(move || {
            if let Some(cache) = weak.upgrade() {
                assert!(
                    cache.state.try_lock().is_ok(),
                    "payload destructor ran under cache mutex"
                );
                observed.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        cache.insert(read_row(7));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(cache.stats().entries, 1);
        assert_eq!(cache.stats().evictions, 0);
        let held = cache.get(7).unwrap();
        for id in 100..200 {
            cache.insert(read_row(id));
        }
        assert!(cache.get(7).is_none());
        assert_eq!(held.id, 7);
        assert_eq!(held.revision_hash, "revision");
        cache.validate_for_test();
        let before = drops.load(Ordering::SeqCst);
        assert!(before > 0);
        cache.suspend_for_full_publication();
        assert!(drops.load(Ordering::SeqCst) > before);
        assert_eq!(cache.stats().bytes, 0);
        cache.restore_configured_budget();
        cache.insert(read_row(8));
        assert!(cache.get(8).is_some());
        cache.validate_for_test();
    }

    #[test]
    fn publication_shrink_releases_cache_entries_keeps_held_rows_alive_and_blocks_recache() {
        let input = read_row(1);
        let cache =
            DeclarationPayloadCache::new(declaration_payload_bytes(&input).saturating_mul(2));
        let held = cache.insert(input);
        let before = cache.stats();
        assert_eq!(before.entries, 1);
        assert!(before.bytes > 0);

        let shrink = cache.suspend_for_full_publication();
        let after = cache.stats();
        assert_eq!(after.entries, 0);
        assert_eq!(after.bytes, 0);
        assert_eq!(after.effective_budget_bytes, 0);
        assert_eq!(shrink.removed_entries, 1);
        assert_eq!(shrink.removed_bytes, before.bytes);
        assert_eq!(held.id, 1, "request-held Arc must survive cache shrink");
        assert!(cache.get(1).is_none(), "shrink removes the resident entry");

        let reread = cache.insert(read_row(1));
        assert_eq!(reread.id, 1, "a miss still returns the typed row");
        assert_eq!(cache.stats().entries, 0, "disabled cache must not refill");
    }

    #[test]
    fn publication_cache_lease_restores_on_drop_and_stays_disabled_after_commit() {
        let names = crate::query::NameTable::build_from_declaration_name_rows_with_project_context(
            vec![row(1, "lease_test", "lease.c")],
            None,
        );
        let core = names.accounted_bytes();
        let index = SemanticDeclarationIndex::build(names, core.saturating_add(4_096));
        let configured = index.payload_budget_bytes();
        assert!(configured > 0);

        {
            let _lease = index.suspend_payload_cache_for_full_publication();
            assert_eq!(index.effective_payload_budget_bytes(), 0);
        }
        assert_eq!(
            index.effective_payload_budget_bytes(),
            configured,
            "a failed or cancelled replacement build restores the old cache budget",
        );

        let mut lease = index.suspend_payload_cache_for_full_publication();
        lease.commit();
        drop(lease);
        assert_eq!(
            index.effective_payload_budget_bytes(),
            0,
            "a successful full publication keeps its replaced cache disabled",
        );
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
