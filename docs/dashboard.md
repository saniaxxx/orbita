# orbita dashboard

`orbita-dashboard` — локальный веб-интерфейс, который показывает текущий ход голосового
пайплайна: узлы, поток между ними, этапы, логи и метрики. MVP работает на синтетических
событиях и принимает реальные события по HTTP.

## Запуск

```sh
cargo run -p orbita-dashboard -- --listen 127.0.0.1:8901
```

Открыть `http://127.0.0.1:8901/`. По умолчанию включён demo-режим: каждые несколько
секунд проигрывается синтетический ход. Отключить: `--no-demo` или `DEMO=0`.

Сервис слушает только loopback. Аутентификации в MVP нет: не публиковать наружу.

## API

- `GET /api/state` — снимок: последние события и метрики.
- `GET /api/events` — SSE-поток событий (`event: trace`).
- `POST /api/events` — принять одно событие или массив.
- `GET /health` — статус и метрики.
- `GET /` — UI; `/app.js`, `/style.css` — ассеты.

Пример события:

```json
{
  "ts_ms": 1762082862218,
  "turn_id": "turn-1284",
  "service": "asr",
  "stage": "asr.final",
  "duration_ms": 220,
  "data": {"text": "какая сейчас погода в москве"}
}
```

Отправить:

```sh
curl -s -X POST http://127.0.0.1:8901/api/events \
  -H 'content-type: application/json' \
  -d '{"ts_ms":1762082862218,"turn_id":"turn-1284","service":"asr","stage":"asr.final","duration_ms":220,"data":{"text":"привет"}}'
```

## Этапы

| `service` | `stage` | Что означает |
|---|---|---|
| `station` | `turn.start`, `station.first_audio`, `station.play` | пробуждение, первый звук, проигрывание |
| `proxy` | `proxy.tls` | TLS/SNI |
| `quasar` | `quasar.session` | uniproxy |
| `asr` | `asr.partial`, `asr.final` | распознавание |
| `processor` | `processor.request` | запрос в процессор |
| `llm` | `llm.ttft`, `llm.done` | первый токен и ответ; `data.pass` = 1/2 |
| `mcp` | `mcp.request`, `mcp.response` | вызов инструмента |
| `searxng` | `search.results` | результаты поиска |
| `tts` | `tts.chunk` | часть синтеза |
| `dashboard` | `metrics` | VRAM/очередь |

UI сопоставляет `service` с узлом графа и `stage` с этапом таймлайна. Неизвестные
события просто попадают в лог.

## Что дальше

- инструментировать Rust-сервисы и отправлять реальные события;
- logtap для старого Python/Node/TS-стека;
- replay записанной JSONL-сессии с таймингами;
- история ходов в SQLite, фильтры и алерты.
