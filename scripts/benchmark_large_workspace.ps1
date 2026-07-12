[CmdletBinding()]
param(
    [string]$Binary = '',
    [string]$BenchmarkRoot = '',
    [ValidateRange(1, 20)]
    [int]$Repeats = 2,
    [ValidateRange(5, 3600)]
    [int]$TimeoutSeconds = 600,
    [switch]$IncludeFullIndex
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
        catalog_entities = $true
        catalog_call_sites = $true
        catalog_relations = $true
        catalog_call_site_refs = $true
        catalog_build_ms = $true
        catalog_load_anchors_ms = $true
        catalog_load_call_sites_ms = $true
        catalog_group_entities_ms = $true
        catalog_resolve_relations_ms = $true
        catalog_finalize_ms = $true
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
        name_table_ms = $true
        reach_graph_ms = $true
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
if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "release binary not found: $binaryPath"
}
New-Item -ItemType Directory -Force -Path $benchmarkPath | Out-Null

$cases = @(
    [pscustomobject]@{
        Id = 'uboot-board-init-incoming'
        Workspace = Join-Path $repoRoot 'samples\u-boot'
        Database = Join-Path $benchmarkPath 'index-u-boot.sqlite'
        Arguments = @('query', 'calls', (Join-Path $repoRoot 'samples\u-boot'), 'common/board_f.c', '1073', '6', '--incoming', '--db', (Join-Path $benchmarkPath 'index-u-boot.sqlite'))
    },
    [pscustomobject]@{
        Id = 'wine-medium-fanin-incoming'
        Workspace = Join-Path $repoRoot 'samples\wine'
        Database = Join-Path $benchmarkPath 'index-wine.sqlite'
        Arguments = @('query', 'calls', (Join-Path $repoRoot 'samples\wine'), 'dlls/ntdll/heap.c', '1489', '30', '--incoming', '--db', (Join-Path $benchmarkPath 'index-wine.sqlite'))
    },
    [pscustomobject]@{
        Id = 'wine-high-frequency-incoming'
        Workspace = Join-Path $repoRoot 'samples\wine'
        Database = Join-Path $benchmarkPath 'index-wine.sqlite'
        Arguments = @('query', 'calls', (Join-Path $repoRoot 'samples\wine'), 'dlls/kernelbase/console.c', '2295', '36', '--incoming', '--db', (Join-Path $benchmarkPath 'index-wine.sqlite'))
    }
)

if ($IncludeFullIndex) {
    foreach ($sample in @('u-boot', 'wine')) {
        $workspace = Join-Path $repoRoot "samples\$sample"
        $database = Join-Path $benchmarkPath "index-$sample-rebuild.sqlite"
        $cases += [pscustomobject]@{
            Id = "$sample-full-index"
            Workspace = $workspace
            Database = $null
            Arguments = @('index', $workspace, '--db', $database, '--force')
        }
    }
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
        Write-Host "benchmark $($case.Id) run $run/$Repeats"
        $sample = Invoke-SampledProcess -FilePath $binaryPath -ArgumentList $case.Arguments `
            -Timeout $TimeoutSeconds
        $results.Add([pscustomobject]@{
            case_id = $case.Id
            run = $run
            elapsed_ms = $sample.ElapsedMs
            peak_working_set_bytes = $sample.PeakWorkingSetBytes
            peak_private_bytes = $sample.PeakPrivateBytes
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
$report = [ordered]@{
    schema_version = 1
    measured_at = (Get-Date).ToUniversalTime().ToString('o')
    binary_version = ((Get-Item -LiteralPath $binaryPath).VersionInfo.FileVersion | ForEach-Object {
        if ($_) { $_ } else { 'unavailable' }
    })
    sample_interval_ms = 20
    results = $results
}
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $jsonPath -Encoding UTF8

$markdown = [System.Collections.Generic.List[string]]::new()
$markdown.Add('# FossilSense large-workspace benchmark')
$markdown.Add('')
$markdown.Add('| Case | Run | Elapsed ms | Peak WS MiB | Peak private MiB | Catalog build ms | Query us |')
$markdown.Add('|---|---:|---:|---:|---:|---:|---:|')
foreach ($result in $results) {
    $catalogBuild = if ($result.metrics.Contains('catalog_build_ms')) { $result.metrics.catalog_build_ms } else { '-' }
    $queryUs = if ($result.metrics.Contains('query_us')) { $result.metrics.query_us } else { '-' }
    $workingSetMiB = [Math]::Round($result.peak_working_set_bytes / 1MB, 2)
    $privateMiB = [Math]::Round($result.peak_private_bytes / 1MB, 2)
    $markdown.Add("| $($result.case_id) | $($result.run) | $($result.elapsed_ms) | $workingSetMiB | $privateMiB | $catalogBuild | $queryUs |")
}
$markdown | Set-Content -LiteralPath $markdownPath -Encoding UTF8

Write-Host "json_report: $jsonPath"
Write-Host "markdown_report: $markdownPath"
