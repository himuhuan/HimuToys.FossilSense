$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'benchmark_gate_helpers.ps1')
Assert-ReplayProcessSuccess -ExitCode 0 -Output @('running 1 test', 'test result: ok')
foreach ($case in @(@{ Code=0; Lines=@("thread 'tokio-rt-worker' panicked at source.rs:91", 'test result: ok') }, @{ Code=1; Lines=@('failed') })) {
    $rejected = $false
    try { Assert-ReplayProcessSuccess -ExitCode $case.Code -Output $case.Lines } catch { $rejected = $true }
    if (-not $rejected) { throw 'replay process failure was accepted' }
}
Write-Output 'replay process gate tests passed'
