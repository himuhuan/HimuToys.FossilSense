[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet(
        'v142-high-duplication-callable-query',
        'v142-counterpart-scan-cap',
        'v142-large-record-range-hydration',
        'v142-multi-dirty-overlay-merge',
        'v142-signature-help-retrigger',
        'v142-concurrent-publication-hover-definition'
    )]
    [string]$Case,
    [string]$BenchmarkRoot = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if ([string]::IsNullOrWhiteSpace($BenchmarkRoot)) {
    $BenchmarkRoot = Join-Path $repoRoot 'target\benchmark'
}
$benchmarkPath = [System.IO.Path]::GetFullPath($BenchmarkRoot)
New-Item -ItemType Directory -Force -Path $benchmarkPath | Out-Null

$allowedMetrics = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
@(
    'callable_query_us',
    'candidate_rows_scanned',
    'candidate_rows_filtered',
    'candidate_rows_grouped',
    'candidate_rows_returned',
    'candidate_raw',
    'candidate_filtered',
    'candidate_grouped',
    'candidate_returned',
    'candidate_query_truncated',
    'arity_compatible',
    'arity_unknown',
    'arity_incompatible',
    'arity_mismatch_fallback',
    'counterpart_graph_us',
    'counterpart_edges',
    'counterpart_groups',
    'counterpart_strict',
    'counterpart_ambiguous',
    'counterpart_incomplete',
    'candidate_scan_cap',
    'candidate_scan_observed',
    'reach_nodes_visited',
    'hydration_us',
    'hydration_count',
    'hydration_bytes',
    'hydration_sections',
    'hydration_file_bytes',
    'hydration_requested_bytes',
    'hydration_revision_rejections',
    'overlay_merge_us',
    'overlay_parse_us',
    'overlay_documents',
    'signature_help_requests',
    'signature_help_p50_us',
    'signature_help_p95_us',
    'concurrent_query_p50_us',
    'concurrent_query_p95_us',
    'publication_conflicts',
    'generation_mismatches',
    'query_us',
    'reach_us',
    'coverage_scanned',
    'coverage_open',
    'coverage_truncated',
    'fallback_used'
) | ForEach-Object { [void]$allowedMetrics.Add($_) }

$previousCase = [Environment]::GetEnvironmentVariable(
    'FOSSILSENSE_V142_BENCH_CASE',
    [EnvironmentVariableTarget]::Process
)
$previousRoot = [Environment]::GetEnvironmentVariable(
    'FOSSILSENSE_V142_BENCHMARK_ROOT',
    [EnvironmentVariableTarget]::Process
)
try {
    $env:FOSSILSENSE_V142_BENCH_CASE = $Case
    $env:FOSSILSENSE_V142_BENCHMARK_ROOT = $benchmarkPath
    Push-Location $repoRoot
    try {
        $savedErrorAction = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $rawOutput = @(
                & cargo test -p fossilsense --release --bin fossilsense `
                    'semantic_benchmark::benchmark_v142_case' -- `
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
        throw "v1.4.2 semantic benchmark failed for $Case (cargo exit $cargoExit)."
    }

    $emitted = 0
    foreach ($line in $rawOutput) {
        if ($line -match '^([a-z][a-z0-9_]+):\s+([0-9]+)$' -and
            $allowedMetrics.Contains($Matches[1])) {
            Write-Output "$($Matches[1]): $($Matches[2])"
            $emitted += 1
        }
    }
    if ($emitted -ne $allowedMetrics.Count) {
        throw "semantic benchmark emitted $emitted of $($allowedMetrics.Count) required metrics"
    }
} finally {
    [Environment]::SetEnvironmentVariable(
        'FOSSILSENSE_V142_BENCH_CASE',
        $previousCase,
        [EnvironmentVariableTarget]::Process
    )
    [Environment]::SetEnvironmentVariable(
        'FOSSILSENSE_V142_BENCHMARK_ROOT',
        $previousRoot,
        [EnvironmentVariableTarget]::Process
    )
}
