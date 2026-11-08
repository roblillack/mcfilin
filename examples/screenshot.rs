//! Renders the McFilin window offscreen (no display needed) into a PNG, after
//! replaying a few synthetic events, to check how it looks.
//!
//! `cargo run --example screenshot -- PATH OUT.png [WIDTH HEIGHT]`

use std::path::PathBuf;

use saudade::mock::MockBackend;
use saudade::{Accel, Event, Font, Key, Modifiers, MouseButton, Point, Widget, WindowChrome};

fn main() {
    let mut args = std::env::args().skip(1);
    let start = PathBuf::from(args.next().expect("PATH"));
    let out = args.next().expect("OUT.png");
    let w: i32 = args.next().map_or(760, |s| s.parse().unwrap());
    let h: i32 = args.next().map_or(480, |s| s.parse().unwrap());
    let (_config, mut root, _) = mcfilin::window(start, &Default::default(), &Default::default());
    let mut backend = MockBackend::new(w, h).with_scale(2.0);
    if let Some(font) = Font::load_sans() {
        backend = backend.with_sans_font(font);
    }
    backend.render(&mut root); // lays the tree out
    root.focus_first(); // as the runtime does after the first layout
    // Optional scripted events from $EVENTS, separated by ";":
    // "down X Y", "move X Y", "up X Y" for the mouse (prefix the verb with
    // modifiers like "shift+down X Y"), "key [MODS+]KEY" for a key press
    // ("key Shift+Down", "key Ctrl+c", "key F2"), and "type TEXT" for text.
    if let Ok(script) = std::env::var("EVENTS") {
        for cmd in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (verb, arg) = cmd.split_once(' ').unwrap_or((cmd, ""));
            let (mods, verb) = modifiers(verb);
            let events = match verb {
                "key" => {
                    let (mods, key) = modifiers(arg);
                    let key = Accel::from(key).key;
                    let mut evs = vec![Event::KeyDown { key, modifiers: mods }];
                    if let Key::Char(ch) = key
                        && !mods.control
                        && !mods.alt
                    {
                        evs.push(Event::Char { ch, modifiers: mods });
                    }
                    evs
                }
                "type" => arg
                    .chars()
                    .map(|ch| Event::Char { ch, modifiers: Modifiers::default() })
                    .collect(),
                _ => {
                    let mut xy = arg.split_whitespace().map(|n| n.parse().unwrap());
                    let pos = Point::new(xy.next().unwrap(), xy.next().unwrap());
                    let button = MouseButton::Left;
                    vec![match verb {
                        "down" => Event::PointerDown { pos, button, modifiers: mods },
                        "move" => Event::PointerMove { pos },
                        "up" => Event::PointerUp { pos, button, modifiers: mods },
                        other => panic!("unknown event {other}"),
                    }]
                }
            };
            for ev in events {
                backend.dispatch(&mut root, &ev);
                backend.render(&mut root);
            }
        }
    }
    // Let background work (thumbnails) finish, as the live app's ticks would.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    backend.render(&mut root);
    while root.wants_ticks() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        backend.dispatch(&mut root, &Event::Tick);
    }
    let snap = backend.render_framed(&mut root, &WindowChrome::resizable("McFilin"));
    std::fs::write(out, snap.to_png()).unwrap();
}

/// Splits leading "shift+", "ctrl+", "alt+" off `s`.
fn modifiers(mut s: &str) -> (Modifiers, &str) {
    let mut mods = Modifiers::default();
    while let Some((m, rest)) = s.split_once('+').filter(|(_, rest)| !rest.is_empty()) {
        match m.to_ascii_lowercase().as_str() {
            "shift" => mods.shift = true,
            "ctrl" => mods.control = true,
            "alt" => mods.alt = true,
            _ => break,
        }
        s = rest;
    }
    (mods, s)
}
