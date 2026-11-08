//! What McFilin remembers from one run to the next, in two places:
//!
//! - [`Settings`], what the user chose — whether the shelf and the path view
//!   are shown — in
//!   `$XDG_CONFIG_HOME/mcfilin/settings` (by default
//!   `~/.config/mcfilin/settings`), and
//! - [`WindowState`], what just happens to be so — the size of the window —
//!   in `$XDG_STATE_HOME/mcfilin/window` (by default
//!   `~/.local/state/mcfilin/window`), so that a resize doesn't change a file
//!   people keep with their dotfiles.
//!
//! Both files hold `key = value` lines. Unknown keys and values that don't
//! parse are skipped, so a damaged or older file still loads what it can.
//!
//! Only the window's size is kept, not its position: Wayland doesn't let a
//! program know or choose where its window is, and Saudade doesn't expose it
//! elsewhere either.

use std::io;
use std::path::PathBuf;

/// The user's choices, kept in the config folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub shelf: bool,
    pub path_view: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { shelf: true, path_view: true }
    }
}

impl Settings {
    /// The saved settings, or the defaults where there are none.
    pub fn load() -> Self {
        Self::parse(&read(settings_file()))
    }

    pub fn save(&self) -> io::Result<()> {
        write(settings_file(), &self.to_text())
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for (key, value) in pairs(text) {
            let Ok(v) = value.parse() else { continue };
            match key {
                "shelf" => s.shelf = v,
                "path_view" => s.path_view = v,
                _ => {}
            }
        }
        s
    }

    fn to_text(&self) -> String {
        format!("shelf = {}\npath_view = {}\n", self.shelf, self.path_view)
    }
}

/// How the window was left, kept in the state folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowState {
    /// The inner size of the window, in logical pixels.
    pub size: Option<(i32, i32)>,
}

impl WindowState {
    pub fn load() -> Self {
        Self::parse(&read(state_file()))
    }

    pub fn save(&self) -> io::Result<()> {
        write(state_file(), &self.to_text())
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for (key, value) in pairs(text) {
            if key == "size" {
                s.size = value.split_once('x').and_then(|(w, h)| {
                    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
                    (w > 0 && h > 0).then_some((w, h))
                });
            }
        }
        s
    }

    fn to_text(&self) -> String {
        match self.size {
            Some((w, h)) => format!("size = {w}x{h}\n"),
            None => String::new(),
        }
    }
}

/// The `key = value` lines of `text`, trimmed; other lines are skipped.
fn pairs(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
}

/// The file's text, or nothing if there is no file.
fn read(path: Option<PathBuf>) -> String {
    path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default()
}

/// Writes `text` to `path`, creating the folder if need be. Written to a
/// temporary file first and renamed into place, so a crash halfway never
/// leaves a truncated file.
pub(crate) fn write(path: Option<PathBuf>, text: &str) -> io::Result<()> {
    let Some(path) = path else { return Ok(()) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

/// `$XDG_CONFIG_HOME/mcfilin/settings`, defaulting to `~/.config`.
fn settings_file() -> Option<PathBuf> {
    Some(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("mcfilin/settings"))
}

/// `$XDG_STATE_HOME/mcfilin/window`, defaulting to `~/.local/state`.
fn state_file() -> Option<PathBuf> {
    Some(xdg_dir("XDG_STATE_HOME", ".local/state")?.join("mcfilin/window"))
}

/// The base directory `var` names, or `fallback` in the home folder when it
/// is unset or not absolute, as the XDG Base Directory spec has it.
pub(crate) fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|h| h.join(fallback)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = Settings { shelf: false, path_view: false };
        assert_eq!(Settings::parse(&s.to_text()), s);
        let s = Settings { shelf: false, path_view: true };
        assert_eq!(Settings::parse(&s.to_text()), s);
        assert_eq!(Settings::parse(&Settings::default().to_text()), Settings::default());
        let w = WindowState { size: Some((900, 600)) };
        assert_eq!(WindowState::parse(&w.to_text()), w);
        assert_eq!(WindowState::parse(&WindowState::default().to_text()), WindowState::default());
    }

    #[test]
    fn skips_what_it_cannot_read() {
        let s = Settings::parse("junk\npath_view = maybe\nshelf = 0\nother = 1\n");
        assert_eq!(s, Settings::default());
        let w = WindowState::parse("size = 0x5\nsize\n");
        assert_eq!(w, WindowState::default());
    }
}
