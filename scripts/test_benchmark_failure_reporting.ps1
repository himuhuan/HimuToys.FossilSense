$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$testRoot = Join-Path $repoRoot ('target/benchmark-failure-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
$harness = Join-Path $testRoot 'fixture.ps1'
@'
param([string]$Case, [string]$BenchmarkRoot)
if ($Case -eq 'v142-counterpart-scan-cap') { throw 'intentional second-case failure' }
'query_us: 1'
'@ | Set-Content -LiteralPath $harness -Encoding UTF8
try {
    $savedErrorAction = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $output = @(& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'benchmark_large_workspace.ps1') -IncludeV142SemanticCases -V142Harness $harness -CaseFilter 'v142-high-duplication-callable-query,v142-counterpart-scan-cap' -BenchmarkRoot $testRoot -Repeats 1 -TimeoutSeconds 10 2>&1)
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $savedErrorAction
    if ($exitCode -eq 0) { throw 'Fixture failure must fail the benchmark run' }
    $checkpoint = Get-ChildItem -LiteralPath $testRoot -Filter '*partial*.json' | Select-Object -First 1
    if ($null -eq $checkpoint) { throw 'A later benchmark failure discarded the earlier successful case' }
    $report = Get-Content -LiteralPath $checkpoint.FullName -Raw | ConvertFrom-Json
    if ($report.status -ne 'partial' -or @($report.results).Count -ne 1 -or $report.results[0].case_id -ne 'v142-high-duplication-callable-query' -or $report.results[0].metrics.query_us -ne 1) { throw 'Partial report did not preserve the completed case honestly' }
    Write-Output 'Benchmark failure reporting passed.'
} finally {
    $resolvedTest = [System.IO.Path]::GetFullPath($testRoot)
    $allowedRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot 'target')) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $resolvedTest.StartsWith($allowedRoot, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Test cleanup path escaped target' }
    Remove-Item -LiteralPath $resolvedTest -Recurse -Force
}
