function Assert-FullIndexPerformanceGate {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$CaseId,
        [Parameter(Mandatory = $true)][double]$OuterElapsedMs,
        [Parameter(Mandatory = $true)][double]$EngineElapsedMs
    )

    if ($CaseId -notlike '*-full-index') {
        return
    }
    $limitMs = 120000.0
    if ($OuterElapsedMs -gt $limitMs) {
        throw "$CaseId outer elapsed $OuterElapsedMs ms exceeded the 120,000 ms full-index gate"
    }
    if ($EngineElapsedMs -gt $limitMs) {
        throw "$CaseId engine elapsed $EngineElapsedMs ms exceeded the 120,000 ms full-index gate"
    }
}

function Assert-LspLifecycleGate {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$CaseId,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Metrics
    )

    if ($CaseId -notlike '*-lsp-lifecycle') {
        return
    }
    $required = @(
        'lsp_lifecycle_declarations',
        'lsp_lifecycle_files',
        'lsp_lifecycle_warm_cache_percent',
        'lsp_lifecycle_phases_seen_mask',
        'lsp_lifecycle_active_builds_peak',
        'lsp_lifecycle_reserved_bytes_peak',
        'lsp_lifecycle_retained_bytes_peak',
        'lsp_lifecycle_peak_process_bytes',
        'lsp_lifecycle_memory_sample_available',
        'lsp_lifecycle_memory_metric_private_bytes',
        'lsp_lifecycle_old_requests_held_peak',
        'lsp_lifecycle_old_epoch_consistent',
        'lsp_lifecycle_new_epoch_consistent',
        'lsp_lifecycle_generation_mismatches',
        'lsp_lifecycle_database_identity_mismatches',
        'lsp_lifecycle_cancelled_compactions',
        'lsp_lifecycle_hover_requests',
        'lsp_lifecycle_hover_p50_us',
        'lsp_lifecycle_hover_p95_us',
        'lsp_lifecycle_hover_max_us',
        'lsp_lifecycle_definition_requests',
        'lsp_lifecycle_definition_p50_us',
        'lsp_lifecycle_definition_p95_us',
        'lsp_lifecycle_definition_max_us',
        'lsp_lifecycle_completion_requests',
        'lsp_lifecycle_completion_candidates_min',
        'lsp_lifecycle_completion_p95_us',
        'lsp_lifecycle_completion_entries_inspected_min',
        'lsp_lifecycle_completion_entries_inspected_max',
        'lsp_lifecycle_completion_candidate_budget_min',
        'lsp_lifecycle_completion_candidate_budget_max',
        'lsp_lifecycle_completion_indexed_returned_min',
        'lsp_lifecycle_completion_active_entries_min',
        'lsp_lifecycle_completion_truncated_requests',
        'lsp_lifecycle_completion_sql_reads',
        'lsp_lifecycle_dirty_updates_applied',
        'lsp_lifecycle_final_active_builds',
        'lsp_lifecycle_final_reserved_bytes',
        'lsp_lifecycle_database_size_bytes',
        'lsp_lifecycle_elapsed_ms',
        'lsp_lifecycle_rebuild_wall_ms',
        'lsp_lifecycle_write_ms'
    )
    foreach ($name in $required) {
        if (-not $Metrics.Contains($name)) {
            throw "$CaseId lifecycle output is missing required metric $name"
        }
    }
    if ([long]$Metrics.lsp_lifecycle_warm_cache_percent -lt 75) {
        throw "$CaseId warmed less than 75 percent of the effective payload cache"
    }
    if ([long]$Metrics.lsp_lifecycle_phases_seen_mask -ne 31) {
        throw "$CaseId did not observe every required lifecycle phase"
    }
    if ([long]$Metrics.lsp_lifecycle_active_builds_peak -ne 1) {
        throw "$CaseId observed an invalid active heavy-build peak"
    }
    if ([long]$Metrics.lsp_lifecycle_reserved_bytes_peak -gt 268435456 -or
        [long]$Metrics.lsp_lifecycle_reserved_bytes_peak -le 0) {
        throw "$CaseId violated the 256 MiB temporary reservation budget"
    }
    if ([long]$Metrics.lsp_lifecycle_memory_sample_available -ne 1) {
        throw "$CaseId has no usable process memory sample"
    }
    if ([long]$Metrics.lsp_lifecycle_old_epoch_consistent -ne 1 -or
        [long]$Metrics.lsp_lifecycle_new_epoch_consistent -ne 1 -or
        [long]$Metrics.lsp_lifecycle_generation_mismatches -ne 0 -or
        [long]$Metrics.lsp_lifecycle_database_identity_mismatches -ne 0) {
        throw "$CaseId observed a mixed epoch, generation, or database identity"
    }
    if ([long]$Metrics.lsp_lifecycle_cancelled_compactions -lt 1) {
        throw "$CaseId did not cancel stale compaction work"
    }
    if ([long]$Metrics.lsp_lifecycle_hover_requests -lt 1 -or
        [long]$Metrics.lsp_lifecycle_definition_requests -lt 1 -or
        [long]$Metrics.lsp_lifecycle_completion_requests -ne 64) {
        throw "$CaseId did not exercise hover, definition, and completion requests"
    }
    if ([long]$Metrics.lsp_lifecycle_completion_candidates_min -lt 1) {
        throw "$CaseId observed an empty production completion response"
    }
    if ([long]$Metrics.lsp_lifecycle_completion_p95_us -gt 50000 -or
        [long]$Metrics.lsp_lifecycle_completion_entries_inspected_min -lt 1 -or
        [long]$Metrics.lsp_lifecycle_completion_entries_inspected_max -gt 16384 -or
        [long]$Metrics.lsp_lifecycle_completion_candidate_budget_min -ne 16384 -or
        [long]$Metrics.lsp_lifecycle_completion_candidate_budget_max -ne 16384 -or
        [long]$Metrics.lsp_lifecycle_completion_indexed_returned_min -lt 1 -or
        [long]$Metrics.lsp_lifecycle_completion_active_entries_min -lt 500000 -or
        [long]$Metrics.lsp_lifecycle_completion_truncated_requests -ne 64 -or
        [long]$Metrics.lsp_lifecycle_completion_sql_reads -ne 0) {
        throw "$CaseId violated the 64-request production completion gate"
    }
    if ([long]$Metrics.lsp_lifecycle_dirty_updates_applied -ne 3) {
        throw "$CaseId did not verify create, update-during-compaction, and delete"
    }
    if ([long]$Metrics.lsp_lifecycle_hover_p95_us -lt
        [long]$Metrics.lsp_lifecycle_hover_p50_us -or
        [long]$Metrics.lsp_lifecycle_hover_max_us -lt
        [long]$Metrics.lsp_lifecycle_hover_p95_us -or
        [long]$Metrics.lsp_lifecycle_definition_p95_us -lt
        [long]$Metrics.lsp_lifecycle_definition_p50_us -or
        [long]$Metrics.lsp_lifecycle_definition_max_us -lt
        [long]$Metrics.lsp_lifecycle_definition_p95_us) {
        throw "$CaseId emitted invalid hover or definition latency percentiles"
    }
    if ([long]$Metrics.lsp_lifecycle_final_active_builds -ne 0 -or
        [long]$Metrics.lsp_lifecycle_final_reserved_bytes -ne 0) {
        throw "$CaseId leaked a build permit or byte reservation"
    }
    if ([long]$Metrics.lsp_lifecycle_database_size_bytes -le 0) {
        throw "$CaseId did not record a database size"
    }
    if ([long]$Metrics.lsp_lifecycle_elapsed_ms -gt 120000 -or
        [long]$Metrics.lsp_lifecycle_rebuild_wall_ms -gt 120000) {
        throw "$CaseId full index elapsed time exceeded the 120,000 ms gate"
    }
    if ($CaseId -eq 'u-boot-lsp-lifecycle') {
        if ([long]$Metrics.lsp_lifecycle_declarations -lt 500000 -or
            [long]$Metrics.lsp_lifecycle_files -lt 10000) {
            throw "$CaseId sample is below the 500,000 declaration / 10,000 file gate"
        }
        if ([long]$Metrics.lsp_lifecycle_peak_process_bytes -gt 536870912) {
            throw "$CaseId exceeded the 512 MiB lifecycle memory gate"
        }
    }
}
