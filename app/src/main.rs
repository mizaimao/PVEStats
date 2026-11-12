//! PVEStats as a Mac app of its own: the dashboard's page in a window,
//! drawn by WKWebView, the web engine macOS already has.
//!
//! It starts the dashboard program (tui/) with --serve, which polls the
//! machines and serves the page on 127.0.0.1, and shows that page. Closing
//! the window or quitting the app ends the dashboard program too; it also
//! ends by itself if this program is gone. Nothing is left running and
//! nothing is installed.
//!
//! In PVEStats.app (app/make-app) the dashboard program is the one in the
//! app's Resources, and the repo it reads (.env, config.yaml) is the folder
//! the app sits in. Run from app/target, it is tui/target's, and the repo is
//! the one this folder is in. PVESTATS_ROOT names another.

use std::env;
use std::io::{BufRead, BufReader, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{exit, Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use muda::{AboutMetadata, Menu, PredefinedMenuItem, Submenu};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// The page's own background, so the window never flashes white.
const BACKGROUND: (u8, u8, u8, u8) = (13, 13, 13, 255);

fn main() {
    let (root, program) = places().unwrap_or_else(|why| die(&why));
    if !program.is_file() {
        die(&format!("No dashboard program at {}.\nBuild the app with app/make-app.", program.display()));
    }
    let (mut dashboard, url) = Dashboard::start(&program, &root).unwrap_or_else(|why| die(&why));

    let event_loop = EventLoop::new();
    // Measured: the full layout is about 1160 points tall between 1280 and
    // 1500 wide, and grows to about 1280 below that as columns collapse.
    // Three quarters of 1500 wide still holds the two-column row, and a
    // window that doesn't fill the screen is easier to leave open beside
    // other work, which is how this gets used. Never larger than the screen.
    let (width, height) = fit_to_screen(&event_loop, 1125.0, 1230.0);
    let window = WindowBuilder::new()
        .with_title("PVEStats")
        .with_inner_size(LogicalSize::new(width, height))
        // 700 still shows both columns: the layout compresses rather than
        // dropping panels until about 660.
        .with_min_inner_size(LogicalSize::new(700.0, 640.0))
        .with_background_color(BACKGROUND)
        .build(&event_loop)
        .unwrap_or_else(|e| die(&format!("The window could not open: {e}")));
    let _webview = WebViewBuilder::new()
        .with_url(&url)
        // A private window: WebKit keeps the page's data in memory alone.
        // Its usual store is folders of the app's own under ~/Library
        // (WebKit, Caches), and the app is to write nothing outside the
        // repo. The page counts on nothing kept: its theme comes rendered
        // into it from ui.json.
        .with_incognito(true)
        .with_background_color(BACKGROUND)
        .build(&window)
        .unwrap_or_else(|e| die(&format!("The page could not open: {e}")));
    let menu = menus().unwrap_or_else(|e| die(&format!("The menus could not be made: {e}")));

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // The menu bar goes in once the app is running.
            Event::NewEvents(StartCause::Init) => menu.init_for_nsapp(),
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            // Closed, or quit from the menu.
            Event::LoopDestroyed => dashboard.stop(),
            _ => {}
        }
    });
}

/// The repo the dashboard reads, and the dashboard program.
fn places() -> Result<(PathBuf, PathBuf), String> {
    let exe = env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| format!("Where is this program? {e}"))?;
    let named = env::var_os("PVESTATS_ROOT").map(PathBuf::from);
    // …/PVEStats.app/Contents/MacOS/PVEStats
    let contents = exe.parent().and_then(Path::parent).filter(|c| c.ends_with("Contents"));
    if let Some(contents) = contents {
        let app = contents.parent().ok_or("The app has no folder")?;
        let root = named.or_else(|| app.parent().map(Path::to_path_buf)).ok_or("The app has no folder")?;
        return Ok((root, contents.join("Resources").join("pvestats-tui")));
    }
    // app/target/release/pvestats-app
    let root = named
        .or_else(|| exe.ancestors().find(|a| a.join("config.yaml").is_file() && a.join("tui").is_dir()).map(Path::to_path_buf))
        .ok_or("No repo with config.yaml above this program; set PVESTATS_ROOT")?;
    let program = root.join("tui/target/release/pvestats-tui");
    Ok((root, program))
}

/// pvestats-tui --serve, as a child of this program.
struct Dashboard {
    child: Child,
}

impl Dashboard {
    /// Starts it and waits for the address it serves the page at, which it
    /// gives on stderr ("dashboard  http://127.0.0.1:8080"); or why it
    /// could not start.
    fn start(program: &Path, root: &Path) -> Result<(Dashboard, String), String> {
        let mut child = Command::new(program)
            .arg("--serve")
            .arg("--root")
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{} could not start: {e}", program.display()))?;
        let mut stderr = BufReader::new(child.stderr.take().ok_or("no stderr")?);
        let mut first = String::new();
        let _ = stderr.read_line(&mut first);
        let first = first.trim().to_string();
        if let Some(address) = first.strip_prefix("dashboard ").and_then(|rest| rest.split_whitespace().last()) {
            let url = format!("{address}/");
            // Read on, so that what it says later never fills the pipe and
            // stalls it; passed on to this program's stderr.
            thread::spawn(move || {
                for line in stderr.lines().map_while(Result::ok) {
                    eprintln!("{line}");
                }
            });
            return Ok((Dashboard { child }, url));
        }
        let mut rest = String::new();
        let _ = stderr.read_to_string(&mut rest);
        let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
        let said: Vec<&str> = [first.as_str(), rest.trim()].into_iter().filter(|s| !s.is_empty()).collect();
        Err(if said.is_empty() { format!("pvestats-tui stopped: {status}") } else { said.join("\n") })
    }

    /// Asks it to stop, and makes it after five seconds.
    fn stop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: a signal to our own child, which has not been reaped.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The size asked for, held within the largest screen's 94% across and 92%
/// down: a window taller than the screen would be cut off.
fn fit_to_screen(event_loop: &EventLoop<()>, width: f64, height: f64) -> (f64, f64) {
    let (mut most_wide, mut most_high) = (0.0_f64, 0.0_f64);
    for monitor in event_loop.available_monitors() {
        let size: LogicalSize<f64> = monitor.size().to_logical(monitor.scale_factor());
        most_wide = most_wide.max(size.width);
        most_high = most_high.max(size.height);
    }
    if most_wide == 0.0 || most_high == 0.0 {
        return (width, height);
    }
    (width.min(most_wide * 0.94).round(), height.min(most_high * 0.92).round())
}

/// The app menu (Cmd-Q, Cmd-H), Edit (Cmd-C and Cmd-V reach the page only
/// through these), View (full screen) and Window.
fn menus() -> muda::Result<Menu> {
    let about = AboutMetadata { name: Some("PVEStats".into()), ..Default::default() };
    let app = Submenu::with_items(
        "PVEStats",
        true,
        &[
            &PredefinedMenuItem::about(None, Some(about)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ],
    )?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(None),
            &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None),
            &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None),
            &PredefinedMenuItem::select_all(None),
        ],
    )?;
    let view = Submenu::with_items("View", true, &[&PredefinedMenuItem::fullscreen(None)])?;
    let window = Submenu::with_items(
        "Window",
        true,
        &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(None), &PredefinedMenuItem::close_window(None)],
    )?;
    window.set_as_windows_menu_for_nsapp();
    let menu = Menu::new();
    menu.append_items(&[&app, &edit, &view, &window])?;
    Ok(menu)
}

/// Exits with a message that is seen however the app was started: from
/// PVEStats.app there is no terminal, so it goes in a dialog too.
fn die(message: &str) -> ! {
    eprintln!("{message}");
    if !std::io::stdin().is_terminal() {
        let _ = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display alert \"PVEStats could not start\" message (item 1 of argv) as critical",
                "-e",
                "end run",
                message,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    exit(1)
}
