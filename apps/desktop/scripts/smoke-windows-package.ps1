param(
    [Parameter(Mandatory = $true)]
    [string]$BundleRoot,

    [Parameter(Mandatory = $false)]
    [int]$TimeoutSeconds = 90
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $BundleRoot -PathType Container)) {
    [Console]::Error.WriteLine("Tauri bundle directory not found: $BundleRoot")
    exit 2
}

$resolvedBundleRoot = (Resolve-Path -LiteralPath $BundleRoot).Path
$installer = Get-ChildItem -LiteralPath $resolvedBundleRoot -Recurse -File -Filter '*.exe' |
    Where-Object { $_.Name -notmatch '(?i)uninstall|unins' } |
    Sort-Object FullName |
    Select-Object -First 1

if ($null -eq $installer) {
    [Console]::Error.WriteLine("No bundled Windows installer executable found under $resolvedBundleRoot")
    exit 2
}

$installRoot = Join-Path ([System.IO.Path]::GetTempPath()) "hitch-packaged-smoke-$PID"
$installerProcess = $null
$appProcess = $null

function Stop-ProcessTree {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process
    )

    try {
        $taskkill = Join-Path $env:SystemRoot 'System32\taskkill.exe'
        if (Test-Path -LiteralPath $taskkill -PathType Leaf) {
            $killer = [System.Diagnostics.ProcessStartInfo]::new()
            $killer.FileName = $taskkill
            $killer.UseShellExecute = $false
            $killer.Arguments = "/PID $($Process.Id) /T /F"
            $killerProcess = [System.Diagnostics.Process]::Start($killer)
            if ($null -ne $killerProcess) {
                $killerProcess.WaitForExit()
                $killerProcess.Dispose()
            }
        } else {
            $Process.Kill()
        }
        $Process.WaitForExit()
    } catch {
        [Console]::Error.WriteLine("Failed to kill timed-out process $($Process.Id): $_")
    }
}

function Wait-OrTimeout {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process,

        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if (-not $Process.WaitForExit($TimeoutSeconds * 1000)) {
        [Console]::Error.WriteLine("$Description timed out after $TimeoutSeconds seconds")
        Stop-ProcessTree -Process $Process
        exit 124
    }

    if ($Process.ExitCode -ne 0) {
        [Console]::Error.WriteLine("$Description failed with exit code $($Process.ExitCode)")
        exit $Process.ExitCode
    }
}

try {
    if (Test-Path -LiteralPath $installRoot) {
        Remove-Item -LiteralPath $installRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $installRoot | Out-Null

    Write-Host "Installing bundled package: $($installer.FullName)"
    $installInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $installInfo.FileName = $installer.FullName
    $installInfo.UseShellExecute = $false
    $installInfo.ArgumentList.Add('/S')
    $installInfo.ArgumentList.Add("/D=$installRoot")

    $installerProcess = [System.Diagnostics.Process]::Start($installInfo)
    if ($null -eq $installerProcess) {
        [Console]::Error.WriteLine("Failed to launch bundled installer: $($installer.FullName)")
        exit 2
    }
    Wait-OrTimeout -Process $installerProcess -Description "Bundled installer"

    $appExe = Get-ChildItem -LiteralPath $installRoot -Recurse -File -Filter '*.exe' |
        Where-Object { $_.BaseName -in @('Hitch', 'hitch-desktop') } |
        Sort-Object FullName |
        Select-Object -First 1

    if ($null -eq $appExe) {
        [Console]::Error.WriteLine("Packaged app executable not found after installing $($installer.FullName) into $installRoot")
        exit 2
    }

    Write-Host "Smoke-testing installed packaged app: $($appExe.FullName)"
    $appInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $appInfo.FileName = $appExe.FullName
    $appInfo.UseShellExecute = $false
    $appInfo.Environment['HITCH_PACKAGED_SMOKE_TEST'] = '1'
    $appInfo.Environment['HITCH_INSTANCE'] = 'ci-smoke'

    $appProcess = [System.Diagnostics.Process]::Start($appInfo)
    if ($null -eq $appProcess) {
        [Console]::Error.WriteLine("Failed to launch packaged app: $($appExe.FullName)")
        exit 2
    }
    Wait-OrTimeout -Process $appProcess -Description "Packaged smoke test"
} finally {
    if ($null -ne $appProcess) {
        $appProcess.Dispose()
    }
    if ($null -ne $installerProcess) {
        $installerProcess.Dispose()
    }
    if (Test-Path -LiteralPath $installRoot) {
        Remove-Item -LiteralPath $installRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
