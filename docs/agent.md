# orbita-agent

Ядро голосового канала: занимает место processor на порту 8080, говорит с
quasar по тому же WebSocket-протоколу (`/process`), хранит диалоги и задачи в
SQLite и исполняет штатные директивы станции.

## Что умеет

- `/process` — `prepare` + `process`, один финальный `partialResponse`.
- **Директивы станции** (D-15). На `prepare` quasar присылает карту
  `directiveFunctions` (18 функций: громкость, таймеры, будильники, музыка,
  Bluetooth). Агент:
  - хранит карту на соединение;
  - отвечает на громкость и таймеры детерминированным быстрым путём, без LLM;
  - остальные функции вызывает через `function_calls` модели;
  - проверяет имя по пришедшей карте и `required`-поля схемы аргументов;
  - возвращает в `partialResponse.directives` пары `{name, arguments}`, которые
    quasar превращает в `TDirective`;
  - зеркалит нативные таймеры в SQLite (`kind = native_timer`), но **не**
    отправляет их push-ом: таймер исполняет станция.
- **Напоминания** без LLM: «через N минут», «через одну минуту», «через
  двадцать пять минут», «через полчаса», «в HH:MM», «завтра в HH:MM». Время
  считается в `Europe/Moscow`, хранится в UTC.
- **Диалог** с LLM: системный промпт, последние `HISTORY_LIMIT` сообщений,
  локальный OpenAI-совместимый endpoint; ответ разворачивается из JSON-обёртки
  (`text`/`reply`/`content`).
- **Планировщик**: due-задачи уходят в quasar `POST
  /devices/{duid}/push/text` (`{"eventText": "..."}`), retry 30 с, до 3 попыток.
- SQLite WAL, восстановление `running`-задач после рестарта, аудит.

## Архитектура каналов

Ядро не знает про станцию: оно возвращает `Outcome { text, actions }`, где
`Action` — типизированное действие (`SetVolume`, `SetTimer`, `Directive`), а
сессия — `SessionKey { channel, principal }`. Адаптер канала превращает действия
в свои примитивы: станция — в директивы, Telegram и веб со временем — в текст и
уведомления. Поэтому набор функций LLM — свойство канала, а не глобальный.

Код: `src/action.rs` (действия, сессии), `src/fastpath.rs` (громкость, таймеры,
напоминания), `src/channel/station.rs` (карта `prepare`, валидация, директивы),
`src/agent.rs` (маршрутизация и диалог), `src/server.rs` (WebSocket и ответ).

## Запуск локально

```sh
OPENAI_BASE_URL=http://127.0.0.1:8090/v1 \
OPENAI_MODEL=qwen3.5-9b-q4km \
AGENT_DB=/tmp/orbita-agent.db \
QUASAR_API_URL=http://127.0.0.1:31121 \
cargo run -p orbita-agent
```

Проверка без станции:

```sh
cargo run -p orbita-cli -- ask "привет"
cargo run -p orbita-cli -- ask "напомни через одну минуту проверить чай"
```

`ask` ходит в `ws://127.0.0.1:8080/process`.

## Установка и замена processor

На тестовом стенде processor уже удалён: агент занимает 8080, юнит
`deploy/systemd/orbita-agent.service` включён, `orbita-warmup` ждёт агента.

1. Собрать: `cargo build --release -p orbita-agent`.
2. Остановить старый processor (если он ещё есть):
   `systemctl stop orbita-processor && systemctl disable orbita-processor`.
3. Установить юнит, при необходимости поправить `OPENAI_BASE_URL`,
   `QUASAR_API_URL` и `DEFAULT_DUID`.
4. Запустить: `systemctl daemon-reload && systemctl enable --now orbita-agent`,
   проверить `curl -s http://127.0.0.1:8080/health`.
5. quasar не меняется: `PROCESSOR_BASIC_URL` уже указывает на 8080.
6. Проверить `orbita-cli ask "привет"`, голосом «Алиса, привет», «Алиса,
   громче», «Алиса, поставь таймер на две минуты».
7. Напоминание: «напомни через одну минуту проверить чай»; push придёт через
   минуту.

Откат: `systemctl stop orbita-agent && systemctl enable --now orbita-processor`
(юнит processor при необходимости восстанавливается из `deploy/systemd/`).

## Переменные

| Переменная | По умолчанию | Смысл |
|---|---|---|
| `PORT` | `8080` | порт `/process` |
| `OPENAI_BASE_URL` | `http://127.0.0.1:8090/v1` | llama.cpp |
| `OPENAI_MODEL` | `qwen3.5-9b-q4km` | модель |
| `OPENAI_API_KEY` | пусто | ключ llama, если нужен |
| `AGENT_DB` | `/var/lib/orbita-agent/agent.db` | SQLite |
| `QUASAR_API_URL` | `http://127.0.0.1:31121` | management API quasar |
| `DEFAULT_DUID` | нет | станция, если quasar не передал `deviceId` |
| `HISTORY_LIMIT` | `12` | сообщений истории в промпт |
| `SCHEDULER_INTERVAL_MS` | `1000` | тик планировщика |

## Проверки

```sh
cargo test -p orbita-agent
cargo clippy -p orbita-agent --all-targets -- -D warnings
```

36 unit-тестов, контракт `/process`, тесты директив (`tests/directives.rs`,
18 сценариев) и голосовой харнесс `deploy/verify/run-directives.sh` — 13 из 13
сценариев через симулятор станции. Голосом на реальной станции проверены
приветствие, громкость и таймер (L3).

## Чего пока нет

- MCP-инструментов и поиска: «какая погода» отвечает по памяти модели.
- Дома, Frigate, TV, профилей и voice ID.
- Потокового `partialResponse` по мере генерации: одно финальное сообщение.
- Политики подтверждений для опасных действий.

## Дальше

1. MCP-клиент/сервер: `schedule/list/cancel`, `search`, дом.
2. Политика grant/nonce и `orbita-home`.
3. Telegram и веб как каналы поверх того же ядра.
