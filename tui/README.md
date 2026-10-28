# pvestats-tui

The dashboard in a terminal, for an always-on screen. It is a Rust port of
the app's collector (`pvestats/`) and cards (`static/js/cards.js`): the same
sources, intervals, timeouts, stale and waiting rules, and the same `.env`
and `config.yaml`. Display only; it has no Stop buttons and changes nothing
on any machine it watches.

One binary, about 3 MB. Running, it holds about 8 MB of memory and 0.1% of a
CPU, and sends about 1.5 KB/s to the terminal.

## Layout

The layout follows the terminal's width:

- 150 columns or more: status, host and NAS (the rows under them kept free)
  | GPU | the weather forecast and guests.
- 96 to 149: status, host and GPU | NAS and guests.
- Under 96: one column, GPU first, scrolled with the arrow keys or j/k.

The source dots and the clock are a small panel at the top of the first
column, so the other columns start at the top row. Guests fill the rows
left under the other cards: the fullest cards that fit them all (full,
compact, then tiny, up to three across), else one line each. Rows a guest
card has over hold its CPU history, the same height across a row of them.

Charts grow into spare rows up to a set height; a card without a chart
keeps to its own height. Keys: `f` shows the GPU card alone and back, `q`
quits.

In a terminal that can (anything but the Linux text console) it draws
graphs in braille dots shaded from foot to top, meters as rows of small
squares, and big digits in box-drawing strokes, btop style, in the theme
from `config.yaml` (or `ui.json`): true colour when `COLORTERM` says so,
256 colours otherwise. On the text console (`TERM=linux`) it keeps to the
console font's glyphs (full block, two shades, box drawing) and 16 colours,
and turns off console blanking. `--console`, `--unicode` and `--colors`
override.

A `name` in config.yaml titles a card in place of the VM's name in Proxmox.

The right-hand column is a weather forecast, with any alerts in force at its
top, and the guests under it. There the guest cards keep a set height (11
rows) at the foot, and the forecast takes every row above them: when an
alert ends, its rows go to more days, not to the guests. In the United
States the forecast is the National Weather Service's own (api.weather.gov),
with the latest reading from the nearest station for now, and past its seven
days Open-Meteo's outlook, dimmer; elsewhere Open-Meteo's. Both are free, with
no key. Where it is for goes in .env, so it stays off GitHub: `WEATHER_PLACE`
(a place name, looked up once) or `WEATHER_LATITUDE` and `WEATHER_LONGITUDE`;
`WEATHER_UNITS=imperial` for °F, mph and inches. It refreshes every 15
minutes. The card shows a strip every two hours from the next, then the
week as a table.
`tui: theme_tour: <seconds>` in config.yaml tours all twelve themes, naming
each on screen, for picking one. `tui: screen_off: "03:00-05:00"` switches the
display off for those hours every day, through `wlr-randr`, when it runs in
the kiosk (any Wayland session). `--orbit` steps the screen one cell around a
small square every four minutes, against burn-in; it is off by default.
`tui/kiosk off` switches the kiosk's monitor off and pauses the dashboard,
so no machine is asked for anything; `tui/kiosk on` undoes it, and
`tui/kiosk status` says which it is. No sudo; a deploy or a restart of the
screen machine turns it back on. It runs `tui/kiosk-remote` there, which
also takes `schedule show`, `schedule clear` and `schedule set HH:MM-HH:MM`:
the daily screen-off window, kept in `tui/screen_off.local` on the screen
machine (deploys leave it alone, and it wins over config.yaml). moose-mcp's
key runs only a root-owned copy of it, `/usr/local/libexec/kiosk-remote`,
which Frank installs with sudo from a reviewed commit (MOO-200).

## On the Mac

`tui/make-mac-app` builds `PVEStats Terminal.app` in the repo root, with the
app's icon: click it and Terminal opens on the dashboard, which redraws to
fit when the window is resized; q or Esc closes it. It reads this repo's
.env and config.yaml. Build the dashboard for macOS first (`cargo build
--release` in tui/, with `CARGO_HOME=tui/.cargo-home`) on macserver; the
app is then copied into Frank's repo. Run from Terminal, the dashboard may
reach the LAN; run from elsewhere, macOS's local-network permission can
block it ("Connection Failed: Connect error").

## Build

On dev.lan, with cargo's cache inside the folder:

    cd ~/Projects/PVEStats/tui
    CARGO_HOME=$PWD/.cargo-home cargo build --release
    CARGO_HOME=$PWD/.cargo-home cargo test --release

`--snapshot 240x67 --after 12` collects for 12 seconds, prints one frame as
text and exits, for checking a layout without a screen.

## Deploy

`tui/deploy` tests and builds on a build machine, copies the repo to the
machine that shows the dashboard and restarts it there; `tui/deploy --check`
tests the logins. The two machines are set in `tui/deploy.conf` (git-ignored;
start from `deploy.conf.example`). The script's header says what it does,
step by step.

`tui/screenshot [file.png]` saves what that machine's monitor shows, through
the kiosk's Wayland session (`tui/shot`, built on the build machine).

`--demo`, or `tui: demo: true` in config.yaml, puts four made-up Ollama models
and seven GPU processes on ml60's card, to see how a busy GPU lays out.

## Run

It finds `.env` and `config.yaml` in the nearest folder above the binary
that has a `config.yaml`, or in `--root DIR`. So a copy of the repo with the
binary at `tui/bin/pvestats-tui` needs no options.

### Full screen on a machine's display

`pvestats-kiosk.service` puts the dashboard on tty1 in place of its login
prompt, in foot (a terminal) run full screen by cage (a compositor that
shows one program and nothing else). The other consoles (Ctrl+Alt+F2 and
on) still log in as usual. Quitting, or a crash, brings it back after two
seconds. It runs as frank. The font size, and so how much fits, is set in
`foot.ini`; restart the service after changing it.

    sudo apt install --no-install-recommends cage foot
    sudo cp ~/Projects/PVEStats/tui/pvestats-kiosk.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl disable --now getty@tty1.service
    sudo systemctl enable --now pvestats-kiosk.service

To give tty1 its login prompt back:

    sudo systemctl disable --now pvestats-kiosk.service
    sudo systemctl enable --now getty@tty1.service

cage draws a mouse pointer whenever the machine has a pointing device,
and a keyboard with a scroll wheel counts as one. The kiosk service points
cage and foot at `tui/cursor`, a theme whose every cursor is a single
transparent pixel, so no pointer shows. The rule below also stops mice and
touchpads from moving it:

`90-pvestats-no-pointer.rules` makes libinput ignore mice and touchpads;
keyboards and the text consoles are unaffected:

    sudo cp ~/Projects/PVEStats/tui/90-pvestats-no-pointer.rules /etc/udev/rules.d/
    sudo udevadm control --reload
    sudo udevadm trigger --subsystem-match=input --action=change
    sudo systemctl restart pvestats-kiosk.service

To have the pointer back, delete the file from /etc/udev/rules.d and run the
last three lines again.

### On the text console instead

Without a graphics stack, `pvestats-tui.service` runs it straight on tty1's
text console, coarser but with nothing to install. It also lowers the
kernel's console log level, as root, so that kernel messages stop printing
over it (they still reach the journal). Install it the same way, with
`pvestats-tui.service` in place of `pvestats-kiosk.service` and no apt
line. The two replace each other.

### Beside something else

Under tmux, split the terminal and run it in one pane; it lays itself out
for the pane's width and follows resizes.
