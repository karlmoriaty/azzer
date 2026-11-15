# Fetch CUDA 12.x + cuDNN 9 runtime libraries for ONNX Runtime GPU
#
# ALL output stays inside the workspace. Nothing is installed into system
# directories, no registry is touched, no PATH environment variable is modified
# permanently. The DLLs are loaded at runtime by prepending their directory to
# the process PATH (done in Rust, see azzer-api/src/state.rs).
#
# Why pip wheels instead of the NVIDIA installer:
#   - the installer writes to C:\Program Files and requires admin
#   - the wheels contain exactly the DLLs ONNX Runtime needs, and pip mirrors
#     are reachable from this machine
#
# Packages:
#   onnxruntime-gpu            -> onnxruntime.dll + onnxruntime_providers_cuda.dll
#   nvidia-cuda-runtime-cu12   -> cudart64_12.dll
#   nvidia-cublas-cu12         -> cublas64_12.dll, cublasLt64_12.dll
#   nvidia-cudnn-cu12          -> cudnn64_9.dll and friends
#   nvidia-cufft-cu12          -> cufft64_11.dll
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-cuda.ps1
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-cuda.ps1 -Force
#
# Output:
#   native/onnxruntime/  onnxruntime.dll + providers (replaces the CPU build)
#   native/cuda/bin/     CUDA + cuDNN runtime DLLs (~1.2 GB)
#
# NOTE: ASCII only. PowerShell 5.1 reads scripts as ANSI (GBK here).

param([switch]$Force)

$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$workspace = Split-Path $root -Parent
$ortDir = Join-Path $root "native\onnxruntime"
$cudaDir = Join-Path $root "native\cuda\bin"
$wheelDir = Join-Path $workspace ".cache\cuda-wheel"
$tmpDir = Join-Path $workspace ".cache\cuda-extract"

$py = "C:\Users\asus\AppData\Local\Programs\Python\Python312\python.exe"
if (-not (Test-Path $py)) { $py = "python" }

New-Item -ItemType Directory -Force -Path $ortDir, $cudaDir, $wheelDir, $tmpDir | Out-Null

$marker = Join-Path $cudaDir "cudart64_12.dll"
if ((Test-Path $marker) -and -not $Force) {
    Write-Host "[skip] CUDA runtime already deployed. Use -Force to replace."
    exit 0
}

$packages = @(
    "onnxruntime-gpu",
    "nvidia-cuda-runtime-cu12",
    "nvidia-cublas-cu12",
    "nvidia-cudnn-cu12",
    "nvidia-cufft-cu12"
)

Write-Host "[1/4] downloading packages (aliyun mirror)"
foreach ($p in $packages) {
    # 幂等：已下载的 wheel 直接复用。上一次在本机跑到 cufft 时网络超时中断，
    # 若不复用，前四个包（约 1.4GB）要重下一遍。
    # 注意：用 -like 而不是 Get-ChildItem -Filter，后者在本机 FAT32 上
    # 对带下划线的通配符会误匹配，导致"cufft 已缓存"的假阳性。
    $pattern = ($p -replace "-", "_") + "*.whl"
    $cached = Get-ChildItem $wheelDir -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like $pattern }
    if ($cached) {
        $mb = [math]::Round(($cached | Measure-Object Length -Sum).Sum / 1MB, 1)
        Write-Host "      - $p (cached: $($cached.Name -join ','), $mb MB)"
        continue
    }
    Write-Host "      - $p"
    & $py -m pip download $p --no-deps --only-binary=:all: `
        --platform win_amd64 --python-version 3.12 `
        --timeout 180 --retries 5 `
        -d $wheelDir -i https://mirrors.aliyun.com/pypi/simple/
    if ($LASTEXITCODE -ne 0) { throw "pip download failed for $p (exit=$LASTEXITCODE)" }
}

Write-Host "[2/4] extracting"
$wheels = Get-ChildItem $wheelDir -Filter "*.whl"
foreach ($w in $wheels) {
    $mb = [math]::Round($w.Length / 1MB, 1)
    Write-Host "      $($w.Name) ($mb MB)"
    $zip = Join-Path $tmpDir ($w.BaseName + ".zip")
    Copy-Item $w.FullName $zip -Force
    $sub = Join-Path $tmpDir $w.BaseName
    New-Item -ItemType Directory -Force -Path $sub | Out-Null
    Expand-Archive -Path $zip -DestinationPath $sub -Force
}

Write-Host "[3/4] collecting DLLs"

# onnxruntime-gpu: replace the CPU build in native/onnxruntime
$ortDlls = Get-ChildItem $tmpDir -Recurse -Filter "onnxruntime*.dll" |
    Where-Object { $_.DirectoryName -like "*onnxruntime\capi*" }
foreach ($d in $ortDlls) {
    Copy-Item $d.FullName (Join-Path $ortDir $d.Name) -Force
    $mb = [math]::Round($d.Length / 1MB, 1)
    Write-Host "      ort  $($d.Name) ($mb MB)"
}

# CUDA runtime: everything under nvidia/*/bin
$cudaDlls = Get-ChildItem $tmpDir -Recurse -Filter "*.dll" |
    Where-Object { $_.DirectoryName -like "*nvidia*bin*" }
foreach ($d in $cudaDlls) {
    Copy-Item $d.FullName (Join-Path $cudaDir $d.Name) -Force
}
$cudaCount = (Get-ChildItem $cudaDir -Filter "*.dll").Count
Write-Host "      cuda $cudaCount dlls deployed"

Write-Host "[4/4] verifying key DLLs"
$required = @(
    (Join-Path $ortDir "onnxruntime.dll"),
    (Join-Path $ortDir "onnxruntime_providers_cuda.dll"),
    (Join-Path $cudaDir "cudart64_12.dll"),
    (Join-Path $cudaDir "cublas64_12.dll"),
    (Join-Path $cudaDir "cublasLt64_12.dll"),
    (Join-Path $cudaDir "cudnn64_9.dll"),
    (Join-Path $cudaDir "cufft64_11.dll")
)
$missing = @()
foreach ($r in $required) {
    if (Test-Path $r) {
        $mb = [math]::Round((Get-Item $r).Length / 1MB, 1)
        Write-Host ("      OK   {0} ({1} MB)" -f (Split-Path $r -Leaf), $mb)
    } else {
        Write-Host ("      MISS {0}" -f (Split-Path $r -Leaf))
        $missing += (Split-Path $r -Leaf)
    }
}

if ($missing.Count -gt 0) {
    throw ("missing DLLs: " + ($missing -join ", "))
}

$total = (Get-ChildItem $ortDir, $cudaDir -Recurse -File | Measure-Object Length -Sum).Sum
Write-Host ("[done] total {0:N2} GB in native/" -f ($total / 1GB))
