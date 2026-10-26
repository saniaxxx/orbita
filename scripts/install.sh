#!/bin/sh
# Installs orbita as a systemd service. Debian 12+ / Ubuntu 22.04+ (also inside an LXC
# container), as root, from a checkout:
#
#   sudo scripts/install.sh
#
# Speech to text and text to speech (OpenAI-compatible, see servers/) run as separate services,
# not installed here: set their addresses in the config.
#
# Re-running upgrades the packages and the unit files; /etc/orbita/config.yaml and
# /etc/orbita/env are never overwritten.
set -eu

[ $# -eq 0 ] || { echo "usage: $0" >&2; exit 2; }

[ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }
SRC=$(cd "$(dirname "$0")/.." && pwd)

if command -v apt-get >/dev/null 2>&1; then
  apt-get install -y -q python3 python3-venv python3-pip >/dev/null
fi
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' ||
  { echo "Python 3.11+ is required" >&2; exit 1; }

id orbita >/dev/null 2>&1 ||
  useradd --system --home-dir /var/lib/orbita --shell /usr/sbin/nologin orbita

echo "orbita -> /opt/orbita/venv"
[ -x /opt/orbita/venv/bin/python ] || python3 -m venv /opt/orbita/venv
/opt/orbita/venv/bin/pip install -q --upgrade pip
/opt/orbita/venv/bin/pip install -q --upgrade "$SRC"

install -d -m 0750 -o root -g orbita /etc/orbita
if [ ! -e /etc/orbita/config.yaml ]; then
  install -m 0640 -o root -g orbita "$SRC/config.example.yaml" /etc/orbita/config.yaml
  echo "created /etc/orbita/config.yaml: set the satellites and the services"
fi
if [ ! -e /etc/orbita/env ]; then
  printf '# OPENAI_API_KEY=sk-...\n' > /etc/orbita/env
  chown root:orbita /etc/orbita/env
  chmod 0640 /etc/orbita/env
fi

install -m 0644 "$SRC/systemd/orbita.service" /etc/systemd/system/orbita.service
systemctl daemon-reload
systemctl enable orbita.service
echo
echo "next: edit /etc/orbita/config.yaml (satellites, stt, tts, openai), then"
echo "  systemctl restart orbita"
echo "  journalctl -fu orbita"
