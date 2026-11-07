# План перехода на OpenClaw

**Статус:** 08.11.2026. Целевая спека — `docs/spec.md`.

Цель — заменить `orbita-agent` ядром OpenClaw, сохранив quasar, GigaAM, Vosk TTS
и wb-rules. Станция, Wyoming, Telegram и mobile становятся интерфейсами к
одному ядру. Hermes — параллельный challenger, не основной путь.

## Принципы

- Сначала контракты и симулятор, потом стенд.
- `wb-rules` и quasar не переписываются на этом этапе.
- Никаких прямых MQTT/реле из OpenClaw: только MCP.
- Старый `orbita-agent` остаётся fallback до приёмки.
- Каждая фаза заканчивается тестами и rollback-командой.

## Фаза 0. Контракты и границы (2–3 дня)

- Зафиксировать `InboundEvent`, `Reply`, `Action`, `Capabilities`.
- Разделить `speak`, `set_volume`, `set_timer`, `media`, `home`.
- Определить identity: DUID станции, Wyoming device id, Telegram chat id;
  один principal, разные session keys.
- Решить, где живут fast paths: station adapter или tool layer.
- ADR: OpenClaw vs Hermes по autonomy, MCP, memory, scheduler, security.

**Выход:** контракт в `docs/spec.md`, тестовые фикстуры, отсутствие изменений
на стенде.

## Фаза 1. Общие AI-шлюзы (4–6 дней)

- OpenAI-compatible LLM endpoint уже есть; проверить OpenClaw provider.
- STT: streaming endpoint GigaAM + Wyoming ASR adapter.
- TTS: Vosk HTTP + Wyoming TTS adapter; Opus 48 кГц для станции, PCM 16 кГц
  для Wyoming.
- Очередь/приоритет GPU: станция > Wyoming > фоновые объявления.
- Health/metrics: очередь, p50/p95, ошибки.

**Выход:** станция и Wyoming могут пользоваться общими STT/TTS без OpenClaw.
Rollback: старые endpoint’ы остаются.

## Фаза 2. OpenClaw в лаборатории (3–5 дней)

- Отдельный LXC, без секретов станции.
- OpenClaw Gateway + локальная модель + Telegram-канал.
- MCP: `search`, `tv` read-only; home — read-only state.
- Проверить block streaming, tool policy, approvals, рестарт.
- Hermes — параллельный спайк на тех же сценариях.

**Выход:** отчёт: latency, качество, memory, tool policy, стабильность.

## Фаза 3. Station adapter на симуляторе (5–7 дней)

- Адаптер `/process`: `prepare`/`process`, DUID-сессии, `partialResponse`.
- Маппинг `Action → AAPI directive` только здесь.
- Fast path громкости/таймера — по результатам замеров.
- Тесты на `orbita-station-sim`: приветствие, таймер, громкость, погода,
  ошибка MCP, рестарт адаптера.

**Выход:** симулятор проходит те же сценарии, что текущий агент. Rollback —
переключить quasar обратно на `orbita-agent`.

## Фаза 4. Streaming TTS (4–6 дней)

- OpenClaw block streaming: `text_end`, `sentence`, min/max chars.
- Station adapter: TTS на блок, несколько `TtsSpeak`, `Close` только последний.
- Quasar: несколько потоков, корректное прерывание.
- Порог: короткие ответы — один блок; длинные — по предложениям.
- Замер: время до первого звука, суммарный TTS, p95.

**Выход:** первый звук раньше на длинных ответах без регрессии коротких.
Rollback: один поток.

## Фаза 5. Wyoming (4–6 дней)

- `wyoming-gigaam`, `wyoming-vosk`, bridge в OpenClaw.
- Wake/VAD на спутнике; никаких AAPI-директив.
- Проверить один физический спутник и один виртуальный.
- VLAN/allowlist: Wyoming без аутентификации.

**Выход:** второй голосовой интерфейс работает через то же ядро.

## Фаза 6. Дом, MCP и события (5–7 дней)

- `orbita-home-mcp`: state cache, devices, commands, scenes, audit.
- wb-rules публикует события; MCP фильтрует и хранит cursor.
- OpenClaw: `home_state`, `home_scene`, `home_events`.
- MCP Events + signed webhooks для критичных событий; fallback polling.
- Safety: leak/frost/door, подтверждения, rate limit, idempotency.

**Выход:** команды дома через MCP; wb-rules автономен при падении агента.

## Фаза 7. Пилот и миграция (4–6 дней)

- Shadow: станция и OpenClaw слушают одинаковые запросы, ответы сравниваются.
- 2 недели canary на одной станции/комнате.
- Переключение quasar на адаптер OpenClaw.
- Оставить `orbita-agent` выключенным, но развёрнутым для отката.
- Метрики: успех команд, latency, ошибки, подтверждения.

**Выход:** решение о выводе `orbita-agent` из эксплуатации.

## Критерии приёмки

- 20 голосовых сценариев станции: приветствие, таймер, громкость, свет,
  погода, новости, мультик, TV, ошибка инструмента, рестарт.
- 10 текстовых сценариев Telegram.
- 5 сценариев Wyoming.
- p95 до первого звука не хуже текущего; длинные ответы — лучше.
- Директивы, напоминания, память и safety не регрессируют.
- Падение OpenClaw/MCP не ломает wb-rules и станционный fallback.

## Риски

- OpenClaw plugin/channel API меняется; адаптер придётся обновлять.
- Два LLM-мозга при параллельной работе; нужен строгий session routing.
- Стриминг TTS увеличивает суммарную нагрузку на GPU.
- MCP Events требует HTTPS callback; в LAN нужен reverse proxy/Tailscale.
- Смешение контекстов станции, Wyoming и Telegram; identity надо решить до
  миграции.
- Замок/котёл нельзя отдавать агенту без политики и локальных interlocks.

## Rollback

- quasar: вернуть `PROCESSOR_BASIC_URL` на `orbita-agent`.
- OpenClaw: остановить LXC, станция продолжает работать на старом агенте.
- TTS/STT: старые endpoint’ы не удалять до конца пилота.
- Дом: wb-rules не зависит от OpenClaw; MCP можно выключить.
