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

- Each client keeps one connection and asks over it again and again (HTTP/1.1
  keep-alive), so the dashboard's poll every second is one entry in the
  router's connection table. The first version closed the connection after
  each answer, and every closed connection stays in that table for about
  two minutes: the infrastructure agent found 116 from thinkpad on
  2026-10-26. At most 4 clients at once (a fifth is closed unread); a
  connection with no request for 30 seconds is closed.
- Answers are at least 100 ms apart across all clients, a request's head
  gets at most 3 seconds once it has begun, at most 8 KiB of it is read and
  none of it is looked at. A client asking in a tight loop gets ten answers
  a second.
- Per request it reads no file the kernel locks its network stack for: the
  ports' link speeds are read every 30 s, with the devices.
- Each `nvram` or `wl` run is killed after 5 seconds. One that will not die
  even then stops any more from starting until it has gone, so a hung driver
  cannot pile processes up; the device counts just grow old, and
  `devices_age` says how old. Each band keeps its own count, so one network
  that will not answer does not hold back the rest; a radio that is off is
  not asked.
- One keeper (`service.sh`), even if started twice. Should the program ever
  exit, the keeper starts it again after 30 seconds; it sleeps the rest of
  the time. It neither starts without a LAN address nor binds to anything
  else, and should the LAN address change, the program exits for the keeper
  to start it on the new one.
- Nothing logs (the router keeps its log on flash). The program writes
  nothing, and should it ever crash it leaves no core file: the keeper turns
  them off and runs from `/tmp`, which is memory, where it also keeps a
  process id and a lock file. Both use only the firmware's own tools, never
  Entware's on the USB drive.

Tested on the build machine with stand-in `nvram` and `wl`: 300 requests 50
at a time all answered, with memory (2.2 MB), threads (2) and open files (4)
unchanged; a client in a tight loop got 49 answers in 5 s for 10 ms of CPU;
junk and byte-at-a-time heads answered and closed; a `wl` hanging for a
round left answers within the 100 ms gap, never more than one stuck run,
and the counts back once it answered again; a changed LAN address made it
exit; a failing guest network held back only its own band.

## What it answers

```json
{"model": "GT-BE98_PRO", "uptime": 186648.38,
 "cpu": {"busy": 2975904, "total": 74262583},
 "mem": {"total": 2097111040, "available": 891924480},
 "temp": {"cpu": 69.7},
 "wan": {"up": true, "ip": "…", "rx": 65611232984, "tx": 25224388291},
 "ports": [{"name": "eth1", "up": true, "mbps": 10000, "wan": true, "rx": 82099808698, "tx": 67885525746}],
 "wired_devices": 8,
 "bands": [{"name": "5 GHz", "on": true, "devices": 1, "rx": 3663050058, "tx": 9191432552}],
 "devices_age": 12}
```

`cpu` is jiffies over all cores and the byte counters are totals since boot;
the dashboard turns both into rates. `mem` is bytes, `temp` °C (the SoC's
own sensor; the radios' sensors are left alone, since reading them asks each
chip directly). A band's rx is what its devices sent, tx what they got, over
the radio and the networks it carries (`wl0` with `wl0.1` and so on). Two
radios on one band are numbered: "6 GHz-1", "6 GHz-2". A band's `devices`
is null until it is first counted. `devices_age` is seconds since the
oldest band's count, null before any.

A port's `wan` is true for the one the internet comes in on. On Frank's
router the 10G port is set as WAN and carries it as a VLAN (vlan4094 on
eth1), so it is listed among the LAN ports; every download comes in on it
and goes out on another, and the dashboard leaves it out of its wired
total so as not to count internet traffic twice. Found by downloading 50 MB
on dev.lan: eth1 received 53 MB, eth0 sent 52 MB.

`wired_devices` is every device the router has heard from lately on its
LAN bridge, in its neighbour table (/proc/net/arp) or learned by the bridge
on a wired port (/sys/class/net/br0/brforward), less those on Wi-Fi (in a
radio's client list or learned on a Wi-Fi port) and those beyond the
internet port. Counted every 30 s from those two file reads; null until the
first count. Each VM on the PVE host counts, though they all come in on one
port. The router's system sees four wired ports (eth0 to eth3); a band's
`devices` counts each device once, however many of its networks list it.

## Building

    router/build

The build machine needs the router's target once:
`rustup target add aarch64-unknown-linux-musl`. Linking uses the lld that
ships with Rust, so no cross compiler is needed.

## Installing (only with Frank's go-ahead)

Installed on Frank's router on 2026-10-26, from these steps; the dashboard
polls it once a second (measured there: 1.2 ms of the router's CPU a
request, 0.05% of the router at that pace, 0.4 MB of memory). Every SSH
login writes three lines to the router's log, which is on flash, so keep
them few. To update it: `router/build`, then `service.sh stop`, copy the
program over, `service.sh start`.

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

## Removing it completely

Everything it put on the router: `/jffs/addons/pvestats/` (the program and
`service.sh`) and its one line in `/jffs/scripts/services-start`. It runs as
the admin account, the only one Merlin has, so no user is left behind. One
login:

    ssh router '/jffs/addons/pvestats/service.sh stop; sed -i "/pvestats/d" /jffs/scripts/services-start;
        rm -r /jffs/addons/pvestats; ls /jffs/addons/pvestats 2>&1; grep -c pvestats /jffs/scripts/services-start'

The last two lines should say the folder is gone and count 0. What it left
in `/tmp` (its lock and pid files) goes at the next reboot. Then empty
`ROUTER_AGENT_URL` in `.env` and run `tui/deploy`: the router card goes.
