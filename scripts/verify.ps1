[CmdletBinding()]
param(
    [ValidateSet('Local', 'Merge', 'Performance')][string]$Profile = 'Local',
    [ValidateSet('Rust', 'Extension', 'Scripts')][string]$Scope = 'Rust',
    [string]$TestFilter = '',
    [string]$CaseFilter = '',
    [switch]$SkipInstall,
    [switch]$ListSteps
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'verification_plan.ps1')
$steps = @(Get-VerificationPlan $Profile $Scope $TestFilter $CaseFilter $SkipInstall.IsPresent)
if ($ListSteps) { ConvertTo-Json -InputObject $steps -Depth 5; exit 0 }
$logRoot = Join-Path $RepoRoot ("target/verification/{0}-{1}-{2}" -f $Profile, (Get-Date -Format 'yyyyMMdd-HHmmss'), $PID)
New-Item -ItemType Directory -Path $logRoot -Force | Out-Null
$results = [System.Collections.Generic.List[object]]::new()
foreach ($step in $steps) {
    $log = Join-Path $logRoot ("{0:D2}.log" -f $results.Count)
    $timer = [Diagnostics.Stopwatch]::StartNew()
    Push-Location (Join-Path $RepoRoot $step.directory)
    try {
        $command = $step.command
        $arguments = $step.arguments
        Get-Command $command -ErrorAction Stop | Out-Null
        $ErrorActionPreference = 'Continue'
        & $command @arguments > $log 2>&1
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = 'Stop'; Pop-Location }
    $timer.Stop()
    $results.Add(@{ command = $command; arguments = $arguments; exit_code = $code; elapsed_ms = $timer.ElapsedMilliseconds; log = $log })
    $results | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $logRoot 'results.json') -Encoding UTF8
    Write-Host "$command $($arguments -join ' '): exit=$code, $($timer.ElapsedMilliseconds) ms; $log"
    if ($code -ne 0) {
        Get-Content -LiteralPath $log -Tail 35
        throw "Verification failed; full evidence: $logRoot"
    }
}
Write-Host "$Profile verification passed. Evidence: $logRoot" -ForegroundColor Green
