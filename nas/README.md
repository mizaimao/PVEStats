# TrueNAS agent (pvestats-nas)

A small read-only program for TrueNAS. It serves everything PVEStats'
TrueNAS card shows as JSON over HTTP, read from the kernel and ZFS inside
TrueNAS, instead of through TrueNAS's API (middlewared) and Proxmox's. Linear
MOO-192. Not installed yet.

## Why

Measured on TrueNAS on 2026-10-26 (2 min with only the kiosk polling, then 2
min with five extra collectors polling as the dashboards do): middlewared
spends about **25 ms of CPU per API request**, about **0.68% of one core per
dashboard** at one poll every 5 s. The kiosk alone accounted for nearly all
of middlewared's CPU. Proxmox's API, by comparison, costs 2.5 ms a request.

## What it reads

Per request, only files any user may read:

- `/proc/stat` (CPU), `/proc/meminfo` and ZFS's `arcstats` (memory, and the
  ARC, ZFS's cache, which Linux counts as used: 11.3 of 16 GiB on 2026-10-26),
  `/proc/uptime`;
- `/proc/net/dev` for each network device with a device behind it (a NIC,
  real or virtio; not bridges, docker0 or veths);
- `/sys/block/<disk>/stat` for each disk's bytes read and written;
- each pool's state from ZFS's kstat files (`/proc/spl/kstat/zfs/<pool>/state`).

Every 30 s, from a thread of its own: each pool's capacity from `zfs get
used,available` (its root dataset: what the card shows as "used of
usable"), TrueNAS's LAN address (a UDP socket connected towards the gateway,
which sends nothing), and which pool TrueNAS runs from (the one mounted at
`/`). Every 5 minutes: which disk is in which pool (`zpool list -v -H -P`),
each disk's model and size. `/dev/zfs` is open to every user on TrueNAS, so
`zpool` and `zfs` answer an ordinary user. Each run is killed after 5 s, and
one that will not die stops any more from starting.

Nothing is logged and nothing written. The server is the router agent's: one
kept connection per client, at most 4 clients, answers at least 100 ms
apart, idle connections closed after 30 s.

## Building

`nas/build` tests it and builds it on the build machine, glibc linked in:
TrueNAS 25.10 is Debian 12 (glibc 2.36), and the build machine's Debian 13
would otherwise ask for glibc 2.39. Linked in, it runs whatever TrueNAS's
glibc. Output: `nas/bin/pvestats-nas` (git-ignored).

## A test run (only with Frank's go-ahead)

As the `pvestats` user Frank made (no sudo, no admin role; home on the pool,
`/mnt/CompoundZ2FS/NAS/pvestats`, which TrueNAS updates leave alone):

    scp -i ~/.ssh/id_rsa nas/bin/pvestats-nas pvestats@10.10.10.2:
    ssh -i ~/.ssh/id_rsa pvestats@10.10.10.2 'nohup ./pvestats-nas --listen 10.10.10.2:9839 >/dev/null 2>&1 & echo $!'
    curl -s http://10.10.10.2:9839/

then `kill` that pid and `rm ~/pvestats-nas`.

## Measured (test run, 2026-10-26)

Run by hand as `pvestats` for about 10 minutes, then stopped and deleted.

It gives the same figures as the kiosk's card, which reads TrueNAS's API, over the same
seconds:

- CompoundZ2FS: 21.89 TB used and 8.97 TB free, which is 71% of 28.1 TiB, as the card shows.
- Disks sdb to sdf: the card's sizes, and no reads or writes over 10 s, as the card shows.
  sda belongs to the boot pool, which the card also leaves off.
- ens18: 0.5 KB/s in and 1.9 KB/s out. The card shows 0 KiB/s each.
- Address 10.10.10.2 and up 3 d 18 h, as the card shows.
- CPU: 0.4%. The card shows 1%, because Proxmox counts the VM's CPU from outside,
  including QEMU's own work.
- Memory: 1.1 GB available, and 13.0 GB held by the ARC, which Linux counts as used.
  Proxmox's 14.6 GiB counts the ARC too.

Its cost comes from its process counters: its own CPU, plus its `zfs`/`zpool` runs.

| | CPU | |
|---|---|---|
| no requests, 120 s (its 30 s `zfs get` only) | 20 ms | 0.017% of one core |
| 600 requests at 10 a second | 0.41 ms a request | |
| one dashboard polling every 2 s | | about 0.04% of one core |
| TrueNAS's API, one dashboard every 5 s | 25 ms a request | 0.68% of one core |

That is about 60 times less per request. A dashboard polling it every 2 s costs
about a seventeenth of one polling the API every 5 s. It uses 0.8 MB of memory.

## The answer

```
{"uptime":…,"cpus":4,"cpu":{"busy":…,"total":…},
 "mem":{"total":…,"available":…,"arc":…},"ip":"10.10.10.2",
 "nics":[{"name":"ens18","driver":"virtio_net","rx":…,"tx":…}],
 "disks":[{"name":"sdb","model":"ST16000NE000-2RW","size":…,"pool":"CompoundZ2FS","read":…,"write":…},
 …],
 "pools":[{"name":"CompoundZ2FS","state":"ONLINE","used":…,"available":…,"age":12,"boot":false},…]}
```

Counters are totals since boot (disks in bytes); the client makes rates.
`boot` marks the pool TrueNAS runs from, so a client can leave it, and its
disk, off the card. A pool's `age` is the seconds since `zfs get` last gave
its capacity: a run that hangs or fails on that pool keeps the last figures,
and says how old they are. `zpool` and `zfs` are run by full path
(`/usr/sbin/`), whatever PATH the agent starts with.
