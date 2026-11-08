//! Everything McFilin does to the filesystem: reading a directory into the
//! entries a browser column shows, moving and copying files for a drop,
//! creating folders, and handing a file to the desktop to open.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// One row of a browser column.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The name as the filesystem has it, for joining onto the directory.
    pub name: OsString,
    /// The name as shown (lossy UTF-8).
    pub label: String,
    /// A directory, or a symlink to one: something a column can be opened for.
    pub is_dir: bool,
}

/// Reads `dir` into the entries of a column, sorted case-insensitively the
/// way NeXTSTEP's browser lists them: folders and files mixed, by name.
/// Dot files are left out unless `show_hidden` is set.
pub fn read_entries(dir: &Path, show_hidden: bool) -> io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let label = name.to_string_lossy().into_owned();
            if !show_hidden && label.starts_with('.') {
                return None;
            }
            // `file_type` doesn't follow symlinks, `metadata` does: a link to a
            // directory browses like the directory itself.
            let is_dir = match e.file_type() {
                Ok(t) if t.is_symlink() => fs::metadata(e.path()).is_ok_and(|m| m.is_dir()),
                Ok(t) => t.is_dir(),
                Err(_) => false,
            };
            Some(Entry {
                name,
                label,
                is_dir,
            })
        })
        .collect();
    entries.sort_by_cached_key(|e| (e.label.to_lowercase(), e.label.clone()));
    Ok(entries)
}

/// What a drop does with the files dragged onto a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Move,
    Copy,
}

/// Whether dropping `sources` onto the folder `dest` with `op` makes sense at
/// all: never into a dragged folder itself or below it, and never a move into
/// the folder the file already lives in (copying there duplicates it). Purely
/// lexical, for highlighting drop targets while a drag hovers; [`transfer`]
/// checks again against the real, symlink-resolved paths.
pub fn can_drop(sources: &[PathBuf], dest: &Path, op: Op) -> bool {
    !sources.is_empty()
        && sources.iter().all(|src| {
            !dest.starts_with(src) && (op == Op::Copy || src.parent() != Some(dest))
        })
}

/// Moves or copies each of `sources` into the folder `dest`. Returns the new
/// paths of the items that made it and a message for each that didn't; one
/// failure doesn't stop the others.
pub fn transfer(sources: &[PathBuf], dest: &Path, op: Op) -> (Vec<PathBuf>, Vec<String>) {
    let mut done = Vec::new();
    let mut errors = Vec::new();
    for src in sources {
        match transfer_one(src, dest, op) {
            Ok(Some(target)) => done.push(target),
            Ok(None) => {}
            Err(e) => errors.push(format!("{}: {e}", display_name(src))),
        }
    }
    (done, errors)
}

fn transfer_one(src: &Path, dest: &Path, op: Op) -> io::Result<Option<PathBuf>> {
    let Some(name) = src.file_name() else {
        return Err(io::Error::other("can't move or copy a root directory"));
    };
    let same_dir = src.parent() == Some(dest);
    if same_dir && op == Op::Move {
        return Ok(None); // dropped back where it came from
    }

    // Resolve symlinks before the "into itself" check, so a link pointing into
    // the dragged folder can't send a copy recursing forever. The source's own
    // last component stays unresolved: dragging a symlink moves the link.
    let real_src = src
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.join(name))
        .unwrap_or_else(|| src.to_path_buf());
    let real_dest = dest.canonicalize()?;
    if real_dest.starts_with(&real_src) {
        return Err(io::Error::other("can't put a folder inside itself"));
    }

    let target = if same_dir {
        unique_copy_name(dest, name)
    } else {
        dest.join(name)
    };
    if target.symlink_metadata().is_ok() {
        return Err(io::Error::other(format!(
            "“{}” already exists in “{}”",
            display_name(&target),
            display_name(dest)
        )));
    }

    match op {
        Op::Copy => copy_recursive(src, &target)?,
        Op::Move => match fs::rename(src, &target) {
            Ok(()) => {}
            // Another filesystem: rename can't, so copy and then delete — and
            // only delete once the copy is complete.
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                copy_recursive(src, &target)?;
                remove_recursive(src)?;
            }
            Err(e) => return Err(e),
        },
    }
    Ok(Some(target))
}

/// "notes.txt" copied next to itself becomes "notes copy.txt", then
/// "notes copy 2.txt", and so on.
fn unique_copy_name(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A dot file ("".bashrc") has its whole name as the stem and no extension.
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| {
            let suffix = if n == 1 {
                " copy".to_string()
            } else {
                format!(" copy {n}")
            };
            dir.join(format!("{stem}{suffix}{ext}"))
        })
        .find(|p| p.symlink_metadata().is_err())
        .expect("an unused name")
}

fn copy_recursive(src: &Path, target: &Path) -> io::Result<()> {
    let meta = src.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        let link = fs::read_link(src)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(link, target);
        #[cfg(not(unix))]
        {
            let _ = link;
            return fs::copy(src, target).map(|_| ());
        }
    }
    if meta.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &target.join(entry.file_name()))?;
        }
        fs::set_permissions(target, meta.permissions())?;
        Ok(())
    } else {
        fs::copy(src, target).map(|_| ())
    }
}

fn remove_recursive(path: &Path) -> io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Creates a folder named "New Folder" in `dir` — or "New Folder 2", … if
/// that's taken — and returns its path.
pub fn new_folder(dir: &Path) -> io::Result<PathBuf> {
    for n in 1.. {
        let name = if n == 1 {
            "New Folder".to_string()
        } else {
            format!("New Folder {n}")
        };
        let path = dir.join(name);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Hands `path` to the desktop to open in whatever application it's
/// associated with.
pub fn open_with_system(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The last component of `path`, or the path itself for a root.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A byte count the way people read it: "812 bytes", "4.2 KB", "1.3 GB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcfilin-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sorts_mixed_case_insensitively_and_hides_dot_files() {
        let dir = scratch("sort");
        fs::create_dir(dir.join("beta")).unwrap();
        fs::write(dir.join("Alpha.txt"), "").unwrap();
        fs::write(dir.join("gamma"), "").unwrap();
        fs::write(dir.join(".hidden"), "").unwrap();
        let labels: Vec<_> = read_entries(&dir, false)
            .unwrap()
            .into_iter()
            .map(|e| (e.label, e.is_dir))
            .collect();
        assert_eq!(
            labels,
            [
                ("Alpha.txt".to_string(), false),
                ("beta".to_string(), true),
                ("gamma".to_string(), false)
            ]
        );
        assert_eq!(read_entries(&dir, true).unwrap().len(), 4);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn moves_copies_and_refuses_overwrites() {
        let dir = scratch("transfer");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::write(a.join("sub/file.txt"), "hi").unwrap();
        fs::create_dir(&b).unwrap();

        // Copy a folder tree.
        let (done, errors) = transfer(std::slice::from_ref(&a), &b, Op::Copy);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(done, [b.join("a")]);
        assert_eq!(fs::read_to_string(b.join("a/sub/file.txt")).unwrap(), "hi");

        // Copying again into the same place collides.
        let (_, errors) = transfer(std::slice::from_ref(&a), &b, Op::Copy);
        assert_eq!(errors.len(), 1);

        // Copying next to itself makes "… copy".
        let file = a.join("sub/file.txt");
        let (done, _) = transfer(std::slice::from_ref(&file), &a.join("sub"), Op::Copy);
        assert_eq!(done, [a.join("sub/file copy.txt")]);

        // Moving a folder into itself is refused, and leaves it alone.
        let (_, errors) = transfer(std::slice::from_ref(&a), &a.join("sub"), Op::Move);
        assert_eq!(errors.len(), 1);
        assert!(a.join("sub").is_dir());

        // A real move.
        let (done, errors) = transfer(std::slice::from_ref(&file), &b, Op::Move);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(done, [b.join("file.txt")]);
        assert!(!file.exists());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drop_targets() {
        let src = vec![PathBuf::from("/a/b")];
        assert!(can_drop(&src, Path::new("/a/c"), Op::Move));
        assert!(!can_drop(&src, Path::new("/a"), Op::Move)); // already there
        assert!(can_drop(&src, Path::new("/a"), Op::Copy)); // duplicate
        assert!(!can_drop(&src, Path::new("/a/b"), Op::Copy)); // into itself
        assert!(!can_drop(&src, Path::new("/a/b/c"), Op::Move)); // below itself
        assert!(can_drop(&src, Path::new("/a/bc"), Op::Move));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(812), "812 bytes");
        assert_eq!(human_size(4300), "4.2 KB");
        assert_eq!(human_size(50 * 1024 * 1024), "50 MB");
    }
}
