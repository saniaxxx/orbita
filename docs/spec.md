# orbita — целевая спецификация

**Статус:** 08.11.2026, rev 2.1. Заменяет старые спеки; они доступны в
git-истории. Текущий стенд — `docs/state.md`, план — `docs/plan.md`.

## 1. Назначение

orbita — автономный домашний агент. Станция, Wyoming-спутник, Telegram,
мобильное приложение и веб — равноправные интерфейсы к одному ядру. Дом,
телевизор, поиск, календарь и музыка — инструменты ядра.

Голосовой стек не является центром системы. Яндекс-директивы, AAPI и quasar —
частности станционного интерфейса, а не контракт ядра.

## 2. Слои

```
┌─────────────────────────────────────────────────────────────┐
│ Интерфейсы                                                  │
│  станция/quasar · Wyoming · Telegram · mobile · web         │
│  wake, VAD, audio in/out, device capabilities, протоколы    │
└───────────────┬──────────────────────────────┬──────────────┘
                │ InboundEvent / OutboundEvent  │ STT / TTS
                ▼                               ▼
┌───────────────────────────────┐   ┌─────────────────────────┐
│ Ядро: OpenClaw                │   │ Общие AI-сервисы        │
│ память, планировщик, skills,  │   │ LLM (llama.cpp)         │
│ политики, approvals, MCP      │   │ STT (GigaAM)            │
│                               │   │ TTS (Vosk)              │
└───────────────┬───────────────┘   │ VAD, embeddings         │
                │ MCP                └─────────────────────────┘
                ▼
┌─────────────────────────────────────────────────────────────┐
│ Интеграции: home · tv · search · calendar · music           │
└─────────────────────────────────────────────────────────────┘
```

### 2.1. Общие AI-сервисы

- **LLM** — OpenAI-совместимый endpoint, локальный llama.cpp. Вызывается
  **только ядром OpenClaw**. Интерфейсы LLM не используют.
- **STT** — GigaAM. Используется интерфейсами: станция/quasar, Wyoming,
  голосовые сообщения Telegram.
- **TTS** — Vosk TTS. Используется интерфейсами для озвучивания ответа.
- Дополнительно: VAD, embeddings, поиск. Это инфраструктура, не агент.

### 2.2. LLM serving

**Единственный потребитель LLM — ядро (OpenClaw).** Станция, адаптер,
Wyoming, Telegram-канал и MCP-серверы LLM не вызывают: громкость и разбор
таймеров/напоминаний — детерминированные, без LLM. Legacy `orbita-agent`
обращается к llama только пока обслуживает станцию; после переключения quasar
на адаптер он — выключенный по смыслу fallback.

Поэтому «очередь к llama» — это очередь работ **внутри ядра**: голосовой ход
со станции может ждать, только пока OpenClaw занят своим же другим ходом —
Telegram-диалогом, фоновой автоматизацией с LLM (`agentTurn`), heartbeat
(по умолчанию каждые 30 мин) или ночным memory-dreaming. Напоминания из
адаптера срабатывают без LLM (command payload). Приоритет голоса внутри ядра
и выключение ненужных фоновых LLM-задач — задача конфигурации OpenClaw, а не
второго слота «для станции».

Один endpoint — не значит «один слот на всё». Нужно явно определить:

- грамматику/формат на **запрос**, а не глобальный GBNF: legacy-агенту JSON,
  OpenClaw — обычный текст и tool calls;
- `-np ≥ 2` или отдельные endpoint’ы для голоса и фона, с бюджетом KV/VRAM;
- приоритет: голосовой ход > интерактивный Telegram > фоновые задачи;
- запрет фоновых LLM-задач во время голосового хода;
- замер prefill системного промпта OpenClaw (skills+tools) на qwen3.5-9b q4;
- отдельный fallback: OpenClaw и legacy не должны одновременно владеть
  планировщиком и напоминаниями.

Без этого NFR-2 и «legacy как fallback» несовместимы.

### 2.3. Ядро

OpenClaw — основной кандидат: память, планировщик, skills, MCP-клиент,
политики инструментов, approvals, несколько каналов. Hermes — challenger для
сравнения; решение фиксируется после POC (D-16).

Ядро не знает про AAPI, `sound_louder`, quasar, Wyoming и Opus. Оно оперирует
текстом, инструментами и MCP-интеграциями.

### 2.4. Интерфейсы

| Интерфейс | Транспорт | Особенности |
|---|---|---|
| Станция | quasar/AAPI, `/process` WS | директивы, несколько TTS-потоков, push |
| Wyoming satellite | Wyoming/TCP | wake/VAD на спутнике, свой pipeline |
| Telegram/mobile/web | каналы OpenClaw | текст, медиа, голосовые сообщения |

Интерфейс владеет wake word, VAD, аудиоустройством, громкостью и протоколом.
Он не содержит памяти, планировщика и бизнес-логики дома.

## 3. Контракт ядра

```
InboundEvent {
  turn_id, request_id, timestamp,
  input_kind,           // voice | text | event | command
  text,                 // после STT
  stt_confidence,
  device_id, room,      // room: Option, у Telegram комнаты нет
  principal, household,
  session_key,
  locale, timezone,
  capabilities,         // audio_out, volume, display, media, actions[]
  shadow,               // adapter hint; shadow-инстанс обязателен
}

Reply {
  turn_id,
  blocks: [TextBlock { seq, text, final }],
  should_listen,
  error,
}

OutboundEvent {
  event_id, kind,       // reminder | alert | announce
  target,               // principal | device | room | household
  household,
  urgency,              // normal | important | critical
  text,
  created_at_ms, expires_at_ms,
  require_ack, retry,
}
```

- **Контракт v0.** Он фиксируется в фазе 0 и ревизуется после POC фазы 2,
  когда выбран механизм device-actions.
- `directives` не входят в Reply. Они живут только в station-адаптере.
- Действия устройства (`SetVolume`, `SetTimer`, `MediaPlay`) доставляются не
  как `Reply.actions`, а через механизм, который поддерживает выбранное ядро:
  client-side tools, device-MCP или structured output. **Решено (08.11.2026,
  L3): client tools OpenClaw** — адаптер передаёт функции в `tools` запроса
  `/v1/chat/completions`, OpenClaw возвращает `tool_calls`, адаптер исполняет
  их и отвечает `role: tool`, чтобы агент закончил ход. Проверено на
  `device_set_timer`/`device_step_volume`; по D-26 громкость ядру больше не
  передаётся, а единственный client tool сейчас — `reminder_create`
  (обязательство во времени в планировщик ядра).
- Capabilities устройства (карта директив из `prepare`) превращаются в
  `Capabilities.actions` (`set_volume`, `step_volume`, `set_timer`).
  `DeviceAction::StepVolume` — относительная громкость (louder/quieter);
  адаптер рендерит её в директиву сам, из intent layer.
- Голосовой ход несёт системную инструкцию стиля (1–2 предложения, без
  списков и источников, текущее время МСК). Сессия ядра для станции —
  `station:<duid>:<дата МСК>`: уточнения работают, история не растёт без конца.
- `HomeAction` в Reply отсутствует: дом — только MCP-tool.
- `OutboundEvent` нужен для напоминаний, событий дома и объявлений.
  Семантика: `target` резолвится в конкретное устройство/комнату с учётом
  quiet hours; доставка имеет ack/retry/expires_at; offline-device не
  блокирует остальные цели.
- `should_listen` сохраняет поведение станции после ответа.
- `Reply` — агрегат для хранения; живой стрим идёт кадрами `ReplyBlock
  {turn_id, seq, text, final}`. `DeviceAction`, `Cancel` и `OutboundAck` —
  отдельные v0-сообщения для адаптера.
- Планировщик OpenClaw эмитит `OutboundEvent`; путь до станции — через
  station-adapter/channel plugin, это отдельный чек-лист POC.

## 4. Потоковый ответ и TTS

OpenClaw умеет block streaming (`text_end`, `breakPreference: sentence`).
Граница ответственности:

- Если station-адаптер стоит **за `/process`**, он возвращает несколько
  `partialResponse`; TTS и `TtsSpeak` выполняет quasar.
- Если адаптер встроен **в quasar**, он может сам вызывать TTS и отправлять
  `TtsSpeak`/`Close`.
- Выбор фиксируется ADR D-22 в фазе 0. Нельзя одновременно утверждать
  «adapter шлёт TtsSpeak» и «quasar не переписывается».

Требования к streaming:

- промежуточные `TtsSpeak` не закрываются; `Close` — только у последнего;
- при прерывании/обрыве WS все потоки закрываются/отменяются;
- tool calls, reasoning и служебный текст не озвучиваются;
- блок режется по предложениям; порог подбирается замером, не «20–40 символов»;
- короткие ответы — один блок; стриминг только при подтверждённом выигрыше;
- TTS: Opus 48 кГц для станции, PCM для Wyoming, ресемплинг на адаптере.

Streaming — отдельный gate (D-19): сначала спайк перекрытия LLM+TTS на одном
GPU, затем тесты прерываний и пауз. Без этого multi-stream не включается.

## 5. Интеграции через MCP

Ядро общается с железом и внешними системами **только через MCP**:

- `home` — сервис orbita (`orbita-home-mcp`) на `orbita-hub`: сам говорит с
  Wiren Board по MQTT и отдаёт MCP-инструменты;
- `tv` — сервис orbita `orbita-tv` на `orbita-hub` (`:8898/mcp`): TCL через
  Remote v2, Sber через ADB (`deploy/tv/tv-adb.py`);
- `search` — сервис orbita `orbita-mcp-search` (`:8899/mcp`) поверх SearXNG;
- позже: calendar, music, cameras, intercom.

### 5.1. Дом

```
OpenClaw → orbita-home-mcp (orbita-hub) → MQTT Wiren Board → устройства
```

- `orbita-home-mcp` — сервис orbita, как `orbita-tv`: подписывается на MQTT
  Wiren Board, держит state cache, публикует команды в `…/on`-топики,
  подтверждает `executed` по state-топику с таймаутом, хранит сцены, policy,
  идемпотентность и аудит **у себя**. wb-rules под MCP **не переписываются**
  и ничего про MCP не знают (решение владельца, 08.11.2026).
- wb-rules остаются тем, чем являются сейчас: локальные автоматизации и
  safety/interlocks Wiren Board, работающие без orbita. home-MCP не обходит
  их — он управляет теми же топиками, что и физические выключатели.
- Агент не имеет MQTT-креденшелов и не знает сырых топиков.
- Текущий `orbita-home` (MQTT-клиент внутри legacy-агента) — основа для
  backend `orbita-home-mcp`, сам по себе подлежит замене.

Инструменты (MCP-сервер `home` даёт префикс `home_`):

- `devices(room?, kind?)` — фильтр по комнате принимает падежи;
- `state(device_id)`;
- `command(device_id, capability, value, request_id, confirm?)`;
- `scenes()` / `scene_run(name, request_id?)`;
- `events(cursor, epoch?, limit?)`;
- `health()`.

Команда возвращает `executed | simulated | denied | failed | conflict` +
фактическое состояние. Состояние имеет timestamp и `stale`; `command`
идемпотентна по `request_id`, но тот же id с другими аргументами даёт
`conflict`.

### 5.2. События дома

- state cache обновляется всегда;
- события — только значимые переходы: `home.leak`, `home.frost`,
  `home.door`, `home.motion`, `home.mode`;
- телеметрия не будит LLM, а обновляет состояние;
- доставка — MCP Events (экспериментальный профиль с signed webhooks) или
  `home_events` + планировщик; для LAN нужен HTTPS callback;
- payload события — недоверенные данные, не инструкция;
- safety-сценарии выполняются в wb-rules независимо от OpenClaw;
- события выводит сам `orbita-home-mcp` из изменений MQTT-топиков (wb-rules
  ничего не публикуют специально);
- критические оповещения (leak/frost) `orbita-home-mcp` дублирует напрямую в
  Telegram/станцию, без LLM;
- дедупликация, rate limit, quiet hours.

## 6. Идентичность и подтверждения

- `household` — общий дом; `person` — конкретный человек.
- Голосовые устройства (станция, Wyoming) работают как `household` с
  ограниченной памятью; приватная память Telegram не озвучивается в комнатах.
- `DEFAULT_DUID` — временное ограничение TS-quasar; deviceId надо передавать.
- Опасные действия: подтверждение **out-of-band** (Telegram/mobile) с TTL;
  голосовой PIN не считается безопасным. До этого dangerous-capabilities в
  home-MCP — deny by default.

## 7. Безопасность

- OpenClaw — отдельный LXC/хост, без секретов станции и доступа к
  `/etc/orbita/secrets.env`.
- Allowlist tools: без `exec`/`fs`/`browser`, если не доказана необходимость;
  сторонние skills — pinned, без автоустановки; egress firewall LXC.
- Тест на prompt injection из поиска/событий: инструменты не должны давать
  дополнительных прав.
- Замок, котёл, ворота, калитка — dangerous: fail-safe, аудит, подтверждение.
- MCP-серверы слушают приватную сеть/loopback, с токеном или mTLS.
  Реализовано (08.11.2026) общим крейтом `orbita-mcp-http`: сетевые клиенты —
  только с `Authorization: Bearer`, loopback на том же хосте доверен, без
  токена сервис не стартует на сетевом адресе. Ядро не получает SSH/root
  доступа к хостам интеграций.
- Wyoming не имеет аутентификации: только доверенная VLAN/allowlist.
- Если OpenClaw/MCP недоступны, wb-rules и голосовой стек продолжают работать.

## 8. Fast paths и shadow

- Детерминированные интенты (громкость, таймер, напоминания, свет, TV) — это
  бизнес-логика. Их место фиксируется D-25: отдельный intent layer перед
  ядром или tools в ядре; прямые MCP-вызовы мимо ядра запрещены.
- Shadow — отдельный инстанс ядра с отключёнными cron, планировщиком,
  OutboundEvent и записью в постоянную память. MCP смотрят в dry-run/read-only;
  для home-MCP это отдельный процесс с `HOME_MCP_SIMULATED=1`, а не флаг,
  который OpenClaw обязан пробросить. State читается из реального home-MCP.
  Сравниваются tool calls/аргументы, а не только текст. Tee запросов — в
  quasar или station-адаптере, фиксируется ADR.
- Два ядра не должны одновременно владеть планировщиком, напоминаниями и push.

## 9. Требования

- **FR-11.** Ядро — OpenClaw; интерфейсы подключаются адаптерами.
- **FR-12.** LLM вызывает только ядро; STT/TTS — общие сервисы интерфейсов
  (для голосовых сообщений Telegram допускается вызов STT ядром через
  provider-плагин — уточнить).
- **FR-13.** Станция, Wyoming и текстовые каналы используют один контракт
  `InboundEvent/Reply/OutboundEvent`; директивы не выходят за station-адаптер.
- **FR-14.** Дом доступен только через home-MCP; wb-rules сохраняет safety.
- **FR-15.** Streaming-ответ и пофразовый TTS — только после gate D-19.
- **FR-16.** TV: Remote v2 для TCL, ADB для Sber/SaluteTV; «мультик» по
  умолчанию — TV в детской.
- **FR-17.** MCP-события дома доставляются с фильтрацией и severity; критичные
  дублируются без LLM.
- **FR-18.** Опасные действия требуют out-of-band подтверждения и аудита.
- **FR-19.** OutboundEvent доставляет напоминания и объявления в выбранный
  target, а не broadcast во все интерфейсы.
- **FR-20.** Shadow-режим не выполняет побочных эффектов.

- **NFR-1.** Локальный контур; облако не обязательно.
- **NFR-2.** p95 до первого звука — baseline фиксируется в фазе 0; цель ≤2 с
  на коротких ответах, без деградации fast paths.
- **NFR-3.** Дом работает при падении OpenClaw/MCP.
- **NFR-4.** Один LLM-сервис (один endpoint или несколько endpoint’ов одного
  сервиса), один STT, один TTS; без дублей на интерфейс.
- **NFR-5.** Секреты не в git; сертификаты `0600`.
- **NFR-6.** OpenClaw pinned version; contract-тесты адаптера в CI.

## 10. Решения

- **D-16. OpenClaw — ядро (кандидат).** Станция/Wyoming/Telegram — адаптеры;
  Hermes сравнивается в POC, финальный выбор после него.
- **D-17. Три слоя.** AI-сервисы, ядро, интерфейсы. Интеграции — вертикальные
  MCP-домены, не отдельный слой.
- **D-18. Дом — сервис orbita `orbita-home-mcp` поверх MQTT Wiren Board.**
  Никаких прямых MQTT/реле из агента; wb-rules под MCP не меняются.
- **D-19. Потоковый TTS — gate.** Блоки по предложениям, несколько
  `TtsSpeak`, `Close` только у последнего; включать после спайка и тестов
  прерываний.
- **D-20. Старый `orbita-agent` — legacy.** Остаётся fallback до приёмки; кто
  владеет планировщиком и напоминаниями, фиксируется на каждой фазе.
- **D-21. LLM serving.** Грамматика per-request, ≥2 слота или раздельные
  endpoint’ы, приоритет голоса, замер prefill.
- **D-22. Граница station adapter — за `/process` (08.11.2026).** Отдельный
  сервис `orbita-station-adapter` на `orbita-brain` рядом с Gateway (токен не
  покидает loopback); живой TS-quasar не меняется, TtsSpeak/Close остаются у
  него. Детерминированный слой громкости/таймера (D-25) — общий крейт
  `orbita-intents`, вызывается адаптером до ядра. Для streaming (D-19) quasar
  всё равно должен научиться нескольким `partialResponse` и пофразовому TTS.
- **D-23. Идентичность и approvals.** household + person; опасные действия
  подтверждаются out-of-band.
- **D-24. Shadow — отдельный инстанс ядра.** У него отключены cron,
  планировщик, OutboundEvent и запись в постоянную память; MCP смотрят в
  dry-run/read-only. Флага в InboundEvent недостаточно: OpenClaw не обязан
  пробрасывать его в tools/call.
- **D-25. Deterministic intent layer.** Fast paths — явный слой с аудитом, не
  скрытая логика интерфейса.
- **D-26. Настройки устройства — у интерфейса, обязательства во времени —
  у ядра (08.11.2026, решение владельца).**
  - Громкость, плеер, Bluetooth, звуки (`set_sound_volume*`, `player_*`,
    `bluetooth`, `play_iot_sound`) — пульт колонки. Их исполняет адаптер через
    intent layer; ядру они не передаются как tools (в `Capabilities` они
    остаются как описание устройства).
  - Напоминания, будильники и таймеры — одно понятие «обязательство во
    времени». Им владеет планировщик ядра (OpenClaw automations), доставка —
    в любой канал по выбору пользователя: `announce` в Telegram, `webhook` в
    station-адаптер → push на колонку через quasar. «Напомни через час в
    телеграм» сказано колонке — доставляется в Telegram.
  - Нативные `timer_*`/`alarm_*` станции не являются источником истины и в
    целевой схеме не используются (`timer_stop/cancel/pause/resume`,
    `alarm_*` требуют `id` колонки). Нативный таймер остаётся только как
    офлайн-режим адаптера без планировщика.
  - Реализация (08.11.2026, L3): адаптер создаёт задачу `openclaw cron add`
    (command payload, без LLM при срабатывании) — детерминированно для
    «таймер/засеки/напомни …» и через client tool `reminder_create` для
    свободных формулировок; станционная доставка — webhook на
    `/outbound?device=…&key=…` → quasar `push/text`. Секрет webhook проверяется.

## 11. Не входит в эту спеку

- замена quasar/AAPI на другой протокол станции;
- замена Vosk TTS/GigaAM без отдельного решения;
- управление замком/котлом голосом без политики подтверждений;
- облачный хостинг и multi-tenant.

## 12. Текущее состояние

Работающий стенд описан в `docs/state.md`. Исторические замеры и старые
решения — в git-истории. План перехода — `docs/plan.md`.
