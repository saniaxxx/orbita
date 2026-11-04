# orbita — задание агенту-развёртывателю

Документ для агента, который меняет стенд. Сначала прочитай `docs/state.md`:
там текущая топология и что уже работает. Здесь — как безопасно вносить
изменения. Проверки описаны в `docs/agent-verification.md`.

## 0. Готовый промпт

```text
Ты — агент-развёртыватель стека orbita на стенде. Работай по docs/agent-deploy.md.

Сначала прочитай AGENTS.md, docs/state.md, docs/spec.md (§2, §5, §10, §12)
и docs/agent-verification.md. Ветка — feat/rust; main не трогать.

CT 101 `orbita` уже развёрнут: не создавай его заново. https-proxy уже
перенесён туда, станция ходит через .103. Старые юниты и прокси в CT 100
оставлены как путь отката.

Перед любым изменением сделай бэкап и запиши команду отката. Работай по
фазам: 0 — инвентаризация (только чтение), 1 — обновление control plane в
CT 101, 2 — станция/прокси (только с подтверждением), 3 — Glagol и музыка
(только с подтверждением), 4 — GPU и Strata (отдельный POC). Фазы 2–4 не
начинай без явного разрешения человека.

Не коммить секреты, записи голоса и приватные запросы. Не перезагружай pve3
и не трогай станцию без разрешения. Если чего-то не хватает (доступ, IP,
диск, RAM) — остановись и спроси.

После каждой фазы пришли отчёт: команды, ключевой вывод, что изменено,
порты, юниты, риски, откат, уровень проверки. L0–L2 — офлайн, L3–L4 —
только с реальной станцией.
```

## 1. Факты о стенде

| Что | Значение |
|---|---|
| Хост | `pve3`, Proxmox VE |
| CT 100 `alice` | `192.168.250.102`, GPU RTX 5060, llama.cpp, GigaAM, Silero, старый quasar, SearXNG, путь отката |
| CT 101 `orbita` | `192.168.250.103`, CPU control plane: processor, mcp-search, dashboard, https-proxy |
| Станция | `192.168.250.115`, домены Яндекса в её `/etc/hosts` → `.103` |
| Rust processor | `0.0.0.0:8080` |
| Rust mcp-search | `127.0.0.1:8899` |
| Rust dashboard | `0.0.0.0:8901` |
| https-proxy | CT 101, `80/443/8443`, upstream — старый quasar `.102:31120` |
| Старый quasar | `.102:31120/31121`, `PROCESSOR_BASIC_URL=http://192.168.250.103:8080/process` |
| llama.cpp | `.102:8090`, модель `qwen3.5-9b-q4km` |
| SearXNG | `.102:8888` |
| GigaAM (Python) | `.102:8006` |
| Silero TTS | `.102`, прод-синтез |
| Модели Rust ASR/TTS | `/opt/orbita/models/...` в CT 101; не в git |

CT 101: Debian 13, 2 ядра, 2048 МиБ, 10 ГБ, unprivileged, без GPU, onboot.
Тяжёлая сборка на 2 ГБ уже один раз приводила к зависанию pve3 — собирай с
`CARGO_BUILD_JOBS=2` и `CARGO_TARGET_DIR=/var/tmp/orbita-target` или на
отдельной машине; после сборки target удаляй.

## 2. Фаза 0 — инвентаризация (только чтение)

На `pve3`:

```sh
pveversion -v
pct list
cat /etc/pve/lxc/100.conf
cat /etc/pve/lxc/101.conf
free -h; df -h; nproc
```

В CT 101:

```sh
ip -br addr; ip route
systemctl --no-pager --type=service --state=running
ss -ltnp
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8899/health
curl -s http://127.0.0.1:8901/health
curl -s http://127.0.0.1:8901/api/state
journalctl -u orbita-processor -u orbita-mcp-search -u orbita-dashboard -n 50 --no-pager
```

В CT 100:

```sh
nvidia-smi
systemctl --no-pager --type=service --state=running
curl -s http://127.0.0.1:8090/v1/models
```

Ничего не менять на этой фазе. Зафиксировать свободные IP, RAM, диск, состояние
юнитов и модель LLM.

## 3. Фаза 1 — обновление control plane в CT 101

CT 101 уже существует. Если меняешь Rust-сервисы:

```sh
cd /opt/orbita
git fetch origin feat/rust
git checkout feat/rust
git pull --ff-only
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release \
  -p orbita-processor -p orbita-mcp-search -p orbita-dashboard -p orbita-cli \
  -p orbita-agent
```

Юниты лежат в `deploy/systemd/orbita-*.service`; они запускают бинарники из
`/opt/orbita/target/release`. Если юнит менялся:

```sh
install -m 0644 deploy/systemd/orbita-processor.service /etc/systemd/system/
install -m 0644 deploy/systemd/orbita-mcp-search.service /etc/systemd/system/
install -m 0644 deploy/systemd/orbita-dashboard.service /etc/systemd/system/
install -m 0644 deploy/systemd/orbita-warmup.service /etc/systemd/system/
systemctl daemon-reload
systemctl restart orbita-processor orbita-mcp-search orbita-dashboard
systemctl restart orbita-warmup
```

Секреты — только в `/etc/orbita/secrets.env` (права 600, вне git). Не
перезаписывай его из репозитория.

Если прямого доступа в CT 101 нет, собери на `pve3`/другой машине и доставь
бинарники и юниты через `pct push 101 <file> <path>`.

Проверки после обновления:

```sh
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8899/health
curl -s http://127.0.0.1:8901/api/state
journalctl -u orbita-processor -u orbita-mcp-search -u orbita-dashboard -n 50 --no-pager
```

Откат: вернуть предыдущий коммит/бинарники и перезапустить юниты. Если менялся
юнит — вернуть его из бэкапа.

### 3.1. Замена processor на orbita-agent (только с подтверждением)

Агент занимает тот же порт 8080 и говорит по тому же `/process`, поэтому quasar
менять не нужно.

> На тестовом стенде processor уже заменён агентом: директивы станции
> реализованы (D-15), громкость и таймеры проверены голосом, харнесс —
> 13/13. Юнит `orbita-processor` удалён, `orbita-warmup` ждёт агента.
> Документация — `docs/agent.md`.

```sh
# в CT 101
systemctl stop orbita-processor
systemctl disable orbita-processor
install -m 0644 deploy/systemd/orbita-agent.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now orbita-agent
curl -s http://127.0.0.1:8080/health
```

Проверки:

```sh
/opt/orbita/target/release/orbita-cli ask "привет"
/opt/orbita/target/release/orbita-cli ask "напомни через 1 минуту проверить чай"
journalctl -u orbita-agent -n 50 --no-pager
```

Затем — голосом через станцию и напоминание с реальным push. Если quasar не
передаёт `deviceId`, задать `DEFAULT_DUID` в юните.

Откат:

```sh
systemctl stop orbita-agent
systemctl disable orbita-agent
systemctl enable --now orbita-processor
```

Не удалять `orbita-processor` и его юнит, пока голосовой ход и напоминание не
проверены на станции.

## 4. Фаза 2 — станция и прокси (только с подтверждением)

Прокси уже в CT 101, станция уже смотрит на `.103`. Менять это без
необходимости нельзя: любая ошибка отправит станцию в настоящий Яндекс.

Если нужно откатить:

1. В `deploy/station/hosts-yandex.txt` вернуть адрес `.102`.
2. Прогнать `bash deploy/install/station-hosts.sh` на машине с ADB.
3. Убедиться, что старый прокси в CT 100 жив.
4. Проверить, что станция отвечает.

Если нужно перенастроить upstream нового прокси:

```sh
# в CT 101
systemctl cat https-proxy
# изменить YANDEX_UPSTREAM_URL, затем
systemctl daemon-reload && systemctl restart https-proxy
```

Перед изменением — бэкап юнита и конфига nginx. После — TLS-проверка доменов и
реальный голосовой ход.

## 5. Фаза 3 — Glagol и музыка (только с подтверждением)

### 5.1. Постоянный ключ Glagol

В юните Rust-quasar задать `GLAGOL_JWT_KEY` (32+ случайных байта) и
перезапустить. Без этого токены умирают после каждого рестарта.

### 5.2. Спайк

```sh
cargo run -p orbita-glagol -- discover
cargo run -p orbita-glagol -- token \
  --device-id <DUID> --platform yandexmicro \
  --quasar http://192.168.250.103:31120
cargo run -p orbita-glagol -- state \
  --host <IP> --port <PORT> \
  --device-id <DUID> --platform yandexmicro --watch 5
```

Ожидаемо: станция найдена по mDNS, токен получен, в логе quasar есть
`check_token`, `state` печатает сообщения. Затем негативный тест с токеном от
quasar с другим ключом и `play-url` на локальный файл. Подробно —
`docs/glagol.md`.

Если спайк проходит — можно ставить Music Assistant в отдельный LXC
`orbita-music` (Docker допустим для внешних сервисов, `network_mode: host`).
Если нет — не начинать MA: сначала разобраться с токеном/`device_list`.

### 5.3. `glagol/device_list`

В `orbita-quasar` его пока нет. Если MA не находит станцию — нужен реальный
запрос/ответ со стенда, затем реализация. Не придумывать схему по памяти.

## 6. Фаза 4 — GPU и Strata (отдельный POC)

Не входит в обычное обновление. Требует:

- одной A2 16 ГБ на первом этапе: Strata + TTS + GigaAM, резерв 2048 МиБ;
- проверки, что CUDA-сборка поддерживает A2 (sm_86);
- POC поддерживаемой модели Strata, замеров TTFT/токенов;
- сохранения llama.cpp как fallback;
- отдельного подтверждения и плана отката.

Позже — T400 под STT/TTS, если есть слот, обдув и запас по БП.

## 7. Стоп-условия

Остановиться и спросить, если:

- нет доступа к `pve3` или CT 101;
- требуется ребут `pve3` или станции;
- нужно менять `hosts` станции или upstream прокси без явного разрешения;
- нет свободного диска/RAM под сборку;
- старый стек уже нездоров — сначала разобраться, потом менять;
- появляются секреты, записи голоса или приватные запросы в git.

## 8. Отчёт

```text
Фаза:
Команды:
Что изменено:
Порты и адреса:
Юниты:
Проверки (вывод):
Уровень проверки: L0/L1/L2/L3/L4
Расхождения:
Риски:
Откат:
Что осталось:
```
