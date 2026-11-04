#!/bin/bash
# Black-box verification of the agent's station directives with the simulator.
#
# Run on CT 101 (quasar, agent, simulator in one place):
#   DUID=<station-duid> deploy/verify/run-directives.sh
#
# Requires: orbita-station-sim (release), jq; sqlite3 and journalctl are used
# when available. The script only talks to the local quasar and reads the
# agent DB/logs. It does not restart services.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
SIM=${STATION_SIM:-/opt/orbita/target/release/orbita-station-sim}
URL=${QUASAR_WS:-ws://127.0.0.1:31120/uniproxy.alice.yandex.net/uni.ws}
REPORT=${REPORT:-/tmp/orbita-verify-directives.json}
OUT=${OUT:-/tmp/orbita-verify-directives.log}
DB=${AGENT_DB:-/var/lib/orbita-agent/agent.db}
AGENT_UNIT=${AGENT_UNIT:-orbita-agent}
QUASAR_UNIT=${QUASAR_UNIT:-quasar-server}
REMOTE_SSH=${REMOTE_SSH:-}
REMOTE_CT=${REMOTE_CT:-101}
START=$(date -u +%Y-%m-%dT%H:%M:%S)

if [ -z "${DUID:-}" ]; then
  echo "DUID is required (the station id quasar knows)" >&2
  exit 2
fi
if [ ! -x "$SIM" ]; then
  echo "simulator not found: $SIM" >&2
  exit 2
fi
if ! command -v jq >/dev/null; then
  echo "jq is required" >&2
  exit 2
fi

SCENARIO=${SCENARIO:-$HERE/scenarios/directives.json}
if [ "${VOICE:-0}" = 1 ]; then
  if ! command -v say >/dev/null || ! command -v ffmpeg >/dev/null; then
    echo "VOICE=1 needs macOS say and ffmpeg" >&2
    exit 2
  fi
  workdir=$(mktemp -d /tmp/orbita-verify-voice.XXXXXX)
  trap 'rm -rf "$workdir"' EXIT
  phrases=(
    "Алиса, привет, как у тебя дела"
    "Алиса, сделай громче, пожалуйста"
    "Алиса, сделай тише, пожалуйста"
    "Алиса, установи громкость 4, пожалуйста"
    "Алиса, поставь таймер на 2 минуты, пожалуйста"
    "Алиса, поставь таймер на две минуты, пожалуйста"
    "Алиса, поставь таймер на 30 секунд, пожалуйста"
    "Алиса, напомни через 20 минут проверить чай"
    ""
  )
  steps='['
  for index in "${!phrases[@]}"; do
    file="$workdir/$index.ogg"
    if [ -z "${phrases[$index]}" ]; then
      ffmpeg -y -loglevel error -f lavfi -i anullsrc=r=48000:cl=mono -t 2.0 \
        -c:a libopus -b:a 32k "$file"
    else
      say -v Milena -o "$workdir/$index.aiff" "${phrases[$index]}"
      ffmpeg -y -loglevel error -i "$workdir/$index.aiff" -ar 48000 -ac 1 \
        -af "volume=0.03,apad=pad_dur=2.0" -c:a libopus -b:a 32k "$file"
    fi
    if [ "$index" -gt 0 ]; then
      steps+=','
    fi
    steps+=$(jq -cn --arg file "$file" '{type: "voice", file: $file}')
  done
  steps+=']'
  printf '%s' "$steps" > "$workdir/scenario.json"
  SCENARIO="$workdir/scenario.json"
fi

pass=0
fail=0
ok() { pass=$((pass + 1)); printf 'PASS  %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf 'FAIL  %s\n' "$1"; }
check() { if "$@"; then ok "$1"; else bad "$1"; fi; }
has_directive() {
  jq -e --argjson index "$1" --arg name "$2" \
    '.steps[$index].report.directives | index($name) != null' "$REPORT" >/dev/null
}
step_text_nonempty() {
  jq -e --argjson index "$1" '.steps[$index].report.text | length > 0' "$REPORT" >/dev/null
}
step_text_matches() {
  jq -e --argjson index "$1" --arg pattern "$2" \
    '.steps[$index].report.text | test($pattern; "i")' "$REPORT" >/dev/null
}

echo "running simulator: $SIM"
if ! "$SIM" --url "$URL" --duid "$DUID" \
  --replay "$SCENARIO" \
  --report "$REPORT" --timeout 30 >"$OUT" 2>&1; then
  bad "simulator replay"
  tail -30 "$OUT" >&2
  echo "report: $REPORT"
  exit 1
fi
ok "simulator replay"

check step_text_nonempty 0
check has_directive 1 sound_louder
check has_directive 2 sound_quiter
check has_directive 3 sound_set_level
check has_directive 4 timer_set_directive
check has_directive 5 timer_set_directive
check has_directive 6 timer_set_directive
check step_text_matches 8 'не расслышала'

sqlite_available=1
if [ -n "$REMOTE_SSH" ]; then
  if ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
    "pct exec $REMOTE_CT -- command -v sqlite3" >/dev/null 2>&1; then
    timers=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
      "pct exec $REMOTE_CT -- sqlite3 $DB \"select count(*) from tasks where kind='native_timer';\"" 2>/dev/null || echo 0)
    reminders=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
      "pct exec $REMOTE_CT -- sqlite3 $DB \"select count(*) from tasks where kind='reminder';\"" 2>/dev/null || echo 0)
  elif command -v sqlite3 >/dev/null; then
    db_dir=$(dirname "$DB")
    db_base=$(basename "$DB")
    tmpdir=$(mktemp -d)
    if ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
      "pct exec $REMOTE_CT -- tar -C $db_dir -czf - ." 2>/dev/null | tar -xz -C "$tmpdir" 2>/dev/null; then
      timers=$(sqlite3 "$tmpdir/$db_base" "select count(*) from tasks where kind='native_timer';" 2>/dev/null || echo 0)
      reminders=$(sqlite3 "$tmpdir/$db_base" "select count(*) from tasks where kind='reminder';" 2>/dev/null || echo 0)
      rm -rf "$tmpdir"
    else
      rm -rf "$tmpdir"
      sqlite_available=0
      echo "SKIP  remote sqlite checks (copy failed)"
    fi
  else
    sqlite_available=0
    echo "SKIP  remote sqlite checks (no local sqlite3)"
  fi
  agent_logs=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
    "pct exec $REMOTE_CT -- journalctl -u $AGENT_UNIT --since '$START' --no-pager" 2>/dev/null || true)
  quasar_logs=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
    "pct exec $REMOTE_CT -- journalctl -u $QUASAR_UNIT --since '$START' --no-pager" 2>/dev/null || true)
else
  if command -v sqlite3 >/dev/null && [ -r "$DB" ]; then
    timers=$(sqlite3 "$DB" "select count(*) from tasks where kind='native_timer';" 2>/dev/null || echo 0)
    reminders=$(sqlite3 "$DB" "select count(*) from tasks where kind='reminder';" 2>/dev/null || echo 0)
  else
    sqlite_available=0
    echo "SKIP  sqlite checks (sqlite3 or $DB unavailable)"
  fi
  agent_logs=$(journalctl -u "$AGENT_UNIT" --since "$START" --no-pager 2>/dev/null || true)
  quasar_logs=$(journalctl -u "$QUASAR_UNIT" --since "$START" --no-pager 2>/dev/null || true)
fi

if [ "$sqlite_available" -eq 1 ]; then
  if [ "${timers:-0}" -ge 3 ] 2>/dev/null; then
    ok "sqlite native_timer >= 3 (got $timers)"
  else
    bad "sqlite native_timer >= 3 (got ${timers:-0})"
  fi
  if [ "${reminders:-0}" -ge 1 ] 2>/dev/null; then
    ok "sqlite reminder >= 1 (got $reminders)"
  else
    bad "sqlite reminder >= 1 (got ${reminders:-0})"
  fi
fi

if printf '%s\n' "$agent_logs" | grep -q 'prepare functions=18'; then
  ok "agent log: prepare functions=18"
else
  bad "agent log: prepare functions=18"
fi
if printf '%s\n' "$quasar_logs" | grep -q "Converting directive 'smart_speaker_"; then
  ok "quasar log: converting a smart_speaker directive"
else
  bad "quasar log: converting a smart_speaker directive"
fi

echo "report: $REPORT"
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
