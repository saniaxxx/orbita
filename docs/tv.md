# Android TV / Google TV

`orbita-tv` — отдельный MCP-сервер: агент подключает его как
`tv#http://127.0.0.1:8898/mcp` и получает инструменты `tv_state`, `tv_power`,
`tv_key`, `tv_volume`, `tv_app`, `tv_input`, `tv_list`.

Основной протокол — **Google TV Remote v2** (порты 6466/6467, TLS с клиентским
сертификатом, protobuf). ADB не используем: полный shell, порт 5555 и ключи,
которые легко утекают. Cast/AirPlay остаются только как запасной путь для
медиа и не управляют питанием.

## Pairing (один раз на телевизор)

Pairing — операторская процедура, её делает человек у телевизора. Проще всего
временным Python-адаптером `androidtvremote2` (Apache-2.0); рантайм сервера —
Rust и от Python не зависит.

```sh
python3.13 -m venv /tmp/av-poc
/tmp/av-poc/bin/pip install androidtvremote2
# запустить скрипт, который вызывает async_start_pairing()
# на экране TV появится 6-значный код; передать его в async_finish_pairing()
```

Сертификат и ключ клиента — по одному на телевизор, только `0600`:

```
/etc/orbita/tv/<id>/client.crt
/etc/orbita/tv/<id>/client.key
```

В git они не попадают. При повторном pairing старые файлы можно заменить:
телевизор хранит список сопряжённых клиентов, лишние удаляются в настройках.

## Конфиг

`/etc/orbita/tv.json`, образец — `deploy/tv/tv.example.json`. Обязательные поля:
`id`, `host`, `protocol`; для `remote_v2` — `cert` и `key`. `room` и `aliases`
нужны для голоса, `mac` — для Wake-on-LAN, `enabled: false` убирает устройство
из выбора и из инструментов.

## Инструменты

| Инструмент | Что делает |
|---|---|
| `tv_state` | включён ли, громкость, текущее приложение; выключенный TV не будит |
| `tv_power` | явное `on=true/false`, не toggle; проверяет состояние; при включении пробует Wake-on-LAN |
| `tv_key` | HOME, BACK, UP/DOWN/LEFT/RIGHT, OK, PLAY_PAUSE, NEXT, PREVIOUS, STOP, HDMI1–4, TV, INPUT; `POWER` запрещён |
| `tv_volume` | up/down/mute/unmute/set |
| `tv_app` | youtube, netflix, kinopoisk, ivi, okko или deep link |
| `tv_input` | hdmi1–hdmi4, tv |
| `tv_list` | что настроено |

Питание только явное: слепой toggle мог бы выключить работающий телевизор.
Клавиши сериализуются мьютексом на устройство.

## Ограничения

- Включение из глубокого сна зависит от телевизора: Remote v2 доступен, только
  если включён сетевой ждущий режим. Иначе нужен Wake-on-LAN (не все TV его
  принимают по Wi-Fi) или ИК/CEC.
- Sber с «Салютом» может не отдавать Remote v2: остаются Cast/AirPlay, без
  питания и громкости.
- Протокол неофициальный; обновление прошивки может его сломать. Держим
  возможность быстрого отката на Cast.
- Сертификаты — секреты. Не коммитить, не пересылать в чатах, права `0600`.

## Развёртывание

```sh
install -m 0644 deploy/systemd/orbita-tv.service /etc/systemd/system/
install -d -m 0755 /etc/orbita/tv
install -m 0600 <client.crt> /etc/orbita/tv/<id>/client.crt
install -m 0600 <client.key> /etc/orbita/tv/<id>/client.key
install -m 0600 deploy/tv/tv.json /etc/orbita/tv.json
systemctl daemon-reload && systemctl enable --now orbita-tv
```

В юните агента добавить `tv#http://127.0.0.1:8898/mcp` в
`AGENT_MCP_SERVER_URLS` и перезапустить `orbita-agent`.
