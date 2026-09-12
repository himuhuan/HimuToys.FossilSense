function Get-VerificationPlan {
    param([string]$Profile, [string]$Scope, [string]$TestFilter, [string]$CaseFilter, [bool]$SkipInstall)
    $steps = [System.Collections.Generic.List[object]]::new()
    function Add-Step($command, $arguments, $directory = '.') {
        $steps.Add(@{ command = $command; arguments = @($arguments); directory = $directory })
    }
    if ($Profile -eq 'Performance') {
        if (-not $CaseFilter) { throw 'Performance requires an explicit -CaseFilter; select only affected scenarios.' }
        if ($CaseFilter -eq 'relation-semantic-foundation') {
            Add-Step powershell @('-NoProfile', '-File', 'scripts/benchmark_relation_foundation.ps1')
            return $steps.ToArray()
        }

        Add-Step cargo @('build', '--release', '-p', 'fossilsense')
        Add-Step cargo @('test', '--release', '-p', 'fossilsense', '--bin', 'fossilsense', '--no-run')
        Add-Step powershell @('-NoProfile', '-File', 'scripts/benchmark_large_workspace.ps1', '-Repeats', '1', '-IncludeFullIndex', '-IncludeEngineHydration', '-IncludeCompletionReplay', '-IncludeLspLifecycle', '-IncludeBindingReplay', '-IncludeCacheReplay', '-IncludeV142SemanticCases', '-CaseFilter', $CaseFilter)
        return $steps.ToArray()
    }
    if ($Profile -eq 'Local' -and $Scope -eq 'Rust' -and -not $TestFilter) {
        throw 'Local Rust verification requires -TestFilter. Use Merge for all tests.'
    }
    if ($Profile -eq 'Merge' -or $Scope -eq 'Rust') {
        Add-Step cargo @('fmt', '--all', '--', '--check')
        if ($Profile -eq 'Merge') { Add-Step cargo @('clippy', '-p', 'fossilsense', '--all-targets', '--', '-D', 'warnings') }
        $arguments = @('test', '-p', 'fossilsense')
        if ($Profile -eq 'Local') { $arguments += $TestFilter }
        Add-Step cargo $arguments
    }
    if ($Profile -eq 'Merge' -or $Scope -eq 'Scripts') {
        foreach ($script in @('test_architecture_fitness.js', 'test_c_frontend_conformance.mjs', 'test_c_frontend_clang.mjs', 'architecture_fitness.js')) { Add-Step node @("scripts/$script") }
        foreach ($script in @('test_verification_plan.ps1', 'test_release_hardening.ps1', 'test_benchmark_entrypoints.ps1', 'test_memory_stability_policy.ps1')) {
            Add-Step powershell @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "scripts/$script")
        }
    }
    if ($Profile -eq 'Merge' -or $Scope -eq 'Extension') {
        if (-not $SkipInstall) { Add-Step pnpm @('install', '--frozen-lockfile') 'extensions/vscode' }
        Add-Step pnpm @('test') 'extensions/vscode'
    }
    return $steps.ToArray()
}
