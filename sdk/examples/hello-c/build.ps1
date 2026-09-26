# Builds kumiko.hello-c into <out-dir>\plugin.wasm with wasi-sdk (WASI_SDK_PATH, by default
# ~\.local\wasi-sdk). The Windows twin of build.sh.
param([string]$Out = (Join-Path $PSScriptRoot 'target'))
$ErrorActionPreference = 'Stop'
$sdk = if ($env:WASI_SDK_PATH) { $env:WASI_SDK_PATH } else { Join-Path $HOME '.local\wasi-sdk' }
$clang = Join-Path $sdk 'bin\clang.exe'
if (-not (Test-Path $clang)) {
    Write-Error "wasi-sdk not found at $sdk; set WASI_SDK_PATH"
    exit 2
}
New-Item -ItemType Directory -Force $Out | Out-Null
$bindings = Join-Path $PSScriptRoot 'bindings'
& $clang --target=wasm32-wasip2 -O2 -mexec-model=reactor -Wall -Wextra -Werror `
    -I $bindings -o (Join-Path $Out 'plugin.wasm') `
    (Join-Path $PSScriptRoot 'src\hello.c') (Join-Path $bindings 'kumiko_plugin.c') `
    (Join-Path $bindings 'kumiko_plugin_component_type.o')
exit $LASTEXITCODE
