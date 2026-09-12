[CmdletBinding()]
param([switch]$IncludeLegacyReplay)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$benchmarkScript = Join-Path $PSScriptRoot 'benchmark_large_workspace.ps1'
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$gateHelpers = Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1'
if (-not (Test-Path -LiteralPath $gateHelpers -PathType Leaf)) {
    throw 'The benchmark hard-gate helper is missing.'
}
. $gateHelpers
& (Join-Path $PSScriptRoot 'test_benchmark_failure_reporting.ps1')
if (-not $?) { throw 'Benchmark failure reporting regression failed.' }

$bindingMetrics = [ordered]@{}
foreach ($name in (Get-BindingReplayMetricNames)) {
    $bindingMetrics[$name] = if ($name -match '_(requests|correct_targets)$') { 64 } else { 1 }
}
$bindingMetrics.binding_replay_declarations = 500000
$bindingMetrics.binding_replay_files = 10000
Assert-BindingReplayGate -Metrics $bindingMetrics
$bindingMetrics.binding_warm_hover_correct_targets = 63
$bindingRejected = $false
try { Assert-BindingReplayGate -Metrics $bindingMetrics } catch { $bindingRejected = $true }
if (-not $bindingRejected) { throw 'Binding replay accepted an incorrect hover target.' }
$bindingMetrics.binding_warm_hover_correct_targets = 64
$bindingMetrics.Remove('binding_edited_definition_sqlite_read_sessions')
$bindingRejected = $false
try { Assert-BindingReplayGate -Metrics $bindingMetrics } catch { $bindingRejected = $true }
if (-not $bindingRejected) { throw 'Binding replay accepted missing SQL read-session evidence.' }
$bindingHarness = Join-Path $PSScriptRoot 'benchmark_binding_replay.ps1'
if (-not (Test-Path -LiteralPath $bindingHarness -PathType Leaf)) { throw 'Binding replay harness is missing.' }
$bindingCases = @(& $benchmarkScript -IncludeBindingReplay -CaseFilter 'u-boot-binding-replay' -ListCases)
if (($bindingCases -join "`n") -notmatch 'u-boot-binding-replay') { throw 'Binding replay case is not registered.' }

& (Join-Path $PSScriptRoot 'test_benchmark_time_policy.ps1')

. (Join-Path $PSScriptRoot 'fixtures/lifecycle_metrics.ps1')
$validLifecycleMetrics = New-LifecycleMetricsFixture
Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $validLifecycleMetrics -AllowTransientMemoryPeak:$false

$overWallMetrics = $validLifecycleMetrics.Clone()
$overWallMetrics.lsp_lifecycle_rebuild_wall_ms = 120001
$overWallRejected = $false
try { Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $overWallMetrics -ObserveFullIndexTime:$false -AllowTransientMemoryPeak:$false } catch { $overWallRejected = $true }
if (-not $overWallRejected) { throw 'The lifecycle gate accepted a full rebuild wall time above 120,000 ms.' }

$missingLifecycleMetrics = $validLifecycleMetrics.Clone()
$missingLifecycleMetrics.Remove('lsp_lifecycle_old_epoch_consistent')
$missingRejected = $false
try {
    Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $missingLifecycleMetrics
} catch {
    $missingRejected = $true
}
if (-not $missingRejected) {
    throw 'The LSP lifecycle gate accepted a result with a missing required field.'
}

$overMemoryMetrics = $validLifecycleMetrics.Clone()
$overMemoryMetrics.lsp_lifecycle_peak_process_bytes = 805306369
$overMemoryRejected = $false
try {
    Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $overMemoryMetrics
} catch {
    $overMemoryRejected = $true
}
if (-not $overMemoryRejected) {
    throw 'The U-Boot LSP lifecycle gate accepted a peak above 768 MiB.'
}

$mixedGenerationMetrics = $validLifecycleMetrics.Clone()
$mixedGenerationMetrics.lsp_lifecycle_generation_mismatches = 1
$mixedGenerationRejected = $false
try {
    Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $mixedGenerationMetrics
} catch {
    $mixedGenerationRejected = $true
}
if (-not $mixedGenerationRejected) {
    throw 'The LSP lifecycle gate accepted mixed-generation request results.'
}
$shortCompletionMetrics = $validLifecycleMetrics.Clone()
$shortCompletionMetrics.lsp_lifecycle_completion_requests = 63
$shortCompletionRejected = $false
try {
    Assert-LspLifecycleGate -CaseId 'u-boot-lsp-lifecycle' -Metrics $shortCompletionMetrics
} catch {
    $shortCompletionRejected = $true
}
if (-not $shortCompletionRejected) {
    throw 'The LSP lifecycle gate accepted fewer than 64 completion requests.'
}
$defaultCases = @(
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript -ListCases 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0) {
    throw "Default benchmark case listing failed:`n$($defaultCases -join "`n")"
}
if (@($defaultCases | Where-Object { $_ -like 'v142-*' }).Count -ne 0) {
    throw 'v1.4.2 semantic cases leaked into the default benchmark plan.'
}
if ($defaultCases -contains 'u-boot-engine-hydration') {
    throw 'The U-Boot engine hydration case leaked into the default benchmark plan.'
}
if ($defaultCases -contains 'u-boot-completion-replay') {
    throw 'The U-Boot completion replay case leaked into the default benchmark plan.'
}
if ($defaultCases -contains 'u-boot-lsp-lifecycle' -or
    $defaultCases -contains 'wine-lsp-lifecycle') {
    throw 'The LSP lifecycle cases leaked into the default benchmark plan.'
}

$engineCases = @(
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
        -ListCases -IncludeEngineHydration 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0 -or $engineCases -notcontains 'u-boot-engine-hydration') {
    throw "Engine hydration benchmark case listing failed:`n$($engineCases -join "`n")"
}
$completionCases = @(
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
        -ListCases -IncludeCompletionReplay 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0 -or $completionCases -notcontains 'u-boot-completion-replay') {
    throw "Completion replay benchmark case listing failed:`n$($completionCases -join "`n")"
}
$lifecycleCases = @(
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
        -ListCases -IncludeLspLifecycle 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0 -or
    $lifecycleCases -notcontains 'u-boot-lsp-lifecycle' -or
    $lifecycleCases -notcontains 'wine-lsp-lifecycle') {
    throw "LSP lifecycle benchmark case listing failed:`n$($lifecycleCases -join "`n")"
}
$combinedGateCases = @(
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
        -ListCases -IncludeFullIndex -IncludeEngineHydration -IncludeCompletionReplay `
        -CaseFilter 'u-boot-full-index,u-boot-engine-hydration,u-boot-completion-replay' 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0 -or
    $combinedGateCases.Count -ne 3 -or
    $combinedGateCases -notcontains 'u-boot-full-index' -or
    $combinedGateCases -notcontains 'u-boot-engine-hydration' -or
    $combinedGateCases -notcontains 'u-boot-completion-replay') {
    throw "Combined full-index, hydration, and completion filtering failed:`n$($combinedGateCases -join "`n")"
}
$engineHarness = Join-Path $PSScriptRoot 'benchmark_engine_hydration.ps1'
if (-not (Test-Path -LiteralPath $engineHarness -PathType Leaf)) {
    throw 'The engine hydration benchmark harness is missing.'
}

$completionHarness = Join-Path $PSScriptRoot 'benchmark_completion_replay.ps1'
if (-not (Test-Path -LiteralPath $completionHarness -PathType Leaf)) {
    throw 'The completion replay benchmark harness is missing.'
}

$lifecycleHarness = Join-Path $PSScriptRoot 'benchmark_lsp_lifecycle.ps1'
if (-not (Test-Path -LiteralPath $lifecycleHarness -PathType Leaf)) {
    throw 'The LSP lifecycle benchmark harness is missing.'
}


$allCases = @(
    & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
        -ListCases -IncludeV142SemanticCases 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0) {
    throw "v1.4.2 benchmark case listing failed:`n$($allCases -join "`n")"
}
$expectedV142Cases = @(
    'v142-high-duplication-callable-query',
    'v142-counterpart-scan-cap',
    'v142-large-record-range-hydration',
    'v142-multi-dirty-overlay-merge',
    'v142-signature-help-retrigger',
    'v142-concurrent-publication-hover-definition'
)
foreach ($caseId in $expectedV142Cases) {
    if ($allCases -notcontains $caseId) {
        throw "Missing v1.4.2 semantic benchmark entry point: $caseId"
    }
}
if (@($allCases | Where-Object { $_ -like 'v142-*' }).Count -ne $expectedV142Cases.Count) {
    throw 'The v1.4.2 benchmark plan contains an unexpected semantic case.'
}

$realHarness = Join-Path $PSScriptRoot 'benchmark_v142_semantics.ps1'
if (-not (Test-Path -LiteralPath $realHarness -PathType Leaf)) {
    throw 'The default v1.4.2 semantic benchmark harness is missing.'
}


function Invoke-SemanticHarnessCase {
    param(
        [Parameter(Mandatory = $true)][string]$CaseId,
        [Parameter(Mandatory = $true)][string]$BenchmarkRoot
    )

    $output = @(
        & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $realHarness `
            -Case $CaseId -BenchmarkRoot $BenchmarkRoot 2>&1 |
            ForEach-Object { $_.ToString() }
    )
    if ($LASTEXITCODE -ne 0) {
        throw "Semantic harness case $CaseId failed:`n$($output -join "`n")"
    }
    $metrics = @{}
    foreach ($line in $output) {
        if ($line -match '^([a-z][a-z0-9_]+):\s+([0-9]+)$') {
            $metrics[$Matches[1]] = [long]$Matches[2]
        }
    }
    if ($metrics.Count -eq 0) {
        throw "Semantic harness case $CaseId emitted no numeric aggregate metrics."
    }
    return $metrics
}

$testRoot = Join-Path (
    Join-Path $repoRoot 'target'
) ('benchmark-entrypoint-test-' + [guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $testRoot -Force | Out-Null
    $reportHarness = $realHarness
    if (-not $IncludeLegacyReplay) {
        $reportHarness = Join-Path $testRoot 'report-fixture.ps1'
        @'
param([string]$Case, [string]$BenchmarkRoot)
'candidate_raw: 10'
'candidate_filtered: 5'
'candidate_rows_scanned: 10'
'arity_compatible: 5'
'candidate_query_truncated: 1'
'coverage_truncated: 1'
'@ | Set-Content -LiteralPath $reportHarness -Encoding UTF8
    }
    $runOutput = @(
        & ([Diagnostics.Process]::GetCurrentProcess().MainModule.FileName) -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
            -Binary (Join-Path $testRoot 'intentionally-missing-fossilsense.exe') `
            -BenchmarkRoot $testRoot `
            -Repeats 1 `
            -TimeoutSeconds 600 `
            -IncludeV142SemanticCases -V142Harness $reportHarness `
            -CaseFilter 'v142-high-duplication-callable-query' 2>&1 |
            ForEach-Object { $_.ToString() }
    )
    if ($LASTEXITCODE -ne 0) {
        throw "Real semantic benchmark failed:`n$($runOutput -join "`n")"
    }
    $jsonReport = Get-ChildItem -LiteralPath $testRoot -Filter '*.json' -File |
        Select-Object -First 1
    $markdownReport = Get-ChildItem -LiteralPath $testRoot -Filter '*.md' -File |
        Select-Object -First 1
    if ($null -eq $jsonReport -or $null -eq $markdownReport) {
        throw 'Fixture semantic benchmark did not emit both JSON and Markdown reports.'
    }
    $report = Get-Content -Raw -LiteralPath $jsonReport.FullName | ConvertFrom-Json
    $result = @($report.results)[0]
    if ($result.case_id -ne 'v142-high-duplication-callable-query' -or
        $result.metrics.candidate_raw -le $result.metrics.candidate_filtered -or
        $result.metrics.candidate_rows_scanned -ne $result.metrics.candidate_raw -or
        $result.metrics.arity_compatible -le 0 -or
        $result.metrics.candidate_query_truncated -ne 1 -or
        $result.metrics.coverage_truncated -ne 1) {
        throw 'Real semantic aggregate metrics were not preserved in the JSON report.'
    }
    if ([string]::IsNullOrWhiteSpace($report.command_line) -or
        $report.source_revision -notmatch '^[0-9a-f]{40}$' -or
        $report.source_change_fingerprint -notmatch '^[0-9a-f]{64}$' -or
        [string]::IsNullOrWhiteSpace($report.machine.os_version) -or
        $report.machine.processor_count -le 0 -or
        [string]::IsNullOrWhiteSpace($result.workspace) -or
        [string]::IsNullOrWhiteSpace($result.sample_revision) -or
        $result.sample_change_fingerprint -notmatch '^[0-9a-f]{64}$' -or
        $result.database_size_bytes -lt 0) {
        throw 'Benchmark JSON is missing the command, source fingerprint, machine, sample revision, or database-size evidence required for reproduction.'
    }
    if (Test-Path -LiteralPath (Join-Path $result.workspace '.git')) {
        $actualRevision = (& git -C $result.workspace rev-parse HEAD 2>$null |
            Select-Object -First 1).ToString().Trim()
        if ($result.sample_revision -notmatch '^[0-9a-f]{40}$' -or
            $result.sample_revision -ne $actualRevision) {
            throw 'Benchmark JSON did not preserve the current Git revision of its sample workspace.'
        }
    }
    if ($result.outer_process_metrics_comparable -ne $false -or
        $null -ne $result.elapsed_ms -or
        $null -ne $result.peak_working_set_bytes -or
        $null -ne $result.peak_private_bytes) {
        throw 'PowerShell-wrapper elapsed/memory values must be N/A instead of misleading process metrics.'
    }
    $metricNames = @($result.metrics.PSObject.Properties.Name)
    if ($metricNames -contains 'completion_hot_path_io' -or
        $metricNames -contains 'sensitive_log_values') {
        throw 'Unmeasured zero-valued indicators must not be published as benchmark facts.'
    }
    $markdown = Get-Content -Raw -LiteralPath $markdownReport.FullName
    if ($markdown -notmatch 'v1\.4\.2 semantic request cases' -or
        $markdown -notmatch 'Candidate-resolution aggregates' -or
        $markdown -notmatch 'v142-high-duplication-callable-query' -or
        $markdown -notmatch 'N/A') {
        throw 'Real semantic metrics were not rendered in the Markdown report.'
    }

    if ($IncludeLegacyReplay) {
    $counterpart = Invoke-SemanticHarnessCase `
        -CaseId 'v142-counterpart-scan-cap' -BenchmarkRoot $testRoot
    if ($counterpart.candidate_scan_cap -le 0 -or
        $counterpart.candidate_scan_observed -ne $counterpart.candidate_scan_cap -or
        $counterpart.candidate_query_truncated -ne 1 -or
        $counterpart.coverage_open -ne 1 -or
        $counterpart.reach_nodes_visited -le $counterpart.candidate_scan_cap -or
        $counterpart.counterpart_incomplete -le 0 -or
        $counterpart.counterpart_edges -ne 0) {
        throw 'Counterpart benchmark did not exercise bounded durable recall and open reach degradation.'
    }

    $hydration = Invoke-SemanticHarnessCase `
        -CaseId 'v142-large-record-range-hydration' -BenchmarkRoot $testRoot
    $expectedHydratedBytes = [long]$hydration.hydration_count *
        [long]$hydration.hydration_requested_bytes
    if ($hydration.hydration_requested_bytes -le 0 -or
        $hydration.hydration_file_bytes -le $hydration.hydration_requested_bytes -or
        $hydration.hydration_bytes -ne $expectedHydratedBytes -or
        $hydration.hydration_revision_rejections -ne 1) {
        throw 'Large-record benchmark did not perform bounded file-range IO with a revision guard.'
    }

    $overlay = Invoke-SemanticHarnessCase `
        -CaseId 'v142-multi-dirty-overlay-merge' -BenchmarkRoot $testRoot
    if ($overlay.overlay_documents -ne 48 -or
        $overlay.overlay_parse_us -le 0 -or
        $overlay.overlay_merge_us -lt $overlay.overlay_parse_us) {
        throw 'Dirty-overlay merge timing does not include live parse preparation.'
    }

    $signature = Invoke-SemanticHarnessCase `
        -CaseId 'v142-signature-help-retrigger' -BenchmarkRoot $testRoot
    if ($signature.signature_help_requests -ne 720 -or
        $signature.candidate_returned -le 0 -or
        $signature.signature_help_p95_us -lt $signature.signature_help_p50_us) {
        throw 'Signature Help retrigger benchmark did not execute its full request sequence.'
    }

    $publication = Invoke-SemanticHarnessCase `
        -CaseId 'v142-concurrent-publication-hover-definition' -BenchmarkRoot $testRoot
    if ($publication.publication_conflicts -ne 0 -or
        $publication.generation_mismatches -ne 0 -or
        $publication.candidate_returned -le 0 -or
        $publication.concurrent_query_p95_us -lt $publication.concurrent_query_p50_us) {
        throw 'Concurrent publication benchmark observed a conflict or mixed semantic generation.'
    }
    }
} finally {
    if (Test-Path -LiteralPath $testRoot -PathType Container) {
        foreach ($file in Get-ChildItem -LiteralPath $testRoot -File) {
            Remove-Item -LiteralPath $file.FullName -Force
        }
        Remove-Item -LiteralPath $testRoot -Force
    }
}

Write-Host 'Benchmark entry-point tests passed.' -ForegroundColor Green

& (Join-Path $PSScriptRoot "test_replay_process_gate.ps1")

& (Join-Path $PSScriptRoot "test_benchmark_timeout_evidence.ps1")

& (Join-Path $PSScriptRoot 'test_entity_replay_contract.ps1')
& (Join-Path $PSScriptRoot 'test_cache_replay_contract.ps1')
