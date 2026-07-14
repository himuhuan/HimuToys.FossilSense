[CmdletBinding()]
param(
    [string]$Binary = '',
    [string]$BenchmarkRoot = '',
    [ValidateRange(1, 20)]
    [int]$Repeats = 2,
    [ValidateRange(5, 3600)]
    [int]$TimeoutSeconds = 600,
    [switch]$IncludeFullIndex,
    [switch]$IncludeV142SemanticCases,
    [string]$V142Harness = '',
    [switch]$ListCases,
    [string[]]$CaseFilter = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $Binary) {
    $Binary = Join-Path $PSScriptRoot '..\target\release\fossilsense.exe'
}
if (-not $BenchmarkRoot) {
    $BenchmarkRoot = Join-Path $PSScriptRoot '..\target\benchmark'
}

function Resolve-FullPath([string]$Path) {
    return [System.IO.Path]::GetFullPath($Path)
}

function Quote-ProcessArgument([string]$Value) {
    return '"' + $Value.Replace('\', '\').Replace('"', '\"') + '"'
}

function Invoke-SampledProcess {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$ArgumentList,
        [Parameter(Mandatory = $true)][int]$Timeout
    )

    $quotedArguments = ($ArgumentList | ForEach-Object { Quote-ProcessArgument $_ }) -join ' '
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = $quotedArguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    if (-not $process.Start()) {
        throw "failed to start benchmark process"
    }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $peakWorkingSet = 0L
    $peakPrivateBytes = 0L

    try {
        while (-not $process.HasExited) {
            if ($stopwatch.Elapsed.TotalSeconds -gt $Timeout) {
                $process.Kill()
                throw "benchmark process exceeded ${Timeout}s"
            }
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.WorkingSet64)
            $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $process.PrivateMemorySize64)
            Start-Sleep -Milliseconds 20
        }
        $process.WaitForExit()
        $process.Refresh()
        $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
        $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $process.PrivateMemorySize64)
        $stopwatch.Stop()
        $stdout = @($stdoutTask.Result -split "`r?`n")
        $stderr = @($stderrTask.Result -split "`r?`n")
        if ($process.ExitCode -ne 0) {
            $tail = ($stderr | Select-Object -Last 12) -join [Environment]::NewLine
            throw "benchmark process exited with $($process.ExitCode): $tail"
        }
        return [pscustomobject]@{
            ElapsedMs = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
            PeakWorkingSetBytes = $peakWorkingSet
            PeakPrivateBytes = $peakPrivateBytes
            Stdout = $stdout
        }
    }
    finally {
        if ($process -and -not $process.HasExited) {
            $process.Kill()
        }
        if ($process) {
            $process.Dispose()
        }
    }
}

function Convert-WhitelistedMetrics([string[]]$Lines) {
    $allowed = @{
        relations = $true
        relation_query_entities = $true
        relation_query_call_sites = $true
        relation_query_relations = $true
        relation_query_call_site_refs = $true
        relation_query_ms = $true
        query_us = $true
        files = $true
        indexed = $true
        skipped = $true
        deleted = $true
        symbols = $true
        callable_anchors = $true
        call_sites = $true
        elapsed_ms = $true
        discover_ms = $true
        parse_ms = $true
        write_ms = $true
        check_ms = $true
        include_edge_ms = $true
        secondary_index_ms = $true
        publication_ms = $true
        name_table_ms = $true
        reach_graph_ms = $true
        callable_query_us = $true
        candidate_rows_scanned = $true
        candidate_rows_filtered = $true
        candidate_rows_grouped = $true
        candidate_rows_returned = $true
        candidate_raw = $true
        candidate_filtered = $true
        candidate_grouped = $true
        candidate_returned = $true
        candidate_query_truncated = $true
        arity_compatible = $true
        arity_unknown = $true
        arity_incompatible = $true
        arity_mismatch_fallback = $true
        counterpart_graph_us = $true
        counterpart_edges = $true
        counterpart_groups = $true
        counterpart_strict = $true
        counterpart_ambiguous = $true
        counterpart_incomplete = $true
        candidate_scan_cap = $true
        candidate_scan_observed = $true
        reach_nodes_visited = $true
        hydration_us = $true
        hydration_count = $true
        hydration_bytes = $true
        hydration_sections = $true
        hydration_file_bytes = $true
        hydration_requested_bytes = $true
        hydration_revision_rejections = $true
        overlay_merge_us = $true
        overlay_parse_us = $true
        overlay_documents = $true
        signature_help_requests = $true
        signature_help_p50_us = $true
        signature_help_p95_us = $true
        concurrent_query_p50_us = $true
        concurrent_query_p95_us = $true
        publication_conflicts = $true
        generation_mismatches = $true
        reach_us = $true
        coverage_scanned = $true
        coverage_open = $true
        coverage_truncated = $true
        fallback_used = $true
    }
    $metrics = [ordered]@{}
    foreach ($line in $Lines) {
        if ($line -match '^([a-z][a-z0-9_]+):\s+([0-9]+)$' -and $allowed.ContainsKey($Matches[1])) {
            $metrics[$Matches[1]] = [long]$Matches[2]
        }
    }
    return $metrics
}

$repoRoot = Resolve-FullPath (Join-Path $PSScriptRoot '..')
$binaryPath = Resolve-FullPath $Binary
$benchmarkPath = Resolve-FullPath $BenchmarkRoot
New-Item -ItemType Directory -Force -Path $benchmarkPath | Out-Null

$cases = @(
    [pscustomobject]@{
        Id = 'uboot-board-init-incoming'
        Executable = $binaryPath
        Workspace = Join-Path $repoRoot 'samples\u-boot'
        Database = Join-Path $benchmarkPath 'index-u-boot-rebuild.sqlite'
        ResetDatabase = $null
        Arguments = @('query', 'calls', (Join-Path $repoRoot 'samples\u-boot'), 'common/board_f.c', '1073', '6', '--incoming', '--db', (Join-Path $benchmarkPath 'index-u-boot-rebuild.sqlite'))
    },
    [pscustomobject]@{
        Id = 'wine-medium-fanin-incoming'
        Executable = $binaryPath
        Workspace = Join-Path $repoRoot 'samples\wine'
        Database = Join-Path $benchmarkPath 'index-wine-rebuild.sqlite'
        ResetDatabase = $null
        Arguments = @('query', 'calls', (Join-Path $repoRoot 'samples\wine'), 'dlls/ntdll/heap.c', '1489', '30', '--incoming', '--db', (Join-Path $benchmarkPath 'index-wine-rebuild.sqlite'))
    },
    [pscustomobject]@{
        Id = 'wine-high-frequency-incoming'
        Executable = $binaryPath
        Workspace = Join-Path $repoRoot 'samples\wine'
        Database = Join-Path $benchmarkPath 'index-wine-rebuild.sqlite'
        ResetDatabase = $null
        Arguments = @('query', 'calls', (Join-Path $repoRoot 'samples\wine'), 'dlls/kernelbase/console.c', '2295', '36', '--incoming', '--db', (Join-Path $benchmarkPath 'index-wine-rebuild.sqlite'))
    }
)

if ($IncludeFullIndex) {
    foreach ($sample in @('u-boot', 'wine')) {
        $workspace = Join-Path $repoRoot "samples\$sample"
        $database = Join-Path $benchmarkPath "index-$sample-rebuild.sqlite"
        $cases += [pscustomobject]@{
            Id = "$sample-full-index"
            Executable = $binaryPath
            Workspace = $workspace
            Database = $null
            ResetDatabase = $database
            Arguments = @('index', $workspace, '--db', $database, '--force')
        }
    }
}

# The v1.4.2 semantic cases are deliberately opt-in. They use an in-process
# Rust harness so the normal large-workspace benchmark and CI remain unchanged.
# An executable harness receives `--case` / `--benchmark-root`; a PowerShell
# harness receives `-Case` / `-BenchmarkRoot`.
# It must print integer `name: value` metrics from Convert-WhitelistedMetrics.
if ($IncludeV142SemanticCases) {
    if ([string]::IsNullOrWhiteSpace($V142Harness)) {
        $V142Harness = Join-Path $PSScriptRoot 'benchmark_v142_semantics.ps1'
    }
    $v142HarnessPath = Resolve-FullPath $V142Harness
    if (-not (Test-Path -LiteralPath $v142HarnessPath -PathType Leaf)) {
        throw "v1.4.2 semantic benchmark harness not found: $v142HarnessPath"
    }
    if ([System.IO.Path]::GetExtension($v142HarnessPath) -ieq '.ps1') {
        $v142Executable = 'powershell.exe'
        $v142ArgumentPrefix = @(
            '-NoProfile',
            '-ExecutionPolicy',
            'Bypass',
            '-File',
            $v142HarnessPath
        )
        $v142PowerShellHarness = $true
    } else {
        $v142Executable = $v142HarnessPath
        $v142ArgumentPrefix = @()
        $v142PowerShellHarness = $false
    }
    $v142Cases = @(
        'v142-high-duplication-callable-query',
        'v142-counterpart-scan-cap',
        'v142-large-record-range-hydration',
        'v142-multi-dirty-overlay-merge',
        'v142-signature-help-retrigger',
        'v142-concurrent-publication-hover-definition'
    )
    foreach ($caseId in $v142Cases) {
        $caseArguments = if ($v142PowerShellHarness) {
            @('-Case', $caseId, '-BenchmarkRoot', $benchmarkPath)
        } else {
            @('--case', $caseId, '--benchmark-root', $benchmarkPath)
        }
        $cases += [pscustomobject]@{
            Id = $caseId
            Executable = $v142Executable
            Workspace = $repoRoot
            Database = $null
            ResetDatabase = $null
            # A PowerShell harness starts cargo and the Rust test binary as
            # descendants. Sampling only the wrapper would under-report memory
            # while mixing process startup/compilation into elapsed time.
            OuterMetricsComparable = -not $v142PowerShellHarness
            Arguments = @(
                $v142ArgumentPrefix + $caseArguments
            )
        }
    }
}

if ($CaseFilter.Count -gt 0) {
    $requested = [System.Collections.Generic.HashSet[string]]::new(
        $CaseFilter,
        [System.StringComparer]::OrdinalIgnoreCase
    )
    $cases = @($cases | Where-Object { $requested.Contains($_.Id) })
    if ($cases.Count -ne $requested.Count) {
        $available = ($cases.Id | Sort-Object) -join ', '
        throw "one or more benchmark case filters did not match; selected: $available"
    }
}

if ($ListCases) {
    foreach ($case in $cases) {
        Write-Output $case.Id
    }
    exit 0
}

$requiresReleaseBinary = @(
    $cases | Where-Object {
        $_.Executable -eq $binaryPath
    }
).Count -gt 0
if ($requiresReleaseBinary -and -not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "release binary not found: $binaryPath"
}

$results = [System.Collections.Generic.List[object]]::new()
foreach ($case in $cases) {
    if (-not (Test-Path -LiteralPath $case.Workspace -PathType Container)) {
        Write-Warning "skipping $($case.Id): sample workspace is unavailable"
        continue
    }
    if ($case.Database -and -not (Test-Path -LiteralPath $case.Database -PathType Leaf)) {
        Write-Warning "skipping $($case.Id): benchmark database is unavailable"
        continue
    }
    for ($run = 1; $run -le $Repeats; $run++) {
        if ($case.ResetDatabase) {
            $resetPath = Resolve-FullPath $case.ResetDatabase
            if (-not $resetPath.StartsWith($benchmarkPath, [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "benchmark reset path escaped benchmark root: $resetPath"
            }
            foreach ($suffix in @('', '-wal', '-shm')) {
                $candidate = "$resetPath$suffix"
                if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                    Remove-Item -LiteralPath $candidate -Force
                }
            }
        }
        Write-Host "benchmark $($case.Id) run $run/$Repeats"
        $sample = Invoke-SampledProcess -FilePath $case.Executable -ArgumentList $case.Arguments `
            -Timeout $TimeoutSeconds
        $outerMetricsComparable = if (
            $case.PSObject.Properties.Name -contains 'OuterMetricsComparable'
        ) {
            [bool]$case.OuterMetricsComparable
        } else {
            $true
        }
        $results.Add([pscustomobject]@{
            case_id = $case.Id
            run = $run
            outer_process_metrics_comparable = $outerMetricsComparable
            elapsed_ms = if ($outerMetricsComparable) { $sample.ElapsedMs } else { $null }
            peak_working_set_bytes = if ($outerMetricsComparable) {
                $sample.PeakWorkingSetBytes
            } else {
                $null
            }
            peak_private_bytes = if ($outerMetricsComparable) {
                $sample.PeakPrivateBytes
            } else {
                $null
            }
            metrics = Convert-WhitelistedMetrics $sample.Stdout
        })
    }
}

if ($results.Count -eq 0) {
    throw 'no benchmark case could run; prepare the public samples and indexes first'
}

$stamp = Get-Date -Format 'yyyyMMdd_HHmmss'
$jsonPath = Join-Path $benchmarkPath "large-workspace-$stamp.json"
$markdownPath = Join-Path $benchmarkPath "large-workspace-$stamp.md"
$binaryVersion = 'unavailable'
try {
    $versionLine = @(& $binaryPath --version 2>$null | Select-Object -First 1)
    if ($versionLine.Count -gt 0 -and -not [string]::IsNullOrWhiteSpace($versionLine[0])) {
        $binaryVersion = $versionLine[0].Trim()
    }
} catch {
    # Version metadata is diagnostic only and must not invalidate benchmark data.
}
$report = [ordered]@{
    schema_version = 1
    measured_at = (Get-Date).ToUniversalTime().ToString('o')
    binary_version = $binaryVersion
    sample_interval_ms = 20
    results = $results
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $jsonPath -Encoding UTF8

$markdown = [System.Collections.Generic.List[string]]::new()
$markdown.Add('# FossilSense large-workspace benchmark')
$markdown.Add('')
$markdown.Add('| Case | Run | Elapsed ms | Peak WS MiB | Peak private MiB | Write ms | Secondary index ms | Publication ms | Relation query ms | Query us |')
$markdown.Add('|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|')
foreach ($result in $results) {
    $write = if ($result.metrics.Contains('write_ms')) { $result.metrics.write_ms } else { '-' }
    $secondaryIndex = if ($result.metrics.Contains('secondary_index_ms')) { $result.metrics.secondary_index_ms } else { '-' }
    $publication = if ($result.metrics.Contains('publication_ms')) { $result.metrics.publication_ms } else { '-' }
    $relationQuery = if ($result.metrics.Contains('relation_query_ms')) { $result.metrics.relation_query_ms } else { '-' }
    $queryUs = if ($result.metrics.Contains('query_us')) { $result.metrics.query_us } else { '-' }
    $elapsed = if ($null -eq $result.elapsed_ms) { 'N/A' } else { $result.elapsed_ms }
    $workingSetMiB = if ($null -eq $result.peak_working_set_bytes) {
        'N/A'
    } else {
        [Math]::Round($result.peak_working_set_bytes / 1MB, 2)
    }
    $privateMiB = if ($null -eq $result.peak_private_bytes) {
        'N/A'
    } else {
        [Math]::Round($result.peak_private_bytes / 1MB, 2)
    }
    $markdown.Add("| $($result.case_id) | $($result.run) | $elapsed | $workingSetMiB | $privateMiB | $write | $secondaryIndex | $publication | $relationQuery | $queryUs |")
}

$v142Results = @($results | Where-Object { $_.case_id -like 'v142-*' })
if ($v142Results.Count -gt 0) {
    if ($v142PowerShellHarness) {
        $markdown.Add('')
        $markdown.Add('`Elapsed ms` and process-memory columns are `N/A` for PowerShell-wrapped semantic cases: the wrapper launches cargo/Rust descendants, so only the in-process aggregate metrics below are comparable.')
    }
    $markdown.Add('')
    $markdown.Add('## v1.4.2 semantic request cases')
    $markdown.Add('')
    $markdown.Add('| Case | Run | Callable us | Scanned | Returned | Truncated | Candidate cap | Durable observed | Reach nodes | Counterpart us | Edges | Groups | Ambiguous | Incomplete | Hydration us | Hydrated bytes | Requested bytes | File bytes | Stale rejects | Overlay us | Parse us | Dirty docs | Signature p95 us | Concurrent p95 us | Generation mismatches |')
    $markdown.Add('|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|')
    foreach ($result in $v142Results) {
        $metric = {
            param([string]$Name)
            if ($result.metrics.Contains($Name)) {
                return $result.metrics[$Name]
            }
            return '-'
        }
        $markdown.Add(
            "| $($result.case_id) | $($result.run) | $(& $metric 'callable_query_us') | " +
            "$(& $metric 'candidate_rows_scanned') | $(& $metric 'candidate_rows_returned') | " +
            "$(& $metric 'candidate_query_truncated') | $(& $metric 'candidate_scan_cap') | " +
            "$(& $metric 'candidate_scan_observed') | $(& $metric 'reach_nodes_visited') | " +
            "$(& $metric 'counterpart_graph_us') | $(& $metric 'counterpart_edges') | " +
            "$(& $metric 'counterpart_groups') | $(& $metric 'counterpart_ambiguous') | " +
            "$(& $metric 'counterpart_incomplete') | $(& $metric 'hydration_us') | " +
            "$(& $metric 'hydration_bytes') | $(& $metric 'hydration_requested_bytes') | " +
            "$(& $metric 'hydration_file_bytes') | $(& $metric 'hydration_revision_rejections') | " +
            "$(& $metric 'overlay_merge_us') | $(& $metric 'overlay_parse_us') | " +
            "$(& $metric 'overlay_documents') | " +
            "$(& $metric 'signature_help_p95_us') | $(& $metric 'concurrent_query_p95_us') | " +
            "$(& $metric 'generation_mismatches') |"
        )
    }
    $markdown.Add('')
    $markdown.Add('### Candidate-resolution aggregates')
    $markdown.Add('')
    $markdown.Add('| Case | Run | Raw | Filtered | Grouped | Returned | Arity C/U/I | Strict | Ambiguous | Reach us | Query us | Coverage scanned | Open | Truncated | Fallback | Hydrations |')
    $markdown.Add('|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|')
    foreach ($result in $v142Results) {
        $metric = {
            param([string]$Name)
            if ($result.metrics.Contains($Name)) {
                return $result.metrics[$Name]
            }
            return '-'
        }
        $arity = "$(& $metric 'arity_compatible')/$(& $metric 'arity_unknown')/$(& $metric 'arity_incompatible')"
        $markdown.Add(
            "| $($result.case_id) | $($result.run) | $(& $metric 'candidate_raw') | " +
            "$(& $metric 'candidate_filtered') | $(& $metric 'candidate_grouped') | " +
            "$(& $metric 'candidate_returned') | $arity | $(& $metric 'counterpart_strict') | " +
            "$(& $metric 'counterpart_ambiguous') | $(& $metric 'reach_us') | " +
            "$(& $metric 'query_us') | $(& $metric 'coverage_scanned') | " +
            "$(& $metric 'coverage_open') | $(& $metric 'coverage_truncated') | " +
            "$(& $metric 'fallback_used') | $(& $metric 'hydration_count') |"
        )
    }
}
$markdown | Set-Content -LiteralPath $markdownPath -Encoding UTF8

Write-Host "json_report: $jsonPath"
Write-Host "markdown_report: $markdownPath"
