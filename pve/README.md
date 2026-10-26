# Proxmox host agent (pvestats-pve)

A small read-only program for the Proxmox VE host. It serves the host's and
each running VM's counters as JSON over HTTP, read straight from the kernel
instead of through Proxmox's API (`pveproxy`). Installed on the host on
2026-10-26 with Frank's go-ahead (Linear MOO-191), the one thing PVEStats
installs there. The terminal dashboard polls it every 2 s
(`tui: pve_agent_every` in config.yaml) when `PVE_AGENT_URL` is set in
`.env`, and asks PVE's API every 30 s for the rest.

## What it reads

Only files any user may read, so it runs as an unprivileged user:

- the host: `/proc/stat` (CPU, cores), `/proc/meminfo`, `/proc/loadavg`,
  `/proc/uptime`;
- each running VM, from its cgroup (`/sys/fs/cgroup/qemu.slice/<vmid>.scope`):
  CPU time of every process in it (`cpu.stat`, `usage_usec`: QEMU, its vhost
  workers, swtpm if any), everything charged to it (`memory.current`: guest
  memory QEMU has touched, page cache, the kernel's pages for it; not
  swapped-out pages or hugepages), and its QEMU process's start (the process
  named `kvm`, for its uptime);
- each VM's network devices (`tap<vmid>i<n>`), counted as Proxmox counts
  netin (what the host transmits into the tap) and netout (what it receives).
  A removed device takes its bytes with it, so a fall is a reset.

It runs no programs, logs nothing, writes nothing and asks no VM anything.
QEMU's control socket, which Proxmox uses for backups and migrations, is
left alone, so two figures differ from Proxmox's:

- no disk counters for VMs: Proxmox reads them over that socket;
- a VM's memory is what it holds of the host's, not what its guest uses
  (Proxmox's figure comes from the balloon driver, over the same socket).

VM names, vCPU counts (Proxmox's CPU figure is per vCPU), memory limits, IP
addresses and disk usage stay on the API: VM configs are readable by root and
www-data only.

There is no authentication, as with the router's agent: anyone on the LAN can
read these counters, and four clients that keep asking can hold every slot,
which denies the agent to others but does not load the host.

## Building

`pve/build` tests it and builds it on the build machine (Debian 13, the same
glibc as Proxmox VE 9), into `pve/bin/pvestats-pve` (git-ignored).

## What it may do (pvestats-pve.service)

- **User:** `DynamicUser=yes`: a system user (`pvestats-pve`) that systemd
  makes when the service starts and drops when it stops. No password, no
  login, no home, no groups, not root.
- **Capabilities:** none (`CapabilityBoundingSet=` empty,
  `NoNewPrivileges`).
- **Files:** the whole filesystem read-only (`ProtectSystem=strict`), a
  private empty `/tmp`, no home directories, no devices. It reads only
  world-readable files (above); VM configs and QEMU's sockets are root's
  and stay out of reach.
- **Network:** IPv4 and IPv6 sockets only; listens on 10.10.10.100:9838.
- **Bounds:** 64 open files, 16 tasks, no core files; gives up after 10
  failed starts in 5 minutes. Nothing per request reaches the journal.

## Installing (done 2026-10-26; only with Frank's go-ahead)

From the repo, after `pve/build`, as root on the host:

    scp pve/bin/pvestats-pve pve/pvestats-pve.service root@pve.lan:/tmp/
    ssh root@pve.lan 'install -m 755 -o root -g root /tmp/pvestats-pve /usr/local/bin/pvestats-pve
        install -m 644 -o root -g root /tmp/pvestats-pve.service /etc/systemd/system/pvestats-pve.service
        rm /tmp/pvestats-pve /tmp/pvestats-pve.service
        systemd-analyze verify /etc/systemd/system/pvestats-pve.service
        systemctl daemon-reload && systemctl start pvestats-pve && systemctl enable pvestats-pve'
    curl -s http://10.10.10.100:9838/ | head -c 300

Then `PVE_AGENT_URL=http://10.10.10.100:9838/` in `.env`, and `tui/deploy`.
To update: `pve/build`, the same `install` of the program (never `cp` over
the running one), `systemctl restart pvestats-pve`.

## Removing it completely

Everything it put on the host is the program, its unit and the unit's boot
link; its user exists only while it runs and keeps no files.

    ssh root@pve.lan 'systemctl disable --now pvestats-pve
        rm -f /etc/systemd/system/pvestats-pve.service /usr/local/bin/pvestats-pve
        systemctl daemon-reload
        systemctl status pvestats-pve 2>&1 | head -1; ls /usr/local/bin/pvestats-pve 2>&1'

The last line should say the unit and the program are gone. Then delete the
`PVE_AGENT_URL` line from `.env` and run `tui/deploy`: the dashboard asks
PVE's API every 10 s again, as before the agent.

## A test run (only with Frank's go-ahead)

As root on the host, from `/tmp`, as `nobody`, then removed:

```
setpriv --reuid=65534 --regid=65534 --clear-groups --no-new-privs \
    /tmp/pvestats-pve --listen 10.10.10.100:9838 &
curl -s http://10.10.10.100:9838/
kill %1; rm /tmp/pvestats-pve
```

The server is the router agent's (`router/agent`): one kept connection per
client, at most 4 clients, answers at least 100 ms apart, idle connections
closed after 30 s.

## Measured against the API (2026-10-26)

A test run on the host, as above, polled from the build machine over one
kept connection each, as a dashboard polls:

| | API (`cluster/resources` + `nodes/…/status`) | agent |
|---|---|---|
| host CPU per poll | 4.4 ms (2.2 ms a request) | 0.67 ms |
| at one poll per 10 s, as now | 0.04% of one core | 0.007% |
| at one poll a second | 0.44% of one core | 0.07% |
| figures change | every 10 s (pvestatd) | at every poll |
| memory | (pveproxy, running anyway) | 2.2 MB |

Counted from the CPU time of pveproxy and pvedaemon (with their finished
workers) less a baseline of the same length, and the agent's own. Both
agreed on the figures: VM CPU within the noise of their different windows,
network counters to the megabyte (the API up to 10 s behind), host memory
exactly. On a 24-thread host both costs are negligible; the agent's gain is
figures every second, not CPU.

## The answer

```
{"uptime":288012.45,"cpus":24,"cpu":{"busy":…,"total":…},
 "mem":{"total":…,"available":…},"load":[0.52,0.58,0.59],"guests":[
{"vmid":100,"cpu_usec":…,"mem":…,"netin":…,"netout":…,"uptime":…},
…]}
```

Counters are totals: a client makes rates from one reading to the next.
`cpu` is jiffies over all cores; `cpu_usec` is microseconds.
