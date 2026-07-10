<#
.SYNOPSIS
Builds, tests, and packages the self-contained FossilSense VS Code extension.

.DESCRIPTION
The extension packaging command owns the release binary build, binary staging,
and VSIX creation. This script deliberately does not duplicate those steps;
it provides a reproducible repository-level entry point around that command.

.EXAMPLE
.\build.ps1

.EXAMPLE
.\build.ps1 -SkipInstall -SkipTests
#>
[CmdletBinding()]
param(
    [switch]$SkipInstall,
    [switch]$SkipTests,
    [switch]$SkipReleaseValidation
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = $PSScriptRoot
$ExtensionDir = Join-Path $RepoRoot "extensions\vscode"
$DistDir = Join-Path $RepoRoot "dist"

function Get-RequiredCommand {
    param([Parameter(Mandatory)][string]$Name)

    $command = Get-Command $Name -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        throw "Required command '$Name' was not found on PATH."
    }

    return $command.Source
}

function Invoke-NativeCommand {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @(),
        [Parameter(Mandatory)][string]$WorkingDirectory
    )

    Write-Host "> $FilePath $($Arguments -join ' ')" -ForegroundColor DarkGray
    Push-Location -LiteralPath $WorkingDirectory
    try {
        & $FilePath @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "Command failed with exit code $($LASTEXITCODE): $FilePath $($Arguments -join ' ')"
        }
    } finally {
        Pop-Location
    }
}

function Get-PackageVersion {
    param([Parameter(Mandatory)][string]$PackagePath)

    return (Get-Content -Raw -LiteralPath $PackagePath | ConvertFrom-Json).version
}

Write-Host "FossilSense build and VSIX packaging" -ForegroundColor Cyan
Write-Host "Repository: $RepoRoot"

$Cargo = Get-RequiredCommand "cargo"
$Node = Get-RequiredCommand "node"
$Pnpm = Get-RequiredCommand "pnpm"
$Version = Get-PackageVersion (Join-Path $ExtensionDir "package.json")

Write-Host "cargo: $Cargo" -ForegroundColor DarkGreen
Write-Host "node:  $Node" -ForegroundColor DarkGreen
Write-Host "pnpm:  $Pnpm" -ForegroundColor DarkGreen
Write-Host "version: $Version" -ForegroundColor DarkGreen

if (-not $SkipInstall) {
    Write-Host "`nInstalling locked extension dependencies..." -ForegroundColor Yellow
    # --frozen-lockfile prevents lockfile mutation; --force accepts pnpm's
    # node_modules replacement without an interactive prompt. Together they
    # make a stale local modules directory safe for unattended CI builds.
    Invoke-NativeCommand -FilePath $Pnpm -Arguments @("install", "--frozen-lockfile", "--force") -WorkingDirectory $ExtensionDir
}

if (-not $SkipTests) {
    Write-Host "`nRunning Rust tests..." -ForegroundColor Yellow
    Invoke-NativeCommand -FilePath $Cargo -Arguments @("test", "-p", "fossilsense") -WorkingDirectory $RepoRoot

    Write-Host "`nRunning extension tests..." -ForegroundColor Yellow
    Invoke-NativeCommand -FilePath $Pnpm -Arguments @("run", "test") -WorkingDirectory $ExtensionDir
}

Write-Host "`nCreating self-contained VSIX..." -ForegroundColor Yellow
$existingVsix = @(
    Get-ChildItem -LiteralPath $DistDir -Filter "fossilsense-vscode-$($Version)_BUILD*.vsix" -File -ErrorAction SilentlyContinue |
        ForEach-Object FullName
)
Invoke-NativeCommand -FilePath $Pnpm -Arguments @("run", "package") -WorkingDirectory $ExtensionDir

$newVsix = @(
    Get-ChildItem -LiteralPath $DistDir -Filter "fossilsense-vscode-$($Version)_BUILD*.vsix" -File -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -notin $existingVsix } |
        Sort-Object LastWriteTime -Descending
)
if ($newVsix.Count -ne 1) {
    throw "Expected exactly one new VSIX for v$Version, found $($newVsix.Count)."
}

if (-not $SkipReleaseValidation) {
    Write-Host "`nVerifying release artifact..." -ForegroundColor Yellow
    Invoke-NativeCommand -FilePath (Get-RequiredCommand "powershell") -Arguments @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-File", (Join-Path $RepoRoot "scripts\verify_release_hardening.ps1"),
        "-Version", $Version
    ) -WorkingDirectory $RepoRoot
}

Write-Host "`nVSIX ready: $($newVsix[0].FullName)" -ForegroundColor Green
Write-Host "Install with: code --install-extension `"$($newVsix[0].FullName)`""
