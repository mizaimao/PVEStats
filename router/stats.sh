#!/bin/sh
# PVEStats router agent, for an Asuswrt-Merlin router: the router's own
# counters as JSON, for the dashboard's router card. socat runs one copy per
# request (see service.sh); `stats.sh --json` prints the JSON alone, for
# trying it by hand.
#
# It only reads: /proc, /sys, nvram and `wl assoclist`. It logs nothing, since
# the router keeps its log on flash, and writes only a lock file in /tmp,
# which is memory (and not even that with --json). Byte and CPU counters are
# cumulative since boot; the dashboard turns them into rates.

# The firmware's own tools, never Entware's from the USB drive.
PATH=/sbin:/bin:/usr/sbin:/usr/bin
export PATH

# rx and tx bytes since boot, summed over the interfaces named; 0 for ones it
# has not got. Summed in awk: the shell's arithmetic is 32-bit here, and the
# counters pass 4 GiB in days. A big first number can follow the colon with
# no space between them.
bytes() {
    awk -v want=" $* " '
        { sub(/^ +/, ""); n = index($0, ":") }
        n && index(want, " " substr($0, 1, n - 1) " ") { split(substr($0, n + 1), f, " "); rx += f[1]; tx += f[9] }
        END { printf "%.0f %.0f\n", rx, tx }' /proc/net/dev
}

http=true
[ "$1" = "--json" ] && http=false

# Read the request's head, so closing the socket does not reset it under
# the client: up to its blank line, at most 32 lines, two seconds a line.
# What it asks is never looked at; every request gets the same answer.
if $http; then
    cr=$(printf '\r')
    n=0
    while [ $n -lt 32 ] && read -t 2 -r line; do
        case "$line" in "" | "$cr") break ;; esac
        n=$((n + 1))
    done
    # One run at a time. Should `wl` or `nvram` ever hang, the stuck run
    # keeps the lock (its children inherit fd 9), and every later request
    # is told "busy" at once instead of leaving more stuck runs behind it.
    exec 9>/tmp/pvestats-stats.lock
    if ! flock -n 9; then
        printf 'HTTP/1.0 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'
        exit 0
    fi
fi

model=$(nvram get productid)
uptime=$(cut -d' ' -f1 /proc/uptime)
# busy and total jiffies over all cores: user nice system idle iowait irq softirq steal.
cpu=$(awk '/^cpu / { busy = $2 + $3 + $4 + $7 + $8 + $9; printf "%.0f %.0f\n", busy, busy + $5 + $6 }' /proc/stat)
mem=$(awk '/^MemTotal:/ { t = $2 } /^MemAvailable:/ { a = $2 } END { printf "%.0f %.0f\n", t * 1024, a * 1024 }' /proc/meminfo)
# The SoC's sensor, a plain file. The radios' own sensors (`wl phy_tempsense`)
# are left alone: they ask each chip directly.
cpu_temp=$(awk '{ printf "%.1f", $1 / 1000 }' /sys/class/thermal/thermal_zone0/temp 2>/dev/null)

# The internet side.
wan_if=$(nvram get wan0_ifname)
wan_up=false
[ "$(nvram get wan0_state_t)" = 2 ] && wan_up=true
set -- $(bytes "$wan_if")
wan="{\"up\":$wan_up,\"ip\":\"$(nvram get wan0_ipaddr)\",\"rx\":${1:-0},\"tx\":${2:-0}}"

# The wired LAN ports, in the order the router lists them.
ports=""
for e in $(nvram get lan_ifnames); do
    case "$e" in eth*) ;; *) continue ;; esac
    up=false
    [ "$(cat /sys/class/net/"$e"/operstate 2>/dev/null)" = up ] && up=true
    speed=$(cat /sys/class/net/"$e"/speed 2>/dev/null)
    [ "${speed:-0}" -gt 0 ] 2>/dev/null || speed=0
    set -- $(bytes "$e")
    ports="$ports${ports:+,}{\"name\":\"$e\",\"up\":$up,\"mbps\":$speed,\"rx\":${1:-0},\"tx\":${2:-0}}"
done

# Each radio with the networks it carries (wl0 with wl0.1 and so on): its
# devices, and its traffic. rx is what the devices sent, tx what they got.
bands=""
six=0
for u in 0 1 2 3 4 5; do
    radio=$(nvram get wl${u}_ifname)
    [ -n "$radio" ] || continue
    case "$(nvram get wl${u}_nband)" in
        2) name="2.4 GHz" ;;
        1) name="5 GHz" ;;
        4) six=$((six + 1)); name="6 GHz-$six" ;;
        *) name="$radio" ;;
    esac
    # The radio's other networks (guest ones and the like), as the firmware
    # lists them.
    networks="$radio $(nvram get wl${u}_vifs)"
    devices=0
    for i in $networks; do
        # Empty, not a number, should the router be too short of processes
        # to run `wl`: count nothing rather than end the run mid-reply.
        c=$(wl -i "$i" assoclist 2>/dev/null | grep -c '^assoclist')
        devices=$((devices + ${c:-0}))
    done
    set -- $(bytes $networks)
    on=false
    [ "$(nvram get wl${u}_radio)" = 1 ] && on=true
    bands="$bands${bands:+,}{\"name\":\"$name\",\"on\":$on,\"devices\":$devices,\"rx\":${1:-0},\"tx\":${2:-0}}"
done

set -- $cpu
cpu_json="{\"busy\":${1:-0},\"total\":${2:-0}}"
set -- $mem
json="{\"model\":\"$model\",\"uptime\":${uptime:-0},\"cpu\":$cpu_json,\"mem\":{\"total\":${1:-0},\"available\":${2:-0}},\
\"temp\":{\"cpu\":${cpu_temp:-null}},\"wan\":$wan,\"ports\":[$ports],\"bands\":[$bands]}"

if $http; then
    printf 'HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: %s\r\nConnection: close\r\n\r\n' \
        "$(($(printf '%s' "$json" | wc -c) + 1))"
fi
printf '%s\n' "$json"
