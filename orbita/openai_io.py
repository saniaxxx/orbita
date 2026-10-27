"""OpenAI-compatible API: chat completions, transcriptions, speech."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import Any

import aiohttp

from .audio import CHANNELS, RATE, WIDTH, to_wav
from .config import OpenAIConfig


class OpenAIError(RuntimeError):
    pass


class OpenAIClient:
    def __init__(self, cfg: OpenAIConfig) -> None:
        self._cfg = cfg
        self._base = cfg.base_url.rstrip("/")

    def _headers(self) -> dict[str, str]:
        key = self._cfg.key()
        return {"Authorization": f"Bearer {key}"} if key else {}

    def _timeout(self) -> aiohttp.ClientTimeout:
        return aiohttp.ClientTimeout(total=self._cfg.timeout)

    async def _check(self, resp: aiohttp.ClientResponse, what: str) -> None:
        if resp.status >= 400:
            body = (await resp.text())[:300]
            raise OpenAIError(f"{what}: HTTP {resp.status}: {body}")

    async def chat(self, model: str, messages: list[dict[str, Any]], **params: Any) -> str:
        payload = {"model": model, "messages": messages, "stream": False, **params}
        async with aiohttp.ClientSession(timeout=self._timeout()) as s:
            async with s.post(
                f"{self._base}/chat/completions", json=payload, headers=self._headers()
            ) as resp:
                await self._check(resp, "chat")
                data = await resp.json(content_type=None)
        try:
            content = data["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError):
            raise OpenAIError(f"chat: unexpected response {str(data)[:300]}") from None
        return (content or "").strip()

    async def chat_stream(
        self, model: str, messages: list[dict[str, Any]], **params: Any
    ) -> AsyncIterator[str]:
        """Chat completion with stream=true (server-sent events): yields text deltas."""
        payload = {"model": model, "messages": messages, "stream": True, **params}
        # The total timeout bounds the whole answer; sock_read catches a server gone silent.
        timeout = aiohttp.ClientTimeout(total=self._cfg.timeout, sock_read=self._cfg.timeout)
        async with aiohttp.ClientSession(timeout=timeout) as s:
            async with s.post(
                f"{self._base}/chat/completions", json=payload, headers=self._headers()
            ) as resp:
                await self._check(resp, "chat")
                if "text/event-stream" not in resp.headers.get("Content-Type", ""):
                    # A server that ignores stream=true answers with one JSON body.
                    data = await resp.json(content_type=None)
                    try:
                        content = data["choices"][0]["message"]["content"]
                    except (KeyError, IndexError, TypeError):
                        raise OpenAIError(f"chat: unexpected response {str(data)[:300]}") from None
                    if content:
                        yield content
                    return
                async for raw in resp.content:
                    line = raw.decode("utf-8", "replace").strip()
                    if not line.startswith("data:"):
                        continue
                    data_str = line[5:].strip()
                    if data_str == "[DONE]":
                        return
                    try:
                        event = json.loads(data_str)
                    except json.JSONDecodeError:
                        continue
                    if isinstance(event, dict) and event.get("error"):
                        raise OpenAIError(f"chat: {str(event['error'])[:300]}")
                    for choice in event.get("choices") or []:
                        delta = (choice.get("delta") or {}).get("content")
                        if delta:
                            yield delta

    async def embeddings(self, texts: list[str], model: str) -> list[list[float]]:
        """Texts -> their vectors, in the same order."""
        payload = {"model": model, "input": texts}
        async with aiohttp.ClientSession(timeout=self._timeout()) as s:
            async with s.post(
                f"{self._base}/embeddings", json=payload, headers=self._headers()
            ) as resp:
                await self._check(resp, "embeddings")
                data = await resp.json(content_type=None)
        try:
            items = sorted(data["data"], key=lambda d: d["index"])
            vectors = [[float(x) for x in d["embedding"]] for d in items]
        except (KeyError, TypeError, ValueError):
            raise OpenAIError(f"embeddings: unexpected response {str(data)[:300]}") from None
        if len(vectors) != len(texts):
            raise OpenAIError(f"embeddings: {len(vectors)} vectors for {len(texts)} texts")
        return vectors

    async def transcribe(self, pcm: bytes, model: str, language: str | None) -> str:
        """16 kHz mono 16-bit PCM -> text."""
        form = aiohttp.FormData()
        form.add_field(
            "file",
            to_wav(pcm, RATE, WIDTH, CHANNELS),
            filename="audio.wav",
            content_type="audio/wav",
        )
        form.add_field("model", model)
        form.add_field("response_format", "json")
        if language:
            form.add_field("language", language)
        async with aiohttp.ClientSession(timeout=self._timeout()) as s:
            async with s.post(
                f"{self._base}/audio/transcriptions", data=form, headers=self._headers()
            ) as resp:
                await self._check(resp, "transcriptions")
                data = await resp.json(content_type=None)
        if not isinstance(data, dict) or "text" not in data:
            raise OpenAIError(f"transcriptions: unexpected response {str(data)[:300]}")
        return str(data["text"]).strip()

    async def speech(
        self,
        text: str,
        model: str,
        voice: str | None,
        response_format: str,
        speed: float | None = None,
    ) -> bytes:
        payload: dict[str, Any] = {
            "model": model,
            "input": text,
            "voice": voice or "alloy",
            "response_format": response_format,
        }
        if speed is not None:
            payload["speed"] = speed
        async with aiohttp.ClientSession(timeout=self._timeout()) as s:
            async with s.post(
                f"{self._base}/audio/speech", json=payload, headers=self._headers()
            ) as resp:
                await self._check(resp, "speech")
                data = await resp.read()
        if not data:
            raise OpenAIError("speech: empty audio")
        return data
