[CmdletBinding()]
param(
    [string]$Version = '',
    [Alias('DryRun')]
    [switch]$MetadataOnly,
    [switch]$PrintReleaseFingerprint,
    [string]$RepoRoot = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
    $RepoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
} else {
    $RepoRoot = Resolve-Path -LiteralPath $RepoRoot
}
$RepoRoot = [string]$RepoRoot
$failures = New-Object System.Collections.Generic.List[string]

function Add-Failure {
    param([string]$Message)
    $failures.Add($Message) | Out-Null
}

function Read-JsonFile {
    param([string]$Path)
    Read-Utf8Text $Path | ConvertFrom-Json
}

function Read-Utf8Text {
    param([string]$Path)
    return [System.IO.File]::ReadAllText(
        $Path,
        [System.Text.UTF8Encoding]::new($false)
    )
}

function Get-FileSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)

    $stream = [System.IO.File]::OpenRead($Path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha256.ComputeHash($stream)
        return ([System.BitConverter]::ToString($hash) -replace '-', '').ToLowerInvariant()
    } finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

function Get-ReleaseInputFingerprint {
    param([Parameter(Mandatory = $true)][string]$Root)

    $rootPath = [System.IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $filesByRelativePath = @{}
    $fixedInputs = @(
        'Cargo.toml',
        'Cargo.lock',
        'rust-toolchain',
        'rust-toolchain.toml',
        '.cargo/config',
        '.cargo/config.toml',
        'crates/fossilsense/Cargo.toml',
        'crates/fossilsense/build.rs',
        'extensions/vscode/package.json',
        'extensions/vscode/pnpm-lock.yaml',
        'extensions/vscode/tsconfig.json',
        'extensions/vscode/.vscodeignore',
        'extensions/vscode/README.md',
        'extensions/vscode/LICENSE.txt',
        'extensions/vscode/scripts/package.mjs',
        'scripts/verify_release_hardening.ps1',
        'scripts/test_release_hardening.ps1'
    )
    $inputRoots = @(
        'crates/fossilsense/src',
        'extensions/vscode/src',
        'extensions/vscode/media'
    )

    foreach ($relative in $fixedInputs) {
        $fullPath = Join-Path $rootPath ($relative -replace '/', '\')
        if (Test-Path -LiteralPath $fullPath -PathType Leaf) {
            $filesByRelativePath[$relative] = [System.IO.Path]::GetFullPath($fullPath)
        }
    }
    foreach ($relativeRoot in $inputRoots) {
        $fullRoot = Join-Path $rootPath ($relativeRoot -replace '/', '\')
        if (-not (Test-Path -LiteralPath $fullRoot -PathType Container)) {
            continue
        }
        foreach ($file in Get-ChildItem -LiteralPath $fullRoot -File -Recurse) {
            $fullPath = [System.IO.Path]::GetFullPath($file.FullName)
            $prefix = $rootPath + [System.IO.Path]::DirectorySeparatorChar
            if (-not $fullPath.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "release input escaped repository root"
            }
            $relative = $fullPath.Substring($prefix.Length).Replace('\', '/')
            $filesByRelativePath[$relative] = $fullPath
        }
    }
    if ($filesByRelativePath.Count -eq 0) {
        throw 'no release inputs were found'
    }

    $relativePaths = [string[]]@($filesByRelativePath.Keys)
    [System.Array]::Sort($relativePaths, [System.StringComparer]::Ordinal)
    $manifestLines = [System.Collections.Generic.List[string]]::new()
    foreach ($relative in $relativePaths) {
        $fileHash = Get-FileSha256 $filesByRelativePath[$relative]
        $manifestLines.Add("$relative`t$fileHash")
    }
    $manifest = ($manifestLines -join "`n") + "`n"
    $manifestBytes = [System.Text.UTF8Encoding]::new($false).GetBytes($manifest)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha256.ComputeHash($manifestBytes)
        $fingerprint = ([System.BitConverter]::ToString($hash) -replace '-', '').ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
    return [pscustomobject]@{
        Sha256 = $fingerprint
        FileCount = $relativePaths.Count
    }
}

function Get-SourceState {
    param([Parameter(Mandatory = $true)][string]$Root)

    $commit = 'unavailable'
    $dirty = $true
    try {
        $commitOutput = @(& git -C $Root rev-parse HEAD 2>$null | ForEach-Object {
            $_.ToString().Trim()
        })
        if ($LASTEXITCODE -eq 0 -and $commitOutput.Count -gt 0 -and
            $commitOutput[0] -match '^[0-9a-fA-F]{40,64}$') {
            $commit = $commitOutput[0].ToLowerInvariant()
        }
        $statusOutput = @(& git -C $Root status --porcelain --untracked-files=all 2>$null)
        if ($LASTEXITCODE -eq 0) {
            $dirty = $statusOutput.Count -gt 0
        }
    } catch {
        # Source content fingerprinting remains authoritative without Git.
    }
    return [pscustomobject]@{
        Commit = $commit
        Dirty = $dirty
    }
}

function Read-ZipText {
    param([System.IO.Compression.ZipArchiveEntry]$Entry)
    $stream = $Entry.Open()
    $reader = [System.IO.StreamReader]::new($stream)
    try {
        return $reader.ReadToEnd()
    } finally {
        $reader.Dispose()
        $stream.Dispose()
    }
}

function Get-ZipEntrySha256 {
    param([Parameter(Mandatory = $true)][System.IO.Compression.ZipArchiveEntry]$Entry)

    $stream = $Entry.Open()
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha256.ComputeHash($stream)
        return ([System.BitConverter]::ToString($hash) -replace '-', '').ToLowerInvariant()
    } finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

function Get-ArtifactPayloadFingerprint {
    param(
        [Parameter(Mandatory = $true)][string]$ReleaseInputSha256,
        [Parameter(Mandatory = $true)][string]$NativeBinarySha256,
        [Parameter(Mandatory = $true)][string]$ExtensionBundleSha256,
        [Parameter(Mandatory = $true)][string]$ExtensionManifestSha256
    )

    $payload = (
        "releaseInput`t$ReleaseInputSha256`n" +
        "nativeBinary`t$NativeBinarySha256`n" +
        "extensionBundle`t$ExtensionBundleSha256`n" +
        "extensionManifest`t$ExtensionManifestSha256`n"
    )
    $bytes = [System.Text.UTF8Encoding]::new($false).GetBytes($payload)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha256.ComputeHash($bytes)
        return ([System.BitConverter]::ToString($hash) -replace '-', '').ToLowerInvariant()
    } finally {
        $sha256.Dispose()
    }
}

function Find-NumericRustConstant {
    param(
        [string]$SourceRoot,
        [string]$NamePattern
    )
    $constantPattern = "(?m)\bconst\s+($NamePattern)\s*:\s*[A-Za-z0-9_:]+\s*=\s*([0-9]+)"
    foreach ($file in Get-ChildItem -LiteralPath $SourceRoot -Filter '*.rs' -File -Recurse) {
        $text = Read-Utf8Text $file.FullName
        $match = [regex]::Match($text, $constantPattern)
        if ($match.Success) {
            return [pscustomobject]@{
                Name = $match.Groups[1].Value
                Value = [int64]$match.Groups[2].Value
                Path = $file.FullName
            }
        }
    }
    return $null
}

function Require-DocumentPatterns {
    param(
        [string]$Path,
        [object[]]$Requirements
    )
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Add-Failure "Required document is missing: $Path."
        return
    }
    $text = Read-Utf8Text $Path
    foreach ($requirement in $Requirements) {
        if ($text -notmatch $requirement.Pattern) {
            Add-Failure "$Path is missing $($requirement.Label)."
        }
    }
}

if ($PrintReleaseFingerprint) {
    $fingerprint = Get-ReleaseInputFingerprint $RepoRoot
    $sourceState = Get-SourceState $RepoRoot
    [ordered]@{
        releaseInputSha256 = $fingerprint.Sha256
        releaseInputFileCount = $fingerprint.FileCount
        sourceCommit = $sourceState.Commit
        worktreeDirty = $sourceState.Dirty
    } | ConvertTo-Json -Compress
    exit 0
}

$extensionPackagePath = Join-Path $RepoRoot 'extensions/vscode/package.json'
$cargoTomlPath = Join-Path $RepoRoot 'crates/fossilsense/Cargo.toml'
$cargoLockPath = Join-Path $RepoRoot 'Cargo.lock'
$schemaPath = Join-Path $RepoRoot 'crates/fossilsense/src/store/schema.rs'
$rustSourceRoot = Join-Path $RepoRoot 'crates/fossilsense/src'

$extensionPackage = $null
$extensionVersion = $null
if (Test-Path -LiteralPath $extensionPackagePath -PathType Leaf) {
    $extensionPackage = Read-JsonFile $extensionPackagePath
    $extensionVersion = [string]$extensionPackage.version
} else {
    Add-Failure "Extension package metadata is missing: $extensionPackagePath."
}

$cargoVersion = $null
if (Test-Path -LiteralPath $cargoTomlPath -PathType Leaf) {
    $cargoToml = Read-Utf8Text $cargoTomlPath
    $cargoVersionMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"')
    if ($cargoVersionMatch.Success) {
        $cargoVersion = $cargoVersionMatch.Groups[1].Value
    } else {
        Add-Failure "Could not read the fossilsense package version from $cargoTomlPath."
    }
} else {
    Add-Failure "Crate metadata is missing: $cargoTomlPath."
}

if ($extensionVersion -and $cargoVersion -and $extensionVersion -ne $cargoVersion) {
    Add-Failure "Crate version '$cargoVersion' and extension version '$extensionVersion' disagree."
}

$derivedVersion = if ($extensionVersion -and $cargoVersion -and $extensionVersion -eq $cargoVersion) {
    $extensionVersion
} else {
    $null
}
if ([string]::IsNullOrWhiteSpace($Version)) {
    if ($derivedVersion) {
        $Version = $derivedVersion
    } else {
        $Version = 'unresolved'
        Add-Failure 'Version was omitted and could not be safely derived from matching crate/extension metadata.'
    }
} elseif ($derivedVersion -and $Version -ne $derivedVersion) {
    Add-Failure "Requested release version '$Version' does not match crate/extension version '$derivedVersion'."
}

$parsedReleaseVersion = $null
try {
    $parsedReleaseVersion = [version]$Version
} catch {
    Add-Failure "Release version '$Version' is not a numeric semantic version."
}
$isV142Release = $null -ne $parsedReleaseVersion -and $parsedReleaseVersion -eq [version]'1.4.2'

if (Test-Path -LiteralPath $cargoLockPath -PathType Leaf) {
    $cargoLock = Read-Utf8Text $cargoLockPath
    $lockMatch = [regex]::Match(
        $cargoLock,
        '(?ms)\[\[package\]\]\s*name\s*=\s*"fossilsense"\s*version\s*=\s*"([^"]+)"'
    )
    if (-not $lockMatch.Success) {
        Add-Failure "Cargo.lock has no fossilsense package entry."
    } elseif ($lockMatch.Groups[1].Value -ne $Version) {
        Add-Failure "Cargo.lock fossilsense version is '$($lockMatch.Groups[1].Value)', expected '$Version'."
    }
} else {
    Add-Failure "Cargo.lock is missing: $cargoLockPath."
}

$schemaVersion = $null
$legacyParserVersion = $null
if (Test-Path -LiteralPath $schemaPath -PathType Leaf) {
    $schemaText = Read-Utf8Text $schemaPath
    $schemaMatch = [regex]::Match($schemaText, 'SCHEMA_VERSION\s*:\s*i64\s*=\s*([0-9]+)')
    if ($schemaMatch.Success) {
        $schemaVersion = [int64]$schemaMatch.Groups[1].Value
    } else {
        Add-Failure "Could not read SCHEMA_VERSION from $schemaPath."
    }
    $legacyParserMatch = [regex]::Match(
        $schemaText,
        'parser_version\s+INTEGER\s+NOT\s+NULL\s+DEFAULT\s+([0-9]+)',
        [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
    )
    if ($legacyParserMatch.Success) {
        $legacyParserVersion = [int64]$legacyParserMatch.Groups[1].Value
    }
}

$parserConstant = Find-NumericRustConstant $rustSourceRoot '[A-Z0-9_]*PARSER[A-Z0-9_]*VERSION'
$resolverConstant = Find-NumericRustConstant $rustSourceRoot '[A-Z0-9_]*RESOLVER_VERSION'
$protocolConstant = Find-NumericRustConstant $rustSourceRoot 'RELATION_PROTOCOL_VERSION'

if ($isV142Release) {
    if ($schemaVersion -ne 16) {
        Add-Failure "v1.4.2 requires SCHEMA_VERSION 16, found '$schemaVersion'."
    }
    if ($null -eq $parserConstant -or $parserConstant.Value -lt 2) {
        Add-Failure 'v1.4.2 requires a named parser fact version constant advanced beyond the legacy value 1.'
    }
    if ($null -eq $resolverConstant -or $resolverConstant.Value -le 2) {
        Add-Failure 'v1.4.2 requires an independent resolver version constant advanced beyond the legacy cursor value 2.'
    }
    if ($null -eq $protocolConstant -or $protocolConstant.Value -ne 2) {
        Add-Failure 'Call Relations wire protocol must remain v2 for the v1.4.2 release.'
    }
} else {
    if ($null -eq $schemaVersion) {
        Add-Failure 'A numeric schema version is required.'
    }
    if ($null -eq $parserConstant -and $null -eq $legacyParserVersion) {
        Add-Failure 'A parser fact version could not be identified.'
    }
    if ($null -eq $protocolConstant) {
        Add-Failure 'RELATION_PROTOCOL_VERSION could not be identified.'
    }
}

$versionPattern = [regex]::Escape($Version)
$baseDocumentRequirements = @(
    @{ Label = "release version '$Version'"; Pattern = $versionPattern },
    @{ Label = 'fallback/degradation language'; Pattern = '(?i)fallback|degrad|\u56de\u9000|\u964d\u7ea7' },
    @{ Label = 'a stated limitation or non-goal'; Pattern = '(?i)not support|unsupported|non-goal|cannot|\u4e0d\u652f\u6301|\u4e0d\u505a|\u4e0d\u80fd' }
)
Require-DocumentPatterns (Join-Path $RepoRoot 'README.md') $baseDocumentRequirements
Require-DocumentPatterns (Join-Path $RepoRoot 'extensions/vscode/README.md') $baseDocumentRequirements
Require-DocumentPatterns (Join-Path $RepoRoot 'CLAUDE.md') $baseDocumentRequirements

if ($isV142Release) {
    $semanticReleaseRequirements = @(
        @{ Label = 'counterpart/pairing behavior'; Pattern = '(?i)counterpart|pairing|\.h/.c|\u914d\u5bf9' },
        @{ Label = 'arity behavior'; Pattern = '(?i)arity|\u53c2\u6570\u4e2a\u6570' },
        @{ Label = 'record/struct Hover behavior'; Pattern = '(?i)record|struct' },
        @{ Label = 'typedef/aka behavior'; Pattern = '(?i)typedef|aka' }
    )
    Require-DocumentPatterns (Join-Path $RepoRoot 'README.md') $semanticReleaseRequirements
    Require-DocumentPatterns (Join-Path $RepoRoot 'extensions/vscode/README.md') $semanticReleaseRequirements
    Require-DocumentPatterns (Join-Path $RepoRoot 'CLAUDE.md') @(
        $semanticReleaseRequirements + @(
            @{ Label = 'schema 16'; Pattern = '(?i)schema\s*16' },
            @{ Label = 'closed, bidirectionally unique pairing'; Pattern = '(?i)\u53cc\u5411\u552f\u4e00|bidirectionally\s+unique|degree\s*=\s*1' }
        )
    )
}

$releaseNotesPath = Join-Path $RepoRoot "docs/archive/delivery/DELIVERY-NOTE-$Version.md"
$releaseNotes = $null
$latestVsix = $null
if (-not $MetadataOnly) {
    if (-not (Test-Path -LiteralPath $releaseNotesPath -PathType Leaf)) {
        Add-Failure "Delivery note is missing: $releaseNotesPath."
    } else {
        $releaseNotes = Read-Utf8Text $releaseNotesPath
        $deliveryRequirements = @(
            @{ Label = "release version '$Version'"; Pattern = $versionPattern },
            @{ Label = 'a VSIX artifact declaration'; Pattern = '(?i)VSIX\s+(artifact|\u4ea7\u7269)|VSIX.*BUILD' },
            @{ Label = 'verification performed'; Pattern = '(?i)verification\s+performed|\u9a8c\u8bc1(\u5df2\u6267\u884c|\u7ed3\u679c|\u95e8\u7981)' },
            @{ Label = 'user-visible behavior changes'; Pattern = '(?i)user-visible|behavior\s+changes|\u7528\u6237\u53ef\u89c1|\u884c\u4e3a\u53d8\u5316' },
            @{ Label = 'known limitations/non-goals'; Pattern = '(?i)known\s+(limitations|non-goals)|\u5df2\u77e5(\u9650\u5236|\u975e\u76ee\u6807)|\u4e0d\u80fd\u505a\u4ec0\u4e48' }
        )
        foreach ($requirement in $deliveryRequirements) {
            if ($releaseNotes -notmatch $requirement.Pattern) {
                Add-Failure "Delivery note is missing $($requirement.Label)."
            }
        }
    }

    $distDir = Join-Path $RepoRoot 'dist'
    $vsixPattern = "fossilsense-vscode-$($Version)_BUILD*.vsix"
    $vsixFiles = @()
    if (Test-Path -LiteralPath $distDir -PathType Container) {
        $vsixFiles = @(
            Get-ChildItem -LiteralPath $distDir -Filter $vsixPattern -File -ErrorAction SilentlyContinue |
                Sort-Object LastWriteTime -Descending
        )
    }
    if ($vsixFiles.Count -eq 0) {
        Add-Failure "No VSIX matching dist/$vsixPattern was found."
    } else {
        $latestVsix = $vsixFiles[0]
        $currentReleaseFingerprint = Get-ReleaseInputFingerprint $RepoRoot
        $artifactSha256 = Get-FileSha256 $latestVsix.FullName
        if ($null -ne $releaseNotes) {
            if ($releaseNotes -notmatch [regex]::Escape($latestVsix.Name)) {
                Add-Failure "Delivery note does not name the selected VSIX '$($latestVsix.Name)'."
            }
            if ($releaseNotes -notmatch "(?i)$([regex]::Escape($artifactSha256))") {
                Add-Failure "Delivery note does not record the selected VSIX SHA-256 '$artifactSha256'."
            }
            if ($releaseNotes -notmatch "(?i)$([regex]::Escape($currentReleaseFingerprint.Sha256))") {
                Add-Failure (
                    "Delivery note does not record the current release-input SHA-256 " +
                    "'$($currentReleaseFingerprint.Sha256)'."
                )
            }
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [System.IO.Compression.ZipFile]::OpenRead($latestVsix.FullName)
        $tempDirectory = $null
        $extractedBinary = $null
        try {
            $binaryEntry = $zip.Entries | Where-Object {
                $_.FullName.Replace('\', '/') -eq 'extension/bin/fossilsense.exe'
            } | Select-Object -First 1
            $bundleEntry = $zip.Entries | Where-Object {
                $_.FullName.Replace('\', '/') -eq 'extension/out/extension.js'
            } | Select-Object -First 1
            $packageEntry = $zip.Entries | Where-Object {
                $_.FullName.Replace('\', '/') -eq 'extension/package.json'
            } | Select-Object -First 1
            $buildEntry = $zip.Entries | Where-Object {
                $_.FullName.Replace('\', '/') -eq 'extension/bin/release-build.json'
            } | Select-Object -First 1

            if ($null -eq $binaryEntry -or $binaryEntry.Length -le 0) {
                Add-Failure "VSIX '$($latestVsix.FullName)' is missing a non-empty extension/bin/fossilsense.exe."
            }
            if ($null -eq $bundleEntry -or $bundleEntry.Length -le 0) {
                Add-Failure "VSIX '$($latestVsix.FullName)' is missing a non-empty extension/out/extension.js."
            }
            if ($null -eq $packageEntry) {
                Add-Failure "VSIX '$($latestVsix.FullName)' is missing extension/package.json."
            } else {
                $packagedMetadata = Read-ZipText $packageEntry | ConvertFrom-Json
                if ([string]$packagedMetadata.version -ne $Version) {
                    Add-Failure "VSIX package version is '$($packagedMetadata.version)', expected '$Version'."
                }
                if ([string]$packagedMetadata.name -ne [string]$extensionPackage.name) {
                    Add-Failure "VSIX package name '$($packagedMetadata.name)' does not match '$($extensionPackage.name)'."
                }
            }

            $packagedBuild = $null
            if ($null -eq $buildEntry -or $buildEntry.Length -le 0) {
                Add-Failure (
                    "VSIX '$($latestVsix.FullName)' is missing a non-empty " +
                    'extension/bin/release-build.json source fingerprint.'
                )
            } else {
                try {
                    $packagedBuild = Read-ZipText $buildEntry | ConvertFrom-Json
                } catch {
                    Add-Failure "VSIX release-build.json is not valid JSON."
                }
            }
            if ($null -ne $packagedBuild) {
                if ([int]$packagedBuild.schemaVersion -ne 1) {
                    Add-Failure "VSIX release-build.json has an unsupported schema version."
                }
                if ([string]$packagedBuild.packageVersion -ne $Version) {
                    Add-Failure (
                        "VSIX release-build.json package version is " +
                        "'$($packagedBuild.packageVersion)', expected '$Version'."
                    )
                }
                if ([string]$packagedBuild.releaseInputSha256 -ne
                    $currentReleaseFingerprint.Sha256) {
                    Add-Failure (
                        'VSIX release input fingerprint does not match the current source tree: ' +
                        "packaged='$($packagedBuild.releaseInputSha256)' " +
                        "current='$($currentReleaseFingerprint.Sha256)'."
                    )
                }
                if ([int]$packagedBuild.releaseInputFileCount -ne
                    $currentReleaseFingerprint.FileCount) {
                    Add-Failure (
                        'VSIX release input file count does not match the current source tree: ' +
                        "packaged='$($packagedBuild.releaseInputFileCount)' " +
                        "current='$($currentReleaseFingerprint.FileCount)'."
                    )
                }
                $actualBinarySha256 = if ($null -ne $binaryEntry) {
                    Get-ZipEntrySha256 $binaryEntry
                } else {
                    ''
                }
                $actualBundleSha256 = if ($null -ne $bundleEntry) {
                    Get-ZipEntrySha256 $bundleEntry
                } else {
                    ''
                }
                $actualManifestSha256 = if ($null -ne $packageEntry) {
                    Get-ZipEntrySha256 $packageEntry
                } else {
                    ''
                }
                if ([string]$packagedBuild.nativeBinarySha256 -ne $actualBinarySha256) {
                    Add-Failure 'VSIX staged native-binary SHA-256 does not match release-build.json.'
                }
                if ([string]$packagedBuild.extensionBundleSha256 -ne $actualBundleSha256) {
                    Add-Failure 'VSIX staged extension-bundle SHA-256 does not match release-build.json.'
                }
                if ([string]$packagedBuild.extensionManifestSha256 -ne $actualManifestSha256) {
                    Add-Failure 'VSIX staged extension-manifest SHA-256 does not match release-build.json.'
                }
                if ($actualBinarySha256 -and $actualBundleSha256 -and $actualManifestSha256) {
                    $actualPayloadSha256 = Get-ArtifactPayloadFingerprint `
                        -ReleaseInputSha256 ([string]$packagedBuild.releaseInputSha256) `
                        -NativeBinarySha256 $actualBinarySha256 `
                        -ExtensionBundleSha256 $actualBundleSha256 `
                        -ExtensionManifestSha256 $actualManifestSha256
                    if ([string]$packagedBuild.artifactPayloadSha256 -ne $actualPayloadSha256) {
                        Add-Failure 'VSIX aggregate payload SHA-256 does not match release-build.json.'
                    }
                }
                $packagedCommit = [string]$packagedBuild.sourceCommit
                if ([string]::IsNullOrWhiteSpace($packagedCommit)) {
                    Add-Failure 'VSIX release-build.json does not record a source commit state.'
                } elseif ($null -ne $releaseNotes -and
                    $releaseNotes -notmatch "(?i)$([regex]::Escape($packagedCommit))") {
                    Add-Failure (
                        "Delivery note does not record the packaged source commit '$packagedCommit'."
                    )
                }
            }

            if ($null -ne $binaryEntry -and $binaryEntry.Length -gt 0) {
                if ($env:OS -ne 'Windows_NT') {
                    Add-Failure 'The bundled Windows binary could not be executed because the hardening gate is not running on Windows.'
                } else {
                    $tempDirectory = Join-Path ([System.IO.Path]::GetTempPath()) (
                        'fossilsense-hardening-' + [guid]::NewGuid().ToString('N')
                    )
                    [System.IO.Directory]::CreateDirectory($tempDirectory) | Out-Null
                    $extractedBinary = Join-Path $tempDirectory 'fossilsense.exe'
                    [System.IO.Compression.ZipFileExtensions]::ExtractToFile(
                        $binaryEntry,
                        $extractedBinary,
                        $true
                    )
                    $binaryVersionOutput = @(& $extractedBinary --version 2>&1 | ForEach-Object {
                        $_.ToString()
                    })
                    if ($LASTEXITCODE -ne 0) {
                        Add-Failure "Bundled binary --version exited with code $LASTEXITCODE."
                    } else {
                        $binaryVersionLine = ($binaryVersionOutput -join "`n").Trim()
                        if ($binaryVersionLine -notmatch "(?m)^fossilsense\s+$versionPattern(?:\s|$)") {
                            Add-Failure "Bundled binary reports '$binaryVersionLine', expected fossilsense $Version."
                        }
                    }
                }
            }
        } finally {
            $zip.Dispose()
            if ($extractedBinary -and (Test-Path -LiteralPath $extractedBinary -PathType Leaf)) {
                Remove-Item -LiteralPath $extractedBinary -Force
            }
            if ($tempDirectory -and (Test-Path -LiteralPath $tempDirectory -PathType Container)) {
                Remove-Item -LiteralPath $tempDirectory -Force
            }
        }
    }
}

$parserVersionForReport = if ($null -ne $parserConstant) {
    "$($parserConstant.Name)=$($parserConstant.Value)"
} elseif ($null -ne $legacyParserVersion) {
    "legacy-default=$legacyParserVersion"
} else {
    'unavailable'
}
$resolverVersionForReport = if ($null -ne $resolverConstant) {
    "$($resolverConstant.Name)=$($resolverConstant.Value)"
} elseif ($null -ne $protocolConstant) {
    "legacy-protocol-coupled=$($protocolConstant.Value)"
} else {
    'unavailable'
}
$protocolVersionForReport = if ($null -ne $protocolConstant) {
    [string]$protocolConstant.Value
} else {
    'unavailable'
}
Write-Host (
    "Release metadata: version=$Version schema=$schemaVersion parser=$parserVersionForReport " +
    "resolver=$resolverVersionForReport relation_protocol=$protocolVersionForReport"
)

if ($failures.Count -gt 0) {
    Write-Host 'Release hardening verification failed:' -ForegroundColor Red
    foreach ($failure in $failures) {
        Write-Host "FAIL $failure"
    }
    exit 1
}

if ($MetadataOnly) {
    Write-Host "Release hardening metadata verification passed for v$Version (artifact checks skipped)." -ForegroundColor Green
} else {
    Write-Host "Release hardening verification passed for v$Version." -ForegroundColor Green
    Write-Host "Delivery note: $releaseNotesPath"
    if ($null -ne $latestVsix) {
        Write-Host "VSIX artifact: $($latestVsix.FullName)"
    }
}
