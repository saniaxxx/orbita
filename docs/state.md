# orbita — состояние на 08.11.2026

Короткий снимок для агента, который только что забрал репозиторий.
Целевая архитектура — `docs/spec.md`, план перехода — `docs/plan.md`.
История и старые замеры — в git-истории.

## 1. Куда идём

Автономный домашний агент на сервере. Станция, Wyoming-спутник, Telegram,
mobile и web — интерфейсы к одному ядру. Ядро — OpenClaw (кандидат; Hermes
сравнивается в POC). LLM вызывает только ядро; STT/TTS — общие сервисы.

Станцию обслуживает `orbita-station-adapter` перед OpenClaw. Legacy
`orbita-agent` остановлен и отключён 08.11.2026 (вместе с `orbita-warmup`),
откат — `pct exec 101 -- systemctl enable --now orbita-agent orbita-warmup`.

## 2. Стенд

| Где | Адрес | Что |
|---|---|---|
| pve3 | `192.168.250.221` | Proxmox VE |
| CT 100 `orbita-ai` | `192.168.250.102` (статический) | GPU/AI: llama.cpp, GigaAM, Vosk TTS |
| CT 101 `orbita-hub` | `192.168.250.103` | interfaces/integrations: quasar, mcp-search, orbita-tv, orbita-home-mcp, dashboard, https-proxy, SearXNG; orbita-agent остановлен |
| CT 102 `orbita-brain` | `192.168.250.105` | ядро: OpenClaw 2026.11.1, Node 24, gateway `127.0.0.1:18789` |
| Станция | `192.168.250.115` | домены Яндекса в `/etc/hosts` → `.103` |
| Wiren Board | `192.168.250.110` | MQTT, wb-rules |
| TCL TV | `192.168.250.109` | Remote v2 |
| Sber TV | `192.168.250.111` | SaluteTV, ADB 5555 |

**Стенд после перезагрузки хоста 13.11.2026.** pve3 перезагрузился в 18:08;
CT 100 был на DHCP и получил `.108`, а на `.102` смотрят quasar (precheck TTS),
OpenClaw (llama `:8090`), `orbita-processor` и `secrets.env` — станция не
работала, quasar ушёл в цикл рестартов. Адрес `CT 100` переведён в статический
`.102` (`pct set 100 -net0 …` + `/etc/network/interfaces`), контейнер не
перезапускали; бэкап — `/root/backup-net-20261113/` на pve3 и в CT 100.
Проверено после правки (13.11, L3): quasar, TTS `:9170`, ASR `:8006` (фраза
распознана, `end_of_utt` сработал), адаптер `/process` — 4 хода, стриминг
0,85 с, `shouldListen` на сказке с вопросом, TV/свет отвечают.

**Голос не ждёт фоновую задачу (18.11.2026).** У llama был один слот, и
фоновая генерация держала его минутами: под одной фоновой задачей голосовой
ход не уложился в 45 с, следующий занял 10,2 с вместо 0,6 с. Теперь `-np 2`
(два слота по 16384, суммарный контекст тот же 32768, VRAM 7450/8151 МиБ):
под той же фоновой нагрузкой ходы станции — 3,2 / 1,0 / 0,9 с. В логе llama
видно `n_slots = 2`, фоновая задача на слоте 0, голос на слоте 1. В OpenClaw
`contextWindow` модели выставлен 16384 под размер слота. Бэкапы: юнит
llama — `/root/backup-llama-np-20261118/` на CT 100, конфиг OpenClaw —
`openclaw.json.bak-slots-20261118`. Откат: `-np 1` + `contextWindow 32768`
и рестарт llama.

## 3. Что работает

- **LLM**: локальный OpenAI-совместимый endpoint, `qwen3.5-9b-q4km`.
- **STT**: GigaAM на CUDA/CPU; используется quasar. EOU-фикс (08.11.2026,
  ветка `feat/asr-eou`): energy gate 0.3 + cap 900, усиление один раз
  (`RECOGNIZE_GAIN=1`); проверен L2 на записях и L3 на живом `:8006`.
- **TTS**: Vosk TTS на CUDA; HTTP и Opus 48 кГц для станции. Уникальная фраза
  ~0,93 с, повтор ~0,2 с.
- **Станция**: quasar/AAPI, `/process` → `orbita-station-adapter` → OpenClaw
  (стриминг первого предложения, §4a). Ниже — прогон legacy-агента до
  переключения: директивы громкости/таймеров работают. Голосовой прогон `deploy/verify/run-directives.sh`
  (фразы синтезированы Vosk TTS, не macOS `say`) — 13/13: громкость
  louder/quiter/set-level, 3 таймера, напоминание, пустой шаг. Уровень L3:
  стенд, эмулятор станции, живая станция не задействована (08.11.2026).
- **TV**: `orbita-tv` MCP, `devices=2`; TCL — Remote v2, Sber — ADB через
  `tv-adb.py`; 10 MCP-инструментов у агента.
- **TV-скилл**: «включи мультик …» → VK search/play через ADB, по умолчанию
  детская; питание/громкость через ADB.
- **Дом**: wb-rules владеет safety/сценами; свет Wiren Board. `orbita-home-mcp`
  (08.11.2026, ветка слита в `main`) — реальный backend: state cache из MQTT,
  команды в `…/on`, ack по state-топику (2 с), идемпотентность, policy,
  audit. Оффлайн L2: 20 unit + 1 fake-broker тест `orbita-home`, 15 тестов
  `orbita-home-mcp`, MCP handshake по HTTP. Развёрнут на `orbita-hub`
  (L3, без станции): 20/20 устройств `available`, свет из OpenClaw — 3,7 с,
  `executed`; голосом со станции ещё нет. Сцены не настроены и не выдаются.
- **Поиск**: `orbita-mcp-search` + SearXNG.
- **OpenClaw**: установлен в `orbita-brain`; gateway active на loopback,
  локальная модель `local/qwen3.5-9b-q4km`, context 16384 на слот.
  MCP search/TV подключены напрямую: `http://192.168.250.103:8899/mcp` и
  `:8898/mcp` с `Authorization: Bearer` (см. «MCP по сети» ниже);
  `tools.toolSearch=false` отдаёт схемы напрямую.
  Профиль `minimal` + `bundle-mcp`: в промпте только `search__search`,
  `tv__*` и служебные minimal-инструменты, без `exec`/`fs`/`browser`.
  Промпт ~4.7k токенов, prefill ~2.0 с; проверены реальные tool calls:
  `search__search` и `tv__state`. Уровень — стенд без станции (L3).
  Замеры 08.11: 20 запросов в свежих сессиях — p50 6,0 с, p95 9,2 с;
  search — 1 вызов `search__search`, 4,7–6,7 с; TV — `tv__state`+`tv__list`,
  7,5–9,5 с; уточняющие ответы — 1,3–2,7 с. Все ходы `ok`, `failures=0`.
  TCL (гостиная) не отвечает по сети (таймаут), детская SBER отвечает.
  **D-21 частично:** глобальная GBNF убрана, legacy-агент шлёт `grammar`
  per-request; llama `-c 32768`, KV q8, `-np 2` (16k на слот, с 18.11).
  POC запускать при простое станции. Свободно ~144 МиБ VRAM.

## 4. Что ещё не сделано

- Станция переключена на адаптер (08.11.2026, quasar `PROCESSOR_BASIC_URL` →
  `http://192.168.250.105:8081/process`, бэкап `.env.bak-adapter-20261108`).
  Проверено симулятором голосом через quasar (L3): «привет» — ответ + TTS,
  «сделай громче» — `sound_louder`. Живая станция — проверка владельца.
- Telegram не подключён: нет токена бота и chat id. «Напомни в телеграм»
  честно отвечает «Телеграм пока не подключён, напомню здесь».
- Нет каналов mobile/web. home-MCP развёрнут на `orbita-hub` (L3) и ходит
  через OpenClaw; голосовой проверки со станции ещё нет, нужен владелец.
- LLM вызывает только OpenClaw (станция и адаптер LLM не используют).
  Очередь к одному слоту llama — только между ходами самого OpenClaw:
  Telegram, фоновые `agentTurn`, heartbeat каждые 30 мин, ночной
  memory-dreaming. Legacy ходит в llama, только если quasar вернуть на него.
- Workspace-ворота проходят с `CARGO_TARGET_DIR=/tmp/orbita-target`
  (зависание `orbita-proto` было из-за target на внешнем диске).
- Нет событий дома через MCP с фильтрацией leak/frost/door/motion/mode:
  `home_events` отдаёт изменения MQTT с cursor/epoch, но значимые переходы и
  доставка — фаза 6.
- Нет Wyoming-адаптера и второго голосового интерфейса.
- `orbita-agent` смешивает ядро и station-адаптер; это legacy.

## 4a. Задержка голоса (замеры 08.11.2026)

Где уходят секунды на ходе «вопрос → первый звук» (L3; живые ходы владельца
11:29/11:32 разобраны по логам):

- **Конец фразы (ASR) — был главным, исправлено (§4b).** Конец фразы решает
  наш `qsg-asr` (GigaAM, CT 100), не колонка. ten_vad считал усиленный шум
  комнаты речью, тишины 0,8 с не наступало: «Ты кто?» распознан на 1,7 с,
  финал — через 4,8 с; длинная фраза — по пределу 8 с. Режим
  `quasistreaming` с 2 повторами отвергнут (финалит «С», «Алиса.»).
  Энергетический порог и однократное усиление — §4b.
- **Prompt cache — исправлено.** Минутные часы в системной инструкции
  адаптера сбрасывали кэш llama на первом ходе каждой минуты (3,5–6 с
  prefill 8–14 тыс. токенов). Теперь в system — только дата, время — в конце
  реплики; ходы через границу минуты — prefill ~0,3 с.
- **OpenClaw на 2 ядрах.** Тёплый ход: ~0,3 с до вызова модели. Если с ходом
  совпадает фоновое обслуживание (`maintenance:sqlite-snapshots`,
  12,4 МиБ), до модели проходило до 6,6 с. Ядер стало 4 (08.11,
  `pct set 102 -cores 4`).
- **Холодный ход OpenClaw (13:15, 13:28):** на первом ходе после простоя
  OpenClaw синхронизирует память (`memory sync: indexing memory files`) —
  4,8 с до старта хода, хотя memory-инструментов у агента в профиле `minimal`
  нет (`memory.search.provider` по умолчанию `openai`). Первое предложение
  тогда приходит позже 5 с, quasar шлёт станции `continue` (живая колонка
  перезапрашивает, `orbita-station-sim` continue не умеет — пустой ответ).
  `memory.search.enabled=false` — сделано 08.11 (бэкап
  `openclaw.json.bak-memory-20261108`). Дневная сессия станции
  к вечеру — 15 тыс. токенов (149 сообщений с тестами): промах кэша = 6,5 с;
  кандидат — сброс сессии станции по простою.
- **llama падала по памяти** (12:42 OOM killer по ОЗУ, 13:01 CUDA OOM):
  CT 100 — 12 ГБ без swap, VRAM 7,8/8,1 ГБ; лишний процесс GigaAM (тестовый
  экземпляр ASR) роняет llama → ход 30+ с. Тесты ASR — только с
  `CUDA_VISIBLE_DEVICES=` и одним экземпляром.
- **Стриминг (D-19) включён:** адаптер `STREAM_REPLIES=true`
  (drop-in `20-stream.conf`), quasar `PROCESSOR_STREAMING=true` + патч
  `deploy/quasar/orbita-streaming.patch` (бэкап исходников и `.env` —
  `/root/backup-quasar-stream-20261108/`). Адаптер режет ответ один раз
  (первое предложение + остаток). Первый звук сказки после правки 14.11 —
  2,9 с против 3,3 с полного ответа (L3, симулятор).
- **Вопрос в конце ответа держит диалог.** Адаптер ставит `shouldListen`,
  если ответ кончается вопросом; инструкция запрещает дежурные хвосты «Чем
  ещё помочь?». Рабочий путь на живой колонке (L4, 08.11 и 14.11): один
  `AliceResponse` с `ShouldListen=true`, quasar закрывает диалог, колонка
  сама открывает новый на ответ (08.11 после «Я включу телевизор в
  гостиной?» следующая реплика записана).
- **Стриминг + вопрос вешал колонку (L4, 14.11.2026, 11:41 МСК).** После
  «Сказку. Сказку.» колонка ответила «Что хотите послушать?» и стояла 16
  минут: в логах quasar ни распознавания, ни ответа до нажатия паузы.
  Причина — потоковый путь оставлял диалог открытым и присылал
  `ShouldListen` вторым `AliceResponse` после аудио (плюс `IsStreaming`);
  станция на такой ответ не реагирует. Исправление 14.11: quasar ждёт весь
  текст (части синтезируются параллельно по мере прихода), шлёт **один**
  `AliceResponse` с полным текстом и `ShouldListen` — как в непоточном
  пути, — затем отдаёт части TTS отдельными потоками (`Close` у последнего)
  и закрывает диалог. Симулятор (L3): один ответ, `shouldListen=true`,
  диалог `serverFinished`, первый звук 2,9 с. Живая колонка — проверка
  владельца. Откат: `PROCESSOR_STREAMING_FINAL=false` в `20-stream.conf`
  quasar возвращает прежнее поведение; полностью — снять оба
  `20-stream.conf`; файлы до правки —
  `/root/backup-quasar-deferred-20261114/`.
- **Ворота стриминга (18.11.2026, L3, симулятор).** Короткий ответ — один
  ответ, обычный путь без стриминга (`4` на «два плюс два»). Сказка с
  вопросом — один ответ с `shouldListen=true`, диалог `serverFinished`, первый
  звук 2,7 с. Обрыв соединения посреди озвучки — диалог `webSocketClose`,
  quasar пишет `the rest of the streamed reply is dropped` и не бросает
  исключение; без этой проверки был `WebSocket is not open` со стеком.
  Barge-in на том же соединении симулятором не проверить (он открывает новое
  соединение на каждую реплику); в коде заменивший `dialogId` даёт ту же
  остановку. Решение: стриминг оставляем включённым.
- **Модель повторяла служебную пометку времени** (L3, 14.11: «…хочу ли я
  ещё? [сейчас 12:30]»). Инструкция усилена: пометка не часть реплики, не
  повторять и не пересказывать.
- **«Включи телевизор в гостиной» не включал (08.11, L4).** Модель вызвала
  `tv__power` без `device`, получила «телевизоров несколько, укажи какой» и
  переспросила пользователя. Ответ инструмента теперь велит повторить вызов с
  комнатой из запроса, описание `device` — «комната из запроса». Бэкап
  бинарника — `/root/backup-tv-20261108/`.
- **Сессия ядра — один разговор (08.11, решение владельца: пауза 1 мин).**
  Дневная сессия станции выросла до 15–30 тыс. токенов, модель путалась («ты
  кто?» → «Чем могу помочь?»). Адаптер начинает новую сессию OpenClaw после
  60 с тишины (`SESSION_IDLE_SECS`), считая от конца озвучки ответа. Симулятор
  (L3): сказка + «да, расскажи ещё» — одна новая сессия, модель продолжила ту
  же сказку. Ночная чистка сессий станции старше 7 дней — таймер
  `orbita-station-sessions-cleanup` на CT102 (04:30 МСК). Бэкап бинарника
  адаптера — `/root/backup-sessions-20261108/` на CT102.
- **Симулятор отвечает на `continue`** (эхо `@@mm_semantic_frame`, как живая
  колонка): медленный ход больше не выглядит пустым ответом.
- **quasar ждал legacy-агента при старте:** `orbita-health.conf` проверял
  `127.0.0.1:8080/health` — после остановки агента любой рестарт quasar
  зависал бы и падал. Теперь ждёт адаптер `192.168.250.105:8081/health`.
- Откат стриминга: удалить оба `20-stream.conf`, `daemon-reload`, рестарт
  адаптера и quasar (патч без флага ведёт себя как раньше); полностью — вернуть
  файлы из `/root/backup-quasar-stream-20261108/`. Адаптер прошлой версии —
  `/usr/local/bin/orbita-station-adapter.prev-stream` на CT 102.
- Запись голоса в `qsg-asr` выключена по решению владельца (drop-in
  `30-no-recordings.conf`, `UnsetEnvironment=LOG_PATH`); старые файлы в
  `/tmp/asr-audio` не удалялись.
- Живая станция (L4, владелец, 08.11): длинный ответ со стримингом («сказка»)
  звучит целиком, двумя частями без обрыва.

## 4b. EOU в ASR (08.11.2026, включено на стенде)

- Проблема: ten_vad на усиленном потоке станции считает шум после фразы
  речью, поэтому 800 мс тишины не наступает и quasar получает первый
  `end_of_utt=true` только по `max_utterance` (8 с) или позже. Пример:
  «Ты кто?» — partial 11:32:01.7, final 11:32:06.5.
- Патч `deploy/asr/qsg-orbita.patch` (база qsg `e1770f1`, применён на CT100
  в `/opt/qsg`):
  - energy gate: сегменты VAD с RMS ниже `peak * 0.3` отбрасываются;
    `VAD_ENERGY_GATE_MAX_RMS=900` ограничивает порог, чтобы громкий выброс
    не подавил более тихую речь; пик сбрасывается на каждую фразу;
  - `INPUT_GAIN=30` применяется один раз в streaming path;
    `RECOGNIZE_GAIN=1` — второй раз не усиливаем. Двойное усиление реально
    ломало текст на записях: `Кто?` вместо `Алиса, ты кто?`, `Сань, ты кто?`
    вместо `Ты кто?`.
- Юнит: drop-in `/etc/systemd/system/qsg-asr.service.d/10-orbita-eou.conf`
  с `RECOGNIZE_GAIN=1`, `VAD_ENERGY_GATE_RATIO=0.3`,
  `VAD_ENERGY_GATE_MAX_RMS=900`; `INPUT_GAIN=30` остаётся в основном юните.
  Бэкап юнита и drop-in: `/root/backup-qsg-eou-20261108/`. Откат:
  `git checkout -- asr_core/processors/vad_processor.py quasistreaming-server.py`
  в `/opt/qsg`, удалить `10-orbita-eou.conf`, `daemon-reload`,
  `systemctl restart qsg-asr`.
- Проверка L2 (CPU-экземпляр на записях, `INPUT_GAIN=1`, gate 0.3/cap 900):
  «Алиса, ты кто?» — final 2.98 с, полный текст; длинная 9-секундная фраза —
  EOU примерно через 1 с после последней речи вместо `max_utterance`;
  «Алиса, … пауза … вопрос» не обрезается; ложного финала `С.` при
  `VAD_MIN_SPEECH_DURATION=1.5` нет.
- Проверка L3 (живой `qsg-asr` на GPU, прямой replay в `:8006`): «Алиса, ты
  кто?» — final 2.59 с; длинная фраза — final 7.17 с, полный текст; фраза с
  паузой после «Алиса» — final 3.84 с, полный текст; у всех
  `reason=eou_silence`. `llama` active, новых рестартов нет.
- Симулятор (Vosk TTS → Ogg → quasar, duid станции) на тёплом OpenClaw:
  короткая «Алиса, ты кто?» — первый звук 1,5 с (второй ход; первый,
  разогревающий, 3,4 с), длинная — 5,5 с (первый звук 4,6 с), «Алиса,
  какая сейчас погода в Москве?» — 7,7 с; с паузой 0,5 с после «Алиса» —
  2,4 с. Везде полный ASR-текст и ответ. Пустой ответ в первом прогоне был
  из-за quasar `MAX_WAIT=5 с` + `continue`/`@@mm_semantic_frame`, который
  `orbita-station-sim` тогда не умел (теперь умеет, §4a); это не ASR.
- Краевой случай: искусственная пауза 1,0 с после «Алиса» больше порога
  EOU 800 мс — итог «Алиса, какая сейчас»; запятая и пауза 0,5 с целы.
  Это ограничение 800-мс EOU, а не регрессия energy gate.
- Инцидент: тестовые экземпляры ASR на `:8016` без `CUDA_VISIBLE_DEVICES=`
  дважды уронили `llama` (12:42 OOM-kill, 13:01 CUDA OOM). Тестовые
  экземпляры остановлены; дальше только с пустым `CUDA_VISIBLE_DEVICES=`,
  preflight `free -m` ≥ 2 ГБ, один экземпляр, проверка `nvidia-smi` и
  `systemctl is-active llama` после теста. Текущий `NRestarts=2` у llama —
  следствие этих падений.

## 5. Offline-заготовки

- `crates/orbita-contracts` — v0 контракты `InboundEvent/Reply/OutboundEvent`,
  `ReplyBlock/DeviceAction/Cancel/OutboundAck`, actions-capabilities, golden
  JSON; 6 unit + 2 golden теста.
- `crates/orbita-home` — `Home` (кэш, validate, risk/Grant, rate limit,
  retain=false), `mqtt::start[_with_status]`, broadcast изменений по state и
  `meta/error`, `matches_value` для ack (bool и числа как `f64`), отдельные
  `stale` (возраст) и `available` (есть значение, нет error); 20 unit + 1 fake-broker тест.
- `crates/orbita-home-mcp` — MCP-фасад: backend на MQTT Wiren Board
  (`HOME_MCP_SIMULATED=0`) и fake для shadow; devices/state/command/scenes/
  scene_run/events/health, policy (dangerous deny by default), idempotency,
  epoch курсора, audit, ack по state-топику; 15 тестов.
- `crates/orbita-intents` — громкость, таймер (в т.ч. «засеки N минут»),
  парсер напоминаний; общий для legacy-агента и адаптера; legacy
  `fastpath`/`reminder` стали обёртками, 18/18 directive-тестов зелёные.
- `crates/orbita-station-adapter` — quasar `/process` → `InboundEvent` →
  громкость локально / обязательства во времени в планировщик OpenClaw /
  остальное в OpenClaw → директивы; `/outbound` принимает доставку
  планировщика и шлёт push на колонку. 12 unit + 8 интеграционных тестов
  (фейковые ядро, планировщик, уведомитель, Gateway) и живой `tests/live.rs`.

## 5a. Station adapter на стенде (08.11.2026, L3)

- `orbita-brain`: `orbita-station-adapter.service` (user `openclaw` — нужен
  CLI OpenClaw для планировщика; `0.0.0.0:8081`), секреты
  `OPENCLAW_TOKEN`/`OUTBOUND_KEY` в `/etc/orbita/station-adapter.env`
  (root:openclaw 0640). Бинарник собран на `orbita-hub` в `/opt/orbita-feat`
  (рабочее дерево `/opt/orbita` не тронуто).
- OpenClaw: `gateway.http.endpoints.chatCompletions.enabled` и
  `cron.webhookSsrfPolicy.allowedHostnames=["127.0.0.1"]` (бэкапы
  `openclaw.json.bak-adapter-20261108`, `openclaw.json.bak-cron-20261108`).
- Маршруты (D-26): громкость — intents, 7–16 мс, без LLM; «таймер на N»,
  «засеки N минут», «напомни через/в …» — задача `openclaw cron add`
  (command payload `printf`, без LLM при срабатывании, `--delete-after-run`),
  ~1,3 с на запуск CLI; свободные формулировки — OpenClaw вызывает client tool
  `reminder_create`; остальное — OpenClaw с системной инструкцией голосового
  стиля и сессией `station:<duid>:<дата МСК>`.
- Живой прогон `/process` (станция и quasar не в цикле), 7/7: громкость 9–16 мс;
  таймеры 1,3 с; «через пять минуток скажи мне проверить духовку» →
  `reminder_create`, 4,4 с; «привет» 0,9 с; погода — «облачно, ночью около нуля,
  днём до плюс трёх», 1,4–2,9 с.
- Сквозная доставка (фейковый duid, беззвучно): таймер 30 с и напоминание
  через 1 мин сработали вовремя, планировщик → `/outbound` → quasar push 200.
- Найдено и исправлено: модель отвечала «засекаю» без вызова инструмента;
  теперь «засеки» — детерминированный путь, а инструкция запрещает обещания
  без вызова. Плохие ответы про погоду шли из накопленной истории сессии.
- Откат адаптера: `systemctl disable --now orbita-station-adapter` на
  `orbita-brain`; прошлые бинарник/юнит/env — `*.prev` в `/usr/local/bin` и
  `/root`; конфиг OpenClaw — бэкапы выше, затем `systemctl restart openclaw`.
- **Переключение станции (выполнено 08.11.2026; команда для повтора):**

  ```sh
  pct exec 101 -- sh -c 'cd /opt/quasar-server && cp -p .env .env.bak-adapter-20261108 && sed -i "s|^PROCESSOR_BASIC_URL=.*|PROCESSOR_BASIC_URL=http://192.168.250.105:8081/process|" .env && systemctl restart quasar-server'
  ```

  Откат: `pct exec 101 -- sh -c 'cd /opt/quasar-server && cp -p
  .env.bak-adapter-20261108 .env && systemctl restart quasar-server'`.
  `orbita-agent` остаётся запущенным: его уже поставленные напоминания
  доставятся, и он нужен для мгновенного отката.

## 5b. home-MCP (08.11.2026, развёрнут, L3)

- Реальный backend подписывается на state/`meta/error` топики, публикует
  команды только с `retain=false` и подтверждает `executed` по state-топику с
  таймаутом `HOME_ACK_TIMEOUT_MS` (по умолчанию 2000 мс). Таймаут — `failed`
  с фактическим состоянием.
- `stale` — возраст последнего сообщения (WB публикует retained и только по
  изменению), `available` — есть значение и нет `meta/error`. Для опасных
  действий проверяется `available`, а не 30-секундный возраст.
- `meta/error`: живой WB публикует на уровне контрола
  (`/devices/wb-…/controls/K1/meta/error`); `orbita-home` подписывается на
  настроенный топик и, если он control-level, дополнительно на
  `/devices/wb-…/meta/error`. `deploy/home/home.json` остаётся per-control,
  device-level — fallback.
- Регрессия: 51 topic при request channel 100 и подписке до старта loop +
  на ConnAck блокировали event loop — state не приходил (`available=0`).
  Исправлено: loop подписывается только на ConnAck, канал sized по числу
  топиков; тест `crates/orbita-home/tests/mqtt_state.rs` (60 устройств,
  fake broker) падает на старом коде и проходит на исправленном.
- Опасные capability — `denied` до появления out-of-band approval;
  `confirm="approved"` из tool call игнорируется. Сцен нет: `scenes=[]`,
  `scene_run` → `denied`.
- `events` отдаёт изменения MQTT с cursor/epoch и дедупликацией; значимые
  переходы (leak/frost/door/motion/mode) — фаза 6.
- Оффлайн-проверка (L2): `cargo test -p orbita-home -p orbita-home-mcp`
  (20 unit + 1 fake-broker + 15), `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, MCP `initialize`/`tools/list`/`health` по HTTP.
  В реальном режиме без брокера health — `degraded`,
  `broker=disconnected`, а `command` возвращает `failed` по ack-таймауту
  (2 с) с фактическим состоянием.
- **Развёрнуто на стенде 08.11.2026 (L3, без станции).** `orbita-home-mcp`
  на `orbita-hub` `:8897` (юнит из `deploy/systemd`, бинарник собран в
  `/opt/orbita-feat`), `mqtt connected`, `health`: 20 устройств, все
  `available`. Стендовый `/etc/orbita/home.json` **не заменялся** и остаётся
  per-control (`…/controls/K1/meta/error`, стандарт живого WB); строка
  `install … home.json` из списка ниже не выполнялась. `deploy/home/home.json`
  тоже возвращён на per-control; device-level топик добавляется автоматически
  как fallback (сейчас ни одного `meta/error` в брокере нет). Бэкап —
  `/root/backup-home-mcp-20261108/home.json.prev`.
- OpenClaw: `mcp.servers.home` (`http://192.168.250.103:8897/mcp`, Bearer как у
  tv/search), hot reload без рестарта; бэкап
  `openclaw.json.bak-home-mcp-20261108`. Первая попытка: bundle-mcp отказался
  запускать сервер — schemars отдавал для `command.value` схему `true`, zod
  OpenClaw её не принимает; исправлено явной схемой
  (`boolean|number|string`) и тестом на объектные схемы свойств.
- Проверено: MCP напрямую — кладовка `on`→`off`, оба `executed` с ack по
  state-топику, оба перехода в `events`; через OpenClaw — «где горит свет»
  ответ по реальному состоянию (остров, столовая 63%), «включи/выключи свет
  в кладовке» — по 3,7 с (два tool call: поиск устройства + команда),
  `executed`. Свет возвращён в исходное состояние. Голосом со станции — ещё
  нет (L3 через Gateway).
- Команды развёртывания (выполнены):

  ```sh
  # CT101, дерево с этим коммитом (например /opt/orbita-feat).
  mkdir -p /root/backup-home-mcp-20261108
  cp -p /etc/orbita/home.json /root/backup-home-mcp-20261108/home.json.prev
  cp -p /opt/orbita/target/release/orbita-home-mcp \
    /root/backup-home-mcp-20261108/orbita-home-mcp.prev 2>/dev/null || true

  CARGO_TARGET_DIR=/opt/orbita/target nice -n 19 cargo build --release -j 2 \
    -p orbita-home-mcp
  install -m 0644 deploy/systemd/orbita-home-mcp.service \
    /etc/systemd/system/orbita-home-mcp.service
  # Конфиг на стенде оставлен per-control; deploy/home/home.json теперь
  # совпадает с ним. Переустанавливать только по решению владельца:
  # install -m 0600 deploy/home/home.json /etc/orbita/home.json
  systemctl daemon-reload
  systemctl enable --now orbita-home-mcp
  systemctl status orbita-home-mcp --no-pager
  journalctl -u orbita-home-mcp -n 50 --no-pager
  curl -s http://127.0.0.1:8897/health
  ```

  `MCP_TOKEN` уже в `/etc/orbita/mcp.env` (тот же, что у tv/search); юнит
  задаёт `HOST=0.0.0.0`, `MCP_ALLOWED_HOSTS=192.168.250.103`,
  `MQTT_CLIENT_ID=orbita-home-mcp` (legacy client id не переиспользуется).
  Диагностика backend без команд светом:

  ```sh
  MQTT_CLIENT_ID=orbita-home-diag /opt/orbita/target/release/orbita-home \
    --config /etc/orbita/home.json list
  ```

  В OpenClaw (CT102) добавить `mcp.servers.home` по образцу tv/search:
  `url=http://192.168.250.103:8897/mcp`, тот же
  `Authorization: Bearer $MCP_TOKEN`; перед правкой — бэкап
  `openclaw.json`, затем `systemctl restart openclaw`. `openclaw mcp probe`
  требует остановленный gateway, поэтому проверять реальным ходом агента.
  Команду светом выполнять осознанно (в доме дети) и только с владельцем.
- Откат: `systemctl disable --now orbita-home-mcp`; вернуть
  `/etc/orbita/home.json` и бинарник из
  `/root/backup-home-mcp-20261108/`; на CT102 вернуть бэкап `openclaw.json`
  и перезапустить OpenClaw. Legacy-агент остаётся остановленным по решению
  владельца; `pct exec 101 -- systemctl enable --now orbita-agent
  orbita-warmup` — отдельное решение.

## 6. Откат POC (важно)

Ветки сведены в `main` 08.11.2026; последнее состояние до OpenClaw —
коммит `0f54562` (legacy-агент с глобальной GBNF). Legacy-агент на
`orbita-hub` остановлен и собран из `f836d93` (grammar per-request). Откат
к legacy — связанная операция, а не просто `git checkout 0f54562`:

1. На `orbita-brain`: `systemctl disable --now openclaw orbita-station-adapter`
   (если OpenClaw больше не нужен); конфиг OpenClaw —
   `/home/openclaw/.openclaw/openclaw.json.bak-20261108`.
2. На `orbita-ai`: вернуть `/root/llama.service.bak-20261108` (глобальная GBNF,
   32k/q8), `systemctl daemon-reload && systemctl restart llama`, проверить
   `/v1/models`.
3. На `orbita-hub`: вернуть бинарник и юнит из
   `/root/backup-grammar-20261108/`, `daemon-reload && systemctl restart
   orbita-agent`.
4. Проверить `/health` агента и `orbita-cli ask "привет"`.

### MCP по сети (08.11.2026, вместо SSH-туннеля)

- Было: `orbita-mcp-tunnel` на `orbita-brain` пробрасывал порты по SSH как
  `root@192.168.250.103` — у контейнера с агентом был root-доступ к hub.
  Туннель, его юнит и ключ удалены; `authorized_keys` root на `orbita-hub`
  пуст (бэкап `/root/backup-mcp-auth-20261108/authorized_keys`).
- Стало: общий крейт `orbita-mcp-http` — rmcp `allowed_hosts` из
  `MCP_ALLOWED_HOSTS`, `Authorization: Bearer $MCP_TOKEN` для сетевых
  клиентов (loopback на hub — legacy-агент — доверен, как раньше), отказ
  стартовать на не-loopback адресе без токена. `orbita-tv` и
  `orbita-mcp-search` слушают `0.0.0.0` (drop-in `20-mcp-auth.conf`), токен в
  `/etc/orbita/mcp.env` (0600). OpenClaw передаёт заголовок из `mcp.servers.*.headers`
  (бэкап `openclaw.json.bak-mcp-direct-20261108`).
- Проверено (L3): loopback без токена 200, сеть без токена 401, с токеном 200;
  без туннеля OpenClaw ответил про погоду (search) и TV в детской (tv).
- Откат: на `orbita-hub` вернуть бинарники/юниты из
  `/root/backup-mcp-auth-20261108/`, удалить drop-in'ы, `daemon-reload`,
  перезапустить `orbita-tv orbita-mcp-search orbita-agent`; на `orbita-brain`
  вернуть `openclaw.json.bak-mcp-direct-20261108` (туннель тогда придётся
  создать заново с новым ключом).

## 7. Ближайшие шаги

1. Фаза 0: контракты, identity, baseline, ADR LLM/adapter.
2. LLM serving + общие STT/TTS-шлюзы.
3. POC OpenClaw/Hermes с go/no-go по device-actions и streaming.
4. home-MCP: развернуть на CT101, проверить команды/ack реальным светом
   (wb-rules не меняются, D-18).
5. Station adapter на `orbita-station-sim`.
6. Streaming TTS — отдельный gate.
7. События/outbound, затем Wyoming.
8. Shadow/canary и миграция.

Полный план — `docs/plan.md`.

## 8. Проверки

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Для текущего TV-стека: `cargo test -p orbita-tv -p orbita-agent`,
`deploy/tv/tv-adb.py state` на CT 101.
