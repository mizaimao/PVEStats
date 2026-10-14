# Backlog

Suggested next steps, roughly by value. Nothing here is committed work — it is
a list to pick from.

## Built but unused

**Card-expand history view.** `GET /api/history/{vmid}` has been serving PVE
`rrddata` since the HTTP server was written and nothing calls it. Clicking a
card could expand it to hour / day / week / month history, straight from PVE's
own RRD files — no extra collection, no extra load. The response also carries
PSI pressure metrics (`pressurecpusome`, `pressureiofull`, and the memory
equivalents) that PVE exposes and the design spec never mentions.

This is the largest capability currently sitting idle.

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
  `requirements.txt`; if it comes back, set `PLAYWRIGHT_BROWSERS_PATH` to a
  git-ignored folder in the repo, or its browsers land in `~/Library/Caches`.
- `ui.json` persists appearance settings server-side, so a setting changed
  while testing stays changed. Delete the file to return to `config.yaml`
  defaults.
- Effects measured against an ~11% baseline: grain free, scanlines +1%,
  glow ×1 +5%, glow ×2.5 +16%. Anything that animates a *painted* property is
  expensive — aurora cost +73% before it was rewritten to animate opacity.
