$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
$metrics = [ordered]@{}
foreach ($name in (Get-BindingReplayMetricNames)) {
    $metrics[$name] = if ($name -match '_(requests|correct_targets)$') { 64 } else { 1 }
}
$metrics['binding_replay_declarations'] = 500000
$metrics['binding_replay_files'] = 10000
Assert-BindingReplayGate -Metrics $metrics
foreach ($invalid in @(@('entity_visits_max',65), @('entity_edges_max',1025), @('entity_locations_max',257))) {
    $key = 'binding_cold_hover_' + $invalid[0]
    $metrics[$key] = $invalid[1]
    $rejected = $false
    try { Assert-BindingReplayGate -Metrics $metrics } catch { $rejected = $true }
    if (-not $rejected) { throw "Entity replay accepted an exceeded budget: $key" }
    $metrics[$key] = 1
}
Write-Host 'Entity replay budget contract passed.'
