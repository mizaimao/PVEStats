#!/bin/sh
# Starts, stops or reports the PVEStats router agent: socat listening on the
# router's LAN address only, port 9837, running stats.sh once per request,
# at most four at a time.
#
#   service.sh start     what /jffs/scripts/services-start runs at boot
#   service.sh stop
#   service.sh status
#
# `start` leaves one keeper behind, which runs socat and starts it again half
# a minute after it ever exits; a second `start` finds the keeper and does
# nothing. Nothing here logs, since the router keeps its log on flash, and the
# only files written are the keeper's process id and lock, in /tmp, which is
# memory.

# The firmware's own tools, never Entware's from the USB drive.
PATH=/sbin:/bin:/usr/sbin:/usr/bin
export PATH

PORT=9837
DIR=/jffs/addons/pvestats
PIDFILE=/tmp/pvestats-agent.pid
LOCK=/tmp/pvestats-keeper.lock

# Whether the process in the pid file is our keeper, not some later process
# that was given the same id. cmdline's arguments are NUL-separated.
keeper_alive() {
    pid=$(cat "$PIDFILE" 2>/dev/null)
    [ -n "$pid" ] && tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q 'service.sh keep'
}

# The listening socat and any copies it has forked.
listeners() {
    ps w | awk -v want="TCP4-LISTEN:$PORT," 'index($0, want) && !/awk/ { print $1 }'
}

case "$1" in
    start)
        keeper_alive && exit 0
        nohup "$DIR/service.sh" keep </dev/null >/dev/null 2>&1 &
        ;;
    keep)
        # One keeper, even if two starts race: the second finds the lock
        # held and leaves.
        exec 8>"$LOCK"
        flock -n 8 || exit 0
        echo $$ >"$PIDFILE"
        while :; do
            ip=$(nvram get lan_ipaddr)
            if [ -n "$ip" ]; then
                # 8>&-: socat and its children do not hold the keeper's lock.
                # -T 15: a connection idle that long is closed.
                socat -T 15 TCP4-LISTEN:$PORT,bind="$ip",reuseaddr,fork,max-children=4 EXEC:"$DIR/stats.sh" 8>&-
            fi
            sleep 30
        done
        ;;
    stop)
        # The keeper first, so it cannot start socat again.
        keeper_alive && kill "$pid"
        rm -f "$PIDFILE"
        for p in $(listeners); do kill "$p" 2>/dev/null; done
        ;;
    status)
        if netstat -ltn | grep -q ":$PORT "; then echo running; else echo stopped; fi
        ;;
    *)
        echo "usage: $0 start|stop|status" >&2
        exit 2
        ;;
esac
