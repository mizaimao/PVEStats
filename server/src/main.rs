//! pvestats-server: PVEStats' collector and local dashboard server, in
//! place of pvestats/ (Python), with the same routes and the same replies.
//! app.py's window shows its page; run it on its own to serve a browser.
//!
//!     pvestats-server [--root DIR]
//!
//! The root is the repo: config.yaml, .env, ui.json and static/. Binds
//! 127.0.0.1 only. The one file it writes is ui.json, when the page's
//! settings change.

mod collector;
mod config;
mod http;
mod net;
mod sources;
mod state;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use collector::Collector;
use config::Config;

/// `--root` wins; then PVESTATS_ROOT; then the nearest folder above the
/// program that holds a config.yaml (it sits in server/bin or
/// server/target/release); then the current one.
fn root(flag: Option<PathBuf>) -> PathBuf {
    if let Some(root) = flag.or_else(|| std::env::var_os("PVESTATS_ROOT").map(PathBuf::from)) {
        return root;
    }
    let exe = std::env::current_exe().ok();
    let above = exe.as_deref().and_then(Path::parent).and_then(|dir| dir.ancestors().find(|d| d.join("config.yaml").is_file()));
    above.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut flag = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => flag = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                eprintln!("usage: pvestats-server [--root DIR]");
                return;
            }
            other => {
                eprintln!("pvestats-server: unknown argument {other}");
                exit(2);
            }
        }
    }
    let root = root(flag);
    let config = match Config::load(&root) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            // config.yaml and .env are edited by hand; a typo stops here.
            eprintln!("Could not read the configuration (config.yaml or .env): {e}");
            exit(1);
        }
    };
    let address = format!("127.0.0.1:{}", config.port);
    let listener = match TcpListener::bind(&address) {
        Ok(l) => l,
        Err(e) => {
            // Usually an instance already running.
            eprintln!(
                "Cannot bind {address}: {e}.\nPVEStats may already be running. Check with:\n  lsof -nP -iTCP:{} -sTCP:LISTEN",
                config.port
            );
            exit(1);
        }
    };
    // Gone with whatever started it: app.py's window, or a terminal. A
    // force-quit app never runs its own clean-up, and a server left behind
    // would go on polling every source with no window, on a fanless Mac.
    // Its parent changing (to launchd) means that one is gone.
    // SAFETY: getppid has no preconditions and cannot fail.
    let parent = unsafe { libc::getppid() };
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if unsafe { libc::getppid() } != parent {
            exit(0);
        }
    });
    let collector = Arc::new(Collector::new(Arc::clone(&config)));
    collector.start();
    eprintln!("dashboard  http://{address}");
    http::serve(listener, collector);
}
