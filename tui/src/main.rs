//! PVEStats in a terminal: the app's collector and cards, for an always-on
//! screen such as a Linux text console. Display only; it changes nothing on
//! any machine it watches.

mod config;
mod format;
mod json;
mod net;
mod sources;
mod state;
mod ui;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;

use config::Config;
use sources::{agent::Agent, host::Host, lock, nas_agent::NasAgent, pve::Pve, router::Router, run_poller, weather::Weather, Shared};
use ui::theme::{Colors, Look};
use ui::{Screen, Tour};

const USAGE: &str = "\
usage: pvestats-tui [options]

Reads .env and config.yaml from the PVEStats folder.

  --root DIR          the PVEStats folder (default: found above the binary)
  --console           only glyphs the Linux console font has (default on TERM=linux)
  --unicode           finer block glyphs (default elsewhere)
  --colors MODE       console, 256 or rgb (default: from TERM and COLORTERM)
  --snapshot WxH      print one frame as text instead of running, then exit
  --after SECONDS     how long to collect before the snapshot (default 12)
  --focus             start on the GPU card alone
  --demo              made-up models and GPU processes on the
                      ml60 card, for checking how a busy GPU lays out
  --orbit             step the screen around one cell every 4 minutes, against burn-in

keys: f  GPU card alone / everything     up, down, j, k  scroll a narrow column
      q  quit";

struct Args {
    root: Option<PathBuf>,
    console: Option<bool>,
    colors: Option<Colors>,
    snapshot: Option<(u16, u16)>,
    after: f64,
    focus: bool,
    demo: bool,
    orbit: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { root: None, console: None, colors: None, snapshot: None, after: 12.0, focus: false, demo: false, orbit: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--root" => args.root = Some(PathBuf::from(value()?)),
            "--console" => args.console = Some(true),
            "--unicode" => args.console = Some(false),
            "--colors" => {
                args.colors = Some(match value()?.as_str() {
                    "console" | "16" => Colors::Console,
                    "256" => Colors::Indexed,
                    "rgb" | "truecolor" => Colors::Rgb,
                    other => return Err(format!("unknown colour mode {other}")),
                })
            }
            "--snapshot" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--snapshot takes WxH, e.g. 240x67")?;
                let size = (w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?);
                args.snapshot = Some(size);
            }
            "--after" => args.after = value()?.parse().map_err(|_| "bad --after")?,
            "--focus" => args.focus = true,
            "--demo" => args.demo = true,
            "--orbit" => args.orbit = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("pvestats-tui: {e}\n\n{USAGE}");
        exit(2);
    });
    let root = config::find_root(args.root.clone());
    let mut config = Config::load(&root).unwrap_or_else(|e| {
        eprintln!("pvestats-tui: {e}");
        exit(1);
    });
    config.demo |= args.demo;
    let config = Arc::new(config);
    let shared =
        Arc::new(Shared::new(config.window, config.pve_interval(), config.pve_agent_every, config.nas_agent_every));
    start(&config, &shared);

    let term = std::env::var("TERM").unwrap_or_default();
    let on_console = term == "linux" || term.starts_with("linux-");
    let colors = args.colors.unwrap_or_else(|| {
        let truecolor = matches!(std::env::var("COLORTERM").as_deref(), Ok("truecolor" | "24bit"));
        if on_console {
            Colors::Console
        } else if truecolor {
            Colors::Rgb
        } else {
            Colors::Indexed
        }
    });
    let look = Look::new(&config.theme, colors, args.console.unwrap_or(on_console));
    let mut screen = Screen::new();
    screen.focus = args.focus;
    // A snapshot is for checking a layout, which a step would shift.
    screen.orbit = args.orbit && args.snapshot.is_none();

    if let Some((w, h)) = args.snapshot {
        thread::sleep(Duration::from_secs_f64(args.after.max(0.0)));
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("a test backend never fails");
        let mut touring = Touring::new(config.tour);
        let look = touring.look(&look, &mut screen);
        let drawn = terminal.draw(|frame| {
            let area = frame.area();
            screen.draw(frame.buffer_mut(), area, &shared, &config, &look);
        });
        drawn.expect("a test backend never fails");
        let buffer = terminal.backend().buffer();
        let mut out = io::stdout().lock();
        for y in 0..h {
            let line: String = (0..w).map(|x| buffer[(x, y)].symbol()).collect();
            // A reader that stops early (`| head`) is not an error.
            if writeln!(out, "{}", line.trim_end()).is_err() {
                break;
            }
        }
        return;
    }

    if on_console {
        // The console blanks itself after ten minutes by default, which an
        // always-on dashboard must not: unblank, then switch off the blank
        // and powerdown timers (what `setterm -blank 0 -powerdown 0` sends).
        print!("\x1b[13]\x1b[9;0]\x1b[14;0]");
        let _ = io::stdout().flush();
    }
    if let Err(e) = run(&mut screen, &shared, &config, &look) {
        eprintln!("pvestats-tui: {e}");
        exit(1);
    }
}

/// One thread per source, each on its own interval.
fn start(config: &Arc<Config>, shared: &Arc<Shared>) {
    let missing = || "not configured — see .env".to_string();
    if config.pve_configured() {
        // Whether the host agent answers now: PVE then skips the node
        // status it would only repeat.
        let s = shared.clone();
        let host_live = Box::new(move || {
            let status = lock(&s.host).status();
            status.ok && !status.stale
        });
        let interval = config.pve_interval();
        let (mut pve, s) = (Pve::new(config.clone(), interval, host_live), shared.clone());
        thread::spawn(move || run_poller(&s.pve, interval, || pve.poll(&s.pve)));
    } else {
        lock(&shared.pve).fail(missing());
    }
    if config.host_configured() {
        let (mut host, s) = (Host::new(config.pve_agent_url.clone()), shared.clone());
        let every = config.pve_agent_every;
        thread::spawn(move || run_poller(&s.host, every, || host.poll(&s.host, &s.pve)));
    }
    if config.agent_configured() {
        let (mut agent, s) = (Agent::new(config.agent_url.clone(), config.demo), shared.clone());
        thread::spawn(move || run_poller(&s.agent, sources::AGENT_INTERVAL, || agent.poll(&s.agent)));
    } else {
        lock(&shared.agent).fail(missing());
    }
    if config.router_shown() {
        let url = Some(config.router_url.clone()).filter(|u| !u.is_empty());
        let (mut router, s) = (Router::new(url), shared.clone());
        thread::spawn(move || run_poller(&s.router, sources::ROUTER_INTERVAL, || router.poll(&s.router)));
    }
    if config.weather_configured() {
        let (mut weather, s) = (Weather::new(config.clone()), shared.clone());
        thread::spawn(move || run_poller(&s.weather, sources::WEATHER_INTERVAL, || weather.poll(&s.weather)));
    }
    // TrueNAS only through its own agent: its API is no longer asked.
    if config.nas_agent_configured() {
        let (mut nas, s) = (NasAgent::new(config.nas_agent_url.clone()), shared.clone());
        let every = config.nas_agent_every;
        thread::spawn(move || run_poller(&s.nas, every, || nas.poll(&s.nas)));
    } else {
        lock(&shared.nas).fail(missing());
    }
}

fn run(screen: &mut Screen, shared: &Shared, config: &Config, base: &Look) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut power = Power::new(config);
    let mut touring = Touring::new(config.tour);
    let result = (|| loop {
        power.tick();
        let look = touring.look(base, screen);
        terminal.draw(|frame| {
            let area = frame.area();
            screen.draw(frame.buffer_mut(), area, shared, config, look);
        })?;
        // Redraw every second, the agent's pace, or at once on a key or a
        // resize. Only changed cells are sent, so an idle screen costs little.
        if !event::poll(Duration::from_secs(1))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('f') => screen.focus = !screen.focus,
            KeyCode::Down | KeyCode::Char('j') => screen.scroll = screen.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => screen.scroll = screen.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => screen.scroll = screen.scroll.saturating_add(10),
            KeyCode::PageUp => screen.scroll = screen.scroll.saturating_sub(10),
            KeyCode::Home => screen.scroll = 0,
            _ => {}
        }
    })();
    ratatui::restore();
    result
}

/// While `tui: theme_tour` is set in config.yaml, every theme in turn, so
/// the one liked best can be picked off the screen itself.
struct Touring {
    every: f64,
    started: Instant,
    current: Option<(usize, Look)>,
}

impl Touring {
    fn new(every: f64) -> Self {
        Touring { every, started: Instant::now(), current: None }
    }

    /// The look to draw with now, and the tour's place on the screen.
    fn look<'a>(&'a mut self, base: &'a Look, screen: &mut Screen) -> &'a Look {
        if self.every <= 0.0 {
            return base;
        }
        let names: Vec<&str> = ui::theme::names().collect();
        let elapsed = self.started.elapsed().as_secs_f64();
        let index = (elapsed / self.every) as usize % names.len();
        screen.tour = Some(Tour { index, next_in: (self.every - elapsed % self.every).ceil() as u64 });
        if self.current.as_ref().map(|(i, _)| *i) != Some(index) {
            self.current = Some((index, base.retheme(names[index])));
        }
        self.current.as_ref().map_or(base, |(_, look)| look)
    }
}

/// Switches the display off for a daily window (`tui: screen_off` in
/// config.yaml), against burn-in, through wlr-randr. Only inside a Wayland
/// session such as the kiosk: anywhere else there is no display of its own
/// to switch. Each change is applied once, and the first tick applies the
/// state the clock calls for, so a restart inside the window still ends it.
///
/// tui/screen_off.local, written by tui/kiosk-remote and left alone by
/// deploys, wins over config.yaml: one line, "HH:MM-HH:MM" or "none". It is
/// looked at once a minute and read again when it changed.
struct Power {
    configured: Option<(u32, u32)>,
    window: Option<(u32, u32)>,
    local: PathBuf,
    /// When the file was last looked at, and its modification time then.
    looked: Option<(Instant, Option<SystemTime>)>,
    off: Option<bool>,
}

impl Power {
    fn new(config: &Config) -> Self {
        let local = config.root.join("tui").join("screen_off.local");
        Power { configured: config.screen_off, window: config.screen_off, local, looked: None, off: None }
    }

    fn reread(&mut self) {
        if self.looked.is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(60)) {
            return;
        }
        let modified = std::fs::metadata(&self.local).and_then(|m| m.modified()).ok();
        let changed = self.looked.map_or(true, |(_, before)| before != modified);
        self.looked = Some((Instant::now(), modified));
        if changed {
            let text = std::fs::read_to_string(&self.local).ok();
            self.window = local_window(text.as_deref()).unwrap_or(self.configured);
        }
    }

    fn tick(&mut self) {
        self.reread();
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return;
        }
        let Some((from, to)) = self.window else {
            // The window was cleared while the screen was off for it.
            if self.off == Some(true) {
                self.off = Some(false);
                thread::spawn(|| switch_outputs(true));
            }
            return;
        };
        let (h, m) = ui::local_hm();
        let now = h * 60 + m;
        let off = if from < to { now >= from && now < to } else { now >= from || now < to };
        if self.off != Some(off) {
            self.off = Some(off);
            thread::spawn(move || switch_outputs(!off));
        }
    }
}

/// What tui/screen_off.local says: a window, Some(None) for "none", and
/// None when there is no such file or it says nothing usable, which leaves
/// config.yaml's window in force.
fn local_window(text: Option<&str>) -> Option<Option<(u32, u32)>> {
    let text = text?.lines().next()?.trim();
    if text == "none" {
        return Some(None);
    }
    config::hours(text).map(Some)
}

/// Every output wlr-randr lists, on or off. Only outputs not already so are
/// switched: turning on a display that is on would make it flicker each
/// time the dashboard restarts. A missing wlr-randr leaves the display as
/// it is.
fn switch_outputs(on: bool) {
    let Ok(list) = std::process::Command::new("wlr-randr").output() else { return };
    let text = String::from_utf8_lossy(&list.stdout);
    for (output, enabled) in outputs(&text) {
        if enabled != on {
            let _ = std::process::Command::new("wlr-randr")
                .args(["--output", output, if on { "--on" } else { "--off" }])
                .status();
        }
    }
}

/// (name, enabled) for each output in wlr-randr's listing: a name line,
/// then indented details, one of them "Enabled: yes" or "Enabled: no".
fn outputs(text: &str) -> Vec<(&str, bool)> {
    let mut found = Vec::new();
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) {
            if let Some(name) = line.split_whitespace().next() {
                found.push((name, true));
            }
        } else if let Some(state) = line.trim().strip_prefix("Enabled:") {
            if let Some(last) = found.last_mut() {
                last.1 = state.trim() == "yes";
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_local_screen_off_window() {
        assert_eq!(super::local_window(Some("03:00-05:00\n")), Some(Some((180, 300))));
        assert_eq!(super::local_window(Some("none\n")), Some(None));
        // Nothing usable, or no file: config.yaml's window stands.
        assert_eq!(super::local_window(Some("garbage")), None);
        assert_eq!(super::local_window(Some("")), None);
        assert_eq!(super::local_window(None), None);
    }

    #[test]
    fn wlr_randr_listing() {
        let text = "HDMI-A-2 \"Monitor (HDMI-A-2)\"\n  Make: X\n  Enabled: no\n  Modes:\n    1920x1080 px\nDP-1 \"Other\"\n  Enabled: yes\n";
        assert_eq!(super::outputs(text), vec![("HDMI-A-2", false), ("DP-1", true)]);
    }
}
