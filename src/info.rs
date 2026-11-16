//! What the info column says about a selected file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fsops::human_size;
use crate::mime;

/// The facts about one file, gathered once when it is selected.
pub struct FileInfo {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    /// The type's description: "PNG image".
    pub kind: Option<String>,
    pub size: u64,
    pub is_dir: bool,
    /// Modification time in whole seconds since the epoch — what thumbnails
    /// are checked against.
    pub mtime: i64,
    /// Label/value rows, in the order shown.
    pub rows: Vec<(&'static str, String)>,
}

impl FileInfo {
    pub fn gather(path: &Path) -> Option<FileInfo> {
        let link = fs::symlink_metadata(path).ok()?;
        // A dangling symlink still has something to say about itself.
        let meta = fs::metadata(path).unwrap_or_else(|_| link.clone());
        let db = mime::db();
        let mime = if link.file_type().is_symlink() && fs::metadata(path).is_err() {
            "inode/symlink".to_string()
        } else {
            db.guess(path, &meta)
        };
        let kind = db.comment(&mime);
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);

        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if let Some(k) = &kind {
            rows.push(("Kind", k.clone()));
        }
        rows.push(("Type", mime.clone()));
        if !meta.is_dir() {
            let size = meta.len();
            let exact = if size >= 1024 {
                format!(" ({} bytes)", group_digits(size))
            } else {
                String::new()
            };
            rows.push(("Size", format!("{}{exact}", human_size(size))));
        }
        if let Ok(t) = meta.modified() {
            rows.push(("Modified", format_time(t)));
        }
        if let Ok(t) = meta.created() {
            rows.push(("Created", format_time(t)));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            rows.push((
                "Permissions",
                format!("{} ({:o})", mode_string(&link), meta.mode() & 0o7777),
            ));
            rows.push(("Owner", user_name(meta.uid())));
            rows.push(("Group", group_name(meta.gid())));
        }
        #[cfg(not(unix))]
        if meta.permissions().readonly() {
            rows.push(("Attributes", "read-only".to_string()));
        }
        if link.file_type().is_symlink()
            && let Ok(target) = fs::read_link(path)
        {
            rows.push(("Link to", target.display().to_string()));
        }

        Some(FileInfo {
            path: path.to_path_buf(),
            name: crate::fsops::display_name(path),
            mime,
            kind,
            size: meta.len(),
            is_dir: meta.is_dir(),
            mtime,
            rows,
        })
    }
}

/// What the info column says about several selected items.
pub fn summarize(paths: &[PathBuf]) -> Vec<(&'static str, String)> {
    let (mut files, mut dirs, mut bytes) = (0u64, 0u64, 0u64);
    for p in paths {
        match fs::metadata(p) {
            Ok(m) if m.is_dir() => dirs += 1,
            Ok(m) => {
                files += 1;
                bytes += m.len();
            }
            Err(_) => files += 1,
        }
    }
    let mut rows = Vec::new();
    if files > 0 {
        rows.push(("Files", files.to_string()));
        rows.push(("Size", human_size(bytes)));
    }
    if dirs > 0 {
        rows.push(("Folders", dirs.to_string()));
    }
    rows
}

/// 1234567 → "1,234,567".
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A time as local "YYYY-MM-DD HH:MM".
pub fn format_time(t: SystemTime) -> String {
    if let Some((y, mo, d, h, mi, _)) = local_parts(t) {
        return format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}");
    }
    let secs = epoch_secs(t);
    // UTC, from the days since the epoch (Howard Hinnant's civil_from_days).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn epoch_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// A time in the local time zone: year, month, day, hour, minute, second.
/// `None` where the system can't say.
pub fn local_parts(t: SystemTime) -> Option<(i32, i32, i32, i32, i32, i32)> {
    #[cfg(unix)]
    {
        let time = epoch_secs(t) as libc::time_t;
        // SAFETY: `tm` is plain old data that `localtime_r` fills in; both
        // pointers are valid for the call.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
            return Some((
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec,
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = t;
    None
}

/// `ls -l`'s "-rwxr-xr-x".
#[cfg(unix)]
fn mode_string(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let t = meta.file_type();
    let kind = if t.is_symlink() {
        'l'
    } else if t.is_dir() {
        'd'
    } else if t.is_fifo() {
        'p'
    } else if t.is_socket() {
        's'
    } else if t.is_block_device() {
        'b'
    } else if t.is_char_device() {
        'c'
    } else {
        '-'
    };
    let mode = meta.mode();
    let mut s = String::from(kind);
    for (shift, special, special_char) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        let x = bits & 1 != 0;
        s.push(match (mode & special != 0, x) {
            (true, true) => special_char,
            (true, false) => special_char.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

#[cfg(unix)]
pub(crate) fn user_name(uid: u32) -> String {
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: all pointers point at live, correctly sized storage; on success
    // `pw_name` points into `buf`, which outlives its use here.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
        if rc == 0 && !result.is_null() && !pwd.pw_name.is_null() {
            return std::ffi::CStr::from_ptr(pwd.pw_name)
                .to_string_lossy()
                .into_owned();
        }
    }
    uid.to_string()
}

#[cfg(unix)]
fn group_name(gid: u32) -> String {
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: as in `user_name`.
    unsafe {
        let mut grp: libc::group = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let rc = libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut result);
        if rc == 0 && !result.is_null() && !grp.gr_name.is_null() {
            return std::ffi::CStr::from_ptr(grp.gr_name)
                .to_string_lossy()
                .into_owned();
        }
    }
    gid.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits() {
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[test]
    fn gathers_this_file() {
        let info = FileInfo::gather(Path::new(file!())).unwrap();
        assert_eq!(info.name, "info.rs");
        assert!(info.rows.iter().any(|(k, _)| *k == "Size"));
        #[cfg(unix)]
        assert!(
            info.rows
                .iter()
                .any(|(k, v)| *k == "Permissions" && v.starts_with('-'))
        );
    }
}
