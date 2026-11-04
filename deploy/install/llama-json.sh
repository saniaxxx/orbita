#!/bin/bash
# Install the orbita drop-in for llama.service: thinking off and a GBNF JSON
# grammar, so the processor always receives the JSON object it parses.
#
#   scp deploy/llama-json.gbnf deploy/systemd/llama.service.d/orbita.conf \
#       deploy/install/llama-json.sh root@pve3:/tmp/
#   ssh root@pve3
#   pct push 100 /tmp/llama-json.gbnf /tmp/llama-json.gbnf
#   pct push 100 /tmp/orbita.conf /tmp/orbita.conf
#   pct push 100 /tmp/llama-json.sh /tmp/llama-json.sh
#   pct exec 100 -- bash /tmp/llama-json.sh
#
# The drop-in repeats the full ExecStart of llama.service, because systemd
# replaces the whole command; keep it in sync with the unit.
set -euo pipefail

SRC=${SRC:-/tmp}
install -m 644 "$SRC/llama-json.gbnf" /etc/llama-json.gbnf
install -d /etc/systemd/system/llama.service.d
install -m 644 "$SRC/orbita.conf" /etc/systemd/system/llama.service.d/orbita.conf
systemctl daemon-reload
systemctl restart llama
sleep 10
systemctl is-active llama
ss -ltn | grep 8090
