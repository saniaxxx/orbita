#!/bin/sh
# Run on the router (OpenWrt 24.10, dnsmasq). Removes the dnsmasq `address=`
# list, which in this project contains only the Yandex domains that used to
# point at the old proxy host. Yandex domains must resolve on the station
# itself from /etc/hosts (deploy/station/hosts-yandex.txt), so the router must
# not override them.
#
#   ssh root@192.168.250.1 'sh -s' < deploy/install/router-dns.sh
#
# Back up /etc/config/dhcp before running. Rollback:
#   cp /etc/config/dhcp.bak-orbita-<date> /etc/config/dhcp
#   uci commit dhcp && /etc/init.d/dnsmasq restart
set -eu

SECTION=$(uci show dhcp | sed -n 's/^\(dhcp\.[^.=]*\)=dnsmasq$/\1/p' | head -1)
[ -n "$SECTION" ] || { echo "no dnsmasq section in /etc/config/dhcp" >&2; exit 1; }

if uci -q get "$SECTION.address" >/dev/null 2>&1; then
  uci delete "$SECTION.address"
  echo "removed the dnsmasq address list"
else
  echo "no address list in $SECTION"
fi

uci commit dhcp
/etc/init.d/dnsmasq restart
