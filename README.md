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
  → речь в текст: OpenAI-совместимый /audio/transcriptions (GigaAM, Whisper...)
  → ответ: таймеры → правила → LLM (OpenAI-совместимый /chat/completions) → MQTT (Node-RED)
  → текст в речь: OpenAI-совместимый /audio/speech (Piper...)
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

Распознавание и синтез речи — отдельные сервисы с OpenAI-совместимым API
(`/audio/transcriptions`, `/audio/speech`), их адреса задаются в конфиге (`stt`, `tts`).
Подойдут Speaches, LocalAI, OpenAI или серверы из [`servers/`](#серверы-распознавания-и-синтеза):
GigaAM, Piper и мост к любому Wyoming-синтезатору.

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

stt:
  language: ru
  openai: {base_url: http://192.168.1.21:8001/v1}     # servers/gigaam_openai.py
tts:
  voice: ru_RU-fr6800-medium
  openai: {base_url: http://192.168.1.22:8003/v1}     # wyoming_openai.py + русский Piper

intents:
  - type: timers
  - type: rules
    rules:
      - match: "(который|сколько) (час|времени)"
        reply: "Сейчас {time}."
  - type: openai
    model: qwen2.5-7b-instruct
```

У `stt` и `tts` свой `openai:` (`base_url`, `api_key`), если распознавание и синтез живут не на
том сервере, что LLM; без него — общая секция `openai`. Формат синтеза по умолчанию `wav`: его
точно проиграет GStreamer колонки.

Wyoming orbita больше не поддерживает: ключи `engine`, `host`, `port` у `stt`/`tts` — ошибка
конфига с подсказкой.

### Серверы распознавания и синтеза

В [`servers/`](servers) — однофайловые OpenAI-совместимые серверы на aiohttp, каждый со своим
unit-файлом в `systemd/`:

| Сервер | API | Порт | Unit |
|---|---|---|---|
| `gigaam_openai.py` — [GigaAM](https://github.com/salute-developers/GigaAM) (русский, с пунктуацией) | `POST /v1/audio/transcriptions` | 8001 | `gigaam-openai.service` |
| `piper_openai.py` — [Piper](https://github.com/OHF-Voice/piper1-gpl) | `POST /v1/audio/speech` (`wav`, `pcm`) | 8002 | `piper-openai.service` |
| `wyoming_openai.py` — мост к Wyoming-синтезатору | `POST /v1/audio/speech` (`wav`, `pcm`) | 8003 | `wyoming-openai.service` |

Все отдают `GET /v1/models` и `GET /health`, ошибки — `{"error": {"message": ...}}`, ключ
(`--api-key`) — по желанию. Модель загружается и прогревается при старте, запросы выполняются по
очереди.

#### Русский Piper (рекомендуется)

Стандартные русские голоса Piper путают ударения и не читают числа. Лучше звучит
[доработанный wyoming-piper](https://github.com/mitrokun/espeak-ng-data) от mitrokun:
исправленный словарь espeak-ng, омографы через silero-stress, ёфикатор, нормализация чисел,
процентов и годов. Голоса для него — в [rraaww/ru_piper](https://huggingface.co/rraaww/ru_piper)
(обучены на этом словаре, со стандартным звучат неправильно, и наоборот: стандартные irina,
ruslan, denis, dmitri с новым словарём портятся).

Он существует только как Wyoming-сервер, поэтому orbita ходит к нему через мост
`wyoming_openai.py`:

```
orbita → wyoming-openai.service (:8003, OpenAI API) → piper-ru.service (127.0.0.1:10200, Wyoming)
```

Установка — по микрогайду mitrokun в `/opt/piper-ru`; для видеокарты `onnxruntime` заменяется на
`onnxruntime-gpu[cuda,cudnn]`, `LD_LIBRARY_PATH` в `systemd/piper-ru.service` указывает на
библиотеки CUDA из venv. Голоса — в `/var/lib/piper-ru`. Мост отдаёт только голоса из `--voices`
(первый — по умолчанию): wyoming-piper числит установленным весь свой каталог и скачивает голос
при первом обращении.

На RTX 5060 фраза синтезируется за 0,2–0,4 с (на Pentium G5420 — около секунды), голос
занимает 0,4–0,8 ГБ видеопамяти. Держите в памяти один голос (`--max-cached-voices 1`), если
видеокарта общая с LLM.

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

Ответы правил, таймеров и Node-RED короткие и идут одним файлом. Каждая фраза синтезируется
целиком (`/audio/speech` в формате `wav`). Выключить: `tts.streaming: false` (для всех) или
`streaming: false` у колонки.

```
orbita/
  satellite.py   соединение с колонкой, протокол голосового диалога, объявления, таймеры
  vad.py         конец фразы
  speech.py      STT/TTS через OpenAI-совместимый API
  streaming.py   потоковый ответ: фразы → TTS → растущий WAV
  sentences.py   разбиение текста LLM на фразы
  audio.py       PCM и WAV
  openai_io.py   клиент OpenAI-совместимого API
  intents.py     цепочка обработчиков: rules, openai, mqtt; история диалога
  timers.py      таймеры по-русски
  bus.py         MQTT
  http_store.py  HTTP-сервер ответов: файлы и потоки
servers/         OpenAI-совместимые серверы: gigaam_openai.py (STT), piper_openai.py (TTS),
                 wyoming_openai.py (мост к Wyoming-синтезатору)
systemd/         orbita.service, gigaam-openai.service, piper-openai.service, piper-ru.service,
                 wyoming-openai.service
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
