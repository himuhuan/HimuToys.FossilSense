[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$evidence = Join-Path $root ("target/benchmark/relation-{0}-{1}" -f (Get-Date -Format 'yyyyMMdd-HHmmss'), $PID)
New-Item -ItemType Directory -Path $evidence -Force | Out-Null
$previousRoot = $env:FOSSILSENSE_RELATION_BENCH_ROOT
Push-Location $root
try {
    $inputs = @(& rg --files crates scripts | Sort-Object | ForEach-Object {
        "$((Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash) $_"
    })
    $inputs | Set-Content -LiteralPath (Join-Path $evidence 'inputs.sha256') -Encoding UTF8
    $processor = Get-CimInstance Win32_Processor | Select-Object -First 1
    $machine = Get-CimInstance Win32_ComputerSystem
    @{
        source_commit = (& git rev-parse HEAD)
        input_manifest_sha256 = (Get-FileHash -LiteralPath (Join-Path $evidence 'inputs.sha256') -Algorithm SHA256).Hash
        captured_at = (Get-Date).ToString('o')
        os = [Environment]::OSVersion.VersionString
        processor = $processor.Name
        logical_processors = $machine.NumberOfLogicalProcessors
        physical_memory_bytes = $machine.TotalPhysicalMemory
        rustc = @(& rustc -vV)
        memory_measure = 'Windows Private Bytes'
    } | ConvertTo-Json -Depth 3 | Set-Content -LiteralPath (Join-Path $evidence 'environment.json') -Encoding UTF8
    $env:FOSSILSENSE_RELATION_BENCH_ROOT = $evidence
    & cargo test --release -p fossilsense --bin fossilsense 'server::tests::relation_benchmark::benchmark_relation_foundation' -- --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) { throw "Relation benchmark failed with exit $LASTEXITCODE; evidence: $evidence" }
    if (-not (Test-Path -LiteralPath (Join-Path $evidence 'metrics.json'))) { throw 'Missing relation benchmark metrics' }
    Write-Host "Relation benchmark passed. Evidence: $evidence"
} finally {
    $env:FOSSILSENSE_RELATION_BENCH_ROOT = $previousRoot
    Pop-Location
}
