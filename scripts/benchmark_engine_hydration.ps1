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
    throw "engine hydration database not found: $databasePath"
}
if (-not (Test-Path -LiteralPath $workspacePath -PathType Container)) {
    throw "engine hydration workspace not found: $workspacePath"
}

$coldMetrics = @(
    'engine_hydration_declarations',
    'engine_hydration_files',
    'engine_hydration_recall_bytes',
    'engine_hydration_memory_before_bytes',
    'engine_hydration_single_private_bytes',
    'engine_hydration_single_peak_private_bytes',
    'engine_hydration_two_generation_private_bytes',
    'engine_hydration_peak_private_bytes',
    'engine_hydration_first_build_ms',
    'engine_hydration_second_build_ms',
    'engine_hydration_first_name_strings_bytes',
    'engine_hydration_first_name_paths_projects_bytes',
    'engine_hydration_first_name_postings_bytes',
    'engine_hydration_first_name_fixed_bytes',
    'engine_hydration_second_name_strings_bytes',
    'engine_hydration_second_name_paths_projects_bytes',
    'engine_hydration_second_name_postings_bytes',
    'engine_hydration_second_name_fixed_bytes',
    'engine_hydration_first_file_relations_bytes',
    'engine_hydration_second_file_relations_bytes',
    'engine_hydration_second_generation_incremental_bytes'
)
$warmMetrics = @(
    'warm_publication_declarations',
    'warm_publication_files',
    'warm_publication_payload_budget_bytes',
    'warm_publication_effective_budget_before_bytes',
    'warm_publication_effective_budget_after_bytes',
    'warm_publication_target_bytes',
    'warm_publication_cache_bytes',
    'warm_publication_cache_entries',
    'warm_publication_cache_hits',
    'warm_publication_cache_misses',
    'warm_publication_cache_sql_reads',
    'warm_publication_cache_evictions',
    'warm_publication_shrink_entries',
    'warm_publication_shrink_bytes',
    'warm_publication_single_private_bytes',
    'warm_publication_single_peak_private_bytes',
    'warm_publication_peak_private_bytes',
    'warm_publication_two_generation_private_bytes',
    'warm_publication_second_generation_incremental_bytes',
    'warm_publication_first_build_ms',
    'warm_publication_second_build_ms',
    'warm_publication_first_name_strings_bytes',
    'warm_publication_first_name_paths_projects_bytes',
    'warm_publication_first_name_postings_bytes',
    'warm_publication_first_name_fixed_bytes',
    'warm_publication_second_name_strings_bytes',
    'warm_publication_second_name_paths_projects_bytes',
    'warm_publication_second_name_postings_bytes',
    'warm_publication_second_name_fixed_bytes',
    'warm_publication_first_file_relations_bytes',
    'warm_publication_second_file_relations_bytes',
    'warm_publication_old_epoch_consistent',
    'warm_publication_old_generation_consistent'
)

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
        $testCases = @(
            @{
                Name = 'server::indexing::cache::memory_tests::uboot_engine_hydration_stays_below_private_memory_gate'
                Metrics = $coldMetrics
            },
            @{
                Name = 'server::indexing::cache::memory_tests::uboot_warm_generation_publication_stays_below_private_memory_gate'
                Metrics = $warmMetrics
            }
        )
        foreach ($testCase in $testCases) {
            $savedErrorAction = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                $rawOutput = @(
                    & cargo test --release -p fossilsense --bin fossilsense $testCase.Name -- `
                        --ignored --exact --nocapture 2>&1 |
                        ForEach-Object { $_.ToString() }
                )
                $cargoExit = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = $savedErrorAction
            }
            if ($cargoExit -ne 0) {
                $tail = @($rawOutput | Select-Object -Last 24) -join [Environment]::NewLine
                throw "engine hydration gate $($testCase.Name) failed (cargo exit $cargoExit):$([Environment]::NewLine)$tail"
            }

            $required = [System.Collections.Generic.HashSet[string]]::new(
                [System.StringComparer]::Ordinal
            )
            $testCase.Metrics | ForEach-Object { [void]$required.Add($_) }
            $seen = @{}
            foreach ($line in $rawOutput) {
                if ($line -match '^([a-z][a-z0-9_]+):\s+([0-9]+)$') {
                    $name = $Matches[1]
                    if ($required.Contains($name)) {
                        if ($seen.ContainsKey($name)) {
                            throw "engine hydration gate $($testCase.Name) emitted duplicate metric: $name"
                        }
                        $seen[$name] = $Matches[2]
                    }
                }
            }
            foreach ($name in $required) {
                if (-not $seen.ContainsKey($name)) {
                    throw "engine hydration gate $($testCase.Name) omitted required metric: $name"
                }
                Write-Output "${name}: $($seen[$name])"
            }
        }
    } finally {
        Pop-Location
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
