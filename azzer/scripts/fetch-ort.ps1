# Fetch onnxruntime native library (Windows x64)
#
# Why not use ort's download-binaries: it pulls binaries from GitHub Releases,
# which is unreachable from this machine - the build script hangs indefinitely
# (observed: 15 minutes with zero output, not even an `output` file).
#
# Instead: download the onnxruntime wheel from a domestic pip mirror, extract
# onnxruntime.dll into native/onnxruntime/, and let ort's load-dynamic feature
# load it at runtime.
#
# VERSION CONSTRAINT (2026-11-08):
#   ort 2.0.0-rc.15 requires ONNX Runtime >= 1.31.x
#   onnxruntime-directml is stuck at 1.24.4  -> incompatible
#   so we use the CPU build (onnxruntime 1.31.x) for now.
#   GPU path (DirectML/CUDA) is tracked as a separate task.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-ort.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-ort.ps1 -Force
#
# Output: native/onnxruntime/onnxruntime.dll (~30MB, git-ignored)
#
# NOTE: ASCII only. PowerShell 5.1 reads scripts as ANSI (GBK here); UTF-8
# Chinese text gets mangled and can break quote pairing.

param([switch]$Force)

$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$destDir = Join-Path $root "native\onnxruntime"
# 下载与解压全部落在工作目录内，不写系统盘或其他盘
$workspace = Split-Path $root -Parent
$wheelDir = Join-Path $workspace ".cache\ort-wheel"
$tmpDir = Join-Path $workspace ".cache\ort-extract"

$py = "C:\Users\asus\AppData\Local\Programs\Python\Python312\python.exe"
if (-not (Test-Path $py)) { $py = "python" }

New-Item -ItemType Directory -Force -Path $destDir, $wheelDir, $tmpDir | Out-Null

$existing = Get-ChildItem $destDir -Filter "onnxruntime.dll" -ErrorAction SilentlyContinue
if ($existing -and -not $Force) {
    $mb = [math]::Round($existing.Length / 1MB, 1)
    Write-Host "[skip] already present: $($existing.FullName) ($mb MB). Use -Force to replace."
    exit 0
}

# Pick the newest onnxruntime (CPU) wheel; version must satisfy ort's requirement.
Write-Host "[1/3] downloading onnxruntime wheel (aliyun mirror)"
& $py -m pip download onnxruntime --no-deps --only-binary=:all: `
    --platform win_amd64 --python-version 3.12 `
    -d $wheelDir -i https://mirrors.aliyun.com/pypi/simple/
if ($LASTEXITCODE -ne 0) { throw "pip download failed (exit=$LASTEXITCODE)" }

$whl = Get-ChildItem $wheelDir -Filter "onnxruntime-*.whl" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $whl) { throw "no onnxruntime wheel found in $wheelDir" }
$whlMb = [math]::Round($whl.Length / 1MB, 1)
Write-Host "      wheel: $($whl.Name) ($whlMb MB)"

Write-Host "[2/3] extracting"
$zip = Join-Path $tmpDir ($whl.BaseName + ".zip")
Copy-Item $whl.FullName $zip -Force
Expand-Archive -Path $zip -DestinationPath $tmpDir -Force

$dll = Get-ChildItem $tmpDir -Recurse -Filter "onnxruntime.dll" | Select-Object -First 1
if (-not $dll) { throw "onnxruntime.dll not found inside wheel" }

Write-Host "[3/3] deploying"
Copy-Item $dll.FullName (Join-Path $destDir "onnxruntime.dll") -Force

$final = Get-Item (Join-Path $destDir "onnxruntime.dll")
$finalMb = [math]::Round($final.Length / 1MB, 1)
Write-Host "[done] $($final.FullName) ($finalMb MB)"
Write-Host "       ort loads it via ORT_DYLIB_PATH; cargo-run.cjs already points there."
