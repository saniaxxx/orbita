# План перехода на OpenClaw

**Статус:** 08.11.2026, rev 2.1. Целевая спека — `docs/spec.md`.

Цель — заменить `orbita-agent` ядром OpenClaw, сохранив quasar, GigaAM, Vosk TTS
и wb-rules. Станция, Wyoming, Telegram и mobile становятся интерфейсами к
одному ядру. Hermes — параллельный challenger, не основной путь.

## Принципы

- Сначала контракты и симулятор, потом стенд.
- Никаких прямых MQTT/реле из OpenClaw: только MCP.
- Старый `orbita-agent` остаётся fallback до приёмки; планировщик и напоминания
  в каждый момент принадлежат только одному ядру.
- Каждая фаза заканчивается тестами, baseline-замером и rollback-командой.
- Никаких изменений на рабочем стенде до прохождения gate фаз 2 и 5.
- Оценки — рабочие дни, без буфера на ручные стендовые команды.

## Фаза 0. Контракты, baseline, ADR (3–4 дня)

- `InboundEvent`, `Reply`, `OutboundEvent`, `Action`/tool calls.
- `turn_id/request_id`, `input_kind`, `locale/tz`, `should_listen`, `shadow`.
- Identity: household/person, DUID, Wyoming id, Telegram chat id.
- ADR:
  - LLM serving: грамматика per-request, слоты, приоритеты (D-21);
  - граница station adapter: `/process` или внутри quasar (D-22);
  - OpenClaw vs Hermes: критерии, не окончательный выбор;
  - fast paths: intent layer перед ядром или tools (D-25).
- Baseline p95: текущий голосовой ход, первый звук, TTS, LLM, tool calls.
- Зафиксировать уровни проверки L0–L4 для каждого критерия.

**Выход:** контракт + baseline + ADR. Стенд не меняется.

## Фаза 1. LLM serving и общие AI-шлюзы (5–7 дней)

- LLM: grammar per-request; `-np ≥2` или раздельные endpoint’ы; бюджет KV/VRAM;
  приоритет голоса; замер prefill системного промпта OpenClaw.
- STT: streaming GigaAM + Wyoming ASR adapter.
- TTS: Vosk HTTP + Wyoming TTS adapter; Opus 48 кГц и PCM 16 кГц.
- Очереди/приоритеты GPU: голос > Wyoming > фон.
- Metrics: очередь, p50/p95, ошибки, prefill.

**Gate:** legacy-агент и OpenClaw могут работать одновременно без деградации
голоса; если нет — раздельные endpoint’ы/модели.

**Rollback:** старые endpoint’ы остаются; LLM-конфиг возвращается.

## Фаза 2. POC OpenClaw/Hermes (5–7 дней)

- Отдельный LXC, без секретов станции.
- OpenClaw Gateway + локальная модель + Telegram.
- MCP: `search`, `tv` read-only; home — read-only state.
- Проверить:
  - block streaming и tool calls;
  - механизм device-actions (C3);
  - per-request grammar (C1);
  - outbound/напоминания;
  - tool policy, approvals, рестарт;
  - prompt injection из search.
- Hermes — тот же набор сценариев.

**Gate go/no-go:** выбранное ядро умеет device-actions и streaming без
костылей; иначе — смена ядра/архитектуры, не переход к станции.

**Выход:** отчёт по latency, качеству, памяти, tool policy, безопасности.

## Фаза 3. home-MCP (6–8 дней)

- `home-MCP`: state cache, devices, commands, scenes, audit.
- wb-rules: command-устройства, ack, audit-топик, сцены.
- Read-only state; затем свет и безопасные команды.
- Проверить `executed` по state-топику и таймауту.
- Сверить список устройств с HA backup; отбросить leftovers.
- Дом остаётся автономным при падении MCP.

**Gate:** команда света доходит до wb-rules и подтверждается; при падении
home-MCP свет/автоматизации работают.

## Фаза 4. Station adapter (5–7 дней)

- Выбранная граница (D-22): adapter за `/process` или внутри quasar.
- `prepare/process`, DUID-сессии, `partialResponse`, `should_listen`.
- Маппинг device-actions в AAPI-директивы только здесь.
- Тесты на `orbita-station-sim`: приветствие, таймер, громкость, свет, TV,
  ошибка MCP, рестарт.
- TV/search можно проверить до home-MCP.

**Gate:** симулятор проходит те же сценарии, что текущий агент; rollback —
quasar на `orbita-agent`.

## Фаза 5. Streaming TTS — gate (4–6 дней)

- Спайк перекрытия LLM+TTS на одном GPU: сравнить первый звук и суммарный TTS.
- `blockStreamingBreak: text_end`, `sentence`; порог по замеру.
- Несколько `TtsSpeak`, `Close` только у последнего; тесты:
  - прерывание на 1/2/3-м потоке;
  - обрыв WS;
  - barge-in;
  - короткий ответ.
- Если выигрыша нет — оставить один поток; streaming не включать.

**Gate:** первый звук длинного ответа лучше baseline без регрессии коротких и
без зависших потоков.

## Фаза 6. События и outbound (5–7 дней)

- wb-rules публикует события; home-MCP фильтрует, хранит cursor, dedup.
- `home_events` + MCP Events (signed webhooks) для критичных.
- HTTPS callback в LAN: reverse proxy/Tailscale.
- OutboundEvent: напоминания, объявления, критические уведомления.
- Критичные события дублируются без LLM.
- Quiet hours, rate limit, TTL.

**Gate:** протечка/заморозка доходят до владельца при упавшем OpenClaw.

## Фаза 7. Wyoming pipeline (8–12 дней)

- Wyoming ASR/TTS adapters.
- Pipeline-orchestrator: discovery, wake→audio→STT→intent→TTS→play, timers,
  announce.
- Один физический спутник + один виртуальный.
- VLAN/allowlist; бюджет VRAM (история: wyoming-openai вытеснял llama).

**Gate:** спутник работает через то же ядро, без AAPI-директив; станция не
деградирует.

## Фаза 8. Shadow, canary, миграция (5–7 дней)

- Shadow: `InboundEvent.shadow=true`; MCP возвращает `simulated`; сравнение
  tool calls/аргументов, не только текста.
- Перенос SQLite-задач/напоминаний; правило владельца планировщика.
- Canary 1–2 недели на одной станции/комнате.
- Переключение quasar на адаптер OpenClaw.
- `orbita-agent` остаётся развёрнутым, но выключенным.

**Gate:** 20 голосовых сценариев, 10 Telegram, 5 Wyoming; p95 не хуже baseline;
директивы, напоминания, память, safety без регрессий.

## Критерии приёмки

- 20 голосовых сценариев станции: приветствие, таймер, громкость, свет,
  погода, новости, мультик, TV, ошибка инструмента, рестарт.
- 10 текстовых сценариев Telegram.
- 5 сценариев Wyoming.
- p95 до первого звука не хуже baseline; длинные ответы — лучше.
- Сравнение tool calls в shadow совпадает по смыслу и аргументам.
- Падение OpenClaw/MCP не ломает wb-rules и станционный fallback.
- Каждый критерий помечен уровнем L0–L4 и числом прогонов.

## Риски

- LLM: глобальная GBNF/один слот; prefill OpenClaw; конкуренция голоса и фона.
- Adapter/quasar: TtsSpeak принадлежит quasar; streaming требует изменений.
- Device-actions: OpenClaw может не дать удобного механизма; go/no-go.
- Outbound: напоминания и события без push-пути теряются.
- Shadow без dry-run выполняет действия дважды.
- Identity: общий principal озвучит приватную память; DUID не передаётся.
- Home: home-MCP потребует command/ack/audit в wb-rules.
- Wyoming: без HA нужен свой orchestrator; VRAM.
- OpenClaw: API меняется; сторонние skills и prompt injection.
- Сроки 41–58 дней без буфера; стендовые команды — через человека.

## Rollback

- quasar: вернуть `PROCESSOR_BASIC_URL` на `orbita-agent`.
- OpenClaw: остановить LXC; станция на старом агенте.
- Планировщик: экспорт напоминаний/памяти перед canary; при откате —
  импорт обратно или явная потеря с записью в аудит.
- TTS/STT: старые endpoint’ы не удалять до конца пилота.
- Дом: wb-rules не зависит от OpenClaw; home-MCP можно выключить.
- Streaming: вернуть один поток/один `TtsSpeak`.
