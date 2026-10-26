//! The cards, after the app's (static/js/cards.js): the Proxmox host, the
//! GPU guest, the NAS guest, and one small card per other guest.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::draw::{bar, Big, Card, Chart, Col, Meter, Mirror, Part, Table, Trace, Upright};
use super::theme::Look;
use crate::config::{Host, View};
use crate::format::{self as fmt, MIB, TIB};
use crate::sources::{AgentData, Guest, NasData, Nic, Node, PveData};
use crate::state::{Source, Status};

/// One guest's place on screen. `guest` is None for the GPU and NAS cards
/// until PVE first answers: they have sources of their own and must not
/// wait for PVE's (spec §2, §4.2).
pub struct Slot<'a> {
    pub vmid: u64,
    pub name: String,
    pub view: View,
    pub guest: Option<&'a Guest>,
}

fn style(color: Color) -> Style {
    Style::default().fg(color)
}

fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// "label value", as the stat chips and tiles print them.
fn chip(look: &Look, label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label} "), style(look.p.muted)),
        Span::styled(value, bold(look.p.ink)),
    ])
}

fn sub(look: &Look, parts: Vec<String>) -> Part {
    let mut spans = Vec::new();
    for (i, part) in parts.into_iter().filter(|p| !p.is_empty()).enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", style(look.p.muted)));
        }
        spans.push(Span::styled(part, style(look.p.ink2)));
    }
    Part::Text(Line::from(spans))
}

fn ratio(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    Some(used? / total.filter(|t| *t > 0.0)?)
}

fn meter(look: &Look, label: &str, ratio: Option<f64>, value: String, value_w: u16) -> Part {
    Part::Meter(Meter {
        label: label.to_string(),
        label_w: 4,
        ratio,
        color: look.p.s1,
        value,
        value_w,
    })
}

fn mem_text(used: Option<f64>, total: Option<f64>) -> String {
    format!("{} / {} GiB", fmt::gib(used), fmt::gib(total))
}

/// Status colour for a ratio against a limit. Colour is never the only
/// channel: the figure is always printed beside it.
fn severity(look: &Look, ratio: Option<f64>, normal: Color) -> Color {
    match ratio {
        Some(r) if r >= 0.9 => look.p.critical,
        Some(r) if r >= 0.8 => look.p.warning,
        _ => normal,
    }
}

fn axis_pct(v: f64) -> String {
    format!("{}", v.round() as i64)
}

fn guest_sub(look: &Look, guest: Option<&Guest>) -> Part {
    let Some(g) = guest else { return sub(look, vec![fmt::DASH.into()]) };
    sub(
        look,
        vec![g.addresses.first().cloned().unwrap_or_default(), format!("up {}", fmt::uptime(g.uptime))],
    )
}

/// Net in/out from PVE's counters, and in-guest disk usage.
fn io_line(look: &Look, guest: Option<&Guest>, disk_io: bool) -> Part {
    let (muted, ink) = (style(look.p.muted), style(look.p.ink));
    let mut spans = vec![Span::styled("net ↓", muted)];
    spans.push(Span::styled(fmt::rate(guest.and_then(|g| g.net_in_bps)), ink));
    spans.push(Span::styled(" ↑", muted));
    spans.push(Span::styled(fmt::rate(guest.and_then(|g| g.net_out_bps)), ink));
    // Storage passed through to a guest bypasses the hypervisor, so its
    // counters sit near zero however busy the disk is: no figures at all
    // rather than ones that mean nothing.
    if disk_io && guest.is_some_and(|g| g.disk_io_visible) {
        spans.push(Span::styled("  io r", muted));
        spans.push(Span::styled(fmt::rate(guest.and_then(|g| g.disk_read_bps)), ink));
        spans.push(Span::styled(" w", muted));
        spans.push(Span::styled(fmt::rate(guest.and_then(|g| g.disk_write_bps)), ink));
    }
    spans.push(Span::styled(" MiB/s", muted));
    Part::Text(Line::from(spans))
}

fn disk_line(look: &Look, guest: Option<&Guest>) -> Part {
    let text = match guest {
        // A card PVE has not reported yet: no data, not no agent.
        None => fmt::DASH.to_string(),
        Some(Guest { filesystem: None, .. }) => "no guest agent".to_string(),
        Some(Guest { filesystem: Some(fs), .. }) => {
            let pct = fmt::pct(ratio(fs.used, Some(fs.total)).map(|r| r * 100.0));
            // TiB past one, so ml60's 3.4 TiB does not read "1616.7 / 3479.3".
            if fs.total >= TIB {
                let tib = |v: f64| format!("{:.1}", v / TIB);
                format!("{pct} · {} / {} TiB", fs.used.map_or(fmt::DASH.into(), tib), tib(fs.total))
            } else {
                format!("{pct} · {} / {} GiB", fmt::gib(fs.used), fmt::gib(Some(fs.total)))
            }
        }
    };
    Part::Text(chip(look, "disk", text))
}

// ------------------------------------------------------------ PVE host

pub fn host(look: &Look, node: &Node, pve: &Source<PveData>, status: &Status) -> Card {
    // Trim pve-manager's build hash: it is noise at a glance.
    let version = node.pveversion.as_deref().unwrap_or("").split('/').take(2).collect::<Vec<_>>().join(" ");
    // The node leads: the card shares its title with the status line.
    let mut info = vec![node.name.clone()];
    info.push(format!("up {}", fmt::uptime(node.uptime)));
    info.push(format!("guests {} / {}", node.guests_running, node.guests_total));
    if let Some(threads) = node.cpus {
        info.push(format!("{threads} threads"));
    }
    let load = node.loadavg.iter().map(|v| format!("{v:.1}")).collect::<Vec<_>>().join(" / ");
    Card {
        title: format!("Proxmox host · {}", node.name),
        right: version,
        stale: !status.ok,
        error: status.error.clone(),
        body: vec![
            sub(look, info),
            Part::Flow(
                30,
                vec![
                    meter(look, "CPU", node.cpu_pct.map(|v| v / 100.0), fmt::pct(node.cpu_pct), 4),
                    meter(look, "Mem", ratio(node.mem, node.maxmem), mem_text(node.mem, node.maxmem), 0),
                ],
            ),
            Part::Chart(Chart {
                traces: vec![Trace::from(pve.series("node.cpu"), 0, "")],
                capacity: pve.capacity(),
                top: Some(100.0),
                floor: 0.0,
                axis: axis_pct,
                seconds_per_sample: pve.interval,
                // The load rides on the caption: a line of its own cost the guests a row.
                caption: format!("Host CPU {} · load {}", fmt::pct(node.cpu_pct), if load.is_empty() { fmt::DASH.into() } else { load }),
                min_rows: 3,
                max_rows: 4,
                weight: 1,
            }),
        ],
    }
}

/// Trial versions of the host card with CPU and memory on one chart and no
/// bars, shown in the right-hand column until one is picked. Each is as tall
/// as it would be in the first column.
pub fn host_variants(look: &Look, node: &Node, pve: &Source<PveData>, status: &Status) -> Vec<Card> {
    let p = look.p;
    let cpu = node.cpu_pct;
    let mem_ratio = ratio(node.mem, node.maxmem);
    let mem_pct = fmt::pct(mem_ratio.map(|r| r * 100.0));
    let gib = mem_text(node.mem, node.maxmem);
    let load = node.loadavg.iter().map(|v| format!("{v:.1}")).collect::<Vec<_>>().join(" ");
    let info = || {
        sub(
            look,
            vec![
                node.name.clone(),
                format!("up {}", fmt::uptime(node.uptime)),
                format!("guests {} / {}", node.guests_running, node.guests_total),
                format!("load {load}"),
            ],
        )
    };
    let chart = |cpu_label: String, mem_label: String, caption: String| {
        Part::Chart(Chart {
            traces: vec![
                Trace::from(pve.series("node.cpu"), 0, &cpu_label),
                Trace::from(pve.series("node.mem_pct"), 1, &mem_label),
            ],
            capacity: pve.capacity(),
            top: Some(100.0),
            floor: 0.0,
            axis: axis_pct,
            seconds_per_sample: pve.interval,
            caption,
            min_rows: 5,
            max_rows: 5,
            weight: 1,
        })
    };
    let card = |name: &str, body: Vec<Part>| Card {
        title: format!("Host, variant {name}"),
        right: node.name.clone(),
        stale: !status.ok,
        error: status.error.clone(),
        body,
    };
    let figure = |label: &str, value: String| {
        Part::Text(Line::from(vec![
            Span::styled(fmt::left(label, 7), style(p.muted)),
            Span::styled(value, bold(p.ink)),
        ]))
    };
    vec![
        // A: the figures ride in the legend, the GiB in the caption.
        card("A", vec![
            info(),
            chart(format!("CPU {}", fmt::pct(cpu)), format!("mem {mem_pct}"), gib.clone()),
        ]),
        // B: the chart, and the figures in a panel at its side.
        card("B", vec![
            info(),
            Part::Row(vec![
                (0, chart("CPU".into(), "mem".into(), String::new())),
                (
                    18,
                    Part::Stack(vec![
                        figure("CPU", fmt::pct(cpu)),
                        figure("Memory", mem_pct.clone()),
                        Part::Text(Line::styled(gib.clone(), style(p.ink2))),
                        Part::Text(Line::styled(format!("load {load}"), style(p.muted))),
                    ]),
                ),
            ]),
        ]),
        // C: the chart, and memory as an upright gauge at its side.
        card("C", vec![
            info(),
            Part::Row(vec![
                (0, chart(format!("CPU {}", fmt::pct(cpu)), "mem".into(), gib.clone())),
                (
                    7,
                    Part::Upright(Upright {
                        label: "Mem".into(),
                        ratio: mem_ratio,
                        color: severity(look, mem_ratio, p.s2),
                        value: mem_pct.clone(),
                    }),
                ),
            ]),
        ]),
        // D: btop's network style, CPU rising and memory hanging.
        card("D", vec![
            info(),
            Part::Mirror(Mirror {
                up: Trace::from(pve.series("node.cpu"), 0, ""),
                down: Trace::from(pve.series("node.mem_pct"), 1, ""),
                up_label: Line::styled("CPU", style(look.flat(0))),
                down_label: Line::styled("Memory", style(look.flat(1))),
                up_value: fmt::pct(cpu),
                down_value: format!("{mem_pct} · {gib}"),
                label_w: 8,
                value_w: 24,
                capacity: pve.capacity(),
                floor: 0.0,
                scale: Some(100.0),
                format: |v| fmt::pct(Some(v)),
                rows: 5,
            }),
        ]),
    ]
}

/// While PVE has given no data the host card cannot exist, so a note in
/// its place says why (spec §2: show the error, do not blank the screen).
pub fn pve_note(look: &Look, status: &Status) -> Card {
    let text = match &status.error {
        Some(e) => format!("Proxmox VE has not answered: {e}"),
        None => "Waiting for Proxmox VE's first answer…".to_string(),
    };
    Card {
        title: "Proxmox VE".into(),
        right: String::new(),
        stale: false,
        error: None,
        body: vec![Part::Text(Line::styled(text, style(look.p.warning)))],
    }
}

// ----------------------------------------------------------------- GPU

pub fn gpu(look: &Look, slot: &Slot, agent: &Source<AgentData>, status: &Status, pve: &Status) -> Card {
    let p = look.p;
    let data = agent.data.as_ref();
    let gpu = data.and_then(|d| d.gpu.as_ref());
    let get = |f: fn(&crate::sources::Gpu) -> Option<f64>| gpu.and_then(f);
    let round = |v: Option<f64>, unit: &str| v.map_or(fmt::DASH.into(), |v| format!("{}{unit}", v.round() as i64));

    let util = get(|g| g.util);
    let digits = Big {
        value: util.map_or(fmt::DASH.to_string(), |u| format!("{}", u.round() as i64)),
        unit: "%".into(),
        slots: 3,
        // Three rows, picked over five on the screen itself; the console keeps
        // its five-row blocks.
        small: !look.console_glyphs,
        style: bold(p.s1),
    };
    // VRAM filling up is the failure that actually bites on this box: a model
    // that no longer fits gets evicted or spills to host memory.
    let vram_ratio = ratio(get(|g| g.vram_used), get(|g| g.vram_total));
    let vram = match gpu {
        Some(g) => mem_text(g.vram_used, g.vram_total),
        None => fmt::DASH.into(),
    };
    // One decimal: the link idles well under 1 MiB/s.
    let pcie_rate = gpu
        .and_then(|g| Some(format!("{:.1} / {:.1}", g.pcie.rx_kbps? / 1024.0, g.pcie.tx_kbps? / 1024.0)))
        .unwrap_or_else(|| fmt::DASH.into());

    let headline = Part::Row(vec![
        (digits.width(look), Part::Big(digits)),
        (
            0,
            Part::Stack(vec![
                Part::Text(Line::styled("GPU utilization", style(p.muted))),
                Part::Gap,
                Part::Flow(
                    16,
                    vec![
                        Part::Tile("VRAM in use".into(), Line::styled(vram, bold(severity(look, vram_ratio, p.ink)))),
                        Part::Tile("PCIe rx/tx MiB/s".into(), Line::styled(pcie_rate, bold(p.ink))),
                    ],
                ),
            ]),
        ),
    ]);

    // Utilization and VRAM share a 0-100 scale, so one plot, nvtop style.
    let chart = Part::Chart(Chart {
        traces: vec![
            Trace::from(agent.series("gpu.util"), 0, "GPU utilization"),
            Trace::from(agent.series("gpu.vram_pct"), 1, "VRAM used"),
        ],
        capacity: agent.capacity(),
        top: Some(100.0),
        floor: 0.0,
        axis: axis_pct,
        seconds_per_sample: agent.interval,
        caption: String::new(),
        min_rows: 5,
        max_rows: 11,
        weight: 3,
    });

    let power = match (get(|g| g.power), get(|g| g.power_limit)) {
        // Draw against the cap (spec §4.2), so throttling at the limit shows.
        (Some(w), Some(cap)) => format!("{} / {} W", w.round() as i64, cap.round() as i64),
        (w, _) => round(w, " W"),
    };
    // NVIDIA parks the link at Gen1 when idle and renegotiates under load,
    // so a low current generation is normal; the ceiling rides beside it.
    let link = gpu.and_then(|g| {
        let mut text = format!("{}.0 x{}", g.pcie.gen?, g.pcie.width?);
        if let Some(max) = g.pcie.max_gen {
            text.push_str(&format!(" · max {max}.0"));
        }
        Some(text)
    });
    let chips = Part::Chips(vec![
        chip(look, "Temp", round(get(|g| g.temp), "°C")),
        chip(look, "Power", power),
        chip(look, "Fan", fmt::pct(get(|g| g.fan))),
        chip(look, "SM clock", get(|g| g.clock_sm).map_or(fmt::DASH.into(), |c| format!("{} MHz", fmt::int(Some(c))))),
        chip(look, "PCIe", link.unwrap_or_else(|| fmt::DASH.into())),
    ]);

    // The guest's CPU and memory are PVE's, at its 10s pace. When PVE is not
    // answering they keep its last values, greyed, rather than looking live.
    let guest = slot.guest;
    let mut pve_parts = Part::Stack(vec![
        Part::Flow(
            30,
            vec![
                meter(look, "CPU", guest.and_then(|g| g.cpu_pct).map(|v| v / 100.0), fmt::pct(guest.and_then(|g| g.cpu_pct)), 4),
                meter(look, "Mem", guest.and_then(|g| ratio(g.mem, g.maxmem)), mem_text(guest.and_then(|g| g.mem), guest.and_then(|g| g.maxmem)), 0),
            ],
        ),
        // ml60's storage is passed through, so the hypervisor never sees its
        // disk traffic: the panel would only ever read zero.
        Part::Flow(24, vec![io_line(look, guest, false), disk_line(look, guest)]),
    ]);
    if !pve.ok {
        pve_parts = Part::Dim(Box::new(pve_parts));
    }

    let models: Vec<Vec<Line>> = data
        .map(|d| d.models.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|m| {
            // A model that spilled to system RAM is an order of magnitude
            // slower, and GPU utilization alone will not say so.
            let processor = m.processor.clone().unwrap_or_else(|| "unknown".into());
            vec![
                Line::styled(m.name.clone(), style(p.ink)),
                Line::styled(processor, style(if m.offloaded { p.warning } else { p.ink2 })),
                Line::styled(format!("{} GiB", fmt::gib(m.size_vram)), style(p.ink)),
                Line::styled(fmt::countdown(m.unload_in), style(p.ink2)),
            ]
        })
        .collect();
    let procs: Vec<Vec<Line>> = gpu
        .map(|g| g.procs.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|proc| {
            vec![
                Line::styled(proc.name.clone(), style(p.ink)),
                Line::styled(proc.pid.map_or(fmt::DASH.into(), |p| p.to_string()), style(p.ink2)),
                Line::styled(
                    proc.vram.map_or(fmt::DASH.into(), |v| format!("{} GiB", fmt::gib(Some(v)))),
                    style(p.ink),
                ),
            ]
        })
        .collect();

    Card {
        title: slot.name.clone(),
        right: format!("vmid {} · GPU", slot.vmid),
        // Stale styling follows the agent, since that is what this card
        // leads on.
        stale: !status.ok,
        error: status.error.clone(),
        body: vec![
            guest_sub(look, guest),
            Part::Gap,
            headline,
            Part::Gap,
            chart,
            Part::Gap,
            chips,
            Part::Gap,
            pve_parts,
            Part::Gap,
            Part::Table(Table {
                caption: None,
                cols: vec![
                    Col { title: "Loaded model", width: 0, right: false },
                    Col { title: "Processor", width: 15, right: false },
                    Col { title: "VRAM", width: 9, right: true },
                    Col { title: "Unloads in", width: 10, right: true },
                ],
                rows: models,
                empty: "no models loaded",
            }),
            Part::Gap,
            Part::Table(Table {
                caption: None,
                cols: vec![
                    Col { title: "GPU process", width: 0, right: false },
                    Col { title: "PID", width: 7, right: true },
                    Col { title: "VRAM", width: 9, right: true },
                ],
                rows: procs,
                empty: "no processes on the GPU",
            }),
        ],
    }
}

// ----------------------------------------------------------------- NAS

/// The role as the app words it, unless config.yaml names the interface.
fn nic_role(name: &str, nic: &Nic, host: Option<&Host>) -> String {
    if let Some(label) = host.and_then(|h| h.nic_labels.get(name)) {
        return label.clone();
    }
    match nic.role.as_deref() {
        Some("virtual") => "virtual (virtio)".into(),
        Some("physical") => {
            let media = if nic.media.is_empty() { "passthrough" } else { &nic.media };
            format!("physical · {media}")
        }
        _ => "unknown".into(),
    }
}

pub fn nas(look: &Look, slot: &Slot, host: Option<&Host>, nas: &Source<NasData>, status: &Status, pve: &Status) -> Card {
    let p = look.p;
    let guest = slot.guest;
    let data = nas.data.as_ref();
    let interfaces = data.map_or(&[][..], |d| d.interfaces.as_slice());

    let mut usage = Part::Flow(
        30,
        vec![
            meter(look, "CPU", guest.and_then(|g| g.cpu_pct).map(|v| v / 100.0), fmt::pct(guest.and_then(|g| g.cpu_pct)), 4),
            meter(look, "Mem", guest.and_then(|g| ratio(g.mem, g.maxmem)), mem_text(guest.and_then(|g| g.mem), guest.and_then(|g| g.maxmem)), 0),
        ],
    );
    if !pve.ok {
        usage = Part::Dim(Box::new(usage));
    }

    // Interface throughput is read from inside TrueNAS, never from PVE's
    // counters (spec §3.3). Each interface gets btop's network graph:
    // receive rising from a shared axis and send hanging below it, each
    // scaled to its own peak, with the speed now and the peak beside it.
    let mut network = Vec::new();
    for (i, (name, nic)) in interfaces.iter().enumerate() {
        if i > 0 {
            network.push(Part::Gap);
        }
        network.push(Part::Text(Line::from(vec![
            Span::styled(name.clone(), bold(p.ink)),
            Span::styled(" · ", style(p.muted)),
            Span::styled(nic_role(name, nic, host), style(p.ink2)),
        ])));
        let label = |series: usize, arrow: &str, word: &'static str| {
            Line::from(vec![
                Span::styled(format!("{arrow} "), style(look.flat(series))),
                Span::styled(word, style(p.muted)),
            ])
        };
        network.push(Part::Mirror(Mirror {
            up: Trace::from(nas.series(&format!("if.{name}.rx")), 0, ""),
            down: Trace::from(nas.series(&format!("if.{name}.tx")), 1, ""),
            up_label: label(0, "↓", "receive"),
            down_label: label(1, "↑", "send"),
            up_value: fmt::rate_auto(nic.rx_bps),
            down_value: fmt::rate_auto(nic.tx_bps),
            label_w: 10,
            value_w: 15,
            capacity: nas.capacity(),
            floor: MIB,
            scale: None,
            format: fmt::rate_auto,
            rows: 4,
        }));
    }
    if network.is_empty() {
        network.push(Part::Text(Line::styled("no interfaces yet", style(p.muted))));
    }

    let rate = |v: f64| Line::styled(fmt::rate(Some(v)), style(p.ink));
    let disk_rows = data
        .map_or(&[][..], |d| d.disks.as_slice())
        .iter()
        .map(|(name, disk)| {
            vec![
                Line::styled(name.clone(), style(p.ink)),
                Line::styled(disk.size.map_or(fmt::DASH.into(), |s| format!("{:.1} TiB", s / TIB)), style(p.ink2)),
                rate(disk.read_bps),
                rate(disk.write_bps),
            ]
        })
        .collect();
    let pool_rows = data
        .map_or(&[][..], |d| d.pools.as_slice())
        .iter()
        .map(|pool| {
            // Usable space where TrueNAS gave it: the raw zpool size counts
            // parity, about twice what a RAIDZ2 pool can hold. Raw, and said
            // so, only as a fallback.
            let usable = pool.usable.is_some() && pool.used_bytes.is_some();
            let (used, total) = if usable { (pool.used_bytes, pool.usable) } else { (pool.allocated, pool.size) };
            let r = ratio(used, total);
            // Graded: a ZFS pool past 80% is a genuine performance concern.
            let color = severity(look, r, p.s1);
            let figure = match (r, total) {
                (Some(r), Some(total)) => {
                    format!("{}% of {:.1} TiB{}", (r * 100.0).round() as i64, total / TIB, if usable { "" } else { " raw" })
                }
                _ => fmt::DASH.into(),
            };
            // Icon plus label: pool state never rides on colour alone.
            let health = Line::from(vec![
                Span::styled("● ", style(if pool.healthy { p.good } else { p.critical })),
                Span::styled(pool.status.clone().unwrap_or_else(|| "?".into()), style(p.ink2)),
            ]);
            vec![Line::styled(pool.name.clone(), style(p.ink)), health, bar(look, r, 8, color), Line::styled(figure, style(p.ink))]
        })
        .collect();

    Card {
        title: slot.name.clone(),
        right: format!("vmid {} · NAS", slot.vmid),
        stale: !status.ok,
        error: status.error.clone(),
        body: vec![
            guest_sub(look, guest),
            usage,
            Part::Gap,
            Part::Stack(network),
            Part::Gap,
            Part::Table(Table {
                caption: None,
                cols: vec![
                    Col { title: "Disk", width: 0, right: false },
                    Col { title: "Size", width: 9, right: true },
                    Col { title: "Read MiB/s", width: 10, right: true },
                    Col { title: "Write MiB/s", width: 11, right: true },
                ],
                rows: disk_rows,
                empty: "no disks yet",
            }),
            Part::Gap,
            Part::Table(Table {
                caption: None,
                cols: vec![
                    Col { title: "Pool", width: 0, right: false },
                    Col { title: "Status", width: 8, right: false },
                    Col { title: "Capacity", width: 8, right: false },
                    Col { title: "Used", width: 15, right: true },
                ],
                rows: pool_rows,
                empty: "no pools yet",
            }),
        ],
    }
}

// --------------------------------------------------------------- guests

/// One guest's card, holding as many lines as its box has rows, most
/// telling first: CPU, memory, network, disk I/O, disk usage, then address
/// and uptime. A wide card puts network and I/O on one line, with memory in
/// GiB.
pub fn guest(look: &Look, slot: &Slot, status: &Status, width: u16, height: u16) -> Card {
    let g = slot.guest;
    let (muted, ink) = (style(look.p.muted), style(look.p.ink));
    let cpu = g.and_then(|g| g.cpu_pct);
    let used = g.and_then(|g| ratio(g.mem, g.maxmem));
    let wide = width.saturating_sub(4) >= 34;
    let rate = |v: Option<f64>| Span::styled(fmt::rate(v), ink);
    let mut lines = vec![meter(look, "CPU", cpu.map(|v| v / 100.0), fmt::pct(cpu), 4)];
    if wide {
        lines.push(meter(look, "Mem", used, mem_text(g.and_then(|g| g.mem), g.and_then(|g| g.maxmem)), 0));
        lines.push(io_line(look, g, true));
        lines.push(disk_line(look, g));
        lines.push(guest_sub(look, g));
    } else {
        lines.push(meter(look, "Mem", used, fmt::pct(used.map(|r| r * 100.0)), 4));
        lines.push(Part::Text(Line::from(vec![
            Span::styled("net ↓", muted),
            rate(g.and_then(|g| g.net_in_bps)),
            Span::styled(" ↑", muted),
            rate(g.and_then(|g| g.net_out_bps)),
        ])));
        // Passed-through storage reads near zero however busy it is.
        lines.push(Part::Text(Line::from(match g.filter(|g| g.disk_io_visible) {
            Some(g) => vec![
                Span::styled("io  r", muted),
                rate(g.disk_read_bps),
                Span::styled(" w", muted),
                rate(g.disk_write_bps),
            ],
            None => vec![Span::styled("io  –", muted)],
        })));
        let disk = g
            .and_then(|g| g.filesystem.as_ref())
            .map_or(fmt::DASH.into(), |fs| fmt::pct(ratio(fs.used, Some(fs.total)).map(|r| r * 100.0)));
        lines.push(Part::Text(chip(look, "disk", disk)));
        if let Some(address) = g.and_then(|g| g.addresses.first()) {
            lines.push(Part::Text(Line::styled(address.clone(), style(look.p.ink2))));
        }
        lines.push(Part::Text(chip(look, "up", fmt::uptime(g.and_then(|g| g.uptime)))));
    }
    lines.truncate(height.saturating_sub(2) as usize);
    Card {
        title: slot.name.clone(),
        right: slot.vmid.to_string(),
        stale: !status.ok,
        error: status.error.clone(),
        body: lines,
    }
}

/// Lines a guest card of this width has to show.
pub fn guest_lines(width: u16, guest: Option<&Guest>) -> u16 {
    if width.saturating_sub(4) >= 34 {
        5
    } else {
        6 + u16::from(guest.is_some_and(|g| !g.addresses.is_empty()))
    }
}

/// Every guest on one line each, for when not even tiny cards fit.
pub fn guest_list(look: &Look, slots: &[&Slot], status: &Status) -> Card {
    let p = look.p;
    let gauge = |r: Option<f64>| {
        let mut line = bar(look, r, 6, p.s1);
        line.spans.push(Span::styled(format!(" {:>4}", fmt::pct(r.map(|r| r * 100.0))), style(p.ink)));
        line
    };
    let rows = slots
        .iter()
        .map(|s| {
            let g = s.guest;
            vec![
                Line::styled(s.name.clone(), style(p.ink)),
                gauge(g.and_then(|g| g.cpu_pct).map(|v| v / 100.0)),
                gauge(g.and_then(|g| ratio(g.mem, g.maxmem))),
                Line::from(vec![
                    Span::styled("↓", style(p.muted)),
                    Span::styled(fmt::rate(g.and_then(|g| g.net_in_bps)), style(p.ink)),
                    Span::styled(" ↑", style(p.muted)),
                    Span::styled(fmt::rate(g.and_then(|g| g.net_out_bps)), style(p.ink)),
                ]),
            ]
        })
        .collect();
    Card {
        title: "Guests".into(),
        right: slots.len().to_string(),
        stale: !status.ok,
        error: status.error.clone(),
        body: vec![Part::Table(Table {
            caption: None,
            cols: vec![
                Col { title: "Guest", width: 0, right: false },
                Col { title: "CPU", width: 11, right: false },
                Col { title: "Mem", width: 11, right: false },
                Col { title: "MiB/s", width: 9, right: false },
            ],
            rows,
            empty: "no other guests running",
        })],
    }
}

// --------------------------------------------------------------- weather

/// Holds the right-hand column for a forecast that is not built yet.
pub fn weather_placeholder(look: &Look) -> Card {
    Card {
        title: "Weather".into(),
        right: String::new(),
        stale: false,
        error: None,
        body: vec![Part::Text(Line::styled("Kept free for a forecast.", style(look.p.muted)))],
    }
}

// ----------------------------------------------------------- theme tour

/// While touring the themes: the one on now, among all of them, so the one
/// liked best can be named.
pub fn tour(look: &Look, names: &[&str], index: usize, next_in: u64) -> Card {
    let p = look.p;
    let mut body = vec![
        Part::Text(Line::styled(names[index].to_string(), bold(p.s1))),
        Part::Text(Line::styled(format!("next in {next_in}s"), style(p.muted))),
        // A blank line that stays one line: a gap would take the spare rows
        // and push the list to the bottom.
        Part::Text(Line::raw("")),
    ];
    for (i, name) in names.iter().enumerate() {
        body.push(Part::Text(if i == index {
            Line::from(vec![Span::styled("▶ ", style(p.s1)), Span::styled(name.to_string(), bold(p.ink))])
        } else {
            Line::styled(format!("  {name}"), style(p.muted))
        }));
    }
    Card {
        title: "Theme tour".into(),
        right: format!("{} / {}", index + 1, names.len()),
        stale: false,
        error: None,
        body,
    }
}
