$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$scriptPath = Join-Path $PSScriptRoot 'benchmark_large_workspace.ps1'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
if ($errors.Count -gt 0) { throw 'Benchmark script has syntax errors' }
foreach ($name in @('Quote-ProcessArgument', 'Invoke-SampledProcess')) {
    $function = $ast.Find({ param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }.GetNewClosure(), $true)
    . ([scriptblock]::Create($function.Extent.Text))
}
$fixture = "[Console]::Out.WriteLine('parsing 7/20 files'); [Console]::Error.WriteLine('fixture diagnostic'); Start-Sleep -Seconds 20"
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixture))
$failure = $null
try { Invoke-SampledProcess -FilePath 'powershell.exe' -ArgumentList @('-NoProfile','-EncodedCommand',$encoded) -Timeout 2 | Out-Null }
catch { $failure = $_.Exception.Data['benchmark_sample'] }
if ($null -eq $failure) { throw 'Timeout discarded process evidence' }
if ($failure.status -ne 'timeout' -or $failure.ElapsedMs -lt 2000 -or
    ($failure.Stdout -join "`n") -notmatch 'parsing 7/20 files' -or
    ($failure.Stderr -join "`n") -notmatch 'fixture diagnostic') { throw 'Timeout evidence is incomplete' }
if ($failure.PeakPrivateBytes -lt 1) { throw 'Timeout lost peak memory evidence' }
'Benchmark timeout evidence passed.'
