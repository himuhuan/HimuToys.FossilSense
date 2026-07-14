[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$hardeningScript = Join-Path $PSScriptRoot 'verify_release_hardening.ps1'
$extensionPackage = Get-Content -Raw -LiteralPath (
    Join-Path $repoRoot 'extensions/vscode/package.json'
) | ConvertFrom-Json
$currentVersion = [string]$extensionPackage.version

function Invoke-HardeningCheck {
    param([string[]]$Arguments)
    $output = @(
        & powershell -NoProfile -ExecutionPolicy Bypass -File $hardeningScript @Arguments 2>&1 |
            ForEach-Object { $_.ToString() }
    )
    return [pscustomobject]@{
        ExitCode = $LASTEXITCODE
        Output = $output
    }
}

function Write-FixtureFile {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$RelativePath,
        [Parameter(Mandatory = $true)][string]$Content
    )
    $path = Join-Path $Root ($RelativePath -replace '/', '\')
    $directory = Split-Path -Parent $path
    [System.IO.Directory]::CreateDirectory($directory) | Out-Null
    [System.IO.File]::WriteAllText(
        $path,
        $Content,
        [System.Text.UTF8Encoding]::new($false)
    )
}

function New-HardeningFixture {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][int]$Schema,
        [Parameter(Mandatory = $true)][int]$Parser,
        [Parameter(Mandatory = $true)][int]$Resolver,
        [Parameter(Mandatory = $true)][int]$Protocol
    )
    $docs = (
        "FossilSense $Version fallback unsupported counterpart arity record typedef " +
        "schema $Schema degree = 1"
    )
    Write-FixtureFile $Root 'Cargo.toml' "[workspace]`nmembers = [`"crates/fossilsense`"]`n"
    Write-FixtureFile $Root 'Cargo.lock' (
        "version = 4`n`n[[package]]`nname = `"fossilsense`"`nversion = `"$Version`"`n"
    )
    Write-FixtureFile $Root 'crates/fossilsense/Cargo.toml' (
        "[package]`nname = `"fossilsense`"`nversion = `"$Version`"`n"
    )
    Write-FixtureFile $Root 'crates/fossilsense/src/main.rs' "fn main() {}`n"
    Write-FixtureFile $Root 'crates/fossilsense/src/store/schema.rs' (
        "pub(crate) const SCHEMA_VERSION: i64 = $Schema;`n"
    )
    Write-FixtureFile $Root 'crates/fossilsense/src/semantic_model.rs' (
        "pub const PARSER_FACT_VERSION: i64 = $Parser;`n"
    )
    Write-FixtureFile $Root 'crates/fossilsense/src/query/callables/mod.rs' (
        "pub const CALLABLE_CANDIDATE_RESOLVER_VERSION: u32 = $Resolver;`n"
    )
    Write-FixtureFile $Root 'crates/fossilsense/src/call_model.rs' (
        "pub const RELATION_PROTOCOL_VERSION: u32 = $Protocol;`n"
    )
    Write-FixtureFile $Root 'extensions/vscode/package.json' (
        (@{ name = 'fossilsense-vscode'; version = $Version } | ConvertTo-Json -Compress) + "`n"
    )
    Write-FixtureFile $Root 'extensions/vscode/pnpm-lock.yaml' "lockfileVersion: '9.0'`n"
    Write-FixtureFile $Root 'extensions/vscode/tsconfig.json' "{}`n"
    Write-FixtureFile $Root 'extensions/vscode/.vscodeignore' "src/**`n"
    Write-FixtureFile $Root 'extensions/vscode/README.md' "$docs`n"
    Write-FixtureFile $Root 'extensions/vscode/LICENSE.txt' "fixture`n"
    Write-FixtureFile $Root 'extensions/vscode/scripts/package.mjs' "export {};`n"
    Write-FixtureFile $Root 'extensions/vscode/src/extension.ts' "export {};`n"
    Write-FixtureFile $Root 'extensions/vscode/media/relations.svg' '<svg></svg>'
    Write-FixtureFile $Root 'README.md' "$docs`n"
    Write-FixtureFile $Root 'CLAUDE.md' "$docs`n"
}

function Read-ReleaseFingerprint {
    param([Parameter(Mandatory = $true)][string]$Root)
    $result = Invoke-HardeningCheck @('-RepoRoot', $Root, '-PrintReleaseFingerprint')
    if ($result.ExitCode -ne 0) {
        throw "Release fingerprint calculation failed:`n$($result.Output -join "`n")"
    }
    return (($result.Output -join "`n") | ConvertFrom-Json)
}

$derived = Invoke-HardeningCheck @('-MetadataOnly')
if ($derived.ExitCode -ne 0) {
    throw "Derived-version metadata check failed:`n$($derived.Output -join "`n")"
}
if (($derived.Output -join "`n") -notmatch (
    'metadata verification passed for v' + [regex]::Escape($currentVersion)
)) {
    throw "Derived-version check did not report v$currentVersion."
}

$explicit = Invoke-HardeningCheck @('-MetadataOnly', '-Version', $currentVersion)
if ($explicit.ExitCode -ne 0) {
    throw "Explicit current-version metadata check failed:`n$($explicit.Output -join "`n")"
}

$mismatchVersion = '99.99.99'
$mismatch = Invoke-HardeningCheck @('-MetadataOnly', '-Version', $mismatchVersion)
if ($mismatch.ExitCode -eq 0) {
    throw 'A mismatched explicit version unexpectedly passed.'
}
if (($mismatch.Output -join "`n") -notmatch 'does not match crate/extension version') {
    throw "Mismatched-version failure did not explain the metadata disagreement:`n$($mismatch.Output -join "`n")"
}

$scriptText = Get-Content -Raw -LiteralPath $hardeningScript
if ($scriptText -match '\[string\]\$Version\s*=\s*"1\.4\.1"') {
    throw 'The hardening script restored the stale 1.4.1 default.'
}
if ($scriptText -match 'docs/archive/delivery/DELIVERY-NOTE-' -or
    $scriptText -match 'Delivery note') {
    throw 'The hardening script still requires a repository delivery-note document.'
}
if ($scriptText -match '\$isV142OrLater') {
    throw 'The v1.4.2-only contract is still incorrectly bound to all future versions.'
}
if ($scriptText -notmatch 'extension/bin/release-build\.json' -or
    $scriptText -notmatch 'release input fingerprint does not match') {
    throw 'The hardening script does not bind the exact VSIX to its release inputs.'
}

$firstFingerprint = Read-ReleaseFingerprint $repoRoot
$secondFingerprint = Read-ReleaseFingerprint $repoRoot
if ($firstFingerprint.releaseInputSha256 -notmatch '^[0-9a-f]{64}$' -or
    $firstFingerprint.releaseInputSha256 -ne $secondFingerprint.releaseInputSha256 -or
    $firstFingerprint.releaseInputFileCount -ne $secondFingerprint.releaseInputFileCount -or
    $firstFingerprint.releaseInputFileCount -le 0) {
    throw 'Release-input fingerprinting is not deterministic.'
}
if (($firstFingerprint | ConvertTo-Json -Compress) -match [regex]::Escape([string]$repoRoot)) {
    throw 'Release fingerprint metadata leaked an absolute repository path.'
}

$targetRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot 'target'))
$fixtureRoot = Join-Path $targetRoot ('release-hardening-test-' + [guid]::NewGuid().ToString('N'))
try {
    $futureRoot = Join-Path $fixtureRoot 'future'
    New-HardeningFixture $futureRoot '1.4.3' 17 3 4 3
    $future = Invoke-HardeningCheck @('-RepoRoot', $futureRoot, '-MetadataOnly')
    if ($future.ExitCode -ne 0) {
        throw "A future release was incorrectly forced through the exact v1.4.2 contract:`n$($future.Output -join "`n")"
    }

    $staleRoot = Join-Path $fixtureRoot 'stale-artifact'
    New-HardeningFixture $staleRoot '1.4.2' 16 2 3 2
    $packagedFingerprint = Read-ReleaseFingerprint $staleRoot
    $artifactName = 'fossilsense-vscode-1.4.2_BUILD20990101_000000.vsix'
    $stageRoot = Join-Path $fixtureRoot 'stage'
    $stageExtension = Join-Path $stageRoot 'extension'
    [System.IO.Directory]::CreateDirectory((Join-Path $stageExtension 'bin')) | Out-Null
    [System.IO.Directory]::CreateDirectory((Join-Path $stageExtension 'out')) | Out-Null
    [System.IO.File]::Copy(
        (Join-Path $staleRoot 'extensions/vscode/package.json'),
        (Join-Path $stageExtension 'package.json')
    )
    [System.IO.File]::Copy(
        $env:ComSpec,
        (Join-Path $stageExtension 'bin/fossilsense.exe')
    )
    Write-FixtureFile $stageExtension 'out/extension.js' "module.exports = {};`n"
    $nativeSha256 = (Get-FileHash -LiteralPath (
        Join-Path $stageExtension 'bin/fossilsense.exe'
    ) -Algorithm SHA256).Hash.ToLowerInvariant()
    $bundleSha256 = (Get-FileHash -LiteralPath (
        Join-Path $stageExtension 'out/extension.js'
    ) -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifestSha256 = (Get-FileHash -LiteralPath (
        Join-Path $stageExtension 'package.json'
    ) -Algorithm SHA256).Hash.ToLowerInvariant()
    $payloadText = (
        "releaseInput`t$($packagedFingerprint.releaseInputSha256)`n" +
        "nativeBinary`t$nativeSha256`n" +
        "extensionBundle`t$bundleSha256`n" +
        "extensionManifest`t$manifestSha256`n"
    )
    $payloadHasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $payloadHash = $payloadHasher.ComputeHash(
            [System.Text.UTF8Encoding]::new($false).GetBytes($payloadText)
        )
        $payloadSha256 = (
            [System.BitConverter]::ToString($payloadHash) -replace '-', ''
        ).ToLowerInvariant()
    } finally {
        $payloadHasher.Dispose()
    }
    $buildManifest = [ordered]@{
        schemaVersion = 1
        packageVersion = '1.4.2'
        releaseInputSha256 = $packagedFingerprint.releaseInputSha256
        releaseInputFileCount = $packagedFingerprint.releaseInputFileCount
        nativeBinarySha256 = $nativeSha256
        extensionBundleSha256 = $bundleSha256
        extensionManifestSha256 = $manifestSha256
        artifactPayloadSha256 = $payloadSha256
        sourceCommit = $packagedFingerprint.sourceCommit
        worktreeDirty = $packagedFingerprint.worktreeDirty
    }
    Write-FixtureFile $stageExtension 'bin/release-build.json' (
        ($buildManifest | ConvertTo-Json -Compress) + "`n"
    )
    $distRoot = Join-Path $staleRoot 'dist'
    [System.IO.Directory]::CreateDirectory($distRoot) | Out-Null
    $artifactPath = Join-Path $distRoot $artifactName
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory($stageRoot, $artifactPath)
    $boundArtifact = Invoke-HardeningCheck @('-RepoRoot', $staleRoot)
    $boundOutput = $boundArtifact.Output -join "`n"
    if ($boundOutput -match 'release input fingerprint does not match' -or
        $boundOutput -match 'staged .* SHA-256 does not match' -or
        $boundOutput -match 'aggregate payload SHA-256 does not match') {
        throw "A freshly bound fixture VSIX failed its fingerprint checks:`n$boundOutput"
    }

    # The artifact remains byte-for-byte unchanged while a release input moves.
    # Full hardening must reject it even though version and filename still match.
    Write-FixtureFile $staleRoot 'crates/fossilsense/src/main.rs' "fn main() { println!(`"changed`";) }`n"
    $stale = Invoke-HardeningCheck @('-RepoRoot', $staleRoot)
    if ($stale.ExitCode -eq 0 -or
        ($stale.Output -join "`n") -notmatch 'release input fingerprint does not match') {
        throw "A source edit after packaging did not invalidate the old VSIX:`n$($stale.Output -join "`n")"
    }
} finally {
    $resolvedFixture = [System.IO.Path]::GetFullPath($fixtureRoot)
    $targetPrefix = $targetRoot.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    if ($resolvedFixture.StartsWith($targetPrefix, [System.StringComparison]::OrdinalIgnoreCase) -and
        (Test-Path -LiteralPath $resolvedFixture -PathType Container)) {
        Remove-Item -LiteralPath $resolvedFixture -Recurse -Force
    }
}

Write-Host 'Release hardening script tests passed.' -ForegroundColor Green
