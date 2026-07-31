[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Database,
    [Parameter(Mandatory = $true)]
    [string]$Workspace
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$databasePath = [System.IO.Path]::GetFullPath($Database)
$workspacePath = [System.IO.Path]::GetFullPath($Workspace)
if (-not (Test-Path -LiteralPath $databasePath -PathType Leaf)) {
    throw "completion replay database not found: $databasePath"
}
if (-not (Test-Path -LiteralPath $workspacePath -PathType Container)) {
    throw "completion replay workspace not found: $workspacePath"
}

$allowedMetrics = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
@(
    'completion_lsp_replay_declarations',
    'completion_lsp_replay_requests',
    'completion_lsp_replay_p50_us',
    'completion_lsp_replay_p95_us',
    'completion_lsp_replay_max_us',
    'completion_lsp_replay_p95_limit_us',
    'completion_lsp_replay_context_p95_ms',
    'completion_lsp_replay_parse_p95_ms',
    'completion_lsp_replay_local_words_p95_ms',
    'completion_lsp_replay_overlay_p95_ms',
    'completion_lsp_replay_worker_p95_ms',
    'completion_lsp_replay_render_p95_ms',
    'completion_lsp_replay_indexed_returned_min',
    'completion_lsp_replay_active_entries_min',
    'completion_lsp_replay_candidate_budget_min',
    'completion_lsp_replay_candidate_budget_max',
    'completion_lsp_replay_truncated_requests',
    'completion_lsp_replay_entries_inspected_min',
    'completion_lsp_replay_entries_inspected_max',
    'completion_lsp_replay_priority_source_probes_max',
    'completion_lsp_replay_priority_source_attempts_max',
    'completion_lsp_replay_priority_sources_initialized_max',
    'completion_lsp_replay_priority_fuzzy_name_probes_max',
    'completion_lsp_replay_priority_fuzzy_declaration_probes_max',
    'completion_lsp_replay_sql_reads'
) | ForEach-Object { [void]$allowedMetrics.Add($_) }

$previousDatabase = [Environment]::GetEnvironmentVariable(
    'FOSSILSENSE_BENCH_DB',
    [EnvironmentVariableTarget]::Process
)
$previousWorkspace = [Environment]::GetEnvironmentVariable(
    'FOSSILSENSE_BENCH_ROOT',
    [EnvironmentVariableTarget]::Process
)
try {
    $env:FOSSILSENSE_BENCH_DB = $databasePath
    $env:FOSSILSENSE_BENCH_ROOT = $workspacePath
    Push-Location $repoRoot
    try {
        $savedErrorAction = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $rawOutput = @(
                & cargo test --release -p fossilsense --bin fossilsense `
                    'server::tests::benchmark_uboot_lsp_completion_replay_stays_within_latency_and_sql_gates' -- `
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
        throw "completion replay gate failed (cargo exit $cargoExit):$([Environment]::NewLine)$tail"
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
        throw "completion replay gate emitted $emitted of $($allowedMetrics.Count) required metrics"
    }
} finally {
    [Environment]::SetEnvironmentVariable(
        'FOSSILSENSE_BENCH_DB',
        $previousDatabase,
        [EnvironmentVariableTarget]::Process
    )
    [Environment]::SetEnvironmentVariable(
        'FOSSILSENSE_BENCH_ROOT',
        $previousWorkspace,
        [EnvironmentVariableTarget]::Process
    )
}
