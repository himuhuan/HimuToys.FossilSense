[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Database,
    [Parameter(Mandatory = $true)]
    [string]$Workspace,
    [Parameter(Mandatory = $true)]
    [ValidateSet('u-boot', 'wine')]
    [string]$Sample
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$databasePath = [System.IO.Path]::GetFullPath($Database)
$workspacePath = [System.IO.Path]::GetFullPath($Workspace)
if (-not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
    throw "LSP lifecycle database not found: $databasePath"
}
if (-not (Test-Path -LiteralPath $workspacePath -PathType Container)) {
    throw "LSP lifecycle workspace not found: $workspacePath"
}

$allowedMetrics = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
@(
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
    'lsp_lifecycle_write_ms'
) | ForEach-Object { [void]$allowedMetrics.Add($_) }

$previousDatabase = $env:FOSSILSENSE_BENCH_DB
$previousWorkspace = $env:FOSSILSENSE_BENCH_ROOT
$previousSample = $env:FOSSILSENSE_BENCH_SAMPLE
try {
    $env:FOSSILSENSE_BENCH_DB = $databasePath
    $env:FOSSILSENSE_BENCH_ROOT = $workspacePath
    $env:FOSSILSENSE_BENCH_SAMPLE = $Sample
    Push-Location $repoRoot
    try {
        $savedErrorAction = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $rawOutput = @(
                & cargo test --release -p fossilsense --bin fossilsense `
                    'server::tests::benchmark_lsp_index_lifecycle_gate' -- `
                    --ignored --exact --nocapture 2>&1 |
                    ForEach-Object { $_.ToString() }
            )
            $cargoExit = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $savedErrorAction
        }
    } finally {
        Pop-Location
    }
    if ($cargoExit -ne 0) {
        $tail = @($rawOutput | Select-Object -Last 24) -join [Environment]::NewLine
        throw "LSP lifecycle gate failed (cargo exit $cargoExit):$([Environment]::NewLine)$tail"
    }

    $metrics = [ordered]@{}
    foreach ($line in $rawOutput) {
        if ($line -match '^([a-z][a-z0-9_]+):\s+([0-9]+)$' -and
            $allowedMetrics.Contains($Matches[1])) {
            if ($metrics.Contains($Matches[1])) {
                throw "LSP lifecycle gate emitted duplicate metric $($Matches[1])"
            }
            $metrics[$Matches[1]] = [long]$Matches[2]
        }
    }
    Assert-LspLifecycleGate -CaseId "$Sample-lsp-lifecycle" -Metrics $metrics
    foreach ($name in $allowedMetrics) {
        Write-Output "${name}: $($metrics[$name])"
    }
} finally {
    $env:FOSSILSENSE_BENCH_DB = $previousDatabase
    $env:FOSSILSENSE_BENCH_ROOT = $previousWorkspace
    $env:FOSSILSENSE_BENCH_SAMPLE = $previousSample
}
