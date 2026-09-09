$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
$benchmarkScript = Join-Path $PSScriptRoot 'benchmark_large_workspace.ps1'
# v1.7.1 acceptance may observe one-time build duration while preserving all
# completion, memory, correctness and process-success gates.
Assert-FullIndexPerformanceGate -CaseId 'wine-full-index' `
    -OuterElapsedMs 150000 -EngineElapsedMs 146737 -ObserveOnly
# Default timing policy observes duration; execution failure remains separate.
Assert-FullIndexPerformanceGate -CaseId 'wine-full-index' -OuterElapsedMs 150000 -EngineElapsedMs 146737
foreach ($elapsed in @(@(120001,120000), @(120000,120001))) {
    $rejected = $false
    try { Assert-FullIndexPerformanceGate -CaseId 'wine-full-index' -OuterElapsedMs $elapsed[0] -EngineElapsedMs $elapsed[1] -ObserveOnly:$false }
    catch { $rejected = $true }
    if (-not $rejected) { throw 'Explicit historical timing policy was not enforced.' }
}
. (Join-Path $PSScriptRoot 'fixtures/lifecycle_metrics.ps1')
$validLifecycleMetrics = New-LifecycleMetricsFixture
$validLifecycleMetrics.lsp_lifecycle_elapsed_ms = 150000
$validLifecycleMetrics.lsp_lifecycle_rebuild_wall_ms = 160000
Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $validLifecycleMetrics -ObserveFullIndexTime -AllowTransientMemoryPeak:$false
foreach ($failure in @(@('lsp_lifecycle_peak_process_bytes',536870913), @('lsp_lifecycle_completion_p95_us',50001), @('lsp_lifecycle_generation_mismatches',1))) {
    $invalid = $validLifecycleMetrics.Clone()
    $invalid[$failure[0]] = $failure[1]
    $rejected = $false
    try { Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $invalid -ObserveFullIndexTime -AllowTransientMemoryPeak:$false } catch { $rejected = $true }
    if (-not $rejected) { throw "Observation must not weaken $($failure[0])" }
}
Write-Host 'Lifecycle timing observation preserves correctness, completion and memory gates.'
