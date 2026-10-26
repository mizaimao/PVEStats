#!/bin/sh
# Starts the PVEStats router agent unless it is already listening: socat on
# the LAN address only, one copy of stats.sh per request.
PORT=9837
netstat -ltn | grep -q ":$PORT " && exit 0
socat TCP-LISTEN:$PORT,bind="$(nvram get lan_ipaddr)",fork,reuseaddr \
    EXEC:/jffs/addons/pvestats/stats.sh </dev/null >/dev/null 2>&1 &
