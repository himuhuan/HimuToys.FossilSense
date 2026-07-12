[CmdletBinding()]
param([switch]$SkipInstall)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$RepoRoot = Split-Path -Parent $PSScriptRoot
$ExtensionRoot = Join-Path $RepoRoot 'extensions\vscode'

function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments, [string]$WorkingDirectory)
    Push-Location -LiteralPath $WorkingDirectory
    try {
        & $Command @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "$Command $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

if (-not $SkipInstall) {
    Invoke-Checked pnpm @('install', '--frozen-lockfile') $ExtensionRoot
}
Invoke-Checked cargo @('fmt', '--all', '--', '--check') $RepoRoot
Invoke-Checked cargo @('clippy', '-p', 'fossilsense', '--all-targets', '--', '-D', 'warnings') $RepoRoot
Invoke-Checked cargo @('test', '-p', 'fossilsense') $RepoRoot
Invoke-Checked node @('scripts/test_architecture_fitness.js') $RepoRoot
Invoke-Checked node @('scripts/architecture_fitness.js') $RepoRoot
Invoke-Checked pnpm @('test') $ExtensionRoot

Write-Host 'FossilSense verification passed.' -ForegroundColor Green
