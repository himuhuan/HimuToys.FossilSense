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

$allowedMetrics = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
@(
    'engine_hydration_declarations',
    'engine_hydration_files',
    'engine_hydration_recall_bytes',
    'engine_hydration_memory_before_bytes',
    'engine_hydration_single_private_bytes',
    'engine_hydration_single_peak_private_bytes',
    'engine_hydration_two_generation_private_bytes',
    'engine_hydration_peak_private_bytes',
    'engine_hydration_first_build_ms',
    'engine_hydration_second_build_ms'
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
                    'server::indexing::cache::memory_tests::uboot_engine_hydration_stays_below_private_memory_gate' -- `
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
        throw "engine hydration memory gate failed (cargo exit $cargoExit):$([Environment]::NewLine)$tail"
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
        throw "engine hydration gate emitted $emitted of $($allowedMetrics.Count) required metrics"
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
