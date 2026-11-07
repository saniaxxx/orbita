# orbita — состояние на 08.11.2026

Короткий снимок для агента, который только что забрал репозиторий.
Целевая архитектура — `docs/spec.md`, план перехода — `docs/plan.md`.
История и старые замеры — в git-истории.

## 1. Куда идём

Автономный домашний агент на сервере. Станция, Wyoming-спутник, Telegram,
mobile и web — интерфейсы к одному ядру. Ядро — OpenClaw (кандидат; Hermes
сравнивается в POC). LLM вызывает только ядро; STT/TTS — общие сервисы.

Текущий `orbita-agent` — legacy: он всё ещё обслуживает станцию, но целевая
архитектура его заменяет.

## 2. Стенд

| Где | Адрес | Что |
|---|---|---|
| pve3 | `192.168.250.221` | Proxmox VE |
| CT 100 `alice` | `192.168.250.102` | GPU: llama.cpp, GigaAM, Vosk TTS |
| CT 101 `orbita` | `192.168.250.103` | quasar, orbita-agent, mcp-search, orbita-tv, dashboard, https-proxy, SearXNG |
| Станция | `192.168.250.115` | домены Яндекса в `/etc/hosts` → `.103` |
| Wiren Board | `192.168.250.110` | MQTT, wb-rules |
| TCL TV | `192.168.250.109` | Remote v2 |
| Sber TV | `192.168.250.111` | SaluteTV, ADB 5555 |

## 3. Что работает

- **LLM**: локальный OpenAI-совместимый endpoint, `qwen3.5-9b-q4km`.
- **STT**: GigaAM на CUDA/CPU; используется quasar.
- **TTS**: Vosk TTS на CUDA; HTTP и Opus 48 кГц для станции. Уникальная фраза
  ~0,93 с, повтор ~0,2 с.
- **Станция**: quasar/AAPI, `/process`; `orbita-agent` отвечает, директивы
  громкости/таймеров работают.
- **TV**: `orbita-tv` MCP, `devices=2`; TCL — Remote v2, Sber — ADB через
  `tv-adb.py`; 10 MCP-инструментов у агента.
- **TV-скилл**: «включи мультик …» → VK search/play через ADB, по умолчанию
  детская; питание/громкость через ADB.
- **Дом**: wb-rules владеет safety/сценами; свет Wiren Board; `orbita-home`
  читает состояние. MCP-фасада дома пока нет.
- **Поиск**: `orbita-mcp-search` + SearXNG.

## 4. Что ещё не сделано

- OpenClaw не развёрнут; POC не начат.
- Нет `home-MCP` и событий дома через MCP.
- Нет Wyoming-адаптера и второго голосового интерфейса.
- Нет streaming-ответа OpenClaw → пофразового TTS; quasar пока один поток.
- Нет единого контракта `InboundEvent/Reply` в коде.
- `orbita-agent` смешивает ядро и station-адаптер; это legacy.

## 5. Ближайшие шаги

1. Фаза 0: контракты, identity, ADR OpenClaw vs Hermes.
2. OpenClaw/Hermes lab: Telegram + MCP search/TV read-only.
3. Station adapter на `orbita-station-sim`.
4. Streaming TTS и multi-stream quasar.
5. home-MCP + wb-rules events.
6. Wyoming.
7. Shadow/canary, затем миграция.

Полный план — `docs/plan.md`.

## 6. Проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Для текущего TV-стека: `cargo test -p orbita-tv -p orbita-agent`,
`deploy/tv/tv-adb.py state` на CT 101.
