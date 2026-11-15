"""Persistent, serial JSONL OCR process. stdout is reserved for protocol messages."""
import contextlib
import gc
import json
import sys
import time
import traceback
from pathlib import Path
from importlib.metadata import version
from backends import MANGA, PADDLE_VL, load_backend, vl_options

sys.stdin.reconfigure(encoding="utf-8")
sys.stdout.reconfigure(encoding="utf-8", line_buffering=True)
protocol = sys.stdout
sys.stdout = sys.stderr
engine = None
info = None


def dispatch(req):
    global engine, info
    op = req["op"]
    if op == "shutdown":
        return {"shutdown": True}
    if op == "init":
        model_id = req.get("model", MANGA)
        if model_id not in (MANGA, PADDLE_VL):
            raise ValueError(f"Unsupported OCR model: {model_id}")
        if model_id == PADDLE_VL:
            vl_options(req)
        import torch
        from transformers.utils import logging as hf_logging
        from huggingface_hub import snapshot_download
        hf_logging.set_verbosity_error()
        want = req.get("device", "auto")
        if want not in ("auto", "cuda", "cpu"):
            raise ValueError("device must be auto, cuda or cpu")
        available = torch.cuda.is_available()
        if want == "cuda" and not available:
            raise RuntimeError(f"CUDA requested but unavailable: torch={torch.__version__}, CUDA build={torch.version.cuda}")
        device = "cuda" if available and want != "cpu" else "cpu"
        engine = None
        info = None
        gc.collect()
        if available:
            torch.cuda.empty_cache()
        local = snapshot_download(model_id, revision=req.get("revision", "main"),
                                  allow_patterns=["*.json", "*.safetensors", "*.txt", "*.model", "*.jinja", "*.tiktoken"])
        engine = load_backend(model_id, local, device, req)
        actual = str(next(engine.model.parameters()).device)
        if device == "cuda" and not actual.startswith("cuda"):
            raise RuntimeError(f"Expected CUDA model, got {actual}")
        info = {"model": model_id, "revision": Path(local).name,
                "device": actual, "cuda_available": available,
                "torch_cuda": torch.version.cuda, "torch_version": torch.__version__,
                "transformers_version": version("transformers"),
                "gpu": torch.cuda.get_device_name(0) if available else "CPU",
                "inference": {**engine.inference, "torch": torch.__version__,
                              "transformers": version("transformers")},
                "capabilities": {"input": "cropped_text_image", "gpu": True,
                    "confidence": False, "max_concurrency": 1,
                    "internal_grayscale": model_id == MANGA,
                    "multiline_input": model_id == PADDLE_VL,
                    "multiline_reliable": False}}
        return info
    if op == "ocr":
        if engine is None or info is None:
            raise RuntimeError("Initialize worker first")
        import torch
        from PIL import Image
        cuda = info["device"].startswith("cuda")
        if cuda:
            torch.cuda.synchronize()
            torch.cuda.reset_peak_memory_stats()
        start = time.perf_counter()
        with Image.open(req["image"]) as image, torch.inference_mode():
            text = engine.recognize(image)
        if cuda:
            torch.cuda.synchronize()
        return {"text": text, "elapsed_ms": (time.perf_counter()-start)*1000,
                "confidence": None,
                "peak_vram_mb": torch.cuda.max_memory_allocated()/1024**2 if cuda else None,
                **info}
    raise ValueError(f"Unknown operation: {op}")


for line in sys.stdin:
    req = {}
    try:
        req = json.loads(line)
        with contextlib.redirect_stdout(sys.stderr):
            result = dispatch(req)
        print(json.dumps({"id": req["id"], "ok": True, "result": result}, ensure_ascii=False), file=protocol)
        if req["op"] == "shutdown":
            break
    except Exception as exc:
        traceback.print_exc(file=sys.stderr)
        print(json.dumps({"id": req.get("id"), "ok": False,
                          "error": f"{type(exc).__name__}: {exc}"}, ensure_ascii=False), file=protocol)
