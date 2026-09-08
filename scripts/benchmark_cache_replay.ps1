[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Database,
    [Parameter(Mandatory = $true)][string]$Workspace
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
$databasePath = [System.IO.Path]::GetFullPath($Database)
$workspacePath = [System.IO.Path]::GetFullPath($Workspace)
if (-not (Test-Path -LiteralPath $databasePath -PathType Leaf)) { throw 'Cache replay database does not exist' }
if (-not (Test-Path -LiteralPath $workspacePath -PathType Container)) { throw 'Cache replay workspace does not exist' }
$previousDatabase = $env:FOSSILSENSE_BENCH_DB
$previousWorkspace = $env:FOSSILSENSE_BENCH_ROOT
try {
    $env:FOSSILSENSE_BENCH_DB = $databasePath
    $env:FOSSILSENSE_BENCH_ROOT = $workspacePath
    Push-Location (Join-Path $PSScriptRoot '..')
    try {
        $savedErrorAction = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            $raw = @(& cargo test --release -p fossilsense --bin fossilsense 'server::tests::cache_replay::benchmark_uboot_declaration_cache_replay' -- --ignored --exact --nocapture 2>&1 | ForEach-Object { $_.ToString() })
            $cargoExit = $LASTEXITCODE
        } finally { $ErrorActionPreference = $savedErrorAction }
    } finally { Pop-Location }
    Assert-ReplayProcessSuccess -ExitCode $cargoExit -Output $raw
    $allowed = @(Get-CacheReplayMetricNames)
    $metrics = [ordered]@{}
    foreach ($line in $raw) {
        if ($line -match '^([a-z][a-z0-9_]+):\s+([0-9]+)$' -and $Matches[1] -in $allowed) { $metrics[$Matches[1]] = [long]$Matches[2] }
    }
    Assert-CacheReplayGate -Metrics $metrics
    foreach ($key in $metrics.Keys) { "${key}: $($metrics[$key])" }
} finally {
    $env:FOSSILSENSE_BENCH_DB = $previousDatabase
    $env:FOSSILSENSE_BENCH_ROOT = $previousWorkspace
}
