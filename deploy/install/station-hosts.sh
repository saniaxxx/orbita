#!/bin/bash
# Pin the Yandex domains to our stack in the station's /etc/hosts, so a cached
# or leaked DNS answer cannot send the station to the real Yandex backend.
#
# Run where the station is reachable over ADB (root):
#   adb connect 192.168.250.115:5555
#   bash station-hosts.sh
#
# The rootfs of the station is mounted read-only; it is remounted rw for the
# write and back to ro afterwards. The original file is kept as
# /etc/hosts.orbita-backup.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
HOSTS=${HOSTS:-$HERE/../station/hosts-yandex.txt}
TARGET=${TARGET:-/etc/hosts}

adb push "$HOSTS" /data/local/tmp/station-hosts >/dev/null

# Rebuild from the two stock lines: repeated runs must not duplicate entries.
adb shell "mount -o remount,rw /
printf '127.0.0.1\tlocalhost\n127.0.1.1\tyandex-station-lite\n' > $TARGET
cat /data/local/tmp/station-hosts >> $TARGET
mount -o remount,ro /
grep -c 192.168.250.102 $TARGET"
