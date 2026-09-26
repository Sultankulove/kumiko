# Where the cold start of the iced prototype goes: the full view, the editor only, an empty
# canvas (toolkit and GPU set-up only).
param([int]$Runs = 4)
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\release\iced-proto.exe"
foreach ($view in "full", "editor", "empty") {
    $env:SPIKE_VIEW = $view
    $times = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $env:SPIKE_BENCH = "start"
        $env:SPIKE_T0 = [string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000)
        $times += ((& $exe | Select-Object -Last 1) | ConvertFrom-Json).cold_start_ms
    }
    Write-Output ("{0,-7} {1}" -f $view, ($times -join ", "))
}
Remove-Item Env:SPIKE_VIEW
