//! McFilin — a file viewer with Miller columns, after NeXTSTEP's File Viewer.
//!
//! Usage: `mcfilin [PATH]` — opens at PATH, or the home folder.
//!
//! The window comes back the size it was closed at, with the path view shown
//! or not as it was then (see [`mcfilin::settings`]).

use std::io;
use std::path::PathBuf;

use mcfilin::settings::{Settings, WindowState};
use saudade::App;

fn main() {
    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    let settings = Settings::load();
    let state = WindowState::load();
    let (config, root, current) = mcfilin::window(start, &settings, &state);
    App::new(config, root).run();

    let (new_settings, new_state) = current();
    if new_settings != settings {
        report("settings", new_settings.save());
    }
    if new_state != state {
        report("window size", new_state.save());
    }
}

fn report(what: &str, result: io::Result<()>) {
    if let Err(err) = result {
        eprintln!("mcfilin: couldn't save the {what}: {err}");
    }
}
