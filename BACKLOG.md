# Backlog

Suggested next steps, roughly by value. Nothing here is committed work — it is
a list to pick from.

## From the audit of 2026-11-25

Checked against the code; not done, because each needs Frank's call or an
install on a homelab machine (two audits first, then his yes).

**Agents**
- ml60's unit (`agent/pvestats-agent.service`) has four sandboxing lines
  where pve's has a dozen. It runs as `ollama` and is reachable through
  Caddy. Worth adding `CapabilityBoundingSet=`, `ProtectKernel*`,
  `RestrictNamespaces`, `RestrictAddressFamilies`, `IPAddressDeny=any` with
  loopback allowed, and `InaccessiblePaths=-/usr/share/ollama/.ollama`,
  tested on ml60 for what NVML tolerates.
- The pve agent answers any Host header, so a web page someone on the LAN
  opens could read the VM list by DNS rebinding. Refuse a Host other than
  its own address.
- NAS: `PrivateTmp=yes` has systemd make folders under /tmp and /var/tmp at
  each start, for an agent that never writes; it can go. And
  `StartLimitBurst=10` in 600 s gives up for good after about 5 minutes of
  failed starts (the network late after boot, say).
- Router agent: reads every port's link speed every 30 s, which nothing
  shows; sends 0 rather than null when `/proc/net/dev` cannot be read,
  which the next good reading turns into one huge spike; two 5 GHz radios
  would share one name (not on the GT-BE98 Pro).
- ml60 agent: an IPv6 `OLLAMA_URL` gets a Host header without brackets; an
  unload while Ollama is down answers "not loaded" for 5 s instead of 502.

**Dashboard**
- The page's ml60 card shows Proxmox's CPU, network, disk, address and
  uptime where the terminal shows ml60's own: `web/page.rs` never sends the
  agent's `inside` readings.
- `/api/state` sends every series every second, most of them never drawn
  (`ram.*`, `gpu.power`, `cpu.<vmid>`, ...), and a stopped guest's series
  stay as nulls for good. Send only what the page draws.
- The page polls with `setInterval` and no timeout, so a stalled server
  piles up requests that can land out of order. Chain `setTimeout` instead.
- A failed settings save (`/api/ui`) looks saved.
- `tui/deploy` does not check `tui/kiosk status` itself.
- PVEStats Terminal closes with no alert when the dashboard dies by a
  signal.
- `send_to_build` never deletes on the build machine a file removed here,
  so a deleted test or `build.rs` is still built there.

## History

**Card-expand history view.** Clicking a card could expand it to hour / day /
week / month history, straight from PVE's own RRD files (`rrddata`) — no
extra collection, no extra load. PVE's reply also carries PSI pressure
metrics (`pressurecpusome`, `pressureiofull`, and the memory equivalents)
that the design spec never mentions.

Nothing serves it today. The Python server had `GET /api/history/{vmid}`
for it, which nothing ever called, and it went with that server on
2026-10-30 (`pvestats/server.py` in git history); the endpoint would be
written again in `tui/src/web/`.

## Adds information

- **Load-reactive glow.** Glow intensity tracks the value, so a GPU at 100%
  visibly burns while an idle one stays dim. Costs nothing — it is one CSS
  variable set per tick — and turns an existing decoration into an encoding.
- **Min / max markers** on charts, labelled. Right now only the current value
  and the axis ceiling are readable.
- **"Now" sweep line** — a vertical marker at the leading edge of each chart.
- **Per-core CPU.** Split a guest's CPU bar into `maxcpu` segments, so eight
  cores at 50% stops looking like one core at 50%.

## Layout

- **Shorten the NAS card.** Still the original complaint: three stacked tables
  make the page a constant 1160px tall at any width, which is why it cannot fit
  the 1169px display. Collapsing idle disks, or putting interfaces and pools
  side by side, is what closes that gap.
- **Persist focus mode** in `ui.json` if it should survive a relaunch. The
  plumbing exists; it is deliberately session-only today.

## Cosmetic

- **Real wallpapers behind the glass themes.** The Vista / Win7 / Snow Leopard
  themes currently approximate their wallpapers with gradients. Using the actual
  images as the page background would make the frosted panes genuinely Aero.
  The reference JPEGs are no longer in the folder, so they would need finding
  again.
- **The settings panel is barely legible on Snow Leopard.** The glass themes
  give it a very translucent background, and at that theme's brightness the
  dashboard behind shows straight through the text. Worst on Snow Leopard;
  visible on Vista and Windows 7 too. Needs a more opaque panel backdrop for
  the glass family specifically.
- **Sparkline area gradient** — fade the wash downward instead of a flat fill.
- **Lucide icons** in card headers (server / cpu / hard-drive / activity).

## Notes worth keeping

- Verify UI work in **WebKit**, not Chromium. `playwright.webkit` matches what
  the app actually runs. Chart glow silently did nothing in WebKit for six
  rounds because it was only ever checked in Chromium. Playwright is not in
  `pyproject.toml`; if it comes back, set `PLAYWRIGHT_BROWSERS_PATH` to a
  git-ignored folder in the repo, or its browsers land in `~/Library/Caches`.
- `ui.json` persists appearance settings server-side, so a setting changed
  while testing stays changed. Delete the file to return to `config.yaml`
  defaults.
- Effects measured against an ~11% baseline: grain free, scanlines +1%,
  glow ×1 +5%, glow ×2.5 +16%. Anything that animates a *painted* property is
  expensive — aurora cost +73% before it was rewritten to animate opacity.
