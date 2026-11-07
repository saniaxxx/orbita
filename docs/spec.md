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
  device_id, room,
  principal, household,
  session_key,
  locale, timezone,
  capabilities,         // audio_out, volume, display, directives, media
  shadow,               // true = только сравнение, без побочных эффектов
}

Reply {
  turn_id,
  blocks: [TextBlock { seq, text, final }],
  should_listen,
  error,
}

OutboundEvent {
  target,               // principal | device | room
  urgency,              // normal | important | critical
  text, ttl,
  require_ack,
  retry_policy,
}
```

- **Контракт v0.** Он фиксируется в фазе 0 и ревизуется после POC фазы 2,
  когда выбран механизм device-actions.
- `directives` не входят в Reply. Они живут только в station-адаптере.
- Действия устройства (`SetVolume`, `SetTimer`, `MediaPlay`) доставляются не
  как `Reply.actions`, а через механизм, который поддерживает выбранное ядро:
  client-side tools, device-MCP или structured output. Это go/no-go POC.
- Capabilities устройства (карта директив из `prepare`) должны превращаться в
  набор tool’ов на ход/канал. Проверка этого — критерий POC.
- `HomeAction` в Reply отсутствует: дом — только MCP-tool.
- `OutboundEvent` нужен для напоминаний, событий дома и объявлений.
  Семантика: `target` резолвится в конкретное устройство/комнату с учётом
  quiet hours; доставка имеет ack/retry/TTL; offline-device не блокирует
  остальные цели.
- `should_listen` сохраняет поведение станции после ответа.
- `blocks` — поток, а не массив готового ответа; `final=true` закрывает ход.
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

- `home` — MCP-фасад над wb-rules;
- `tv` — MCP-сервер, Remote v2 или ADB;
- `search` — MCP-мост к SearXNG;
- позже: calendar, music, cameras, intercom.

### 5.1. Дом

```
OpenClaw → home-MCP → MQTT → wb-rules → устройства
```

- `wb-rules` — единственный исполнитель и владелец safety/interlocks/сцен.
- home-MCP не пишет в реле в обход правил; он вызывает разрешённые команды и
  сцены, проверяет policy и пишет аудит.
- Агент не имеет MQTT-креденшелов и не знает сырых топиков.
- Для `executed` нужны command-устройства/ack/audit в wb-rules. Это отдельная
  работа; текущий `orbita-home` ходит в MQTT из агента и подлежит замене.

Инструменты:

- `home_devices(room?, kind?)`;
- `home_state(device_id)`;
- `home_command(device_id, capability, value, request_id)`;
- `home_scenes()` / `home_scene_run(name)`;
- `home_events(cursor)`;
- `home_health()`.

Команда возвращает `accepted | executed | denied | failed` + фактическое
состояние. Состояние имеет timestamp и `stale`.

### 5.2. События дома

- state cache обновляется всегда;
- события — только значимые переходы: `home.leak`, `home.frost`,
  `home.door`, `home.motion`, `home.mode`;
- телеметрия не будит LLM, а обновляет состояние;
- доставка — MCP Events (экспериментальный профиль с signed webhooks) или
  `home_events` + планировщик; для LAN нужен HTTPS callback;
- payload события — недоверенные данные, не инструкция;
- safety-сценарии выполняются в wb-rules независимо от OpenClaw;
- критические оповещения (leak/frost) дублируются напрямую из wb-rules/MCP в
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
- Wyoming не имеет аутентификации: только доверенная VLAN/allowlist.
- Если OpenClaw/MCP недоступны, wb-rules и голосовой стек продолжают работать.

## 8. Fast paths и shadow

- Детерминированные интенты (громкость, таймер, напоминания, свет, TV) — это
  бизнес-логика. Их место фиксируется D-25: отдельный intent layer перед
  ядром или tools в ядре; прямые MCP-вызовы мимо ядра запрещены.
- Shadow — отдельный инстанс ядра с отключёнными cron, планировщиком,
  OutboundEvent и записью в постоянную память. MCP смотрят в dry-run/read-only;
  state читается из реального home-MCP. Сравниваются tool calls/аргументы, а не
  только текст. Tee запросов — в quasar или station-адаптере, фиксируется ADR.
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
- **FR-19.** OutboundEvent доставляет напоминания и объявления во все
  интерфейсы.
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
- **D-18. Дом — MCP-проекция wb-rules.** Никаких прямых MQTT/реле из агента.
- **D-19. Потоковый TTS — gate.** Блоки по предложениям, несколько
  `TtsSpeak`, `Close` только у последнего; включать после спайка и тестов
  прерываний.
- **D-20. Старый `orbita-agent` — legacy.** Остаётся fallback до приёмки; кто
  владеет планировщиком и напоминаниями, фиксируется на каждой фазе.
- **D-21. LLM serving.** Грамматика per-request, ≥2 слота или раздельные
  endpoint’ы, приоритет голоса, замер prefill.
- **D-22. Граница station adapter.** ADR в фазе 0: adapter за `/process` или
  внутри quasar; TtsSpeak/Close принадлежат quasar. Отдельно выбрать живой
  TS-quasar или Rust `orbita-quasar`; при варианте «за `/process`» quasar
  обязан принимать несколько `partialResponse` и делать пофразовый TTS.
- **D-23. Идентичность и approvals.** household + person; опасные действия
  подтверждаются out-of-band.
- **D-24. Shadow — отдельный инстанс ядра.** У него отключены cron,
  планировщик, OutboundEvent и запись в постоянную память; MCP смотрят в
  dry-run/read-only. Флага в InboundEvent недостаточно: OpenClaw не обязан
  пробрасывать его в tools/call.
- **D-25. Deterministic intent layer.** Fast paths — явный слой с аудитом, не
  скрытая логика интерфейса.

## 11. Не входит в эту спеку

- замена quasar/AAPI на другой протокол станции;
- замена Vosk TTS/GigaAM без отдельного решения;
- управление замком/котлом голосом без политики подтверждений;
- облачный хостинг и multi-tenant.

## 12. Текущее состояние

Работающий стенд описан в `docs/state.md`. Исторические замеры и старые
решения — в git-истории. План перехода — `docs/plan.md`.
