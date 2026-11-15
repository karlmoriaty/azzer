"""Verify that the green-deployed CUDA runtime actually works with ONNX Runtime.

"Installed CUDA" is not the same as "CUDA works". This script proves the latter
by running a real MatMul on a generated ONNX model with CUDAExecutionProvider
forced, then comparing against a NumPy reference.

Prerequisites (all inside the workspace, created by scripts/fetch-cuda.ps1 and
scripts/verify-cuda.ps1):
    <workspace>/.cache/pylibs          onnx, numpy, onnxruntime-gpu
    azzer/native/onnxruntime/          onnxruntime.dll + providers
    azzer/native/cuda/bin/             cudart / cublas / cudnn / cufft

Exit codes:
    0  CUDA served the inference and numbers match
    2  session creation failed
    3  fell back to CPU
    4  numeric mismatch
"""

import os
import sys

# Make sure our green-deployed DLLs win over anything in System32.
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
WORKSPACE = os.path.dirname(ROOT)
LIBS = os.path.join(WORKSPACE, ".cache", "pylibs")
CUDA_BIN = os.path.join(ROOT, "native", "cuda", "bin")
ORT_DIR = os.path.join(ROOT, "native", "onnxruntime")
VERIFY = os.path.join(WORKSPACE, ".cache", "verify")

if LIBS not in sys.path:
    sys.path.insert(0, LIBS)
os.environ["PATH"] = CUDA_BIN + os.pathsep + ORT_DIR + os.pathsep + os.environ.get("PATH", "")

os.makedirs(VERIFY, exist_ok=True)

import numpy as np  # noqa: E402
import onnx  # noqa: E402
from onnx import TensorProto, helper  # noqa: E402
import onnxruntime as ort  # noqa: E402

MODEL = os.path.join(VERIFY, "tiny_matmul.onnx")

print("onnxruntime version :", ort.__version__)
print("available providers :", ort.get_available_providers())
print("cuda bin            :", CUDA_BIN, "(exists)" if os.path.isdir(CUDA_BIN) else "(MISSING)")
print("ort dir             :", ORT_DIR, "(exists)" if os.path.isdir(ORT_DIR) else "(MISSING)")

# --- 1. build a minimal model -------------------------------------------------
X = helper.make_tensor_value_info("X", TensorProto.FLOAT, [1, 64])
W = helper.make_tensor_value_info("W", TensorProto.FLOAT, [64, 64])
Y = helper.make_tensor_value_info("Y", TensorProto.FLOAT, [1, 64])
node = helper.make_node("MatMul", ["X", "W"], ["Y"])
graph = helper.make_graph([node], "tiny_matmul", [X, W], [Y])
model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
model.ir_version = 10  # avoid IR version warnings with older runtimes
onnx.save(model, MODEL)
print("model saved         :", MODEL)

# --- 2. run with CUDA forced --------------------------------------------------
so = ort.SessionOptions()
so.log_severity_level = 3
try:
    sess = ort.InferenceSession(
        MODEL,
        sess_options=so,
        providers=["CUDAExecutionProvider", "CPUExecutionProvider"],
    )
except Exception as e:  # noqa: BLE001
    print("SESSION ERROR       :", type(e).__name__, e)
    sys.exit(2)

used = sess.get_providers()
print("providers in use    :", used)

rng = np.random.default_rng(0)
x = rng.standard_normal((1, 64), dtype=np.float32)
w = rng.standard_normal((64, 64), dtype=np.float32)
y = sess.run(None, {"X": x, "W": w})[0]
ref = x @ w
err = float(np.max(np.abs(y - ref)))
print("max abs error       :", err)

# --- 3. verdict ---------------------------------------------------------------
if "CUDAExecutionProvider" not in used:
    print("RESULT: FAIL - CUDA provider not used (fell back to CPU)")
    sys.exit(3)
if err > 1e-3:
    print("RESULT: FAIL - numeric mismatch (CUDA kernels produced wrong values)")
    sys.exit(4)
print("RESULT: PASS - CUDA provider served the inference")
sys.exit(0)
