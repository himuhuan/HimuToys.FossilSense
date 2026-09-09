# Process execution and sampling only; acceptance policy lives in benchmark_gate_helpers.ps1.
function Quote-ProcessArgument([string]$Value) {
    return '"' + $Value.Replace('\', '\').Replace('"', '\"') + '"'
}
function Invoke-SampledProcess {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$ArgumentList,
        [Parameter(Mandatory = $true)][int]$Timeout
    )

    $quotedArguments = ($ArgumentList | ForEach-Object { Quote-ProcessArgument $_ }) -join ' '
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = $quotedArguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    if (-not $process.Start()) {
        throw "failed to start benchmark process"
    }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $peakWorkingSet = 0L
    $peakPrivateBytes = 0L
    $timedOut = $false

    try {
        while (-not $process.HasExited) {
            if ($stopwatch.Elapsed.TotalSeconds -gt $Timeout) {
                $timedOut = $true
                $process.Kill()
                throw "benchmark process exceeded ${Timeout}s"
            }
            $process.Refresh()
            $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.WorkingSet64)
            $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $process.PrivateMemorySize64)
            Start-Sleep -Milliseconds 20
        }
        $process.WaitForExit()
        $process.Refresh()
        $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
        $peakPrivateBytes = [Math]::Max($peakPrivateBytes, $process.PrivateMemorySize64)
        $stopwatch.Stop()
        $stdout = @($stdoutTask.Result -split "`r?`n")
        $stderr = @($stderrTask.Result -split "`r?`n")
        if ($process.ExitCode -ne 0) {
            $tail = ($stderr | Select-Object -Last 12) -join [Environment]::NewLine
            throw "benchmark process exited with $($process.ExitCode): $tail"
        }
        return [pscustomobject]@{
            ElapsedMs = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
            PeakWorkingSetBytes = $peakWorkingSet
            PeakPrivateBytes = $peakPrivateBytes
            Stdout = $stdout
        }
    }
    catch {
        $message = $_.Exception.Message
        if (-not $process.HasExited) { $process.Kill() }
        # A failed run is still evidence. Bound shutdown/output collection;
        # do not report the time used to save diagnostics as engine runtime.
        $stopwatch.Stop()
        [void]$process.WaitForExit(5000)
        $failure = [System.Exception]::new($message)
        $failure.Data['benchmark_sample'] = [pscustomobject]@{
            status = if ($timedOut) { 'timeout' } else { 'failed' }
            ElapsedMs = [Math]::Round($stopwatch.Elapsed.TotalMilliseconds, 3)
            PeakWorkingSetBytes = $peakWorkingSet
            PeakPrivateBytes = $peakPrivateBytes
            Stdout = if ($stdoutTask.IsCompleted) { @($stdoutTask.Result -split "`r?`n") } else { @() }
            Stderr = if ($stderrTask.IsCompleted) { @($stderrTask.Result -split "`r?`n") } else { @() }
        }
        throw $failure
    }
    finally {
        if ($process -and -not $process.HasExited) {
            $process.Kill()
        }
        if ($process) {
            $process.Dispose()
        }
    }
}
