//! Runtime memory observability shared by the LSP resource monitor and the
//! headless `fossilsense memory` CLI.
//!
//! Category values are structure-level estimates in the same spirit as
//! `query::name_updates::hash_table_bytes`: they attribute the dominant bytes
//! so users and developers can see where memory goes. They are not allocator
//! promises — the process-level private-bytes/RSS gates remain the
//! authoritative measurement, and everything the categories cannot attribute
//! stays visible as `process.other_bytes`.

use std::mem::size_of;

use serde::{Deserialize, Serialize};

use crate::declaration_index::DeclarationPayloadCacheStats;

/// Estimated bucket bytes for a hash table with `capacity` slots. hashbrown
/// stores one control byte beside each bucket.
pub(crate) fn hash_table_bytes<K, V>(capacity: usize) -> usize {
    capacity.saturating_mul(size_of::<(K, V)>().saturating_add(1))
}

/// Estimated element bytes for a `Vec` with `capacity` slots (no per-element
/// heap contents; callers add those separately).
pub(crate) fn vec_bytes<T>(capacity: usize) -> usize {
    capacity.saturating_mul(size_of::<T>())
}

/// Process-level totals. `other_bytes` covers everything the categories do
/// not itemize: the tokio runtime, allocator overhead and fragmentation,
/// SQLite connections, older index generations still held by in-flight
/// requests, live/external parse caches, reference role/search caches, the
/// include path index, call read handles, workspace semantics, and the parsed
/// fact tables inside candidate overlay snapshots. A large `other_bytes` is
/// expected and is not by itself a leak signal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessMemoryReport {
    pub total_bytes: u64,
    pub attributed_bytes: u64,
    pub other_bytes: u64,
}

/// Mutually exclusive structural estimates for the resident NameTable. Their
/// sum explains the name-index core only; Private Bytes/RSS remains the
/// process-level source of truth.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameIndexMemoryComponents {
    pub bytes: u64,
    pub declaration_entry_bytes: u64,
    pub name_record_bytes: u64,
    pub original_name_bytes: u64,
    pub lowercase_name_bytes: u64,
    pub shared_name_bytes: u64,
    pub path_metadata_bytes: u64,
    pub project_metadata_bytes: u64,
    pub sorting_index_bytes: u64,
    pub short_prefix_posting_bytes: u64,
    pub fuzzy_posting_bytes: u64,
    pub prefix_path_posting_bytes: u64,
    pub path_posting_bytes: u64,
    pub project_posting_bytes: u64,
    pub fixed_overhead_bytes: u64,
}

impl NameIndexMemoryComponents {
    fn recompute_bytes(&mut self) {
        self.bytes = self
            .declaration_entry_bytes
            .saturating_add(self.name_record_bytes)
            .saturating_add(self.original_name_bytes)
            .saturating_add(self.lowercase_name_bytes)
            .saturating_add(self.shared_name_bytes)
            .saturating_add(self.path_metadata_bytes)
            .saturating_add(self.project_metadata_bytes)
            .saturating_add(self.sorting_index_bytes)
            .saturating_add(self.short_prefix_posting_bytes)
            .saturating_add(self.fuzzy_posting_bytes)
            .saturating_add(self.prefix_path_posting_bytes)
            .saturating_add(self.path_posting_bytes)
            .saturating_add(self.project_posting_bytes)
            .saturating_add(self.fixed_overhead_bytes);
    }

    fn saturating_add_assign(&mut self, other: &Self) {
        self.declaration_entry_bytes = self
            .declaration_entry_bytes
            .saturating_add(other.declaration_entry_bytes);
        self.name_record_bytes = self
            .name_record_bytes
            .saturating_add(other.name_record_bytes);
        self.original_name_bytes = self
            .original_name_bytes
            .saturating_add(other.original_name_bytes);
        self.lowercase_name_bytes = self
            .lowercase_name_bytes
            .saturating_add(other.lowercase_name_bytes);
        self.shared_name_bytes = self
            .shared_name_bytes
            .saturating_add(other.shared_name_bytes);
        self.path_metadata_bytes = self
            .path_metadata_bytes
            .saturating_add(other.path_metadata_bytes);
        self.project_metadata_bytes = self
            .project_metadata_bytes
            .saturating_add(other.project_metadata_bytes);
        self.sorting_index_bytes = self
            .sorting_index_bytes
            .saturating_add(other.sorting_index_bytes);
        self.short_prefix_posting_bytes = self
            .short_prefix_posting_bytes
            .saturating_add(other.short_prefix_posting_bytes);
        self.fuzzy_posting_bytes = self
            .fuzzy_posting_bytes
            .saturating_add(other.fuzzy_posting_bytes);
        self.prefix_path_posting_bytes = self
            .prefix_path_posting_bytes
            .saturating_add(other.prefix_path_posting_bytes);
        self.path_posting_bytes = self
            .path_posting_bytes
            .saturating_add(other.path_posting_bytes);
        self.project_posting_bytes = self
            .project_posting_bytes
            .saturating_add(other.project_posting_bytes);
        self.fixed_overhead_bytes = self
            .fixed_overhead_bytes
            .saturating_add(other.fixed_overhead_bytes);
    }
}

/// The always-resident compact recall table behind ordinary completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameIndexMemoryReport {
    pub bytes: u64,
    pub entry_count: u64,
    #[serde(default)]
    pub components: NameIndexMemoryComponents,
    pub base_segment_bytes: u64,
    pub delta_segments_bytes: u64,
    pub delta_segment_count: u64,
    pub fallback_table_bytes: u64,
}

/// LRU cache of hydrated declaration payloads shared by hover, navigation,
/// and completion details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclarationCacheMemoryReport {
    pub bytes: u64,
    pub entry_count: u64,
    pub budget_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub sql_reads: u64,
}

/// File-to-file include graph plus the include/Go-import completion tables,
/// the indexed file list, and project context evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRelationsMemoryReport {
    pub bytes: u64,
    pub reach_graph_bytes: u64,
    pub include_edge_count: u64,
    pub include_table_bytes: u64,
    pub go_import_table_bytes: u64,
    pub indexed_files_bytes: u64,
    pub file_count: u64,
    pub project_context_bytes: u64,
}

/// Unsaved editor document text held by the server plus request overlay
/// snapshots derived from it. `bytes` counts document and overlay *source
/// text* — the dominant cost of a dirty editing session; the parsed fact
/// tables overlays build from that text are comparatively small and stay
/// inside `process.other_bytes`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDocumentsMemoryReport {
    pub bytes: u64,
    pub document_count: u64,
    pub overlay_bytes: u64,
}

/// One point-in-time memory observation. Itemized categories reflect the
/// currently published index generation of every workspace root; older
/// generations retained by in-flight requests are covered by
/// `process.other_bytes` instead of being double-counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryReport {
    pub process: ProcessMemoryReport,
    pub name_index: NameIndexMemoryReport,
    pub declaration_cache: DeclarationCacheMemoryReport,
    pub file_relations: FileRelationsMemoryReport,
    pub open_documents: OpenDocumentsMemoryReport,
    pub index_disk_bytes: u64,
    pub timestamp: u64,
}

/// Static per-generation part of one engine snapshot. Computed once per
/// published generation and cached on the snapshot; dynamic parts (payload
/// cache counters, open documents) are sampled on every report tick.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotMemoryReport {
    pub name_table_bytes: usize,
    pub name_entry_count: usize,
    pub name_index_components: NameIndexMemoryComponents,
    pub base_segment_bytes: usize,
    pub delta_segments_bytes: usize,
    pub delta_segment_count: usize,
    pub fallback_table_bytes: usize,
    pub reach_graph_bytes: usize,
    pub include_edge_count: usize,
    pub include_table_bytes: usize,
    pub go_import_table_bytes: usize,
    pub indexed_files_bytes: usize,
    pub file_count: usize,
    pub project_context_bytes: usize,
}

/// Result of hydrating one workspace's read model from an existing index,
/// produced by the headless `fossilsense memory` CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HydratedMemoryReport {
    pub report: MemoryReport,
    pub declarations: usize,
    pub files: usize,
    pub hydration_ms: u64,
    pub memory_before_bytes: u64,
    pub memory_after_bytes: u64,
    pub hydration_delta_bytes: u64,
}

/// Dynamic sample of one snapshot's declaration payload cache.
pub(crate) struct DeclarationCacheSample {
    pub(crate) stats: DeclarationPayloadCacheStats,
    pub(crate) budget_bytes: usize,
}

impl MemoryReport {
    pub(crate) fn assemble(
        snapshots: &[SnapshotMemoryReport],
        declaration_caches: &[DeclarationCacheSample],
        open_documents: OpenDocumentsMemoryReport,
        process_total_bytes: u64,
        index_disk_bytes: u64,
        timestamp: u64,
    ) -> Self {
        let mut name_index = NameIndexMemoryReport::default();
        let mut file_relations = FileRelationsMemoryReport::default();
        for snapshot in snapshots {
            let mut components = snapshot.name_index_components.clone();
            if components.bytes == 0 && snapshot.name_table_bytes > 0 {
                components.fixed_overhead_bytes =
                    u64::try_from(snapshot.name_table_bytes).unwrap_or(u64::MAX);
            }
            components.recompute_bytes();
            name_index.components.saturating_add_assign(&components);
            name_index.entry_count = name_index
                .entry_count
                .saturating_add(snapshot.name_entry_count as u64);
            name_index.base_segment_bytes = name_index
                .base_segment_bytes
                .saturating_add(snapshot.base_segment_bytes as u64);
            name_index.delta_segments_bytes = name_index
                .delta_segments_bytes
                .saturating_add(snapshot.delta_segments_bytes as u64);
            name_index.delta_segment_count = name_index
                .delta_segment_count
                .saturating_add(snapshot.delta_segment_count as u64);
            name_index.fallback_table_bytes = name_index
                .fallback_table_bytes
                .saturating_add(snapshot.fallback_table_bytes as u64);

            file_relations.reach_graph_bytes = file_relations
                .reach_graph_bytes
                .saturating_add(snapshot.reach_graph_bytes as u64);
            file_relations.include_edge_count = file_relations
                .include_edge_count
                .saturating_add(snapshot.include_edge_count as u64);
            file_relations.include_table_bytes = file_relations
                .include_table_bytes
                .saturating_add(snapshot.include_table_bytes as u64);
            file_relations.go_import_table_bytes = file_relations
                .go_import_table_bytes
                .saturating_add(snapshot.go_import_table_bytes as u64);
            file_relations.indexed_files_bytes = file_relations
                .indexed_files_bytes
                .saturating_add(snapshot.indexed_files_bytes as u64);
            file_relations.file_count = file_relations
                .file_count
                .saturating_add(snapshot.file_count as u64);
            file_relations.project_context_bytes = file_relations
                .project_context_bytes
                .saturating_add(snapshot.project_context_bytes as u64);
        }
        name_index.components.recompute_bytes();
        name_index.bytes = name_index
            .components
            .bytes
            .saturating_add(name_index.fallback_table_bytes);
        file_relations.bytes = file_relations
            .reach_graph_bytes
            .saturating_add(file_relations.include_table_bytes)
            .saturating_add(file_relations.go_import_table_bytes)
            .saturating_add(file_relations.indexed_files_bytes)
            .saturating_add(file_relations.project_context_bytes);

        let mut declaration_cache = DeclarationCacheMemoryReport::default();
        for sample in declaration_caches {
            declaration_cache.bytes = declaration_cache
                .bytes
                .saturating_add(sample.stats.bytes as u64);
            declaration_cache.entry_count = declaration_cache
                .entry_count
                .saturating_add(sample.stats.entries as u64);
            declaration_cache.budget_bytes = declaration_cache
                .budget_bytes
                .saturating_add(sample.budget_bytes as u64);
            declaration_cache.hits = declaration_cache.hits.saturating_add(sample.stats.hits);
            declaration_cache.misses = declaration_cache.misses.saturating_add(sample.stats.misses);
            declaration_cache.evictions = declaration_cache
                .evictions
                .saturating_add(sample.stats.evictions);
            declaration_cache.sql_reads = declaration_cache
                .sql_reads
                .saturating_add(sample.stats.sql_reads);
        }

        let attributed_bytes = name_index
            .bytes
            .saturating_add(declaration_cache.bytes)
            .saturating_add(file_relations.bytes)
            .saturating_add(open_documents.bytes);
        let process = ProcessMemoryReport {
            total_bytes: process_total_bytes,
            attributed_bytes,
            other_bytes: process_total_bytes.saturating_sub(attributed_bytes),
        };

        Self {
            process,
            name_index,
            declaration_cache,
            file_relations,
            open_documents,
            index_disk_bytes,
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> SnapshotMemoryReport {
        SnapshotMemoryReport {
            name_table_bytes: 1_000,
            name_entry_count: 42,
            name_index_components: NameIndexMemoryComponents::default(),
            base_segment_bytes: 700,
            delta_segments_bytes: 100,
            delta_segment_count: 2,
            fallback_table_bytes: 50,
            reach_graph_bytes: 300,
            include_edge_count: 9,
            include_table_bytes: 80,
            go_import_table_bytes: 20,
            indexed_files_bytes: 60,
            file_count: 7,
            project_context_bytes: 10,
        }
    }

    #[test]
    fn assemble_sums_categories_and_derives_other_bytes() {
        let report = MemoryReport::assemble(
            &[sample_snapshot()],
            &[DeclarationCacheSample {
                stats: DeclarationPayloadCacheStats {
                    hits: 5,
                    misses: 3,
                    sql_reads: 2,
                    evictions: 1,
                    bytes: 400,
                    entries: 11,
                    ..DeclarationPayloadCacheStats::default()
                },
                budget_bytes: 8_000,
            }],
            OpenDocumentsMemoryReport {
                bytes: 90,
                document_count: 2,
                overlay_bytes: 30,
            },
            10_000,
            777,
            1_234_567,
        );

        assert_eq!(report.name_index.bytes, 1_050);
        assert_eq!(report.name_index.entry_count, 42);
        assert_eq!(report.name_index.base_segment_bytes, 700);
        assert_eq!(report.name_index.delta_segments_bytes, 100);
        assert_eq!(report.name_index.delta_segment_count, 2);
        assert_eq!(report.name_index.fallback_table_bytes, 50);

        assert_eq!(report.declaration_cache.bytes, 400);
        assert_eq!(report.declaration_cache.entry_count, 11);
        assert_eq!(report.declaration_cache.budget_bytes, 8_000);
        assert_eq!(report.declaration_cache.hits, 5);
        assert_eq!(report.declaration_cache.misses, 3);
        assert_eq!(report.declaration_cache.evictions, 1);
        assert_eq!(report.declaration_cache.sql_reads, 2);

        assert_eq!(report.file_relations.bytes, 300 + 80 + 20 + 60 + 10);
        assert_eq!(report.file_relations.include_edge_count, 9);
        assert_eq!(report.file_relations.file_count, 7);

        let expected_attributed = 1_050 + 400 + (300 + 80 + 20 + 60 + 10) + 90;
        assert_eq!(report.process.attributed_bytes, expected_attributed);
        assert_eq!(report.process.total_bytes, 10_000);
        assert_eq!(report.process.other_bytes, 10_000 - expected_attributed);
        assert_eq!(report.index_disk_bytes, 777);
        assert_eq!(report.timestamp, 1_234_567);
    }

    #[test]
    fn assemble_keeps_name_components_separate_from_fallback_and_saturates_each_field() {
        let report = MemoryReport::assemble(
            &[sample_snapshot(), sample_snapshot()],
            &[],
            OpenDocumentsMemoryReport::default(),
            100_000,
            0,
            0,
        );

        assert_eq!(report.name_index.components.bytes, 2_000);
        assert_eq!(report.name_index.bytes, 2_100);
        assert_eq!(
            report.name_index.bytes,
            report
                .name_index
                .components
                .bytes
                .saturating_add(report.name_index.fallback_table_bytes),
        );

        let saturated = SnapshotMemoryReport {
            name_table_bytes: usize::MAX,
            fallback_table_bytes: usize::MAX,
            ..SnapshotMemoryReport::default()
        };
        let report = MemoryReport::assemble(
            &[saturated.clone(), saturated],
            &[],
            OpenDocumentsMemoryReport::default(),
            u64::MAX,
            0,
            0,
        );
        assert_eq!(report.name_index.components.bytes, u64::MAX);
        assert_eq!(report.name_index.bytes, u64::MAX);
        assert_eq!(report.process.other_bytes, 0);
    }

    #[test]
    fn assemble_saturates_other_bytes_when_estimates_exceed_process_total() {
        let report = MemoryReport::assemble(
            &[sample_snapshot()],
            &[],
            OpenDocumentsMemoryReport::default(),
            10,
            0,
            0,
        );
        assert_eq!(report.process.other_bytes, 0);
        assert!(report.process.attributed_bytes > report.process.total_bytes);
    }

    #[test]
    fn assemble_merges_multiple_workspace_roots() {
        let report = MemoryReport::assemble(
            &[sample_snapshot(), sample_snapshot()],
            &[],
            OpenDocumentsMemoryReport::default(),
            100_000,
            0,
            0,
        );
        assert_eq!(report.name_index.entry_count, 84);
        assert_eq!(report.file_relations.file_count, 14);
        assert_eq!(report.name_index.bytes, 2_100);
    }

    #[test]
    fn memory_report_serializes_with_camel_case_fields() {
        let report = MemoryReport::assemble(
            &[sample_snapshot()],
            &[],
            OpenDocumentsMemoryReport::default(),
            10_000,
            1,
            2,
        );
        let json = serde_json::to_string(&report).expect("serialize memory report");
        for field in [
            "\"totalBytes\"",
            "\"attributedBytes\"",
            "\"otherBytes\"",
            "\"nameIndex\"",
            "\"components\"",
            "\"declarationEntryBytes\"",
            "\"sharedNameBytes\"",
            "\"fixedOverheadBytes\"",
            "\"baseSegmentBytes\"",
            "\"deltaSegmentCount\"",
            "\"fallbackTableBytes\"",
            "\"declarationCache\"",
            "\"budgetBytes\"",
            "\"sqlReads\"",
            "\"fileRelations\"",
            "\"reachGraphBytes\"",
            "\"includeEdgeCount\"",
            "\"indexedFilesBytes\"",
            "\"projectContextBytes\"",
            "\"openDocuments\"",
            "\"documentCount\"",
            "\"overlayBytes\"",
            "\"indexDiskBytes\"",
        ] {
            assert!(json.contains(field), "report JSON must contain {field}");
        }
    }

    #[test]
    fn estimation_helpers_grow_with_capacity() {
        assert_eq!(hash_table_bytes::<u8, u8>(0), 0);
        assert_eq!(vec_bytes::<u8>(0), 0);
        assert!(hash_table_bytes::<String, Vec<u8>>(16) > 0);
        assert!(vec_bytes::<String>(16) >= 16 * size_of::<String>());
    }
}
