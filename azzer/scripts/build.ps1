# 编译包装：先注入 MSVC 环境，再执行 cargo
#
# 用法：
#   .\scripts\build.ps1              # cargo build
#   .\scripts\build.ps1 check        # cargo check
#   .\scripts\build.ps1 test         # cargo test
#   .\scripts\build.ps1 run          # cargo run
#
# 不传参数时等价于 build。

param([string]$Command = "build", [string[]]$Rest)

$ErrorActionPreference = "Stop"
. "$PSScriptRoot\msvc-env.ps1"

$cargo = "E:\C_Data_Migration\Users\.cargo\bin\cargo.exe"
if (-not (Test-Path $cargo)) { $cargo = "cargo" }

Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    & $cargo $Command @Rest
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
