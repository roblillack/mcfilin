//! McFilin — a file viewer with Miller columns, after NeXTSTEP's File Viewer.
//!
//! Usage: `mcfilin [PATH]` — opens at PATH, or the home folder.

use std::path::PathBuf;

use saudade::App;

fn main() {
    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    let (config, root) = mcfilin::window(start);
    App::new(config, root).run();
}
