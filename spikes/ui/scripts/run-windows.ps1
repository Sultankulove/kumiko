# Runs the UI spike natively on Windows: builds the prototypes, measures cold start, scrolling
# and typing, the key-to-pixel latency (latency-probe) and checks UI Automation (uia-check).
# Results are JSON lines in target\spike-report-windows.jsonl.
param(
    [string[]]$Toolkits = @("iced", "gpui"),
    [string]$Label = "windows",
    [int]$Samples = 100
)
$ErrorActionPreference = "Continue"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$report = Join-Path $root "target\spike-report-windows.jsonl"

foreach ($package in @("latency-probe") + ($Toolkits | ForEach-Object { "$_-proto" })) {
    cargo build -q --release -p $package
    if ($LASTEXITCODE -ne 0) { Write-Output "build of $package failed"; exit 1 }
}

function Invoke-Bench($exe, $bench) {
    $env:SPIKE_BENCH = $bench
    $env:SPIKE_LABEL = $Label
    $env:SPIKE_REPORT = $report
    $env:SPIKE_T0 = [string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000)
    & $exe | Select-Object -Last 1
}

foreach ($toolkit in $Toolkits) {
    $exe = Join-Path $root "target\release\$toolkit-proto.exe"
    foreach ($bench in @("start", "scroll", "type")) {
        Invoke-Bench $exe $bench
    }
    # Key-to-pixel and accessibility need the interactive window.
    $env:SPIKE_BENCH = "probe"
    $process = Start-Process -FilePath $exe -PassThru -WindowStyle Maximized
    Start-Sleep -Seconds 4
    $env:SPIKE_LABEL = $Label
    $probe = & (Join-Path $root "target\release\latency-probe.exe") "Kumiko UI spike: $toolkit" $Samples
    Write-Output $probe
    Add-Content -Path $report -Value $probe
    $uia = powershell -NoProfile -File (Join-Path $PSScriptRoot "uia-check.ps1") -Title "Kumiko UI spike: $toolkit"
    Write-Output $uia
    Add-Content -Path $report -Value $uia
    Stop-Process -Id $process.Id -ErrorAction SilentlyContinue
}
