# Cold start of a prototype under different graphics settings (Windows). Each variant runs
# `Runs` times; wgpu reads WGPU_BACKEND / WGPU_POWER_PREF / WGPU_ADAPTER_NAME.
param([string]$Toolkit = "iced", [int]$Runs = 5)
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\release\$Toolkit-proto.exe"
$variants = [ordered]@{
    "default"      = @{}
    "dx12"         = @{ WGPU_BACKEND = "dx12" }
    "vulkan"       = @{ WGPU_BACKEND = "vulkan" }
    "dx12-low"     = @{ WGPU_BACKEND = "dx12"; WGPU_POWER_PREF = "low" }
    "dx12-high"    = @{ WGPU_BACKEND = "dx12"; WGPU_POWER_PREF = "high" }
}
foreach ($name in $variants.Keys) {
    foreach ($key in "WGPU_BACKEND", "WGPU_POWER_PREF", "WGPU_ADAPTER_NAME") {
        Remove-Item "Env:$key" -ErrorAction SilentlyContinue
    }
    foreach ($pair in $variants[$name].GetEnumerator()) {
        Set-Item "Env:$($pair.Key)" $pair.Value
    }
    $times = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $env:SPIKE_BENCH = "start"
        $env:SPIKE_LABEL = $name
        $env:SPIKE_T0 = [string]([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() * 1000)
        $line = & $exe | Select-Object -Last 1
        $times += ($line | ConvertFrom-Json).cold_start_ms
    }
    Write-Output ("{0,-10} {1}" -f $name, ($times -join ", "))
}
