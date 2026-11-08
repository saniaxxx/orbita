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
  if ! command -v curl >/dev/null || ! command -v ffmpeg >/dev/null; then
    echo "VOICE=1 needs curl and ffmpeg" >&2
    exit 2
  fi
  # macOS `say` voices are badly recognised by GigaAM (VAD often finds no
  # speech). Synthesize with the stand TTS instead: same domain as the voice
  # path, so the harness measures directives, not ASR quality.
  TTS_URL=${TTS_URL:-http://127.0.0.1:9170}
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
      payload=$(jq -cn --arg input "${phrases[$index]}" \
        '{input: $input, response_format: "wav"}')
      if ! curl -fsS -m 30 -X POST "$TTS_URL/v1/audio/speech" \
        -H 'content-type: application/json' -d "$payload" \
        -o "$workdir/$index.wav"; then
        echo "TTS synthesis failed for: ${phrases[$index]}" >&2
        exit 2
      fi
      ffmpeg -y -loglevel error -i "$workdir/$index.wav" -ar 48000 -ac 1 \
        -af "apad=pad_dur=2.0" -c:a libopus -b:a 32k "$file"
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

# Prints "<native_timers> <reminders>" from the agent DB, or nothing.
db_counts() {
  local db_dir db_base tmpdir timers reminders
  if [ -n "$REMOTE_SSH" ]; then
    if ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
      "pct exec $REMOTE_CT -- command -v sqlite3" >/dev/null 2>&1; then
      timers=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
        "pct exec $REMOTE_CT -- sqlite3 $DB \"select count(*) from tasks where kind='native_timer';\"" 2>/dev/null) || return 1
      reminders=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
        "pct exec $REMOTE_CT -- sqlite3 $DB \"select count(*) from tasks where kind='reminder';\"" 2>/dev/null) || return 1
    elif command -v sqlite3 >/dev/null; then
      db_dir=$(dirname "$DB")
      db_base=$(basename "$DB")
      tmpdir=$(mktemp -d)
      if ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
        "pct exec $REMOTE_CT -- tar -C $db_dir -czf - ." 2>/dev/null | tar -xz -C "$tmpdir" 2>/dev/null; then
        timers=$(sqlite3 "$tmpdir/$db_base" "select count(*) from tasks where kind='native_timer';" 2>/dev/null) || { rm -rf "$tmpdir"; return 1; }
        reminders=$(sqlite3 "$tmpdir/$db_base" "select count(*) from tasks where kind='reminder';" 2>/dev/null) || { rm -rf "$tmpdir"; return 1; }
        rm -rf "$tmpdir"
      else
        rm -rf "$tmpdir"
        return 1
      fi
    else
      return 1
    fi
  else
    command -v sqlite3 >/dev/null && [ -r "$DB" ] || return 1
    timers=$(sqlite3 "$DB" "select count(*) from tasks where kind='native_timer';" 2>/dev/null) || return 1
    reminders=$(sqlite3 "$DB" "select count(*) from tasks where kind='reminder';" 2>/dev/null) || return 1
  fi
  printf '%s %s\n' "$timers" "$reminders"
}

counts_before=$(db_counts) || counts_before=""
if [ -z "$counts_before" ]; then
  echo "SKIP  sqlite delta checks (agent DB unavailable)"
fi

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

if [ -n "$counts_before" ]; then
  read -r timers_before reminders_before <<<"$counts_before"
  counts_after=$(db_counts) || counts_after=""
  if [ -z "$counts_after" ]; then
    bad "sqlite delta (post-run read failed)"
  else
    read -r timers_after reminders_after <<<"$counts_after"
    new_timers=$((timers_after - timers_before))
    new_reminders=$((reminders_after - reminders_before))
    if [ "$new_timers" -ge 3 ]; then
      ok "sqlite new native_timer >= 3 (got $new_timers)"
    else
      bad "sqlite new native_timer >= 3 (got $new_timers)"
    fi
    if [ "$new_reminders" -ge 1 ]; then
      ok "sqlite new reminder >= 1 (got $new_reminders)"
    else
      bad "sqlite new reminder >= 1 (got $new_reminders)"
    fi
  fi
fi

if [ -n "$REMOTE_SSH" ]; then
  agent_logs=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
    "pct exec $REMOTE_CT -- journalctl -u $AGENT_UNIT --since '$START' --no-pager" 2>/dev/null || true)
  quasar_logs=$(ssh -o BatchMode=yes -o ConnectTimeout=5 "$REMOTE_SSH" \
    "pct exec $REMOTE_CT -- journalctl -u $QUASAR_UNIT --since '$START' --no-pager" 2>/dev/null || true)
else
  agent_logs=$(journalctl -u "$AGENT_UNIT" --since "$START" --no-pager 2>/dev/null || true)
  quasar_logs=$(journalctl -u "$QUASAR_UNIT" --since "$START" --no-pager 2>/dev/null || true)
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
