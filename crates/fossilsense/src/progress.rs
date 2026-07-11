use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: IndexState,
    pub workspace: String,
    pub phase: Option<String>,
    pub processed_files: usize,
    pub total_files: usize,
    pub indexed_files: usize,
    pub skipped_files: usize,
    pub symbols: usize,
    pub semantic_generation: u64,
    pub elapsed_ms: u128,
    pub discover_ms: u128,
    pub parse_ms: u128,
    pub write_ms: u128,
    pub check_ms: u128,
    pub include_edge_ms: u128,
    pub name_table_ms: u128,
    pub reach_graph_ms: u128,
    pub degraded_capabilities: DegradedCapabilities,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexState {
    Indexing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DegradedCapabilities {
    pub reach_graph: bool,
    pub include_table: bool,
    pub reference_file_list: bool,
    pub project_context: bool,
}

impl DegradedCapabilities {
    pub fn any(&self) -> bool {
        self.reach_graph || self.include_table || self.reference_file_list || self.project_context
    }

    pub fn labels(&self) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if self.reach_graph {
            labels.push("reachGraph");
        }
        if self.include_table {
            labels.push("includeTable");
        }
        if self.reference_file_list {
            labels.push("referenceFileList");
        }
        if self.project_context {
            labels.push("projectContext");
        }
        labels
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub total_files: usize,
    pub processed_files: usize,
    pub indexed_files: usize,
    pub skipped_files: usize,
    pub deleted_files: usize,
    pub symbols: usize,
    pub semantic_generation: u64,
    pub elapsed_ms: u128,
    pub discover_ms: u128,
    pub parse_ms: u128,
    pub write_ms: u128,
    pub check_ms: u128,
    pub include_edge_ms: u128,
    /// Source files whose include edges were rebuilt during a dirty update.
    /// Internal refresh input, not serialized.
    pub include_edge_sources_rebuilt: Vec<String>,
    pub name_table_ms: u128,
    pub reach_graph_ms: u128,
}

impl IndexStatus {
    pub fn indexing_phase(workspace: String, stats: &IndexStats, phase: impl Into<String>) -> Self {
        Self {
            state: IndexState::Indexing,
            workspace,
            phase: Some(phase.into()),
            processed_files: stats.processed_files,
            total_files: stats.total_files,
            indexed_files: stats.indexed_files,
            skipped_files: stats.skipped_files,
            symbols: stats.symbols,
            semantic_generation: stats.semantic_generation,
            elapsed_ms: stats.elapsed_ms,
            discover_ms: stats.discover_ms,
            parse_ms: stats.parse_ms,
            write_ms: stats.write_ms,
            check_ms: stats.check_ms,
            include_edge_ms: stats.include_edge_ms,
            name_table_ms: stats.name_table_ms,
            reach_graph_ms: stats.reach_graph_ms,
            degraded_capabilities: DegradedCapabilities::default(),
            message: None,
        }
    }

    pub fn indexing_with_message(workspace: String, stats: &IndexStats, message: String) -> Self {
        Self {
            state: IndexState::Indexing,
            workspace,
            phase: None,
            processed_files: stats.processed_files,
            total_files: stats.total_files,
            indexed_files: stats.indexed_files,
            skipped_files: stats.skipped_files,
            symbols: stats.symbols,
            semantic_generation: stats.semantic_generation,
            elapsed_ms: stats.elapsed_ms,
            discover_ms: stats.discover_ms,
            parse_ms: stats.parse_ms,
            write_ms: stats.write_ms,
            check_ms: stats.check_ms,
            include_edge_ms: stats.include_edge_ms,
            name_table_ms: stats.name_table_ms,
            reach_graph_ms: stats.reach_graph_ms,
            degraded_capabilities: DegradedCapabilities::default(),
            message: Some(message),
        }
    }

    pub fn ready(workspace: String, stats: &IndexStats) -> Self {
        Self::ready_with_degraded(workspace, stats, DegradedCapabilities::default())
    }

    pub fn ready_with_degraded(
        workspace: String,
        stats: &IndexStats,
        degraded_capabilities: DegradedCapabilities,
    ) -> Self {
        Self {
            state: IndexState::Ready,
            workspace,
            phase: None,
            processed_files: stats.processed_files,
            total_files: stats.total_files,
            indexed_files: stats.indexed_files,
            skipped_files: stats.skipped_files,
            symbols: stats.symbols,
            semantic_generation: stats.semantic_generation,
            elapsed_ms: stats.elapsed_ms,
            discover_ms: stats.discover_ms,
            parse_ms: stats.parse_ms,
            write_ms: stats.write_ms,
            check_ms: stats.check_ms,
            include_edge_ms: stats.include_edge_ms,
            name_table_ms: stats.name_table_ms,
            reach_graph_ms: stats.reach_graph_ms,
            degraded_capabilities,
            message: None,
        }
    }

    pub fn failed(workspace: String, message: String) -> Self {
        Self {
            state: IndexState::Failed,
            workspace,
            phase: None,
            processed_files: 0,
            total_files: 0,
            indexed_files: 0,
            skipped_files: 0,
            symbols: 0,
            semantic_generation: 0,
            elapsed_ms: 0,
            discover_ms: 0,
            parse_ms: 0,
            write_ms: 0,
            check_ms: 0,
            include_edge_ms: 0,
            name_table_ms: 0,
            reach_graph_ms: 0,
            degraded_capabilities: DegradedCapabilities::default(),
            message: Some(message),
        }
    }
}
