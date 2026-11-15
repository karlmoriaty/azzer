# 注入 MSVC 与 Windows SDK 环境变量
#
# 为什么需要这个：本机 Build Tools 装在
#   C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools
# 但没有注册到 VS Installer（vswhere -all 返回空），因此 rustc 无法自动探测
# link.exe，会报 "linker `link.exe` not found"。
#
# 本脚本手工拼出 PATH / INCLUDE / LIB，等价于 vcvars64.bat 的效果。
# 版本号自动探测，BuildTools 或 SDK 升级后无需改脚本。

$ErrorActionPreference = "Stop"

$btRoot = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
$sdkBase = "C:\Program Files (x86)\Windows Kits\10"

if (-not (Test-Path "$btRoot\VC\Tools\MSVC")) {
    throw "找不到 MSVC 工具链：$btRoot\VC\Tools\MSVC"
}

$msvcVer = Get-ChildItem "$btRoot\VC\Tools\MSVC" -Directory |
    Sort-Object { [version]($_.Name -replace '[^0-9.]', '') } -Descending |
    Select-Object -First 1
$msvcRoot = $msvcVer.FullName

$sdkVer = Get-ChildItem "$sdkBase\Lib" -Directory -ErrorAction SilentlyContinue |
    Sort-Object { [version]($_.Name -replace '[^0-9.]', '') } -Descending |
    Select-Object -First 1
if (-not $sdkVer) { throw "找不到 Windows SDK：$sdkBase\Lib" }

$env:PATH = "$msvcRoot\bin\Hostx64\x64;$sdkBase\bin\$($sdkVer.Name)\x64;$env:PATH"
$env:INCLUDE = "$msvcRoot\include;$sdkBase\Include\$($sdkVer.Name)\ucrt;$sdkBase\Include\$($sdkVer.Name)\um;$sdkBase\Include\$($sdkVer.Name)\shared;$sdkBase\Include\$($sdkVer.Name)\winrt"
$env:LIB = "$msvcRoot\lib\x64;$sdkBase\Lib\$($sdkVer.Name)\ucrt\x64;$sdkBase\Lib\$($sdkVer.Name)\um\x64"

if ($env:CARGO_TARGET_DIR -eq $null -or $env:CARGO_TARGET_DIR -eq "") {
    $env:CARGO_TARGET_DIR = "D:\azzer-target"
}

Write-Host "MSVC  : $($msvcVer.Name)"
Write-Host "SDK   : $($sdkVer.Name)"
Write-Host "TARGET: $env:CARGO_TARGET_DIR"
