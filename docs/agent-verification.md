# orbita — туториал для агента-проверяющего

Этот документ — пошаговый сценарий для агента (человека или LLM), который проверяет
стек orbita. Главное правило: **не выдавать офлайн-проверку за проверку на станции**.
Каждый результат помечается уровнем L0–L4.

Актуальная топология и список того, что уже развёрнуто, — в `docs/state.md`.

## 0. Границы и правила

- Работать в единственной ветке `main`; других веток в репозитории нет.
- С этой машины нет доступа к стенду: не запускать `ssh`, `pct`, не перезапускать
  сервисы на 192.168.250.102. Всё, что нужно на стенде, оформлять командами для человека.
- Не коммитить секреты, записи голоса и приватные запросы.
- Не удалять и не перезаписывать данные на стенде без явного разрешения.
- Перед выводами «работает» приложить команды, коды возврата и ключевые строки вывода.

## 1. Уровни проверки

| Уровень | Что проверяет | Где |
|---|---|---|
| L0 | сборка, unit-тесты, clippy, fmt | эта машина |
| L1 | контракты с фейковыми бэкендами | эта машина |
| L2 | replay записанной сессии/фикстур | эта машина |
| L3 | shadow старого и нового сервиса | стенд |
| L4 | canary на реальной станции | стенд |

L0–L2 можно и нужно делать без станции. L3–L4 — только когда есть доступ к стенду.

## 2. Подготовка

Нужны:

- Rust stable (`cargo`, `rustc`);
- `ffmpeg` — только для экспериментов с Ogg/Opus и TTS;
- Python не требуется: рекордер и контрактные скрипты больше не на Python.

Если на машине мало диска, сборку можно вести во временном каталоге:

```sh
export CARGO_TARGET_DIR=/tmp/orbita-target
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  cargo test --workspace
```

Либо во временном RAM-диске (macOS):

```sh
RAMDISK=$(hdiutil attach -nomount ram://7340032 | awk '{print $1}')
diskutil eraseVolume HFS+ RustTarget "$RAMDISK"
export CARGO_TARGET_DIR=/Volumes/RustTarget
```

Не удалять пользовательские кэши и файлы ради места.

## 3. Офлайн-ворота (обязательны)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Ожидаемо: все test binaries заканчиваются `test result: ok`.

Верхнеуровневые `tests/contract/run-*.sh` и Python-рекордер удалены вместе с
Python-сервисами; их роль сейчас выполняют интеграционные тесты в `crates/*/tests`.
Чёрные контрактные скрипты нужно восстановить до переключения quasar/TTS/ASR.

Проверка рекордера:

```sh
cargo test -p orbita-station-sim recorder_writes_loadable_jsonl
```

Ожидаемо: тест проходит и JSONL читается `--replay-frames`.

## 4. Матрица по сервисам

| Сервис | L0/L1 | Что именно |
|---|---|---|
| `orbita-mcp-search` | `cargo test -p orbita-mcp-search` | MCP initialize/list/call, формат ответа, ошибки |
| `orbita-tts` | `cargo test -p orbita-tts` | 26 golden-кейсов текста supertonic, HTTP-контракт, WAV/ffmpeg, кэш, паритет Vosk с Python |
| `orbita-processor` | `cargo test -p orbita-processor` | WS-протокол, JSON LLM, функции, MCP → SearXNG |
| `orbita-asr` | `cargo test -p orbita-asr` | VAD/endpointing, partial/final, WS-контракт, log-mel GigaAM, декодеры |
| `orbita-proto` | `cargo test -p orbita-proto` | сборка 749 `.proto`, round-trip `TServerMessage` |
| `orbita-quasar` | `cargo test -p orbita-quasar` | storage, management API, HTTP-роутеры, glagol JWT |
| `orbita-station-sim` | `cargo test -p orbita-station-sim` | текст, голос, scenario replay, frame replay |
| `orbita-dashboard` | `cargo test -p orbita-dashboard` | snapshot, ingest, static UI |
| `orbita-glagol` | `cargo test -p orbita-glagol` | протокол `audio_play`, varint/protobuf, JSON-запросы; сети нет |
| `orbita-home` | `cargo test -p orbita-home` | модель устройств, риск/grant, stale, rate limit, `retain=false`, MQTT URL |
| `orbita-agent` | `cargo test -p orbita-agent`, `deploy/verify/run-directives.sh` | `/process` black-box, директивы станции, напоминания, SQLite, scheduler; голосовой харнесс 13/13 |

### Dashboard

```sh
cargo run -p orbita-dashboard -- --listen 127.0.0.1:8901
curl -s http://127.0.0.1:8901/api/state
```

Открыть `http://127.0.0.1:8901/`: в demo-режиме должен идти синтетический ход,
узлы подсвечиваются, логи и метрики обновляются. Подробности — `docs/dashboard.md`.

### Glagol (только стенд)

L0 — `cargo test -p orbita-glagol`. Всё остальное требует станции и L2/L3:

```sh
cargo run -p orbita-glagol -- discover
cargo run -p orbita-glagol -- token --device-id <DUID> --platform yandexmicro \
  --quasar http://192.168.250.103:31120
cargo run -p orbita-glagol -- state --host <IP> --port <PORT> \
  --device-id <DUID> --platform yandexmicro --watch 5
```

Проверить, что в логе quasar есть `check_token`, что токен с другим
`GLAGOL_JWT_KEY` отклоняется и что `play-url` играет локальный MP3/FLAC.
Порядок и оговорки — `docs/glagol.md`. Без станции `discover` честно вернёт
`[]`; это не ошибка.

## 5. Симулятор станции

Текстовый режим:

```sh
cargo run -p orbita-station-sim -- --text "привет"
```

Голосовой режим (Ogg/Opus):

```sh
ffmpeg -i phrase.wav -c:a libopus -b:a 32k phrase.ogg
cargo run -p orbita-station-sim -- --audio phrase.ogg --audio-dir /tmp/sim-audio
```

Интерактивный режим:

```sh
cargo run -p orbita-station-sim -- --interactive --report /tmp/sim-report.json
```

Replay сценария:

```json
{
  "steps": [
    {"type": "text", "text": "привет"},
    {"type": "voice", "file": "phrase.ogg", "delay_ms": 1000}
  ]
}
```

```sh
cargo run -p orbita-station-sim -- --replay scenario.json --report /tmp/report.json
```

Ожидаемые поля отчёта: `text`, `asr_text`, `audio_chunks`, `audio_bytes`,
`directives`, `pushes`, `response_ms`, `first_audio_ms`, `total_ms`.

### Проверка директив агента

Готовый сценарий и скрипт с PASS/FAIL:

```sh
# на CT 101, где запущены quasar и orbita-agent
DUID=<duid станции> deploy/verify/run-directives.sh
```

TS quasar не пропускает произвольный `TextInput` в процессор (он знает только
специальные текстовые команды), поэтому голосовой прогон с Mac — основной:

```sh
DUID=<duid> VOICE=1 \
  QUASAR_WS=ws://192.168.250.103:31120/uniproxy.alice.yandex.net/uni.ws \
  REMOTE_SSH=root@192.168.250.221 REMOTE_CT=101 \
  STATION_SIM=/Volumes/DATA/orbita-sim-target/release/orbita-station-sim \
  deploy/verify/run-directives.sh
```

`VOICE=1` синтезирует каждую фразу через `say -v Milena` + ffmpeg в Ogg/Opus
во временном каталоге (в git не попадает), пустой текст — 2 с тишины. Голос
обязательно ослабляется `volume=0.03`: `say` даёт полную шкалу, а `qsg-asr`
включает `INPUT_GAIN=30` для тихого микрофона станции — без ослабления речь
клиппится и распознаётся только последнее слово. Скрипт шлёт Ogg страницами,
как настоящая станция, и ждёт 2,5 с перед `StreamControl.Close`, чтобы VAD
успел финализировать текст. SQLite при `REMOTE_SSH` копируется из CT 101 через
tar и проверяется локальным sqlite3; журналы читаются через
`pct exec 101 journalctl`.

Проверенный прогон 04.11.2026 (13/13 PASS): привет, громче/тише/громкость 4,
таймеры 2 минуты / «две минуты» / 30 секунд, напоминание через 20 минут,
пустой текст. В отчёте директивы станции называются `sound_louder`,
`sound_quiter`, `sound_set_level`, `timer_set_directive`, а не
`smart_speaker_*`: quasar преобразует имена функций в свои директивы.

Скрипт гоняет `deploy/verify/scenarios/directives.json`: привет, громче,
тише, громкость 4, таймер 2 минуты, «таймер на две минуты» (словесное
числительное), таймер 30 секунд, напоминание через 20 минут и пустой текст.
Проверяет:
- в отчёте есть `sound_louder`, `sound_quiter`, `sound_set_level` и
  `timer_set_directive`;
- пустой текст даёт «Я вас не расслышала»;
- в SQLite есть `native_timer` (>=3) и `reminder` (>=1);
- в журнале агента есть `prepare functions=18`, в журнале quasar —
  `Converting directive 'smart_speaker_...`.

Нужны `jq`; `sqlite3` и `journalctl` проверяются, если доступны, иначе SKIP.
Скрипт не перезапускает сервисы. Голосовой шаг можно прогнать отдельно:

```sh
say -v Milena -o /tmp/phrase.aiff "Алиса, громче"
ffmpeg -i /tmp/phrase.aiff -c:a libopus -b:a 32k /tmp/phrase.ogg
cargo run -p orbita-station-sim -- --audio /tmp/phrase.ogg
```

Приватные записи в репозиторий не кладём. Неизвестная функция от LLM и
`prepare` без `process` проверяются Rust-тестами `tests/ws_directives.rs`,
а не чёрным сценарием.

Покадровый replay записи:

```sh
cargo run -p orbita-station-sim -- \
  --replay-frames /tmp/orbita-session.jsonl \
  --frames-out /tmp/replayed.jsonl \
  --report /tmp/frames-report.json
```

## 6. Запись сессии

Активная сессия (симулятор → quasar) пишется на Rust:

```sh
cargo run -p orbita-station-sim -- \
  --text "привет" \
  --record /tmp/orbita-session.jsonl
```

Формат тот же, что читает `--replay-frames`. Голос:

```sh
cargo run -p orbita-station-sim -- \
  --audio phrase.ogg \
  --record /tmp/orbita-voice.jsonl
```

Прозрачный прокси-рекордер для реальной станции (замена `ws_capture.py`) теперь тоже
на Rust. Порядок записи:

1. Запустить прокси на CT 101 (рядом с https-proxy) и направить на него
   `https-proxy`:

```sh
cd /opt/orbita
./target/release/orbita-station-sim \
  --listen 0.0.0.0:31122 \
  --upstream ws://192.168.250.102:31120 \
  --record /var/lib/orbita/recordings/session-$(date +%s).jsonl
```

2. Временно выставить `YANDEX_UPSTREAM_URL=http://127.0.0.1:31122` в юните
   `https-proxy` и перезапустить его. Убедиться, что станция подключилась (в логе
   прокси есть `recording proxy session started`).

3. Задать станцией 2–3 вопроса, дождаться ответов, остановить прокси.

4. Вернуть прежний `YANDEX_UPSTREAM_URL=http://192.168.250.102:31120` (или актуальный
   upstream), перезапустить `https-proxy` и проверить, что станция отвечает.

Прокси пишет одну сессию в файл; для новой сессии задайте новый путь. Запись содержит
голос; хранить её в `tests/fixtures/private/` или `/var/lib` и не коммитить.

## 7. Shadow и canary (стенд)

- **HTTP/TTS/ASR/processor**: поднять старый и новый сервис на разных портах, отправить
  одинаковые запросы, сравнить нормализованные ответы и тайминги. Прод сохраняет старый
  сервис до нуля критичных расхождений.
- **quasar**: живой shadow невозможен (одна станция). Использовать покадровый replay
  записанной сессии против старого и нового сервиса, затем canary с переключением
  upstream nginx и мгновенным откатом.
- **Критерии**: контракты 100 %, фикстуры 100 %, 0 критичных расхождений, p50 не хуже
  baseline +10 %, p95 +20 %, 72 часа soak, проверенный откат.

## 8. Триаж ошибок

Классифицировать:

- L0 — сборка/линт/юнит;
- L1 — контракт (несовпадение формы ответа, кода, заголовка);
- L2 — replay/фикстура (поведение на записанных данных);
- L3/L4 — стенд (сеть, GPU, реальная станция).

Для каждой ошибки приложить:

```text
Уровень:
Команда:
Ожидалось:
Получено:
Лог/артефакт:
Гипотеза:
Откат:
```

Не «исправлять» тест под неверное поведение: сначала выяснить, что является эталоном.

## 9. Текущие известные пробелы

- `tts`: Vosk ru 0.10 на `ort` в проде на 9170; Silero остановлен (файлы для
  отката). HTTP-ответ упирается в ffmpeg на каждый запрос — оптимизация впереди.
- `asr`: GigaAM v3 на `ort` проверен на CPU и CUDA; TEN VAD не портирован,
  используется `VAD=energy`. Голосовой ввод в Rust-quasar ещё не подключён.
- `quasar`: нет голосового ввода, sync и `glagol/device_list`; разбиение ответа
  на фразы отключено (D-11). 04.11.2026 проверено, что при `Close` только у
  последнего `TtsSpeak` станция проигрывает все потоки, но разбиение невыгодно
  по задержке; toggle не добавляем.
- `processor`: нет NLP/классификатора инструментов, потокового разбора
  `function_calls` и JSON-schema валидации аргументов.
- музыка: есть только спайк `orbita-glagol`, на стенде не проверялся; Music
  Assistant не развёрнут; `glagol/device_list` не реализован.
- `orbita-agent` — первая версия: `/process`, SQLite-сессии, напоминания; нет
  MCP, дома, профилей и voice ID.
- `orbita-home` — только базовый крейт (модель, safe-write, stale, grants);
  `wb-rules`, Frigate, Android TV, Bosch/Liebherr не начаты.

Если проверка упирается в один из пробелов, это не регрессия: зафиксировать как
известное ограничение.

## 10. Итоговый отчёт

```text
Ветка/коммит:
Дата:
Уровень проверки:
Команды:
Результаты:
Расхождения:
Производительность:
Известные пробелы:
Риски:
Готовность к следующему шагу: да/нет
```
