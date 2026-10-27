"""OpenAI-compatible text embeddings over a sentence encoder: POST /v1/embeddings.

    python embed_openai.py --model cointegrated/rubert-tiny2 [--port 8004]

JSON {input: str | [str], model}: "model" is ignored (the loaded model answers). Embeddings are
the mean of the last hidden states over the tokens, L2-normalized, so a dot product is the
cosine similarity. For orbita's request classifier (which answer mode a request needs);
rubert-tiny2 takes ~3 ms a request on a CPU without AVX2.

Needs torch (CPU is enough), transformers and aiohttp.
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import time
from concurrent.futures import ThreadPoolExecutor

import torch
from aiohttp import web
from transformers import AutoModel, AutoTokenizer

_LOG = logging.getLogger("embed_openai")

MAX_INPUTS = 256
MAX_TOKENS = 128


def error(status: int, message: str) -> web.Response:
    return web.json_response({"error": {"message": message}}, status=status)


class Encoder:
    def __init__(self, name: str, prefix: str) -> None:
        self.name = name
        self._prefix = prefix
        # One thread for the model: requests queue up in it.
        self._thread = ThreadPoolExecutor(max_workers=1, thread_name_prefix="embed")
        self._tok = AutoTokenizer.from_pretrained(name)
        self._model = AutoModel.from_pretrained(name).eval()
        self._thread.submit(self._embed, ["прогрев"]).result()

    @torch.inference_mode()
    def _embed(self, texts: list[str]) -> list[list[float]]:
        enc = self._tok(
            [self._prefix + t for t in texts],
            return_tensors="pt",
            padding=True,
            truncation=True,
            max_length=MAX_TOKENS,
        )
        out = self._model(**enc).last_hidden_state
        mask = enc["attention_mask"].unsqueeze(-1)
        v = (out * mask).sum(1) / mask.sum(1)
        return torch.nn.functional.normalize(v, dim=-1).tolist()

    async def embed(self, texts: list[str]) -> list[list[float]]:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self._thread, self._embed, texts)


def make_app(enc: Encoder, api_key: str | None) -> web.Application:
    @web.middleware
    async def auth(request: web.Request, handler):
        if api_key and request.path != "/health":
            if request.headers.get("Authorization") != f"Bearer {api_key}":
                return error(401, "invalid API key")
        return await handler(request)

    async def embeddings(request: web.Request) -> web.Response:
        try:
            body = await request.json()
        except ValueError:
            return error(400, "body must be JSON")
        inp = body.get("input")
        texts = [inp] if isinstance(inp, str) else inp
        if not isinstance(texts, list) or not texts or not all(isinstance(t, str) for t in texts):
            return error(400, "'input' must be a string or a non-empty list of strings")
        if len(texts) > MAX_INPUTS:
            return error(400, f"at most {MAX_INPUTS} inputs")
        t = time.monotonic()
        vectors = await enc.embed(texts)
        _LOG.debug("%d inputs in %.3f s", len(texts), time.monotonic() - t)
        data = [{"object": "embedding", "index": i, "embedding": v} for i, v in enumerate(vectors)]
        return web.json_response({"object": "list", "data": data, "model": enc.name})

    async def models(request: web.Request) -> web.Response:
        data = [{"id": enc.name, "object": "model", "owned_by": "local"}]
        return web.json_response({"object": "list", "data": data})

    async def health(request: web.Request) -> web.Response:
        return web.json_response({"status": "ok"})

    app = web.Application(middlewares=[auth])
    app.router.add_post("/v1/embeddings", embeddings)
    app.router.add_get("/v1/models", models)
    app.router.add_get("/health", health)
    return app


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=8004)
    p.add_argument("--model", default="cointegrated/rubert-tiny2")
    p.add_argument("--prefix", default="", help='text prefix, e.g. "query: " for e5 models')
    p.add_argument("--threads", type=int, default=os.cpu_count() or 1)
    p.add_argument("--api-key", help="require Authorization: Bearer <key>")
    p.add_argument("--debug", action="store_true")
    args = p.parse_args()
    logging.basicConfig(level=logging.DEBUG if args.debug else logging.INFO)
    torch.set_num_threads(args.threads)

    t = time.monotonic()
    enc = Encoder(args.model, args.prefix)
    _LOG.info("%s loaded in %.1f s", args.model, time.monotonic() - t)
    web.run_app(make_app(enc, args.api_key), host=args.host, port=args.port, print=None)


if __name__ == "__main__":
    main()
