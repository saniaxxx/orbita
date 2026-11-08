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
| CT 100 `orbita-ai` | `192.168.250.102` | GPU/AI: llama.cpp, GigaAM, Vosk TTS |
| CT 101 `orbita-hub` | `192.168.250.103` | interfaces/integrations: quasar, mcp-search, orbita-tv, orbita-home-mcp, dashboard, https-proxy, SearXNG; orbita-agent остановлен |
| CT 102 `orbita-brain` | `192.168.250.105` | ядро: OpenClaw 2026.11.1, Node 24, gateway `127.0.0.1:18789` |
| Станция | `192.168.250.115` | домены Яндекса в `/etc/hosts` → `.103` |
| Wiren Board | `192.168.250.110` | MQTT, wb-rules |
| TCL TV | `192.168.250.109` | Remote v2 |
| Sber TV | `192.168.250.111` | SaluteTV, ADB 5555 |

## 3. Что работает

- **LLM**: локальный OpenAI-совместимый endpoint, `qwen3.5-9b-q4km`.
- **STT**: GigaAM на CUDA/CPU; используется quasar.
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
- **Дом**: wb-rules владеет safety/сценами; свет Wiren Board; `orbita-home`
  читает состояние. MCP-фасада дома пока нет.
- **Поиск**: `orbita-mcp-search` + SearXNG.
- **OpenClaw**: установлен в `orbita-brain`; gateway active на loopback,
  локальная модель `local/qwen3.5-9b-q4km`, context 32768.
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
  per-request; llama `-c 32768`, KV q8, `-np 1`. OpenClaw и голос делят слот;
  POC запускать при простое станции. Свободно ~144 МиБ VRAM.

## 4. Что ещё не сделано

- Станция переключена на адаптер (08.11.2026, quasar `PROCESSOR_BASIC_URL` →
  `http://192.168.250.105:8081/process`, бэкап `.env.bak-adapter-20261108`).
  Проверено симулятором голосом через quasar (L3): «привет» — ответ + TTS,
  «сделай громче» — `sound_louder`. Живая станция — проверка владельца.
- Telegram не подключён: нет токена бота и chat id. «Напомни в телеграм»
  честно отвечает «Телеграм пока не подключён, напомню здесь».
- Нет каналов mobile/web и home-MCP на реальном wb-rules (свет у legacy есть,
  через адаптер — нет).
- LLM вызывает только OpenClaw (станция и адаптер LLM не используют).
  Очередь к одному слоту llama — только между ходами самого OpenClaw:
  Telegram, фоновые `agentTurn`, heartbeat каждые 30 мин, ночной
  memory-dreaming. Legacy ходит в llama, только если quasar вернуть на него.
- Workspace-ворота проходят с `CARGO_TARGET_DIR=/tmp/orbita-target`
  (зависание `orbita-proto` было из-за target на внешнем диске).
- Нет `home-MCP` и событий дома через MCP.
- Нет Wyoming-адаптера и второго голосового интерфейса.
- Нет streaming-ответа OpenClaw → пофразового TTS; quasar пока один поток.
- `orbita-agent` смешивает ядро и station-адаптер; это legacy.

## 4a. Задержка голоса (замеры 08.11.2026)

Где уходят секунды на ходе «вопрос → первый звук» (L3; живые ходы владельца
11:29/11:32 разобраны по логам):

- **Конец фразы (ASR) — главный.** Конец фразы решает наш `qsg-asr`
  (GigaAM, CT 100), не колонка. ten_vad считает усиленный шум комнаты речью
  (`INPUT_GAIN=30` применяется до VAD), тишины 0,8 с не наступает: «Ты кто?»
  распознан на 1,7 с, финал — через 4,8 с; длинная фраза — по пределу 8 с.
  Режим `quasistreaming` с 2 повторами отвергнут (финалит «С», «Алиса.»).
  Задача у второй сессии: энергетический порог `VAD_ENERGY_GATE_RATIO`
  (код добавлен на CT 100, выключен по умолчанию, бэкап
  `/root/vad_processor.py.bak-energy-20261108`).
- **Prompt cache — исправлено.** Минутные часы в системной инструкции
  адаптера сбрасывали кэш llama на первом ходе каждой минуты (3,5–6 с
  prefill 8–14 тыс. токенов). Теперь в system — только дата, время — в конце
  реплики; ходы через границу минуты — prefill ~0,3 с.
- **OpenClaw на 2 ядрах.** Тёплый ход: ~0,3 с до вызова модели. Если с ходом
  совпадает фоновое обслуживание (`maintenance:sqlite-snapshots`,
  12,4 МиБ), до модели проходит до 6,6 с. Кандидат: `pct set 102 -cores 4`
  (не сделано, решение владельца).
- **llama падала по памяти** (12:42 OOM killer по ОЗУ, 13:01 CUDA OOM):
  CT 100 — 12 ГБ без swap, VRAM 7,8/8,1 ГБ; лишний процесс GigaAM (тестовый
  экземпляр ASR) роняет llama → ход 30+ с. Тесты ASR — только с
  `CUDA_VISIBLE_DEVICES=` и одним экземпляром.
- **Стриминг (D-19) включён:** адаптер `STREAM_REPLIES=true`
  (drop-in `20-stream.conf`), quasar `PROCESSOR_STREAMING=true` + патч
  `deploy/quasar/orbita-streaming.patch` (бэкап исходников и `.env` —
  `/root/backup-quasar-stream-20261108/`). Первое предложение — 0,8–1,4 с от
  начала хода; сказка — 1,0 с против 3,3 с полного ответа. Живая станция —
  проверка владельца.
- **quasar ждал legacy-агента при старте:** `orbita-health.conf` проверял
  `127.0.0.1:8080/health` — после остановки агента любой рестарт quasar
  зависал бы и падал. Теперь ждёт адаптер `192.168.250.105:8081/health`.
- Откат стриминга: удалить оба `20-stream.conf`, `daemon-reload`, рестарт
  адаптера и quasar (патч без флага ведёт себя как раньше); полностью — вернуть
  файлы из `/root/backup-quasar-stream-20261108/`. Адаптер прошлой версии —
  `/usr/local/bin/orbita-station-adapter.prev-stream` на CT 102.
- `qsg-asr` пишет все запросы в `/tmp/asr-audio` (`LOG_PATH`) — приватные
  записи голоса; оставлять ли — решение владельца.

## 5. Offline-заготовки

- `crates/orbita-contracts` — v0 контракты `InboundEvent/Reply/OutboundEvent`,
  `ReplyBlock/DeviceAction/Cancel/OutboundAck`, actions-capabilities, golden
  JSON; 6 unit + 2 golden теста.
- `crates/orbita-home-mcp` — MCP-фасад с fake-backend: devices/state/command/
  scenes/events/health, policy, idempotency, epoch курсора; 7 тестов.
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

## 6. Откат POC (важно)

Legacy-агент на `orbita-hub` сейчас собран из `f836d93` (`feat/openclaw`) и
шлёт grammar per-request; `main` (`0f54562`) ожидает глобальную GBNF. Поэтому
откат — связанная операция, а не просто `git checkout main`:

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
4. home-MCP + команды/ack в wb-rules.
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
