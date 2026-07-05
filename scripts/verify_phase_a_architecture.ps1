$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$Failures = New-Object System.Collections.Generic.List[string]

function Add-Failure([string]$Message) {
    $Failures.Add($Message) | Out-Null
}

function Read-RequiredFile([string]$Path) {
    $FullPath = Join-Path $RepoRoot $Path
    if (-not (Test-Path -LiteralPath $FullPath -PathType Leaf)) {
        Add-Failure "Missing required file: $Path"
        return $null
    }

    return Get-Content -LiteralPath $FullPath -Raw
}

function Assert-Contains([string]$Path, [string]$Content, [string[]]$Patterns) {
    if ($null -eq $Content) {
        return
    }

    foreach ($Pattern in $Patterns) {
        if ($Content -notmatch $Pattern) {
            Add-Failure "File $Path is missing required content matching: $Pattern"
        }
    }
}

$ArchitecturePath = 'docs/architecture/README.md'
$Architecture = Read-RequiredFile $ArchitecturePath
Assert-Contains $ArchitecturePath $Architecture @(
    'VS Code extension',
    'LSP',
    'single Rust binary',
    'parser',
    'store',
    'read model',
    'resolver',
    'ordinary completion',
    'startup flow',
    'full index flow',
    'dirty index flow',
    'query flow',
    'ordinary completion flow',
    'best-effort candidate',
    'confidence',
    'fallback',
    'ambiguity',
    'open scope',
    'cache invalidation',
    'v1\.2\.2',
    'behavior-preserving',
    'docs/architecture/adr/',
    'risk-register\.md',
    'regression-checklist\.md',
    'import-inventory\.md'
)

$AdrFiles = @(
    'docs/architecture/adr/0001-best-effort-candidate-model.md',
    'docs/architecture/adr/0002-sqlite-index-and-read-models.md',
    'docs/architecture/adr/0003-scope-confidence-reason-projection.md',
    'docs/architecture/adr/0004-cache-generation-and-snapshots.md',
    'docs/architecture/adr/0005-v122-behavior-preserving-release.md'
)

foreach ($AdrPath in $AdrFiles) {
    $Adr = Read-RequiredFile $AdrPath
    Assert-Contains $AdrPath $Adr @(
        'Status: Accepted',
        'Context',
        'Decision',
        'Consequences'
    )
}

$BestEffortAdr = Read-RequiredFile 'docs/architecture/adr/0001-best-effort-candidate-model.md'
Assert-Contains 'docs/architecture/adr/0001-best-effort-candidate-model.md' $BestEffortAdr @(
    'DefinitionCandidate',
    'ScopeTier',
    'ResolutionConfidence',
    'ResolutionReason',
    'ReachScope',
    'OpenReason',
    'Occurrence',
    'ReferenceHit',
    'best-effort candidates',
    'MUST NOT describe heuristic name matches as compile-accurate semantic bindings'
)

$ReadModelAdr = Read-RequiredFile 'docs/architecture/adr/0002-sqlite-index-and-read-models.md'
Assert-Contains 'docs/architecture/adr/0002-sqlite-index-and-read-models.md' $ReadModelAdr @(
    'SQLite',
    'durable index',
    'NameTable',
    'ReachGraph',
    'IncludeCompletionTable',
    'hot request paths',
    'direct SQLite access'
)

$ProjectionAdr = Read-RequiredFile 'docs/architecture/adr/0003-scope-confidence-reason-projection.md'
Assert-Contains 'docs/architecture/adr/0003-scope-confidence-reason-projection.md' $ProjectionAdr @(
    'confidence',
    'reason',
    'ambiguity',
    'fallback',
    'semantic binding'
)

$SnapshotAdr = Read-RequiredFile 'docs/architecture/adr/0004-cache-generation-and-snapshots.md'
Assert-Contains 'docs/architecture/adr/0004-cache-generation-and-snapshots.md' $SnapshotAdr @(
    'WorkspaceGeneration',
    'WorkspaceSnapshot',
    'per-request',
    'Arc',
    'cache invalidation',
    'open scope'
)

$ReleaseAdr = Read-RequiredFile 'docs/architecture/adr/0005-v122-behavior-preserving-release.md'
Assert-Contains 'docs/architecture/adr/0005-v122-behavior-preserving-release.md' $ReleaseAdr @(
    'v1\.2\.2',
    'behavior-preserving architecture health release',
    'MUST NOT intentionally change',
    'navigation',
    'completion',
    'coloring',
    'references',
    'configuration',
    'privacy',
    'VSIX'
)

$RiskPath = 'docs/architecture/risk-register.md'
$Risk = Read-RequiredFile $RiskPath
Assert-Contains $RiskPath $Risk @(
    'Risk',
    'User impact',
    'Likelihood',
    'Mitigation',
    'Owner',
    'ordinary completion',
    'ordering',
    'fallback',
    'history boost',
    'metrics',
    'hot-path performance',
    'workspace cache invalidation',
    'stale read models',
    'completion memo',
    'reference cache',
    'lock discipline',
    'documentation drift',
    'scope creep',
    'VSIX packaging'
)

$ChecklistPath = 'docs/architecture/regression-checklist.md'
$Checklist = Read-RequiredFile $ChecklistPath
Assert-Contains $ChecklistPath $Checklist @(
    'indexing/status',
    'definition',
    'ordinary completion',
    'include completion',
    'member completion',
    'references',
    'semantic coloring',
    'hover',
    'signature',
    'configuration',
    'conflicts',
    'privacy',
    'VSIX packaging',
    'isIncomplete=true',
    'short-prefix',
    'truncation',
    'prefix narrowing',
    'evidence-aware ranking',
    'history boost',
    'raw text fallback',
    'no per-keystroke SQLite'
)

$InventoryPath = 'docs/architecture/import-inventory.md'
$Inventory = Read-RequiredFile $InventoryPath
Assert-Contains $InventoryPath $Inventory @(
    'Rust module import inventory',
    'VS Code extension import inventory',
    'crates/fossilsense/src/server.rs',
    'crates/fossilsense/src/store.rs',
    'crates/fossilsense/src/parser.rs',
    'crates/fossilsense/src/resolver.rs',
    'crates/fossilsense/src/completion.rs',
    'extensions/vscode/src/extension.ts',
    'tower_lsp',
    'rusqlite',
    'Phase B fitness allowlist',
    'Generated'
)

if ($Failures.Count -gt 0) {
    Write-Host "Phase A architecture verification failed:" -ForegroundColor Red
    foreach ($Failure in $Failures) {
        Write-Host " - $Failure" -ForegroundColor Red
    }
    exit 1
}

Write-Host "Phase A architecture verification passed." -ForegroundColor Green
