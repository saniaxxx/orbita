# orbita

Голосовой сервер для колонок с [pulsar](https://github.com/Morvex885/pulsar) (Яндекс Станция
Миди / Лайт 2) и других ESPHome-спутников (например,
[linux-voice-assistant](https://github.com/OHF-Voice/linux-voice-assistant)) — **без Home
Assistant**.

Для колонки orbita выглядит как Home Assistant: подключается к ней по ESPHome Native API и ведёт
голосовой диалог. Активационную фразу «Алиса» распознаёт сама колонка, всё остальное делает
orbita:

```
колонка: «Алиса» → звук 16 кГц по ESPHome API
  → orbita: конец фразы (VAD)
  → речь в текст: Wyoming (faster-whisper) или OpenAI-совместимый /audio/transcriptions
  → ответ: таймеры → правила → LLM (OpenAI-совместимый /chat/completions) → MQTT (Node-RED)
  → текст в речь: Wyoming (Piper) или OpenAI-совместимый /audio/speech
  → колонка скачивает ответ по HTTP и проигрывает его
```

Колонка сама речь не синтезирует: в событии `TTS_END` она получает ссылку на готовый файл.

Что умеет:

- голосовой диалог с продолжением: если ответ заканчивается вопросом, колонка слушает дальше
  без «Алиса»;
- **ответ LLM звучит сразу**: текст режется на фразы по мере того, как модель его пишет, первая
  фраза уже играет, пока модель дописывает остальное (см. «Потоковый ответ»);
- **таймеры** по-русски («поставь таймер на пять минут», «отмени таймер», «сколько осталось»):
  отсчёт на часах Станции и звонок, который гасится словом «стоп» или кнопкой;
- **правила**: регулярные выражения → ответ (`{time}`, `{date}`, `{weekday}`, группы
  выражения) и сообщения MQTT, например в Zigbee2MQTT;
- **LLM** через любой OpenAI-совместимый API: vLLM, llama.cpp server, LM Studio, Ollama (`/v1`),
  LocalAI, OpenRouter, OpenAI. Помнит контекст диалога;
- **объявления** из MQTT: `orbita/<колонка>/announce` → колонка произносит текст;
- время и часовой пояс для часов колонки (их тоже обычно присылает HA);
- шифрование ESPHome API (Noise), несколько колонок одновременно.

## Установка (Debian 12+ / Ubuntu 22.04+, в том числе LXC-контейнер)

```sh
git clone https://github.com/saniaxxx/orbita && cd orbita
sudo scripts/install.sh
```

Установщик создаёт пользователя `orbita`, окружение `/opt/orbita/venv`, конфиг
`/etc/orbita/config.yaml` (если его ещё нет), файл секретов `/etc/orbita/env` и сервис
`orbita.service`. orbita слушает 10800/tcp: оттуда колонки скачивают ответы.

Распознавание и синтез речи в репозиторий не входят: это отдельные сервисы, их адреса задаются
в конфиге (`stt`, `tts`). Подойдёт любой сервер Wyoming (wyoming-faster-whisper, wyoming-piper)
или OpenAI-совместимый API (`/audio/transcriptions`, `/audio/speech`).

Дальше:

1. Задайте колонке ключ шифрования API (без HA его никто не запишет):
   ```sh
   KEY=$(head -c32 /dev/urandom | base64)
   adb shell /data/pulsar/ctl.sh api-key "$KEY"
   echo "$KEY"   # -> noise_psk в /etc/orbita/config.yaml
   ```
2. Отредактируйте `/etc/orbita/config.yaml`: колонки, адреса сервисов распознавания и синтеза
   речи, адрес OpenAI-совместимого API и модель. Ключ API кладите в `/etc/orbita/env`
   (`OPENAI_API_KEY=...`).
3. Часовой пояс: `timezone: Europe/Moscow` — orbita передаёт его колонке вместе со временем.
4. Запуск:
   ```sh
   sudo systemctl restart orbita
   journalctl -fu orbita
   orbita -c /etc/orbita/config.yaml --check   # проверка конфига
   ```

Если на колонке ещё стоит Home Assistant, их можно держать одновременно: к pulsar
подключаются до 4 клиентов, голос обслуживает последний подписавшийся. Удалите колонку из HA,
когда orbita заработает.

### LXC (Proxmox)

- Контейнер Debian 12, сеть — мост в той же подсети, что и колонки (без NAT): колонка сама
  скачивает ответы с `http://<IP контейнера>:10800`.
- Docker не нужен: orbita — обычный systemd-сервис. Самой orbita хватает 1 ядра и 256 МБ
  памяти: тяжёлая работа (распознавание, синтез, LLM) идёт в отдельных сервисах.
- Сервисы распознавания и синтеза должны быть доступны из контейнера orbita по сети.

## Конфигурация

Все параметры с комментариями — в [`config.example.yaml`](config.example.yaml). Главное:

```yaml
satellites:
  - name: kitchen
    host: 192.168.1.50
    noise_psk: "base64-ключ"

openai:
  base_url: http://192.168.1.20:8000/v1   # ваш OpenAI-совместимый сервер
  api_key: ""                             # или OPENAI_API_KEY в /etc/orbita/env

stt: {engine: wyoming, host: 192.168.1.30, port: 10300, language: ru}  # или engine: openai
tts: {engine: wyoming, host: 192.168.1.31, port: 10200, voice: ru_RU-irina-medium}

intents:
  - type: timers
  - type: rules
    rules:
      - match: "(который|сколько) (час|времени)"
        reply: "Сейчас {time}."
  - type: openai
    model: qwen2.5-7b-instruct
```

У `stt` и `tts` может быть свой `openai:` (`base_url`, `api_key`), если распознавание и синтез
живут не на том сервере, что LLM. Для `tts.engine: openai` формат ответа по умолчанию `wav`:
его точно проиграет GStreamer колонки.

### MQTT

С `mqtt.host` orbita публикует и слушает:

| Топик | Направление | Содержимое |
|---|---|---|
| `orbita/<колонка>/status` | → | `online` / `offline` (retained) |
| `orbita/<колонка>/transcript` | → | `{"text", "conversation_id"}` |
| `orbita/<колонка>/reply` | → | `{"text", "conversation_id", "handler"}` |
| `orbita/<колонка>/timer` | → | `{"id", "name", "event": "finished"}` |
| `orbita/<колонка>/announce` | ← | текст или `{"text", "media_url", "start_conversation"}` |
| `orbita/<колонка>/intent` | → | `{"id", "text", ...}` — для обработчика `type: mqtt` |
| `orbita/<колонка>/intent/reply` | ← | `{"id", "text", "continue"}`; пустой `text` — «не моё» |

Обработчик `type: mqtt` делает Node-RED «мозгом» для команд умного дома: правила и LLM
отвечают на остальное.

## Как это устроено

orbita повторяет то, что делает Home Assistant с ESPHome-спутником
(`aioesphomeapi.subscribe_voice_assistant`):

1. Колонка услышала «Алиса» и прислала `VoiceAssistantRequest(start)`. orbita отвечает портом 0
   (звук идёт по самому API-соединению) и шлёт `RUN_START`, `STT_START`.
2. Звук идёт одновременно в распознаватель и в VAD (`pymicro-vad`, запасной вариант —
   по энергии). После 0,8 с тишины — `STT_VAD_END`: колонка перестаёт слушать.
3. `STT_END{text}` → `INTENT_START` → цепочка обработчиков →
   `INTENT_END{conversation_id, continue_conversation}`.
4. `TTS_START{text}` → синтез → файл в HTTP-хранилище → `TTS_END{url}` → `RUN_END`.
   Ответ LLM идёт потоком, см. ниже.
5. Ошибки — `ERROR{code}` + `RUN_END` (`stt-no-text-recognized` колонка показывает без звука
   ошибки).

### Потоковый ответ

Так же, как HA со стриминговым TTS:

1. В `RUN_START{url}` колонка сразу получает адрес потока `…/tts/stream/<id>.wav`.
2. LLM отвечает потоком (`stream: true`, server-sent events). Текст режется на фразы: по `.` `!`
   `?` `…` `;` перед словом с заглавной буквы и по переносам строк; короче
   `tts.min_phrase_chars` (20) — склеиваются со следующей.
3. Фразы синтезируются по очереди, PCM дописывается в один WAV с неизвестной длиной.
   С первым звуком колонка получает `INTENT_PROGRESS{tts_start_streaming=1}` и начинает играть,
   пока модель пишет дальше.
4. Когда текст закончился — `INTENT_END`, `TTS_START{text}`, `TTS_END{тот же url}`; `RUN_END` —
   когда озвучена последняя фраза.

Ответы правил, таймеров и Node-RED короткие и идут одним файлом. Синтез через Wyoming отдаёт
звук фразы ещё во время синтеза; OpenAI-совместимый `/audio/speech` — целиком на каждую фразу
(в формате `wav`). Выключить: `tts.streaming: false` (для всех) или `streaming: false` у колонки.

```
orbita/
  satellite.py   соединение с колонкой, протокол голосового диалога, объявления, таймеры
  vad.py         конец фразы
  speech.py      STT/TTS: Wyoming или OpenAI-совместимые
  streaming.py   потоковый ответ: фразы → TTS → растущий WAV
  sentences.py   разбиение текста LLM на фразы
  wyoming_io.py  клиент Wyoming
  openai_io.py   клиент OpenAI-совместимого API
  intents.py     цепочка обработчиков: rules, openai, mqtt; история диалога
  timers.py      таймеры по-русски
  bus.py         MQTT
  http_store.py  HTTP-сервер ответов: файлы и потоки
systemd/         orbita.service
scripts/         install.sh
```

## Разработка

```sh
python3 -m venv .venv && .venv/bin/pip install -e '.[dev]'
.venv/bin/pytest
```

Сквозные тесты (`tests/test_e2e_selftest.py`) гоняют orbita против настоящего ESPHome-сервера
pulsar: утилиты `api_selftest` из pulsar, собранной для компьютера:

```sh
git clone https://github.com/Morvex885/pulsar
cargo build --manifest-path pulsar/satellite/Cargo.toml -p pulsar-esphome --bin api_selftest
ORBITA_SELFTEST_BIN=$PWD/pulsar/satellite/target/debug/api_selftest .venv/bin/pytest
```

Без `api_selftest` эти тесты пропускаются. STT/TTS/LLM в тестах заменены поддельными серверами
(`tests/fakes.py`).

Для проверки голосом без колонки подойдёт linux-voice-assistant на любом Linux-компьютере с
USB-микрофоном: это тот же протокол, но со своей активационной фразой.
