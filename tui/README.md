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

- 200 columns or more (a 1920x1080 Linux console is 240x67): host and
  guests | GPU | NAS.
- 110 to 199: host and GPU | NAS and guests. Guests go compact when the
  rows run short.
- Under 110: one column, GPU first, scrolled with the arrow keys or j/k.

Keys: `f` shows the GPU card alone and back, `q` quits.

On the Linux text console (`TERM=linux`) it draws only glyphs the console
font has (full block, two shades, box drawing) in its 16 colours, and turns
off console blanking. Elsewhere it uses finer blocks and the theme from
`config.yaml` (or `ui.json`), in true colour when `COLORTERM` says so and
256 colours otherwise. `--console`, `--unicode` and `--colors` override.

## Build

On dev.lan, with cargo's cache inside the folder:

    cd ~/Projects/PVEStats/tui
    CARGO_HOME=$PWD/.cargo-home cargo build --release
    CARGO_HOME=$PWD/.cargo-home cargo test --release

`--snapshot 240x67 --after 12` collects for 12 seconds, prints one frame as
text and exits, for checking a layout without a screen.

## Run

It finds `.env` and `config.yaml` in the nearest folder above the binary
that has a `config.yaml`, or in `--root DIR`. So a copy of the repo with the
binary at `tui/bin/pvestats-tui` needs no options.

### On a machine's first text console

`pvestats-tui.service` puts the dashboard on tty1 in place of its login
prompt; the other consoles (Ctrl+Alt+F2 and on) still log in as usual.
Quitting, or a crash, brings it back after two seconds. It runs as frank,
and as root only lowers the kernel's console log level so that kernel
messages stop printing over it (they still reach the journal).

    sudo cp ~/Projects/PVEStats/tui/pvestats-tui.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl disable --now getty@tty1.service
    sudo systemctl enable --now pvestats-tui.service

To give tty1 its login prompt back:

    sudo systemctl disable --now pvestats-tui.service
    sudo systemctl enable --now getty@tty1.service

### Beside something else

Under tmux, split the console and run it in one pane; it lays itself out
for the pane's width and follows resizes.
