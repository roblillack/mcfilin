//! The shelf: folders kept at hand above the path view, as on NeXT's File
//! Viewer.
//!
//! The first place on the shelf is always the home folder. The rest are the
//! bookmarks other file managers and the file chooser keep too: the file
//! `$XDG_CONFIG_HOME/gtk-3.0/bookmarks` (by default
//! `~/.config/gtk-3.0/bookmarks`), one `file://` URI per line, optionally
//! followed by a space and a label. Lines for anything else — another host, a
//! network share — are kept as they are but not shown.
//!
//! Without that file the shelf starts out with the XDG user directories
//! (`$XDG_CONFIG_HOME/user-dirs.dirs`) that exist — leaving out the desktop,
//! templates, and public folders, as GTK does when it first fills in the
//! bookmarks — and the file is written only once the shelf is changed. Changes made by other programs are picked
//! up when the file's modification time changes.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::settings::{write, xdg_dir};

/// A folder on the shelf.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub path: PathBuf,
    /// The name the file gives it, if any; otherwise it goes by its own.
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Line {
    Folder(Bookmark),
    /// A line we don't show, written back as it was.
    Other(String),
}

pub struct Shelf {
    lines: Vec<Line>,
    /// The bookmarks file, if there is a config folder at all.
    file: Option<PathBuf>,
    /// The file's modification time when it was read or written.
    stamp: Option<SystemTime>,
}

impl Shelf {
    /// The user's bookmarks, or the XDG user directories if there is no
    /// bookmarks file yet.
    pub fn load() -> Self {
        let config = xdg_dir("XDG_CONFIG_HOME", ".config");
        let home = std::env::home_dir();
        Self::load_from(
            config.as_ref().map(|c| c.join("gtk-3.0/bookmarks")),
            config.as_ref().map(|c| c.join("user-dirs.dirs")),
            home.as_deref(),
        )
    }

    fn load_from(file: Option<PathBuf>, user_dirs: Option<PathBuf>, home: Option<&Path>) -> Self {
        let stamp = file.as_deref().and_then(modified);
        let lines = match file.as_ref().map(std::fs::read_to_string) {
            Some(Ok(text)) => parse(&text),
            _ => {
                let text = user_dirs
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();
                home.map(|home| {
                    parse_user_dirs(&text, home)
                        .into_iter()
                        .filter(|p| p != home && p.is_dir())
                        .map(|path| Line::Folder(Bookmark { path, label: None }))
                        .collect()
                })
                .unwrap_or_default()
            }
        };
        Self { lines, file, stamp }
    }

    /// Re-reads the bookmarks if another program changed them. Returns whether
    /// it did.
    pub fn refresh(&mut self) -> bool {
        let Some(file) = &self.file else { return false };
        if modified(file) == self.stamp {
            return false;
        }
        let user_dirs = file
            .parent()
            .and_then(Path::parent)
            .map(|c| c.join("user-dirs.dirs"));
        *self = Self::load_from(
            self.file.clone(),
            user_dirs,
            std::env::home_dir().as_deref(),
        );
        true
    }

    /// Writes the bookmarks file, through a symbolic link if it is one, so a
    /// file kept with the dotfiles stays where it is.
    pub fn save(&mut self) -> io::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let target = std::fs::canonicalize(file).unwrap_or_else(|_| file.clone());
        write(Some(target), &to_text(&self.lines))?;
        self.stamp = modified(file);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.bookmarks().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bookmarks(&self) -> impl Iterator<Item = &Bookmark> {
        self.lines.iter().filter_map(|l| match l {
            Line::Folder(b) => Some(b),
            Line::Other(_) => None,
        })
    }

    pub fn get(&self, i: usize) -> Option<&Bookmark> {
        self.bookmarks().nth(i)
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.position(path).is_some()
    }

    pub fn position(&self, path: &Path) -> Option<usize> {
        self.bookmarks().position(|b| b.path == path)
    }

    /// The line of the `i`-th bookmark, or past the last bookmark for `len()`.
    fn line_of(&self, i: usize) -> usize {
        let mut seen = 0;
        for (n, line) in self.lines.iter().enumerate() {
            if let Line::Folder(_) = line {
                if seen == i {
                    return n;
                }
                seen += 1;
            }
        }
        self.lines.len()
    }

    /// Puts the folders among `paths` that aren't on the shelf yet on it,
    /// before the `at`-th bookmark. Returns how many were added.
    pub fn insert(&mut self, at: usize, paths: &[PathBuf]) -> usize {
        let mut line = self.line_of(at.min(self.len()));
        let mut added = 0;
        for path in paths {
            if !path.is_absolute() || self.contains(path) {
                continue;
            }
            let bookmark = Bookmark {
                path: path.clone(),
                label: None,
            };
            self.lines.insert(line, Line::Folder(bookmark));
            line += 1;
            added += 1;
        }
        added
    }

    /// Moves the `from`-th bookmark to the gap before the `to`-th (`len()`
    /// being the end). Returns whether that changed anything.
    pub fn move_to(&mut self, from: usize, to: usize) -> bool {
        let n = self.len();
        if from >= n || to > n || to == from || to == from + 1 {
            return false;
        }
        let line = self.lines.remove(self.line_of(from));
        let to = if to > from { to - 1 } else { to };
        let at = self.line_of(to);
        self.lines.insert(at, line);
        true
    }

    pub fn remove(&mut self, i: usize) -> bool {
        if i >= self.len() {
            return false;
        }
        self.lines.remove(self.line_of(i));
        true
    }
}

impl Bookmark {
    /// What the shelf shows under its icon.
    pub fn label(&self) -> String {
        match &self.label {
            Some(l) => l.clone(),
            None => crate::fsops::display_name(&self.path),
        }
    }
}

/// The name of the user running the program, for the home folder's label.
pub fn user_name() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        crate::info::user_name(unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME")
            .ok()
            .or_else(|| std::env::home_dir().map(|h| crate::fsops::display_name(&h)))
            .unwrap_or_else(|| "Home".to_string())
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

// ============================================================================
// The files.
// ============================================================================

fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let (uri, label) = match line.split_once(' ') {
                Some((uri, label)) => (uri, Some(label.trim()).filter(|l| !l.is_empty())),
                None => (line.trim_end(), None),
            };
            match path_from_uri(uri) {
                Some(path) => Line::Folder(Bookmark {
                    path,
                    label: label.map(str::to_string),
                }),
                None => Line::Other(line.to_string()),
            }
        })
        .collect()
}

fn to_text(lines: &[Line]) -> String {
    let mut text = String::new();
    for line in lines {
        match line {
            Line::Folder(b) => {
                text.push_str(&uri_from_path(&b.path));
                if let Some(label) = &b.label {
                    text.push(' ');
                    text.push_str(label);
                }
            }
            Line::Other(raw) => text.push_str(raw),
        }
        text.push('\n');
    }
    text
}

/// The folders `user-dirs.dirs` names, in its order: lines like
/// `XDG_MUSIC_DIR="$HOME/Music"` or `XDG_MUSIC_DIR="/srv/music"`. The
/// desktop, templates, and public folders aren't worth a place on the shelf.
fn parse_user_dirs(text: &str, home: &Path) -> Vec<PathBuf> {
    const SKIP: &[&str] = &["DESKTOP", "TEMPLATES", "PUBLICSHARE"];
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            let name = key.strip_prefix("XDG_")?.strip_suffix("_DIR")?;
            if SKIP.contains(&name) {
                return None;
            }
            let value = unescape(value.trim().strip_prefix('"')?.strip_suffix('"')?);
            if value == "$HOME" {
                Some(home.to_path_buf())
            } else if let Some(rest) = value.strip_prefix("$HOME/") {
                Some(home.join(rest))
            } else {
                Some(PathBuf::from(value)).filter(|p| p.is_absolute())
            }
        })
        .collect()
}

/// Takes the backslashes out of a shell-escaped string.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        out.extend(if c == '\\' { chars.next() } else { Some(c) });
    }
    out
}

/// The path a local `file://` URI names.
fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None; // another host
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let mut iter = rest.bytes();
    while let Some(b) = iter.next() {
        if b == b'%' {
            let hex = [iter.next()?, iter.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    Some(PathBuf::from(os_string(bytes)?))
}

fn uri_from_path(path: &Path) -> String {
    const KEEP: &[u8] = b"-._~/!$&'()*+,;=:@";
    let mut uri = String::from("file://");
    for &b in os_bytes(path).iter() {
        if b.is_ascii_alphanumeric() || KEEP.contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(unix)]
fn os_string(bytes: Vec<u8>) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn os_string(bytes: Vec<u8>) -> Option<OsString> {
    String::from_utf8(bytes).ok().map(OsString::from)
}

#[cfg(unix)]
fn os_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().into()
}

#[cfg(not(unix))]
fn os_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    path.to_string_lossy().into_owned().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcfilin-shelf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(shelf: &Shelf) -> Vec<String> {
        shelf
            .bookmarks()
            .map(|b| b.path.display().to_string())
            .collect()
    }

    #[test]
    fn uris() {
        let p = Path::new("/home/me/My Stuff/50%/ä");
        let uri = uri_from_path(p);
        assert_eq!(uri, "file:///home/me/My%20Stuff/50%25/%C3%A4");
        assert_eq!(path_from_uri(&uri).as_deref(), Some(p));
        assert_eq!(
            path_from_uri("file://localhost/tmp").as_deref(),
            Some(Path::new("/tmp"))
        );
        assert_eq!(path_from_uri("file://server/tmp"), None);
        assert_eq!(path_from_uri("sftp://host/tmp"), None);
        assert_eq!(path_from_uri("file:///bad%2"), None);
    }

    #[test]
    fn keeps_labels_and_other_lines() {
        let text = "file:///home/me/Documents\nsftp://host/srv Server\n\nfile:///home/me/a%20b Work things\n";
        let lines = parse(text);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[2],
            Line::Folder(Bookmark {
                path: PathBuf::from("/home/me/a b"),
                label: Some("Work things".into())
            })
        );
        assert_eq!(to_text(&lines), text.replace("\n\n", "\n"));
    }

    #[test]
    fn reads_user_dirs() {
        let text = "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_MUSIC_DIR=\"/srv/My\\ Music\"\nXDG_VIDEOS_DIR=\"$HOME\"\nXDG_X_DIR=\"relative\"\nOTHER=\"/x\"\n";
        let home = Path::new("/home/me");
        assert_eq!(
            parse_user_dirs(text, home),
            vec![
                PathBuf::from("/home/me/Downloads"),
                PathBuf::from("/srv/My Music"),
                PathBuf::from("/home/me"),
            ]
        );
    }

    #[test]
    fn starts_with_existing_user_dirs_and_writes_only_when_changed() {
        let dir = scratch("defaults");
        let home = dir.join("home");
        std::fs::create_dir_all(home.join("Music")).unwrap();
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        let user_dirs = dir.join("user-dirs.dirs");
        std::fs::write(
            &user_dirs,
            "XDG_DOCUMENTS_DIR=\"$HOME/Documents\"\nXDG_VIDEOS_DIR=\"$HOME/Videos\"\nXDG_DESKTOP_DIR=\"$HOME\"\nXDG_MUSIC_DIR=\"$HOME/Music\"\n",
        )
        .unwrap();
        let file = dir.join("gtk-3.0/bookmarks");
        let mut shelf = Shelf::load_from(Some(file.clone()), Some(user_dirs.clone()), Some(&home));
        let want = |names: &[&str]| -> Vec<String> {
            names
                .iter()
                .map(|n| home.join(n).display().to_string())
                .collect()
        };
        assert_eq!(paths(&shelf), want(&["Documents", "Music"]));
        assert!(!file.exists());

        assert!(shelf.move_to(1, 0));
        shelf.save().unwrap();
        let again = Shelf::load_from(Some(file.clone()), Some(user_dirs), Some(&home));
        assert_eq!(paths(&again), want(&["Music", "Documents"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inserts_moves_and_removes_around_other_lines() {
        let dir = scratch("edit");
        let file = dir.join("bookmarks");
        std::fs::write(&file, "file:///a\nsmb://x/y\nfile:///b\nfile:///c\n").unwrap();
        let mut shelf = Shelf::load_from(Some(file.clone()), None, None);
        assert_eq!(paths(&shelf), ["/a", "/b", "/c"]);

        // Already there, or not absolute: skipped.
        let added = shelf.insert(1, &["/n".into(), "/b".into(), "rel".into(), "/m".into()]);
        assert_eq!(added, 2);
        assert_eq!(paths(&shelf), ["/a", "/n", "/m", "/b", "/c"]);

        assert!(shelf.move_to(0, 5));
        assert_eq!(paths(&shelf), ["/n", "/m", "/b", "/c", "/a"]);
        assert!(shelf.move_to(3, 0));
        assert_eq!(paths(&shelf), ["/c", "/n", "/m", "/b", "/a"]);
        assert!(!shelf.move_to(2, 2) && !shelf.move_to(2, 3) && !shelf.move_to(9, 0));

        assert!(shelf.remove(4));
        assert!(!shelf.remove(4));
        shelf.insert(99, &["/z".into()]);
        assert_eq!(paths(&shelf), ["/c", "/n", "/m", "/b", "/z"]);
        shelf.save().unwrap();
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("smb://x/y\n")
        );

        // Another program changes the file.
        assert!(!shelf.refresh());
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "file:///q\n").unwrap();
        assert!(shelf.refresh());
        assert_eq!(paths(&shelf), ["/q"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
