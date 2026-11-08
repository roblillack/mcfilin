//! Moving files to the desktop's trash, so a deletion can be undone.
//!
//! On Linux and the BSDs this follows the FreeDesktop.org Trash
//! Specification: a file goes to the home trash (`$XDG_DATA_HOME/Trash`) when
//! it lives on the same filesystem, and otherwise to the trash at the top of
//! its own filesystem (`.Trash/$uid`, or `.Trash-$uid`), so it is never copied
//! across devices. Next to it in `info/` goes a `.trashinfo` file saying where
//! it came from and when, which is what lets file managers put it back.
//!
//! On macOS files go to `~/.Trash`. Elsewhere there is no trash, and
//! [`trash`] says so.

use std::io;
use std::path::Path;

/// Moves `path` to the trash. A symlink is trashed itself, not what it points
/// to.
pub fn trash(path: &Path) -> io::Result<()> {
    imp::trash(path)
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::ffi::OsStr;
    use std::fs;
    use std::io::{self, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::{Path, PathBuf};

    pub fn trash(path: &Path) -> io::Result<()> {
        let path = absolute(path)?;
        let dev = path.symlink_metadata()?.dev();

        if let Some(home) = home_trash() {
            make_trash_dir(&home)?;
            if fs::metadata(&home)?.dev() == dev {
                return trash_into(&home, &path, &path);
            }
        }

        // Another filesystem: its own trash, at the top of it, where the
        // trash info records the path relative to that top.
        let top = mount_point(&path, dev);
        let relative = path.strip_prefix(&top).unwrap_or(&path).to_path_buf();
        // SAFETY: getuid has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        if let Some(shared) = shared_trash(&top, uid)
            && trash_into(&shared, &path, &relative).is_ok()
        {
            return Ok(());
        }
        let own = top.join(format!(".Trash-{uid}"));
        make_trash_dir(&own)?;
        trash_into(&own, &path, &relative)
    }

    /// `$XDG_DATA_HOME/Trash`, defaulting to `~/.local/share/Trash`.
    fn home_trash() -> Option<PathBuf> {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::home_dir().map(|h| h.join(".local/share")))?;
        Some(data.join("Trash"))
    }

    /// `$top/.Trash/$uid`, if the administrator has set up `$top/.Trash` the
    /// way the specification asks: a real directory with the sticky bit set.
    fn shared_trash(top: &Path, uid: u32) -> Option<PathBuf> {
        let dir = top.join(".Trash");
        let meta = dir.symlink_metadata().ok()?;
        if !meta.is_dir() || meta.mode() & 0o1000 == 0 {
            return None;
        }
        let trash = dir.join(uid.to_string());
        make_trash_dir(&trash).ok()?;
        Some(trash)
    }

    /// The path with its folder made absolute and free of symlinks, but its
    /// last component kept as it is, so a symlink stays the thing trashed.
    fn absolute(path: &Path) -> io::Result<PathBuf> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("can't move a root folder to the trash"))?;
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.canonicalize()?,
            _ => std::env::current_dir()?,
        };
        Ok(parent.join(name))
    }

    /// The topmost folder above `path` that is still on device `dev`.
    fn mount_point(path: &Path, dev: u64) -> PathBuf {
        let mut top = path.parent().unwrap_or(path).to_path_buf();
        while let Some(up) = top.parent() {
            match fs::metadata(up) {
                Ok(m) if m.dev() == dev => top = up.to_path_buf(),
                _ => break,
            }
        }
        top
    }

    fn make_trash_dir(trash: &Path) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(trash.join("files"))?;
        builder.create(trash.join("info"))
    }

    /// Moves `path` into the trash at `trash`, recording `original` as where
    /// it came from.
    pub(super) fn trash_into(trash: &Path, path: &Path, original: &Path) -> io::Result<()> {
        let name = path.file_name().expect("checked by `absolute`");
        let info = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            percent_encode(original.as_os_str()),
            deletion_date()
        );
        // Claim a name by creating its info file exclusively: "notes.txt",
        // else "notes.2.txt", "notes.3.txt", …
        for n in 1.. {
            let candidate = numbered(name, n);
            let mut info_name = candidate.clone();
            info_name.push(".trashinfo");
            let info_path = trash.join("info").join(&info_name);
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&info_path)
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            let target = trash.join("files").join(&candidate);
            if target.symlink_metadata().is_ok() {
                // A stray file without its info; leave it and its name be.
                drop(file);
                let _ = fs::remove_file(&info_path);
                continue;
            }
            let moved = file
                .write_all(info.as_bytes())
                .and_then(|()| fs::rename(path, &target));
            if let Err(e) = moved {
                let _ = fs::remove_file(&info_path);
                return Err(e);
            }
            return Ok(());
        }
        unreachable!()
    }

    /// "notes.txt" for 1, "notes.2.txt" for 2, and so on.
    fn numbered(name: &OsStr, n: u32) -> std::ffi::OsString {
        if n == 1 {
            return name.to_os_string();
        }
        let p = Path::new(name);
        let mut out = p.file_stem().unwrap_or(name).to_os_string();
        out.push(format!(".{n}"));
        if let Some(ext) = p.extension() {
            out.push(".");
            out.push(ext);
        }
        out
    }

    /// A path as the trash info file wants it: bytes outside the unreserved
    /// URI characters escaped as `%XX`, slashes kept.
    fn percent_encode(s: &OsStr) -> String {
        let mut out = String::new();
        for &b in s.as_bytes() {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    /// The local time now as "YYYY-MM-DDThh:mm:ss".
    fn deletion_date() -> String {
        let now = std::time::SystemTime::now();
        crate::info::local_parts(now).map_or_else(
            || "1970-01-01T00:00:00".to_string(),
            |(y, mo, d, h, mi, s)| format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn encodes_paths() {
            assert_eq!(
                percent_encode(OsStr::new("/a b/ü.txt")),
                "/a%20b/%C3%BC.txt"
            );
        }

        #[test]
        fn numbers_names() {
            assert_eq!(numbered(OsStr::new("a.txt"), 1), "a.txt");
            assert_eq!(numbered(OsStr::new("a.txt"), 3), "a.3.txt");
            assert_eq!(numbered(OsStr::new(".bashrc"), 2), ".bashrc.2");
        }

        #[test]
        fn trashes_with_info_and_unique_names() {
            let dir = std::env::temp_dir().join(format!("mcfilin-trash-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let trash = dir.join("Trash");
            make_trash_dir(&trash).unwrap();
            for _ in 0..2 {
                fs::write(dir.join("doc.txt"), "x").unwrap();
                trash_into(&trash, &dir.join("doc.txt"), &dir.join("doc.txt")).unwrap();
                assert!(!dir.join("doc.txt").exists());
            }
            assert!(trash.join("files/doc.txt").is_file());
            assert!(trash.join("files/doc.2.txt").is_file());
            let info = fs::read_to_string(trash.join("info/doc.2.txt.trashinfo")).unwrap();
            assert!(info.starts_with("[Trash Info]\nPath=/"), "{info}");
            assert!(info.contains("/doc.txt\nDeletionDate="), "{info}");
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::fs;
    use std::io;
    use std::path::Path;

    pub fn trash(path: &Path) -> io::Result<()> {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("can't move a root folder to the trash"))?;
        let trash = std::env::home_dir()
            .ok_or_else(|| io::Error::other("no home folder"))?
            .join(".Trash");
        let p = Path::new(name);
        let stem = p.file_stem().unwrap_or(name).to_string_lossy().into_owned();
        let ext = p
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let target = (1..)
            .map(|n| match n {
                1 => trash.join(name),
                n => trash.join(format!("{stem} {n}{ext}")),
            })
            .find(|t| t.symlink_metadata().is_err())
            .expect("an unused name");
        fs::rename(path, target)
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;

    pub fn trash(_path: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "there is no trash on this system",
        ))
    }
}
