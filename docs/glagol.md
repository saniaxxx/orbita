# local Glagol spike

`orbita-glagol` — диагностический инструмент, а не сервис. Он проверяет цепочку,
без которой не заработает музыка на станции:

```
mDNS _yandexio._tcp.local.
        │  device_id, platform, host, port
        ▼
GET <quasar>/glagol/token?device_id=...&platform=...
        │  JWT, подписанный нашим quasar
        ▼
wss://<station>:<port>  ── conversationToken ──►  станция
        │                                          │
        │                                          └─ проверяет токен в нашем quasar
        ▼
команды: state, stop, play, next, volume, audio_play
```

## Зачем

Music Assistant и Home Assistant управляют станцией через локальный Glagol
WebSocket. Провайдер MA по умолчанию берёт токен у настоящего
`quasar.yandex.net`, а наша станция живёт в эмулированном облаке. Поэтому
сначала надо доказать, что станция принимает токен от нашего quasar и что
локальный WebSocket вообще отвечает. Если этот шаг не проходит, MA тоже не
заработает.

## Запуск

```sh
cargo run -p orbita-glagol -- discover
```

Ожидаемо: JSON с `device_id`, `platform`, `host`, `port`.

```sh
cargo run -p orbita-glagol -- token \
  --device-id <DUID> --platform yandexmicro \
  --quasar http://192.168.250.103:31120
```

`--quasar` — HTTP-база нашего quasar. По умолчанию `http://127.0.0.1:31120`;
можно задать `QUASAR_URL`.

```sh
cargo run -p orbita-glagol -- state \
  --host 192.168.250.115 --port <PORT> \
  --device-id <DUID> --platform yandexmicro \
  --watch 5
```

`state` подключается, отправляет `softwareVersion` и печатает все сообщения,
которые станция присылает за указанное время.

Остальные команды:

```sh
# пауза / продолжить / треки
orbita-glagol pause|resume|next|prev --host ... --port ... --device-id ... --platform ...
orbita-glagol volume 0.4 --host ... --port ... --device-id ... --platform ...

# произвольный HTTP-поток
orbita-glagol play-url http://192.168.250.103:8097/track.mp3 \
  --format mp3 --title "Track" --artist "Artist" \
  --host ... --port ... --device-id ... --platform ...
```

Станция требует HTTP (не HTTPS) и `Content-Length`. Простейшая проверка —
статический сервер:

```sh
python3 -m http.server 8097 --bind 0.0.0.0
```

(это внешняя команда для стенда, в репозитории Python нет).

`play-url` использует тот же `audio_play`, что и облако: внутри
`externalCommandBypass` с protobuf-полями `1: "audio_play"` и `2: <JSON>`.
Формат `mp3`/`flac`/`wav`/`aac` кодируется как `MP3`, `m3u8` — как `HLS`.

## Что проверить на стенде

1. **Ключ.** В юните Rust-quasar задать постоянный `GLAGOL_JWT_KEY`
   (например, `openssl rand -hex 32`) и перезапустить. Без него после рестарта
   все выданные токены станут невалидными.
2. **mDNS.** `discover` должен найти станцию в той же L2-сети. Через роутер
   multicast обычно не проходит.
3. **Токен.** `token` возвращает `{"status":"ok","token":"..."}`.
4. **Соединение.** `state` показывает сообщения станции; в логе quasar должен
   появиться `check_token`.
5. **Негативный тест.** Взять токен, подписанный другим ключом (например,
   временный quasar с другим `GLAGOL_JWT_KEY`), и попробовать подключиться —
   станция должна отказать. Это доказывает, что проверка действительно идёт
   через наш quasar, а не принимается всё подряд.
6. **Воспроизведение.** `play-url` на локальный MP3/FLAC, затем `volume`,
   `pause`, `resume`, `next`.
7. **Перезапуск.** Рестарт quasar с постоянным ключом, рестарт станции, обрыв
   сети: сколько времени занимает восстановление.

## Известные пробелы

- `glagol/device_list` в quasar ещё не реализован. Для MA он, скорее всего,
  понадобится: провайдер ищет устройства в аккаунте.
- Сертификат станции самоподписанный; клиент его принимает. Это допустимо
  только в доверенной локальной сети.
- Локальный протокол не документирован и может меняться с прошивкой.
- Это не продакшен-клиент: нет переподключения, очереди и подписки на события.
  После спайка те же команды переедут в `orbita-mcp-music`.

## Безопасность

- Токен — bearer-credential. Не логировать его и не коммитить.
- Слушать и подключаться только в LAN; наружу не публиковать.
- `GLAGOL_JWT_KEY` хранить в env-файле с правами 600.
