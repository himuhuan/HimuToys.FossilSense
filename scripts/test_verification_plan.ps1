$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'verification_plan.ps1')
$local = @(Get-VerificationPlan Local Rust 'candidate_service::' '' $true)
if ($local.Count -ne 2 -or $local[1].arguments[-1] -ne 'candidate_service::') { throw 'Local lost its test filter.' }
$rejected = $false
try { Get-VerificationPlan Local Rust '' '' $true } catch { $rejected = $true }
if (-not $rejected) { throw 'Local silently selected every Rust test.' }
$merge = @(Get-VerificationPlan Merge Rust '' '' $true)
if (@($merge | Where-Object { $_.command -eq 'cargo' -and $_.arguments[0] -eq 'test' }).Count -ne 1) { throw 'Merge must test Rust exactly once.' }
if (@($merge | Where-Object { $_.arguments -contains 'scripts/benchmark_large_workspace.ps1' }).Count) { throw 'Merge unexpectedly runs a large benchmark.' }
$performance = @(Get-VerificationPlan Performance Rust '' 'u-boot-lsp-lifecycle' $true)
if ($performance[-1].arguments[-1] -ne 'u-boot-lsp-lifecycle') { throw 'Performance lost explicit case selection.' }
$rejected = $false
try { Get-VerificationPlan Performance Rust '' '' $true } catch { $rejected = $true }
if (-not $rejected) { throw 'Performance silently selected a matrix.' }
Write-Host 'Verification plan behavior passed.'
