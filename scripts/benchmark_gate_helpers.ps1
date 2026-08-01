function Assert-FullIndexPerformanceGate {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][string]$CaseId,
        [Parameter(Mandatory = $true)][double]$OuterElapsedMs,
        [Parameter(Mandatory = $true)][double]$EngineElapsedMs
    )

    if ($CaseId -notlike '*-full-index') {
        return
    }
    $limitMs = 120000.0
    if ($OuterElapsedMs -gt $limitMs) {
        throw "$CaseId outer elapsed $OuterElapsedMs ms exceeded the 120,000 ms full-index gate"
    }
    if ($EngineElapsedMs -gt $limitMs) {
        throw "$CaseId engine elapsed $EngineElapsedMs ms exceeded the 120,000 ms full-index gate"
    }
}
