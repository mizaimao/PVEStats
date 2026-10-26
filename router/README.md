# pvestats router agent

For an Asuswrt-Merlin router (written on a GT-BE98 Pro, firmware 3006.102).
Serves the router's own counters as JSON on its LAN address, port 9837, no
auth, so the dashboard needs no router login: Merlin has only the one admin
account. It reveals traffic totals, device counts per band and the WAN
address, nothing about the devices themselves.

`stats.sh` only reads `/proc`, `/sys`, `nvram` and `wl`; socat, which is in
the firmware, runs one copy per request. A run takes about 0.15 s, so a poll
every 5 s costs under 1% of the router's CPU. Byte and CPU counters are
cumulative since boot; the dashboard turns them into rates.

```json
{"model": "GT-BE98_PRO", "uptime": 186648.38,
 "cpu": {"busy": 2975904, "total": 74262583},
 "mem": {"total": 2097111040, "available": 891924480},
 "temp": {"cpu": 69.7, "radios": [64, 70, 71, 69]},
 "wan": {"up": true, "ip": "…", "rx": 65611232984, "tx": 25224388291},
 "ports": [{"name": "eth0", "up": true, "mbps": 10000, "rx": 99486587416, "tx": 169834333430}],
 "bands": [{"name": "5 GHz", "on": true, "devices": 1, "rx": 3663050058, "tx": 9191432552}]}
```

`cpu` is jiffies over all cores; `mem` and the counters are bytes; `temp` is
°C. A band's rx is what its devices sent, tx what they got, over the radio
and the networks it carries (wl0 with wl0.1 and so on). Two radios on one
band are numbered: "6 GHz-1", "6 GHz-2".

## Trying it without installing

Nothing is written on the router:

    ssh router 'sh -s -- --json' < router/stats.sh

## Installing (only with Frank's go-ahead)

Needs SSH and "Enable JFFS custom scripts and configs" (Administration ›
System), both already on for AdGuard Home. From the repo:

    ssh router 'mkdir -p /jffs/addons/pvestats'
    scp router/stats.sh router/start.sh router:/jffs/addons/pvestats/
    ssh router 'chmod 755 /jffs/addons/pvestats/*.sh'

Then start it at boot, and every ten minutes if it has stopped, from
`/jffs/scripts/services-start` (create it with `#!/bin/sh` and `chmod 755` if
it is missing):

    /jffs/addons/pvestats/start.sh
    cru a pvestats "*/10 * * * * /jffs/addons/pvestats/start.sh"

Run those two lines once by hand to start it now, then check from the LAN:

    curl -s http://<router>:9837/

Uninstalling: remove the two lines, `cru d pvestats`, stop the socat
listening on 9837, and delete `/jffs/addons/pvestats`.
