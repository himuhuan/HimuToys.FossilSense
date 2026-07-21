use crate::progress::DegradedCapabilities;

pub(in crate::server) fn ready_cache_message(
    prefix: &str,
    declaration_count: usize,
    include_count: usize,
    ref_file_count: usize,
    name_table_ms: u128,
    reach_graph_ms: u128,
    degraded: &DegradedCapabilities,
) -> String {
    let mut message = format!(
        "{prefix}: {declaration_count} declarations, include table={include_count} paths, reference files={ref_file_count} (name_table={name_table_ms}ms, reach_graph={reach_graph_ms}ms)"
    );
    if degraded.any() {
        message.push_str("; degraded=");
        message.push_str(&degraded.labels().join(","));
    }
    message
}
