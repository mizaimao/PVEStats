# pvestats router agent

For an Asuswrt-Merlin router (written on a GT-BE98 Pro, firmware 3006.102).
Serves the router's own counters as JSON on its LAN address, port 9837, no
auth, so the dashboard needs no router login: Merlin has only the one admin
account. It reveals traffic totals, device counts per band and the WAN
address, nothing about the devices themselves.

- `agent/` is `pvestats-router`, a small Rust program (standard library
  only) built as one static file for the router's 64-bit ARM. It answers
  HTTP itself.
- `service.sh start|stop|status` keeps it running on the LAN address only.
- `build` tests it and builds it on the build machine from
  `tui/deploy.conf`, into `router/bin/pvestats-router` (git-ignored).

## Why a program and not a script

A shell script run per request started some 35 programs (`nvram`, `wl`,
`awk`) each time: 0.14 s of the router's CPU per request, 3% of the router
at one request a second. The program reads only kernel files per request
(`/proc/net/dev`, `/proc/stat`, `/proc/meminfo`, `/proc/uptime`, the SoC's
thermal zone, each wired port's link) and asks `nvram` and `wl` for what
changes slowly from a second thread: the WAN's state and address and the
devices on each band every 30 s, the interface layout every five minutes.
On the build machine a request takes 0.09 ms of CPU.

## Limits, so it cannot hurt the router

- Requests are answered one at a time, each given at most 3 seconds; at
  most 8 KiB of a request is read and none of it is looked at. A client
  that connects and says nothing delays the next answer by 3 seconds and no
  more.
- Each `nvram` or `wl` run is killed after 5 seconds. One that will not die
  even then stops any more from starting until it has gone, so a hung driver
  cannot pile processes up; the device counts just grow old, and
  `devices_age` says how old.
- One keeper (`service.sh`), even if started twice. Should the program ever
  exit, the keeper starts it again after 30 seconds; it sleeps the rest of
  the time. It neither starts without a LAN address nor binds to anything
  else.
- Nothing logs (the router keeps its log on flash). The program writes
  nothing; the keeper writes a process id and a lock file to `/tmp`, which is
  memory. Both use only the firmware's own tools, never Entware's on the USB
  drive.

Tested on the build machine with stand-in `nvram` and `wl`: 300 requests 50
at a time all answered, with memory (2.1 MB), threads (2) and open files (4)
unchanged; 100 KB of junk answered and closed; a `wl` hanging for 15
seconds left answers under 7 ms, never more than one stuck run, and the
counts back once it answered again.

## What it answers

```json
{"model": "GT-BE98_PRO", "uptime": 186648.38,
 "cpu": {"busy": 2975904, "total": 74262583},
 "mem": {"total": 2097111040, "available": 891924480},
 "temp": {"cpu": 69.7},
 "wan": {"up": true, "ip": "…", "rx": 65611232984, "tx": 25224388291},
 "ports": [{"name": "eth0", "up": true, "mbps": 10000, "rx": 99486587416, "tx": 169834333430}],
 "bands": [{"name": "5 GHz", "on": true, "devices": 1, "rx": 3663050058, "tx": 9191432552}],
 "devices_age": 12}
```

`cpu` is jiffies over all cores and the byte counters are totals since boot;
the dashboard turns both into rates. `mem` is bytes, `temp` °C (the SoC's
own sensor; the radios' sensors are left alone, since reading them asks each
chip directly). A band's rx is what its devices sent, tx what they got, over
the radio and the networks it carries (`wl0` with `wl0.1` and so on). Two
radios on one band are numbered: "6 GHz-1", "6 GHz-2". `devices_age` is
seconds since the devices were counted, null before the first count.

## Building

    router/build

The build machine needs the router's target once:
`rustup target add aarch64-unknown-linux-musl`. Linking uses the lld that
ships with Rust, so no cross compiler is needed.

## Installing (only with Frank's go-ahead)

Needs SSH and "Enable JFFS custom scripts and configs" (Administration ›
System), both already on for AdGuard Home. From the repo, after
`router/build`:

    ssh router 'mkdir -p /jffs/addons/pvestats'
    ssh router 'cat > /jffs/addons/pvestats/pvestats-router' < router/bin/pvestats-router
    ssh router 'cat > /jffs/addons/pvestats/service.sh' < router/service.sh
    ssh router 'chmod 755 /jffs/addons/pvestats/pvestats-router /jffs/addons/pvestats/service.sh'

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
