$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
$metrics = @{}
foreach ($name in @(Get-CacheReplayMetricNames)) { $metrics[$name] = 1 }
$metrics.cache_replay_declarations = 500000
$metrics.cache_replay_files = 10000
foreach ($phase in @('cold','warm','boundary','eviction')) {
    foreach ($feature in @('hover','definition')) {
        $prefix = "cache_${phase}_${feature}"
        $metrics["${prefix}_requests"] = 64
        $metrics["${prefix}_correct_targets"] = 64
        $metrics["${prefix}_bytes"] = 1024
        $metrics["${prefix}_budget_bytes"] = 2048
    }
}
Assert-CacheReplayGate -Metrics $metrics
foreach ($name in @('cache_cold_hover_correct_targets','cache_warm_definition_requests','cache_eviction_hover_evictions')) {
    $invalid = $metrics.Clone(); $invalid[$name] = 0
    $rejected = $false
    try { Assert-CacheReplayGate -Metrics $invalid } catch { $rejected = $true }
    if (-not $rejected) { throw "invalid replay accepted: $name" }
}
$missing = $metrics.Clone(); $missing.Remove('cache_boundary_definition_lock_wait_ns')
$rejected = $false
try { Assert-CacheReplayGate -Metrics $missing } catch { $rejected = $true }
if (-not $rejected) { throw 'missing required metric accepted' }
'cache replay contract tests passed'
