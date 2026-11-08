//! Renders the McFilin window offscreen (no display needed) into a PNG, after
//! replaying a few synthetic events, to check how it looks.
//!
//! `cargo run --example screenshot -- PATH OUT.png [WIDTH HEIGHT]`

use std::path::PathBuf;

use saudade::mock::MockBackend;
use saudade::{Event, Font, Modifiers, MouseButton, Point, WindowChrome};

fn main() {
    let mut args = std::env::args().skip(1);
    let start = PathBuf::from(args.next().expect("PATH"));
    let out = args.next().expect("OUT.png");
    let w: i32 = args.next().map_or(760, |s| s.parse().unwrap());
    let h: i32 = args.next().map_or(480, |s| s.parse().unwrap());
    let (_config, mut root) = mcfilin::window(start);
    let mut backend = MockBackend::new(w, h).with_scale(2.0);
    if let Some(font) = Font::load_sans() {
        backend = backend.with_sans_font(font);
    }
    backend.render(&mut root); // lays the tree out
    // Optional scripted events from $EVENTS: "down X Y; move X Y; up X Y; ctrlup X Y".
    if let Ok(script) = std::env::var("EVENTS") {
        for cmd in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            let pos = Point::new(parts[1].parse().unwrap(), parts[2].parse().unwrap());
            let mods = Modifiers::default();
            let ev = match parts[0] {
                "down" => Event::PointerDown { pos, button: MouseButton::Left, modifiers: mods },
                "move" => Event::PointerMove { pos },
                "up" => Event::PointerUp { pos, button: MouseButton::Left, modifiers: mods },
                other => panic!("unknown event {other}"),
            };
            backend.dispatch(&mut root, &ev);
        }
    }
    let snap = backend.render_framed(&mut root, &WindowChrome::resizable("McFilin"));
    std::fs::write(out, snap.to_png()).unwrap();
}
