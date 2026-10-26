"""Configuration file (YAML) -> dataclasses."""

from __future__ import annotations

import dataclasses
import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import yaml


class ConfigError(ValueError):
    pass


@dataclass
class SatelliteConfig:
    name: str
    host: str
    port: int = 6053
    noise_psk: str | None = None
    # Overrides of the global stt.language / tts.voice / tts.streaming for this satellite.
    language: str | None = None
    voice: str | None = None
    streaming: bool | None = None


@dataclass
class OpenAIConfig:
    """An OpenAI-compatible API: OpenAI, vLLM, llama.cpp server, LM Studio, Ollama (/v1),
    LocalAI, Speaches, OpenRouter..."""

    base_url: str = "http://127.0.0.1:8000/v1"
    # Empty: the environment variable OPENAI_API_KEY (systemd EnvironmentFile).
    api_key: str = ""
    timeout: float = 60.0

    def key(self) -> str:
        return self.api_key or os.environ.get("OPENAI_API_KEY", "")


@dataclass
class SttConfig:
    # "wyoming" (wyoming-faster-whisper and others) or "openai" (/audio/transcriptions).
    engine: str = "wyoming"
    host: str = "127.0.0.1"
    port: int = 10300
    language: str = "ru"
    model: str = "whisper-1"
    # engine "openai": its own API; absent: the top-level openai section.
    openai: OpenAIConfig | None = None


@dataclass
class TtsConfig:
    # "wyoming" (wyoming-piper and others) or "openai" (/audio/speech).
    engine: str = "wyoming"
    host: str = "127.0.0.1"
    port: int = 10200
    voice: str | None = None
    speaker: str | None = None
    model: str = "tts-1"
    # engine "openai": wav (the default, any GStreamer plays it), mp3, flac, opus...
    response_format: str = "wav"
    speed: float | None = None
    # Speak an LLM answer phrase by phrase while it is written: the satellite gets a WAV stream
    # in RUN_START and starts playing with the first phrase. false: one file after the whole
    # answer.
    streaming: bool = True
    # A phrase is spoken once it has this many characters (shorter ones join the next).
    min_phrase_chars: int = 20
    openai: OpenAIConfig | None = None


@dataclass
class VadConfig:
    # "auto": pymicro-vad when it loads, else the energy detector.
    engine: str = "auto"
    threshold: float = 0.5
    # Energy detector: RMS of 16-bit samples above which a 10 ms frame counts as speech.
    energy_threshold: float = 600.0
    speech_seconds: float = 0.3
    silence_seconds: float = 0.8
    # No speech at all within this time: the run ends with stt-no-text-recognized.
    no_speech_seconds: float = 6.0
    timeout_seconds: float = 15.0


@dataclass
class HttpConfig:
    host: str = "0.0.0.0"
    port: int = 10800
    # URL prefix the satellites fetch answers from, e.g. http://192.168.1.10:10800.
    # Empty: the address this host uses to reach each satellite.
    base_url: str = ""
    ttl_seconds: float = 600.0


@dataclass
class MqttConfig:
    host: str = ""
    port: int = 1883
    username: str | None = None
    password: str | None = None
    prefix: str = "orbita"


@dataclass
class Config:
    satellites: list[SatelliteConfig]
    stt: SttConfig = field(default_factory=SttConfig)
    tts: TtsConfig = field(default_factory=TtsConfig)
    vad: VadConfig = field(default_factory=VadConfig)
    http: HttpConfig = field(default_factory=HttpConfig)
    mqtt: MqttConfig = field(default_factory=MqttConfig)
    # The default OpenAI-compatible API for the "openai" engines and the "openai" intent handler.
    openai: OpenAIConfig = field(default_factory=OpenAIConfig)
    # Handler chain, tried in order: [{type: rules, ...}, {type: openai, ...}, {type: mqtt, ...}].
    intents: list[dict[str, Any]] = field(default_factory=list)
    fallback_reply: str = "Извините, я не поняла."
    error_reply: str = "Что-то пошло не так."
    # IANA name sent to the satellites with the time (the clock on a Yandex Station); empty: this
    # host's zone.
    timezone: str = ""
    # Seconds a conversation (history for the LLM, conversation_id) is kept between turns.
    conversation_ttl: float = 300.0


def _build(cls: type, data: Any, where: str) -> Any:
    if data is None:
        return cls()
    if not isinstance(data, dict):
        raise ConfigError(f"{where}: expected a mapping")
    names = {f.name for f in dataclasses.fields(cls)}
    unknown = set(data) - names
    if unknown:
        raise ConfigError(f"{where}: unknown key(s) {', '.join(sorted(unknown))}")
    try:
        return cls(**data)
    except TypeError as e:
        raise ConfigError(f"{where}: {e}") from None


def parse(data: Any) -> Config:
    if not isinstance(data, dict):
        raise ConfigError("config: expected a mapping")
    data = dict(data)
    sats = data.pop("satellites", None)
    if not sats or not isinstance(sats, list):
        raise ConfigError("satellites: at least one satellite is required")
    satellites = [_build(SatelliteConfig, s, f"satellites[{i}]") for i, s in enumerate(sats)]
    names = [s.name for s in satellites]
    if len(set(names)) != len(names):
        raise ConfigError("satellites: names must be unique")
    sections = {
        "stt": SttConfig,
        "tts": TtsConfig,
        "vad": VadConfig,
        "http": HttpConfig,
        "mqtt": MqttConfig,
        "openai": OpenAIConfig,
    }
    kwargs: dict[str, Any] = {"satellites": satellites}
    for key, cls in sections.items():
        if key in data:
            section = data.pop(key)
            if key in ("stt", "tts") and isinstance(section, dict) and "openai" in section:
                section = dict(section)
                section["openai"] = _build(OpenAIConfig, section["openai"], f"{key}.openai")
            kwargs[key] = _build(cls, section, key)
    for key in ("stt", "tts"):
        engine = (kwargs.get(key) or sections[key]()).engine
        if engine not in ("wyoming", "openai"):
            raise ConfigError(f"{key}.engine: expected wyoming or openai, got {engine!r}")
    intents = data.pop("intents", None) or []
    if not isinstance(intents, list) or not all(
        isinstance(i, dict) and "type" in i for i in intents
    ):
        raise ConfigError("intents: expected a list of mappings with a 'type'")
    kwargs["intents"] = intents
    for key in ("fallback_reply", "error_reply", "timezone", "conversation_ttl"):
        if key in data:
            kwargs[key] = data.pop(key)
    if data:
        raise ConfigError(f"config: unknown key(s) {', '.join(sorted(data))}")
    return Config(**kwargs)


def load(path: str | Path) -> Config:
    with open(path, encoding="utf-8") as f:
        return parse(yaml.safe_load(f))
