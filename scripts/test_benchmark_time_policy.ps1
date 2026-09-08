$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
$benchmarkScript = Join-Path $PSScriptRoot 'benchmark_large_workspace.ps1'
# v1.7.1 acceptance may observe one-time build duration while preserving all
# completion, memory, correctness and process-success gates.
Assert-FullIndexPerformanceGate -CaseId 'wine-full-index' `
    -OuterElapsedMs 150000 -EngineElapsedMs 146737 -ObserveOnly
$benchmarkSource = Get-Content -LiteralPath $benchmarkScript -Raw
if ($benchmarkSource -notmatch 'ObserveFullIndexTime' -or
    $benchmarkSource -notmatch 'full_index_time_policy') {
    throw 'Observed build timing must be selectable and recorded in benchmark evidence.'
}

foreach ($elapsed in @(@(120001,120000), @(120000,120001))) {
    $rejected = $false
    try { Assert-FullIndexPerformanceGate -CaseId 'wine-full-index' -OuterElapsedMs $elapsed[0] -EngineElapsedMs $elapsed[1] }
    catch { $rejected = $true }
    if (-not $rejected) { throw 'Default timing gate was weakened.' }
}
Write-Host 'Full-index timing policy tests passed.'
