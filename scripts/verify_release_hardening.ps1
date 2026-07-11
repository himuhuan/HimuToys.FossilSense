param(
    [string]$Version = "1.3.4"
)

$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$failures = New-Object System.Collections.Generic.List[string]

function Add-Failure {
    param([string]$Message)
    $failures.Add($Message) | Out-Null
}

function Read-JsonFile {
    param([string]$Path)
    Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
}

$extensionPackagePath = Join-Path $repoRoot "extensions/vscode/package.json"
$cargoTomlPath = Join-Path $repoRoot "crates/fossilsense/Cargo.toml"
$releaseNotesPath = Join-Path $repoRoot "dist/DELIVERY-NOTE-$Version.md"

$extensionPackage = Read-JsonFile $extensionPackagePath
if ($extensionPackage.version -ne $Version) {
    Add-Failure "extensions/vscode/package.json version is '$($extensionPackage.version)', expected '$Version'."
}

$cargoToml = Get-Content -Raw -LiteralPath $cargoTomlPath
if ($cargoToml -notmatch "(?m)^version\s*=\s*`"$([regex]::Escape($Version))`"") {
    Add-Failure "crates/fossilsense/Cargo.toml package version does not match '$Version'."
}

if (-not (Test-Path -LiteralPath $releaseNotesPath)) {
    Add-Failure "Release notes are missing: $releaseNotesPath."
} else {
    $releaseNotes = Get-Content -Raw -LiteralPath $releaseNotesPath
    $requiredPhrases = @(
        "behavior-preserving",
        "Verification performed",
        "VSIX artifact",
        "Unchanged capabilities",
        "Known non-goals"
    )
    foreach ($phrase in $requiredPhrases) {
        if ($releaseNotes -notlike "*$phrase*") {
            Add-Failure "Release notes are missing required phrase '$phrase'."
        }
    }
}

$distDir = Join-Path $repoRoot "dist"
$vsixPattern = "fossilsense-vscode-$($Version)_BUILD*.vsix"
$vsixFiles = @(Get-ChildItem -LiteralPath $distDir -Filter $vsixPattern -File -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending)
if ($vsixFiles.Count -eq 0) {
    Add-Failure "No VSIX matching dist/$vsixPattern was found."
} else {
    $latestVsix = $vsixFiles[0]
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($latestVsix.FullName)
    try {
        $binaryEntry = $zip.Entries | Where-Object { $_.FullName -eq "extension/bin/fossilsense.exe" } | Select-Object -First 1
        if ($null -eq $binaryEntry) {
            Add-Failure "VSIX '$($latestVsix.FullName)' does not contain extension/bin/fossilsense.exe."
        } elseif ($binaryEntry.Length -le 0) {
            Add-Failure "VSIX '$($latestVsix.FullName)' contains an empty extension/bin/fossilsense.exe."
        }
    } finally {
        $zip.Dispose()
    }
}

if ($failures.Count -gt 0) {
    Write-Host "Release hardening verification failed:" -ForegroundColor Red
    foreach ($failure in $failures) {
        Write-Host "FAIL $failure"
    }
    exit 1
}

Write-Host "Release hardening verification passed for v$Version." -ForegroundColor Green
if ($vsixFiles.Count -gt 0) {
    Write-Host "VSIX artifact: $($vsixFiles[0].FullName)"
}
