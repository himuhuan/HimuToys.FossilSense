$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
. (Join-Path $PSScriptRoot 'fixtures/lifecycle_metrics.ps1')
$validLifecycleMetrics = New-LifecycleMetricsFixture
$validLifecycleMetrics.lsp_lifecycle_peak_process_bytes = 550000000
$validLifecycleMetrics.lsp_lifecycle_stable_max_bytes = 400000000
$validLifecycleMetrics.lsp_lifecycle_stable_samples = 101
$validLifecycleMetrics.lsp_lifecycle_stable_window_ms = 10000
$validLifecycleMetrics.lsp_lifecycle_above_limit_ms = 300
$validLifecycleMetrics.lsp_lifecycle_longest_above_limit_ms = 300
Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $validLifecycleMetrics -AllowTransientMemoryPeak
foreach ($failure in @(@('lsp_lifecycle_above_limit_ms',-1), @('lsp_lifecycle_peak_process_bytes',805306369), @('lsp_lifecycle_above_limit_ms',30001), @('lsp_lifecycle_stable_max_bytes',536870913), @('lsp_lifecycle_stable_samples',99), @('lsp_lifecycle_stable_window_ms',9999), @('lsp_lifecycle_longest_above_limit_ms',10001))) {
    $invalid = $validLifecycleMetrics.Clone()
    $invalid[$failure[0]] = $failure[1]
    $rejected = $false
    try { Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $invalid -AllowTransientMemoryPeak } catch { $rejected = $true }
    if (-not $rejected) { throw "Stability gate accepted invalid $($failure[0])" }
}
Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $validLifecycleMetrics
$strictRejected = $false
try { Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $validLifecycleMetrics -AllowTransientMemoryPeak:$false } catch { $strictRejected = $true }
if (-not $strictRejected) { throw 'Default strict peak gate changed.' }
Write-Host 'Transient peak policy requires stable memory and a bounded excursion.'
