# TrueNAS agent (pvestats-nas)

A small read-only program for TrueNAS. It serves everything PVEStats'
TrueNAS card shows as JSON over HTTP, read from the kernel and ZFS inside
TrueNAS, instead of through TrueNAS's API (middlewared) and Proxmox's. Linear
MOO-192. Not installed yet: see Installing, below.

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

## What it may do

It runs as a transient systemd service, `pvestats-nas`, started at each boot
by a TrueNAS Init/Shutdown **Command**. TrueNAS keeps the command in its own
config database, which only root can change, and runs it as root through
`sh -c`. Root runs only `systemd-run`. The program itself runs as
`pvestats`, and only root and `pvestats` can change it: `NAS` is a dataset
of its own, owned by root, and nobody else may write to it (ACL checked
2026-10-26). `pvestats`'s home inside it is mode 700. TrueNAS updates keep
the command, and the program lives on the pool, so updates leave both
alone.

- **User:** `pvestats` (uid 3001), which Frank made for this. It has no sudo
  and no admin role.
- **Capabilities:** none (`CapabilityBoundingSet=` empty, `NoNewPrivileges`).
- **Files:** the whole filesystem is read-only (`ProtectSystem=strict`), with
  a private empty `/tmp`. `/home`, `/root` and `/run/user` are hidden, and
  the kernel's tunables, modules and cgroups are read-only.
- **Devices:** it can open any device every user may open. That includes
  `/dev/zfs`, which `zpool` and `zfs` need. So `PrivateDevices` stays off,
  and so does `ProtectClock`, because it quietly closes every device except
  the clock's.
- **Network:** IPv4 and IPv6 sockets only. It listens on 10.10.10.2:9839.
- **Bounds:** 64 open files, 16 tasks, no core files. A start that fails is
  retried every 30 s, and it gives up after 10 starts within 10 minutes. 10
  starts 30 s apart fit inside that window, so it gives up after about 5
  minutes. That is long enough for the network to come up at boot, and
  limits a crash loop to a few dozen journal lines. Its own error line,
  such as "cannot listen on …", goes to the journal; nothing per request
  does.
- **Shutdown:** it is stopped before its dataset is unmounted
  (`RequiresMountsFor`). While it runs, it keeps that dataset busy. Before
  exporting the pool or locking the dataset, run
  `sudo systemctl stop pvestats-nas`.

Checked on TrueNAS, as `pvestats`, in user-level units (2026-10-26):

- `zpool list -v -H -P` and `zfs get used,available` give the same output
  under the syscall limits as without them.
- systemd accepts the unit settings: start limit, `StandardError`,
  `RequiresMountsFor`.
- The agent's `--once` answer under every limit has the same CPU, memory,
  NIC and disk figures. Pools were the only gap: ZFS hides them inside the
  user namespace that a user-level unit needs. The system service runs
  outside such a namespace.

## Installing (only with Frank's go-ahead)

1. From the repo, after `nas/build`, copy the program into pvestats's home:

       scp -i ~/.ssh/id_rsa nas/bin/pvestats-nas pvestats@10.10.10.2:
       ssh -i ~/.ssh/id_rsa pvestats@10.10.10.2 'chmod 755 ~/pvestats-nas'

2. In TrueNAS's web UI, go to System › Advanced Settings › Init/Shutdown
   Scripts › Add:
   - Description: `PVEStats agent (read-only; nas/README.md in PVEStats)`
   - Type: Command
   - When: Post Init
   - Timeout: 10
   - Command (one line):

         systemd-run --unit=pvestats-nas --uid=pvestats -p Restart=always -p RestartSec=30 -p StartLimitIntervalSec=600 -p StartLimitBurst=10 -p StandardOutput=null -p StandardError=journal -p RequiresMountsFor=/mnt/CompoundZ2FS/NAS/pvestats -p NoNewPrivileges=yes -p ProtectSystem=strict -p ProtectHome=yes -p PrivateTmp=yes -p ProtectKernelTunables=yes -p ProtectKernelModules=yes -p ProtectControlGroups=yes -p ProtectHostname=yes -p RestrictAddressFamilies="AF_INET AF_INET6" -p RestrictNamespaces=yes -p LockPersonality=yes -p MemoryDenyWriteExecute=yes -p RestrictRealtime=yes -p SystemCallArchitectures=native -p CapabilityBoundingSet= -p LimitNOFILE=64 -p TasksMax=16 -p LimitCORE=0 /mnt/CompoundZ2FS/NAS/pvestats/pvestats-nas --listen 10.10.10.2:9839

3. To start it now without a reboot, go to System › Shell and run the same
   command with `sudo` in front. systemd refuses the name while an earlier
   try is still loaded, so before running it again, run
   `sudo systemctl stop pvestats-nas; sudo systemctl reset-failed pvestats-nas`.
   For the same reason, the command does nothing when the agent is already
   running. The group is pvestats's own (`--uid` alone sets it).

4. Check it from the repo:

       curl -s http://10.10.10.2:9839/ | head -c 300

   Then put `NAS_AGENT_URL=http://10.10.10.2:9839/` in `.env` and run
   `tui/deploy`.

To update:

1. Run `nas/build`.
2. Copy the program with the same `scp`, to `~/pvestats-nas.new`.
3. `mv` it over the old one. A running program can't be overwritten in
   place.
4. In TrueNAS's shell, run `sudo systemctl restart pvestats-nas`.

If it gave up after failed starts, run `sudo systemctl reset-failed
pvestats-nas` first; until then, `systemctl status` shows it as failed.

## Removing it completely

It leaves three things on TrueNAS: the Init/Shutdown entry, the service
(gone at the next boot anyway) and the program in pvestats's home.

1. Go to System › Advanced Settings › Init/Shutdown Scripts and delete the
   "PVEStats agent" entry.
2. In System › Shell, run
   `sudo systemctl stop pvestats-nas; sudo systemctl reset-failed pvestats-nas; systemctl status pvestats-nas`.
   The last command should say the unit could not be found.
3. From the repo, run
   `ssh -i ~/.ssh/id_rsa pvestats@10.10.10.2 'rm -f ~/pvestats-nas ~/pvestats-nas.new; ls -a ~'`.
   The listing should show only dotfiles.
4. If nothing else uses the `pvestats` user, delete it in Credentials ›
   Users. That also removes the SSH key. TrueNAS may leave its home folder
   behind: check with `sudo ls -la /mnt/CompoundZ2FS/NAS/pvestats` in the
   shell, and once you've seen what is in it, remove it with
   `sudo rm -r /mnt/CompoundZ2FS/NAS/pvestats`.

Then delete the `NAS_AGENT_URL` line from `.env` and run `tui/deploy`. The
card reads TrueNAS's API every 5 s again, as it did before the agent.

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
