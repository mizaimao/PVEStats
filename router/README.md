# pvestats router agent

For an Asuswrt-Merlin router (written on a GT-BE98 Pro, firmware 3006.102).
Serves the router's own counters as JSON on its LAN address, port 9837, no
auth, so the dashboard needs no router login: Merlin has only the one admin
account. It reveals traffic totals, device counts per band and the WAN
address, nothing about the devices themselves.

- `stats.sh` reads `/proc`, `/sys`, `nvram` and `wl assoclist`, and answers
  one request. A run takes about 0.15 s, so a poll every 5 s costs under 1%
  of the router's CPU.
- `service.sh start|stop|status` keeps socat, which is in the firmware,
  listening on the LAN address only and running `stats.sh` per request.

Both use only the firmware's tools, never Entware's on the USB drive. Neither
logs anything (the router keeps its log on flash) or writes anywhere but
`/tmp`, which is memory: a process id and two lock files.

## Limits, so it cannot hurt the router

- At most four requests at once (socat `max-children=4`), and one run of
  `stats.sh` at a time; the others are told 503 at once. Should `wl` or
  `nvram` ever hang, the stuck run keeps the lock and every later request
  gets a 503, rather than more stuck runs piling up behind it.
- A request's head is read for at most 32 lines, two seconds a line, and a
  connection idle for 15 seconds is closed. What a request asks is never
  looked at; every one gets the same answer.
- One keeper, even if started twice. Should socat ever exit, the keeper
  starts it again after 30 seconds; it sleeps the rest of the time. It
  neither starts without a LAN address nor binds to anything else.

## What it answers

```json
{"model": "GT-BE98_PRO", "uptime": 186648.38,
 "cpu": {"busy": 2975904, "total": 74262583},
 "mem": {"total": 2097111040, "available": 891924480},
 "temp": {"cpu": 69.7},
 "wan": {"up": true, "ip": "…", "rx": 65611232984, "tx": 25224388291},
 "ports": [{"name": "eth0", "up": true, "mbps": 10000, "rx": 99486587416, "tx": 169834333430}],
 "bands": [{"name": "5 GHz", "on": true, "devices": 1, "rx": 3663050058, "tx": 9191432552}]}
```

`cpu` is jiffies over all cores, cumulative like the byte counters; the
dashboard turns both into rates. `mem` is bytes, `temp` °C (the SoC's own
sensor; the radios' sensors are left alone, since reading them asks each chip
directly). A band's rx is what its devices sent, tx what they got, over the
radio and the networks it carries (`wl0` with `wl0.1` and so on). Two radios
on one band are numbered: "6 GHz-1", "6 GHz-2".

## Trying it without installing

Nothing is written on the router:

    ssh router 'sh -s -- --json' < router/stats.sh

## Installing (only with Frank's go-ahead)

Needs SSH and "Enable JFFS custom scripts and configs" (Administration ›
System), both already on for AdGuard Home. From the repo:

    ssh router 'mkdir -p /jffs/addons/pvestats'
    ssh router 'cat > /jffs/addons/pvestats/stats.sh' < router/stats.sh
    ssh router 'cat > /jffs/addons/pvestats/service.sh' < router/service.sh
    ssh router 'chmod 755 /jffs/addons/pvestats/stats.sh /jffs/addons/pvestats/service.sh'

Start it at boot from `/jffs/scripts/services-start`, adding one line and
leaving any others (amtm's, AdGuard Home's) as they are:

    ssh router 'f=/jffs/scripts/services-start; [ -f $f ] || printf "#!/bin/sh\n" > $f;
        grep -q pvestats $f || echo "/jffs/addons/pvestats/service.sh start" >> $f; chmod 755 $f'

Start it now and check it from the LAN:

    ssh router '/jffs/addons/pvestats/service.sh start; sleep 2; /jffs/addons/pvestats/service.sh status'
    curl -s http://<router>:9837/

## Uninstalling

    ssh router '/jffs/addons/pvestats/service.sh stop; sed -i "/pvestats/d" /jffs/scripts/services-start;
        rm -r /jffs/addons/pvestats'

What it left in `/tmp` goes at the next reboot.
