"""Model adapters. Heavy dependencies are imported only for the chosen backend."""
from pathlib import Path

MANGA = "muscgab/manga-ocr-base-CPT"
PADDLE_VL = "PaddlePaddle/PaddleOCR-VL-1.6"


def vl_options(request):
    pixels = int(request.get("max_pixels", 1003520))
    tokens = int(request.get("max_new_tokens", 512))
    if not 65536 <= pixels <= 1605632:
        raise ValueError("Paddle VL max_pixels must be 65536..1605632")
    if not 16 <= tokens <= 1024:
        raise ValueError("Paddle VL max_new_tokens must be 16..1024")
    return pixels, tokens


class MangaBackend:
    def __init__(self, local, device, request):
        from manga_ocr import MangaOcr
        self.ocr = MangaOcr(local, force_cpu=device == "cpu")
        self.model = self.ocr.model.eval()
        self.inference = {"adapter": "manga-v1", "internal_grayscale": True}

    def recognize(self, image):
        return self.ocr(image)


class PaddleVLBackend:
    def __init__(self, local, device, request):
        import torch
        from transformers import AutoProcessor, AutoModelForImageTextToText
        self.max_pixels, self.max_tokens = vl_options(request)
        dtype = torch.bfloat16 if device == "cuda" else torch.float32
        self.processor = AutoProcessor.from_pretrained(local, trust_remote_code=False)
        self.model = AutoModelForImageTextToText.from_pretrained(
            local, dtype=dtype, attn_implementation="sdpa", trust_remote_code=False,
        ).to(device).eval()
        self.inference = {"adapter": "paddle-vl-1.6-v1", "prompt": "OCR:",
                          "max_pixels": self.max_pixels,
                          "max_new_tokens": self.max_tokens, "do_sample": False,
                          "dtype": str(dtype), "attention": "sdpa"}

    def recognize(self, image):
        messages = [{"role": "user", "content": [
            {"type": "image", "image": image.convert("RGB")},
            {"type": "text", "text": "OCR:"},
        ]}]
        inputs = self.processor.apply_chat_template(
            messages, add_generation_prompt=True, tokenize=True,
            return_dict=True, return_tensors="pt",
            images_kwargs={"size": {
                "shortest_edge": min(self.processor.image_processor.size["shortest_edge"], self.max_pixels),
                "longest_edge": self.max_pixels,
            }},
        ).to(self.model.device)
        outputs = self.model.generate(**inputs, max_new_tokens=self.max_tokens,
                                      do_sample=False, num_beams=1)
        generated = outputs[0, inputs["input_ids"].shape[-1]:]
        eos = self.model.generation_config.eos_token_id
        eos = eos if isinstance(eos, list) else [eos]
        if len(generated) >= self.max_tokens and int(generated[-1]) not in eos:
            raise RuntimeError("OCR reached token limit; result withheld to avoid truncated subtitles. Increase max_new_tokens or crop a smaller region.")
        return self.processor.decode(generated, skip_special_tokens=True).strip()


def load_backend(model_id, local, device, request):
    if model_id == MANGA:
        return MangaBackend(local, device, request)
    if model_id == PADDLE_VL:
        return PaddleVLBackend(local, device, request)
    raise ValueError(f"Unsupported OCR model: {model_id}")
