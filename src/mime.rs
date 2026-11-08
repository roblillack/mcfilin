//! File types after the freedesktop.org Shared MIME-info Database: the
//! `globs2`, `subclasses` and `aliases` files and the per-type XML files with
//! their human-readable descriptions, read from `$XDG_DATA_HOME/mime` and each
//! of `$XDG_DATA_DIRS/mime`.
//!
//! Files are typed by name first (the globs). Only a file no glob matches has
//! its first bytes looked at, and only for a handful of well-known signatures
//! and the text/binary distinction — the database's own `magic` rules are not
//! evaluated.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The database, loaded on first use.
pub fn db() -> &'static MimeDb {
    static DB: OnceLock<MimeDb> = OnceLock::new();
    DB.get_or_init(MimeDb::load)
}

/// The XDG base directories for data, most important first:
/// `$XDG_DATA_HOME`, then each of `$XDG_DATA_DIRS`.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(home) => dirs.push(PathBuf::from(home)),
        None => {
            if let Some(home) = std::env::home_dir() {
                dirs.push(home.join(".local/share"));
            }
        }
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs
}

struct Glob {
    weight: u32,
    mime: String,
    pattern: String,
    case_sensitive: bool,
}

pub struct MimeDb {
    globs: Vec<Glob>,
    /// Type → the types it is a subclass of.
    parents: HashMap<String, Vec<String>>,
    /// Alias → canonical type.
    aliases: HashMap<String, String>,
}

impl MimeDb {
    fn load() -> Self {
        let mut db = MimeDb {
            globs: Vec::new(),
            parents: HashMap::new(),
            aliases: HashMap::new(),
        };
        for dir in data_dirs() {
            let mime = dir.join("mime");
            if let Ok(text) = fs::read_to_string(mime.join("globs2")) {
                db.globs.extend(text.lines().filter_map(parse_glob));
            }
            if let Ok(text) = fs::read_to_string(mime.join("subclasses")) {
                for (child, parent) in text.lines().filter_map(|l| l.split_once(' ')) {
                    let list = db.parents.entry(child.to_string()).or_default();
                    if !list.iter().any(|p| p == parent) {
                        list.push(parent.to_string());
                    }
                }
            }
            if let Ok(text) = fs::read_to_string(mime.join("aliases")) {
                for (alias, canonical) in text.lines().filter_map(|l| l.split_once(' ')) {
                    db.aliases
                        .entry(alias.to_string())
                        .or_insert_with(|| canonical.to_string());
                }
            }
        }
        db
    }

    /// The canonical name of `mime`, resolving aliases.
    pub fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map_or(mime, String::as_str)
    }

    /// The type of the file at `path`, whose (symlink-followed) metadata is
    /// `meta`.
    pub fn guess(&self, path: &Path, meta: &fs::Metadata) -> String {
        if let Some(special) = special_type(meta) {
            return special.to_string();
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(mime) = self.guess_by_name(&name) {
            return self.canonical(mime).to_string();
        }
        if meta.len() == 0 {
            return "application/x-zerosize".to_string();
        }
        sniff(path).to_string()
    }

    /// The type the globs give a file named `name`: the matching pattern of
    /// the highest weight, the longest pattern among equals.
    fn guess_by_name(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.globs
            .iter()
            .filter(|g| {
                if g.case_sensitive {
                    glob_match(&g.pattern, name)
                } else {
                    glob_match(&g.pattern, &lower)
                }
            })
            .max_by_key(|g| (g.weight, g.pattern.len()))
            .map(|g| g.mime.as_str())
    }

    /// `mime` and everything it is a kind of, nearest first: its declared
    /// parent types, then `text/plain` for any text type, then
    /// `application/octet-stream` for any file at all.
    pub fn ancestors(&self, mime: &str) -> Vec<String> {
        let mut out = vec![self.canonical(mime).to_string()];
        let mut i = 0;
        while i < out.len() {
            if let Some(parents) = self.parents.get(&out[i]) {
                for p in parents {
                    let p = self.canonical(p).to_string();
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
            i += 1;
        }
        let mut implicit = Vec::new();
        if out[0].starts_with("text/") {
            implicit.push("text/plain");
        }
        if !out[0].starts_with("inode/") {
            implicit.push("application/octet-stream");
        }
        for m in implicit {
            if !out.iter().any(|o| o == m) {
                out.push(m.to_string());
            }
        }
        out
    }

    /// The description of `mime` the database has in English — "Rust source
    /// code", "PNG image".
    pub fn comment(&self, mime: &str) -> Option<String> {
        let mime = self.canonical(mime);
        data_dirs().into_iter().find_map(|dir| {
            let xml = fs::read_to_string(dir.join("mime").join(format!("{mime}.xml"))).ok()?;
            // The untranslated <comment> is the one without an xml:lang.
            let start = xml.find("<comment>")? + "<comment>".len();
            let end = start + xml[start..].find("</comment>")?;
            Some(unescape(&xml[start..end]))
        })
    }
}

fn parse_glob(line: &str) -> Option<Glob> {
    if line.starts_with('#') {
        return None;
    }
    let mut parts = line.splitn(4, ':');
    let weight = parts.next()?.parse().ok()?;
    let mime = parts.next()?.to_string();
    let pattern = parts.next()?.to_string();
    let flags = parts.next().unwrap_or("");
    if mime == "__NOGLOBS__" {
        return None;
    }
    let case_sensitive = flags.split(',').any(|f| f == "cs");
    Some(Glob {
        weight,
        mime,
        pattern: if case_sensitive {
            pattern
        } else {
            pattern.to_lowercase()
        },
        case_sensitive,
    })
}

fn special_type(meta: &fs::Metadata) -> Option<&'static str> {
    if meta.is_dir() {
        return Some("inode/directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let t = meta.file_type();
        if t.is_fifo() {
            return Some("inode/fifo");
        }
        if t.is_socket() {
            return Some("inode/socket");
        }
        if t.is_block_device() {
            return Some("inode/blockdevice");
        }
        if t.is_char_device() {
            return Some("inode/chardevice");
        }
    }
    None
}

/// Types a file no glob matches by its first bytes: a few common signatures,
/// otherwise text if it reads as UTF-8 without NULs, otherwise binary.
fn sniff(path: &Path) -> &'static str {
    let mut buf = [0u8; 512];
    let n = fs::File::open(path)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    let head = &buf[..n];
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF8", "image/gif"),
        (b"%PDF-", "application/pdf"),
        (b"PK\x03\x04", "application/zip"),
        (b"\x1f\x8b", "application/gzip"),
        (b"\x7fELF", "application/x-executable"),
        (b"#!/bin/sh", "application/x-shellscript"),
        (b"#!/bin/bash", "application/x-shellscript"),
        (b"#!/usr/bin/env bash", "application/x-shellscript"),
    ];
    if let Some((_, mime)) = SIGNATURES.iter().find(|(sig, _)| head.starts_with(sig)) {
        return mime;
    }
    let text = !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            // A multi-byte character cut off by the end of the buffer is fine.
            Err(e) => e.error_len().is_none(),
        };
    if text {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Shell-style wildcard matching: `*`, `?` and `[…]` classes (with ranges
/// and `!` negation).
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    // Where to resume after the last `*`: (pattern index, name index).
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi + 1, ni));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, len)) = match_class(&p[pi..], n[ni])
                        && matched
                    {
                        pi += len;
                        ni += 1;
                        continue;
                    }
                }
                c if c == n[ni] => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, sn)) => {
                pi = sp;
                ni = sn + 1;
                star = Some((sp, sn + 1));
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Matches `c` against the class at the start of `p` (which begins with
/// `[`). Returns whether it matched and how long the class is, or `None` for
/// an unterminated class.
fn match_class(p: &[char], c: char) -> Option<(bool, usize)> {
    let mut i = 1;
    let negate = matches!(p.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            matched |= p[i] <= c && c <= p[i + 2];
            i += 3;
        } else {
            matched |= p[i] == c;
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "main.rsx"));
        assert!(glob_match("makefile", "makefile"));
        assert!(glob_match("*.[ch]", "a.h"));
        assert!(!glob_match("*.[!ch]", "a.h"));
        assert!(glob_match("readme*", "readme.md"));
        assert!(glob_match("*.tar.*", "x.tar.gz"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[test]
    fn system_database() {
        // Only meaningful where shared-mime-info is installed.
        let db = db();
        if db.globs.is_empty() {
            return;
        }
        let meta = fs::metadata(file!()).unwrap();
        assert_eq!(db.guess(Path::new("x.png"), &meta), "image/png");
        assert_eq!(db.guess(Path::new("X.PNG"), &meta), "image/png");
        assert!(db.ancestors("text/x-csrc").contains(&"text/plain".to_string()));
        assert!(db.comment("image/png").is_some());
    }
}
