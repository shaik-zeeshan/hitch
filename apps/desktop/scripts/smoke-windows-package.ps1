param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$ExePath,

    [Parameter(Mandatory = $false)]
    [int]$TimeoutSeconds = 90
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
    [Console]::Error.WriteLine("Packaged app executable not found: $ExePath")
    exit 2
}

$resolvedExe = (Resolve-Path -LiteralPath $ExePath).Path
$process = $null
try {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $resolvedExe
    $startInfo.UseShellExecute = $false
    $startInfo.Environment['HITCH_PACKAGED_SMOKE_TEST'] = '1'
    $startInfo.Environment['HITCH_INSTANCE'] = 'ci-smoke'

    $process = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $process) {
        [Console]::Error.WriteLine("Failed to launch packaged app: $resolvedExe")
        exit 2
    }

    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        [Console]::Error.WriteLine("Packaged smoke test timed out after $TimeoutSeconds seconds: $resolvedExe")
        try {
            $taskkill = Join-Path $env:SystemRoot 'System32\taskkill.exe'
            if (Test-Path -LiteralPath $taskkill -PathType Leaf) {
                $killer = [System.Diagnostics.ProcessStartInfo]::new()
                $killer.FileName = $taskkill
                $killer.UseShellExecute = $false
                $killer.Arguments = "/PID $($process.Id) /T /F"
                $killerProcess = [System.Diagnostics.Process]::Start($killer)
                if ($null -ne $killerProcess) {
                    $killerProcess.WaitForExit()
                    $killerProcess.Dispose()
                }
            } else {
                $process.Kill()
            }
            $process.WaitForExit()
        } catch {
            [Console]::Error.WriteLine("Failed to kill timed-out packaged app process $($process.Id): $_")
        }
        exit 124
    }

    exit $process.ExitCode
} finally {
    if ($null -ne $process) {
        $process.Dispose()
    }
}
