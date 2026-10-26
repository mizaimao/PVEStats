#!/bin/sh
# Starts, stops or reports the PVEStats router agent, pvestats-router,
# listening on the router's LAN address only, port 9837.
#
#   service.sh start     what /jffs/scripts/services-start runs at boot
#   service.sh stop
#   service.sh status
#
# `start` leaves one keeper behind, which runs the agent and starts it again
# half a minute after it ever exits; a second `start` finds the keeper and
# does nothing. Nothing here logs, since the router keeps its log on flash,
# and the only files written are the keeper's process id and lock, in /tmp,
# which is memory.

# The firmware's own tools, never Entware's from the USB drive. The agent
# runs `nvram` and `wl` from this PATH too.
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
                # 8>&-: the agent does not hold the keeper's lock.
                "$DIR/pvestats-router" --listen "$ip:$PORT" 8>&-
            fi
            sleep 30
        done
        ;;
    stop)
        # The keeper first, so it cannot start the agent again.
        keeper_alive && kill "$pid"
        rm -f "$PIDFILE"
        for p in $(pidof pvestats-router); do kill "$p" 2>/dev/null; done
        ;;
    status)
        if netstat -ltn | grep -q ":$PORT "; then echo running; else echo stopped; fi
        ;;
    *)
        echo "usage: $0 start|stop|status" >&2
        exit 2
        ;;
esac
