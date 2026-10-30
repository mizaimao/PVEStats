//! With --serve: the same sources as the terminal dashboard, served as the
//! web page (static/) and its JSON instead of drawn here. The Mac app's
//! window (app.py) shows the page; a browser on this computer can too.
//!
//! Binds 127.0.0.1 only. The one file it writes is ui.json, when the page's
//! settings change.

mod http;
mod page;
mod settings;

use std::net::TcpListener;
use std::process::exit;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::config::Config;
use crate::sources::{lock, Shared};
use settings::Settings;

pub struct Web {
    config: Arc<Config>,
    shared: Arc<Shared>,
    settings: Settings,
}

impl Web {
    /// Re-lengths every ring buffer: the page's charts span the "window"
    /// setting.
    fn set_history_window(&self, seconds: f64) {
        let seconds = seconds.clamp(30.0, 3600.0).trunc();
        let s = &self.shared;
        lock(&s.pve).resize(seconds);
        lock(&s.host).resize(seconds);
        lock(&s.agent).resize(seconds);
        lock(&s.nas).resize(seconds);
        lock(&s.router).resize(seconds);
    }
}

pub fn run(config: Arc<Config>, shared: Arc<Shared>) -> ! {
    let address = format!("127.0.0.1:{}", config.port);
    let listener = TcpListener::bind(&address).unwrap_or_else(|e| {
        // Usually an instance already running.
        eprintln!(
            "Cannot bind {address}: {e}.\nPVEStats may already be running. Check with:\n  lsof -nP -iTCP:{} -sTCP:LISTEN",
            config.port
        );
        exit(1);
    });
    // Gone with whatever started it: app.py's window, or a terminal. A
    // force-quit app never runs its own clean-up, and a server left behind
    // would go on polling every source with no window, on a fanless Mac.
    // Its parent changing (to launchd) means that one is gone.
    // SAFETY: getppid has no preconditions and cannot fail.
    let parent = unsafe { libc::getppid() };
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(2));
        if unsafe { libc::getppid() } != parent {
            exit(0);
        }
    });
    let settings = Settings::new(config.root.clone(), &config.ui);
    let web = Arc::new(Web { config, shared, settings });
    web.set_history_window(web.settings.window());
    // The first line app.py reads: where the page is.
    eprintln!("dashboard  http://{address}");
    http::serve(listener, web)
}
