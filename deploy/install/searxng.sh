#!/bin/bash
# Install SearXNG in CT 100 as a systemd service on 127.0.0.1:8888.
#
#   scp deploy/searxng/settings.yml deploy/systemd/searxng.service \
#       deploy/install/searxng.sh root@pve3:/tmp/
#   ssh root@pve3
#   pct exec 100 -- mkdir -p /tmp/searxng
#   pct push 100 /tmp/settings.yml /tmp/searxng/settings.yml
#   pct push 100 /tmp/searxng.service /tmp/searxng.service
#   pct push 100 /tmp/searxng.sh /tmp/searxng.sh
#   pct exec 100 -- bash /tmp/searxng.sh
set -euo pipefail

SRC=${SRC:-/tmp/searxng}
DEST=${DEST:-/opt/searxng}

DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git curl >/dev/null 2>&1

if [ ! -d "$DEST/.git" ]; then
  rm -rf "$DEST"
  git clone -q --depth 1 https://github.com/searxng/searxng "$DEST"
fi
python3 -m venv "$DEST/.venv"
"$DEST/.venv/bin/pip" install -q -r "$DEST/requirements.txt" granian

id searxng >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin searxng
mkdir -p /etc/searxng
SECRET=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
sed "s/__SECRET__/$SECRET/" "$SRC/settings.yml" > /etc/searxng/settings.yml
chown -R searxng:searxng /etc/searxng "$DEST"
chmod 750 /etc/searxng
chmod 640 /etc/searxng/settings.yml

install -m 644 /tmp/searxng.service /etc/systemd/system/searxng.service
systemctl daemon-reload
systemctl enable --now searxng
sleep 6
systemctl is-active searxng

curl -s -m 40 "http://127.0.0.1:8888/search?q=%D0%BF%D0%BE%D0%B3%D0%BE%D0%B4%D0%B0&format=json" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print("results:", len(d.get("results", [])), "unresponsive:", d.get("unresponsive_engines"))'
echo FINISH_DONE
