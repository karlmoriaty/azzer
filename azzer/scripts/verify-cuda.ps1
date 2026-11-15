# Verify that the deployed CUDA runtime actually works with ONNX Runtime
#
# "Installed CUDA" is not the same as "CUDA works". This script proves the
# latter by running a real inference on a tiny ONNX model with the CUDA
# execution provider forced, and printing which provider actually served it.
#
# Everything stays inside the workspace:
#   .cache/pylibs/          python packages installed with --target
#   .cache/verify/          generated test model
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/verify-cuda.ps1
#
# Exit code 0 = CUDA provider served the inference. Non-zero = it did not.

$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$workspace = Split-Path $root -Parent
$libs = Join-Path $workspace ".cache\pylibs"
$verify = Join-Path $workspace ".cache\verify"
$cudaBin = Join-Path $root "native\cuda\bin"
$ortDir = Join-Path $root "native\onnxruntime"

$py = "C:\Users\asus\AppData\Local\Programs\Python\Python312\python.exe"
if (-not (Test-Path $py)) { $py = "python" }

New-Item -ItemType Directory -Force -Path $libs, $verify | Out-Null

Write-Host "[1/4] installing onnxruntime-gpu + onnx into workspace (target dir)"
# 幂等：镜像偶发 SSL EOF，已装好就不重复拉
$haveNumpy = Test-Path (Join-Path $libs "numpy")
$haveOnnx = Test-Path (Join-Path $libs "onnx")
$haveOrt = Test-Path (Join-Path $libs "onnxruntime")

if ($haveNumpy -and $haveOnnx) {
    Write-Host "      numpy + onnx already present, skip"
} else {
    & $py -m pip install --target $libs --upgrade --no-cache-dir `
        "numpy" "onnx" `
        -i https://mirrors.aliyun.com/pypi/simple/ --quiet
    if ($LASTEXITCODE -ne 0) { throw "pip install (onnx) failed" }
}

if ($haveOrt) {
    Write-Host "      onnxruntime already present, skip"
} else {
    # 复用 fetch-cuda 已下载的 wheel，不再走网络
    $wheelDir = Join-Path $workspace ".cache\cuda-wheel"
    & $py -m pip install --target $libs --upgrade --no-cache-dir --no-deps `
        --no-index --find-links $wheelDir "onnxruntime-gpu==1.31.1" --quiet
    if ($LASTEXITCODE -ne 0) { throw "pip install (onnxruntime-gpu) failed" }
}

Write-Host "[2/4] generating a tiny ONNX model"
# 必须在这里就设 PYTHONPATH：gen.py 要 import onnx，而包装在 $libs（--target），
# 不在系统 site-packages 里。
$env:PYTHONPATH = $libs
$modelPath = Join-Path $verify "tiny_matmul.onnx"
$genScript = Join-Path $verify "gen.py"
$gen = @"
import numpy as np
import onnx
from onnx import helper, TensorProto

X = helper.make_tensor_value_info('X', TensorProto.FLOAT, [1, 64])
W = helper.make_tensor_value_info('W', TensorProto.FLOAT, [64, 64])
Y = helper.make_tensor_value_info('Y', TensorProto.FLOAT, [1, 64])
node = helper.make_node('MatMul', ['X', 'W'], ['Y'])
graph = helper.make_graph([node], 'tiny_matmul', [X, W], [Y])
model = helper.make_model(graph, opset_imports=[helper.make_opsetid('', 17)])
model.ir_version = 10
onnx.save(model, r'$modelPath')
print('model saved:', r'$modelPath')
"@
Set-Content -Path $genScript -Value $gen -Encoding UTF8
& $py $genScript
if ($LASTEXITCODE -ne 0) { throw "model generation failed" }

Write-Host "[3/4] running inference with CUDAExecutionProvider forced"
$runScript = Join-Path $verify "run.py"
$run = @"
import os, sys, numpy as np
import onnxruntime as ort

print('onnxruntime version :', ort.__version__)
print('available providers :', ort.get_available_providers())

so = ort.SessionOptions()
so.log_severity_level = 3
try:
    sess = ort.InferenceSession(
        r'$modelPath',
        sess_options=so,
        providers=['CUDAExecutionProvider', 'CPUExecutionProvider'],
    )
except Exception as e:
    print('SESSION ERROR:', type(e).__name__, e)
    sys.exit(2)

used = sess.get_providers()
print('providers in use   :', used)

rng = np.random.default_rng(0)
x = rng.standard_normal((1, 64), dtype=np.float32)
w = rng.standard_normal((64, 64), dtype=np.float32)
y = sess.run(None, {'X': x, 'W': w})[0]
ref = x @ w
err = float(np.max(np.abs(y - ref)))
print('max abs error      :', err)

if 'CUDAExecutionProvider' not in used:
    print('RESULT: FAIL - CUDA provider not used (fell back to CPU)')
    sys.exit(3)
if err > 1e-3:
    print('RESULT: FAIL - numeric mismatch')
    sys.exit(4)
print('RESULT: PASS - CUDA provider served the inference')
"@
Set-Content -Path $runScript -Value $run -Encoding UTF8

# 让 python 找到我们绿色部署的 CUDA dll 与 onnxruntime
$env:PYTHONPATH = $libs
$env:PATH = "$cudaBin;$ortDir;$env:PATH"
& $py $runScript
$code = $LASTEXITCODE
Write-Host "[4/4] exit code = $code"
exit $code
