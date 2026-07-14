[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$benchmarkScript = Join-Path $PSScriptRoot 'benchmark_large_workspace.ps1'
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$defaultCases = @(
    & powershell -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript -ListCases 2>&1 |
        ForEach-Object { $_.ToString() }
)
if ($LASTEXITCODE -ne 0) {
    throw "Default benchmark case listing failed:`n$($defaultCases -join "`n")"
}
if (@($defaultCases | Where-Object { $_ -like 'v142-*' }).Count -ne 0) {
    throw 'v1.4.2 semantic cases leaked into the default benchmark plan.'
}

$allCases = @(
    & powershell -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
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
$harnessSource = Get-Content -Raw -LiteralPath $realHarness
if ($harnessSource -notmatch 'cargo test' -or
    $harnessSource -notmatch 'semantic_benchmark::benchmark_v142_case') {
    throw 'The default v1.4.2 harness does not execute the in-process Rust semantics.'
}

function Invoke-SemanticHarnessCase {
    param(
        [Parameter(Mandatory = $true)][string]$CaseId,
        [Parameter(Mandatory = $true)][string]$BenchmarkRoot
    )

    $output = @(
        & powershell -NoProfile -ExecutionPolicy Bypass -File $realHarness `
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
    $runOutput = @(
        & powershell -NoProfile -ExecutionPolicy Bypass -File $benchmarkScript `
            -Binary (Join-Path $testRoot 'intentionally-missing-fossilsense.exe') `
            -BenchmarkRoot $testRoot `
            -Repeats 1 `
            -TimeoutSeconds 600 `
            -IncludeV142SemanticCases `
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
} finally {
    if (Test-Path -LiteralPath $testRoot -PathType Container) {
        foreach ($file in Get-ChildItem -LiteralPath $testRoot -File) {
            Remove-Item -LiteralPath $file.FullName -Force
        }
        Remove-Item -LiteralPath $testRoot -Force
    }
}

Write-Host 'Benchmark entry-point tests passed.' -ForegroundColor Green
