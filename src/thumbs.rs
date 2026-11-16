//! Thumbnails after the freedesktop.org Thumbnail Managing Standard.
//!
//! A file's thumbnail lives in `$XDG_CACHE_HOME/thumbnails/<size>/` under the
//! MD5 of the file's URI, as a PNG whose `Thumb::MTime` text chunk records the
//! file's modification time; a thumbnail whose `Thumb::MTime` doesn't match is
//! stale. A thumbnail the cache doesn't have yet is made by one of the
//! thumbnailers installed under `<data dir>/thumbnailers/*.thumbnailer` (the
//! same ones GNOME, MATE and friends use), picked by MIME type, and written
//! back to the cache for every other program to share. A file no thumbnailer
//! manages to render gets a marker under `fail/mcfilin-<version>/`, so it isn't
//! tried again until it changes.
//!
//! The work runs on a background thread. [`Thumbnails::get`] answers right
//! away — with the image, "not yet", or "none" — and [`Thumbnails::poll`]
//! picks up what the thread has finished since.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use md5::{Digest, Md5};

use crate::mime;

/// How long a thumbnailer may take before it is given up on.
const THUMBNAILER_TIMEOUT: Duration = Duration::from_secs(15);
/// How many finished thumbnails are kept in memory.
const KEEP: usize = 24;
/// Files larger than this aren't decoded by McFilin itself.
const MAX_SELF_DECODE: u64 = 64 * 1024 * 1024;

/// The sizes the standard defines, each a folder of the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Size {
    Normal,
    Large,
    XLarge,
    XXLarge,
}

impl Size {
    pub fn pixels(self) -> u32 {
        match self {
            Size::Normal => 128,
            Size::Large => 256,
            Size::XLarge => 512,
            Size::XXLarge => 1024,
        }
    }

    fn dir(self) -> &'static str {
        match self {
            Size::Normal => "normal",
            Size::Large => "large",
            Size::XLarge => "x-large",
            Size::XXLarge => "xx-large",
        }
    }

    /// The smallest size at least `px` device pixels across.
    pub fn for_pixels(px: f32) -> Size {
        [Size::Normal, Size::Large, Size::XLarge]
            .into_iter()
            .find(|s| s.pixels() as f32 >= px)
            .unwrap_or(Size::XXLarge)
    }

    /// Where to look in the cache, best first, for a thumbnail of this size:
    /// this size, then larger ones (they scale down fine), then smaller ones.
    fn search_order(self) -> Vec<Size> {
        let all = [Size::Normal, Size::Large, Size::XLarge, Size::XXLarge];
        let i = all.iter().position(|s| *s == self).unwrap_or(1);
        let mut order: Vec<Size> = all[i..].to_vec();
        order.extend(all[..i].iter().rev());
        order
    }
}

/// A decoded image, straight (not premultiplied) RGBA, one `u32` per pixel
/// as `0xAARRGGBB`.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

pub enum Thumb {
    Pending,
    Ready(Rc<Image>),
    None,
}

struct Job {
    key: Key,
    mime: String,
    size: Size,
}

/// A thumbnail is asked for by file and modification time, so a file that
/// changes is looked up afresh.
type Key = (PathBuf, i64);

enum Outcome {
    Image(Image),
    None,
    /// Overtaken by a newer request before it was started.
    Skipped,
}

pub struct Thumbnails {
    states: HashMap<Key, Thumb>,
    /// Keys of the finished thumbnails held in memory, oldest first.
    order: VecDeque<Key>,
    jobs: Sender<Job>,
    results: Receiver<(Key, Outcome)>,
}

impl Default for Thumbnails {
    fn default() -> Self {
        Self::new()
    }
}

impl Thumbnails {
    pub fn new() -> Self {
        let (jobs, job_rx) = channel::<Job>();
        let (result_tx, results) = channel();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || worker(job_rx, result_tx))
            .expect("spawning the thumbnail thread");
        Self {
            states: HashMap::new(),
            order: VecDeque::new(),
            jobs,
            results,
        }
    }

    /// The thumbnail of `path` (of type `mime`, last modified at `mtime`
    /// seconds), starting to make one if nobody has asked yet.
    pub fn get(&mut self, path: &Path, mime: &str, mtime: i64, size: Size) -> &Thumb {
        let key = (path.to_path_buf(), mtime);
        if !self.states.contains_key(&key) {
            let job = Job {
                key: key.clone(),
                mime: mime.to_string(),
                size,
            };
            let state = if self.jobs.send(job).is_ok() {
                Thumb::Pending
            } else {
                Thumb::None
            };
            self.states.insert(key.clone(), state);
        }
        &self.states[&key]
    }

    /// Whether something is still being worked on.
    pub fn pending(&self) -> bool {
        self.states.values().any(|s| matches!(s, Thumb::Pending))
    }

    /// Takes in what the background thread has finished. Returns whether
    /// anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, outcome)) = self.results.try_recv() {
            changed = true;
            match outcome {
                // Asked again later, it is queued anew.
                Outcome::Skipped => {
                    self.states.remove(&key);
                }
                Outcome::None => {
                    self.states.insert(key, Thumb::None);
                }
                Outcome::Image(img) => {
                    self.states.insert(key.clone(), Thumb::Ready(Rc::new(img)));
                    self.order.push_back(key);
                    while self.order.len() > KEEP {
                        if let Some(old) = self.order.pop_front() {
                            self.states.remove(&old);
                        }
                    }
                }
            }
        }
        changed
    }
}

fn worker(jobs: Receiver<Job>, results: Sender<(Key, Outcome)>) {
    while let Ok(mut job) = jobs.recv() {
        // Only the newest request matters: the selection has moved on from
        // the others.
        while let Ok(newer) = jobs.try_recv() {
            if results.send((job.key, Outcome::Skipped)).is_err() {
                return;
            }
            job = newer;
        }
        let outcome = match thumbnail(&job.key.0, &job.mime, job.key.1, job.size) {
            Some(img) => Outcome::Image(img),
            None => Outcome::None,
        };
        if results.send((job.key, outcome)).is_err() {
            return;
        }
    }
}

// ============================================================================
// The standard.
// ============================================================================

/// `$XDG_CACHE_HOME/thumbnails`.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".cache")))?;
    Some(base.join("thumbnails"))
}

/// The `file://` URI of an absolute path, escaped the way GLib's
/// `g_filename_to_uri` does it — the URI's MD5 names the thumbnail, so it has
/// to come out byte for byte the same as everyone else's.
pub fn file_uri(path: &Path) -> String {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = path.to_string_lossy().replace('\\', "/").into_bytes();
    let mut uri = String::from("file://");
    for b in bytes {
        let keep = b.is_ascii_alphanumeric() || b"!$&'()*+,-./:=@_~".contains(&b);
        if keep {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

fn md5_hex(s: &str) -> String {
    Md5::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn thumbnail(path: &Path, mime: &str, mtime: i64, size: Size) -> Option<Image> {
    let cache = cache_dir()?;
    // The standard asks not to make thumbnails of thumbnails.
    if path.starts_with(&cache) {
        return None;
    }
    let uri = file_uri(path);
    let name = format!("{}.png", md5_hex(&uri));
    let mtime_text = mtime.to_string();

    for s in size.search_order() {
        if let Some(img) = load_valid(&cache.join(s.dir()).join(&name), &uri, &mtime_text) {
            return Some(img);
        }
    }
    let fail = cache
        .join("fail")
        .join(concat!("mcfilin-", env!("CARGO_PKG_VERSION")))
        .join(&name);
    if load_valid(&fail, &uri, &mtime_text).is_some() {
        return None;
    }

    let made = run_thumbnailers(path, &uri, mime, size).or_else(|| {
        // Without a thumbnailer for PNGs, read the image ourselves.
        let small = fs::metadata(path).is_ok_and(|m| m.len() <= MAX_SELF_DECODE);
        (mime == "image/png" && small)
            .then(|| decode_png(path).ok().map(|(img, _)| img))
            .flatten()
    });
    match made {
        Some(img) => {
            let img = scale_to_fit(img, size.pixels());
            let target = cache.join(size.dir()).join(&name);
            let _ = save(&target, &img, &uri, &mtime_text);
            Some(img)
        }
        None => {
            // Only a thumbnailer that was there and failed earns a marker;
            // a type nothing renders isn't a failure of this file.
            if thumbnailer_for(mime).is_some() {
                let blank = Image {
                    width: 1,
                    height: 1,
                    pixels: vec![0],
                };
                let _ = save(&fail, &blank, &uri, &mtime_text);
            }
            None
        }
    }
}

/// Reads the thumbnail at `file` if it belongs to `uri` as it was at
/// `mtime`.
fn load_valid(file: &Path, uri: &str, mtime: &str) -> Option<Image> {
    let (img, text) = decode_png(file).ok()?;
    let get = |key: &str| text.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let fresh = get("Thumb::MTime") == Some(mtime);
    let ours = get("Thumb::URI").is_none_or(|u| u == uri);
    (fresh && ours).then_some(img)
}

/// Writes `img` as a thumbnail with the metadata the standard requires,
/// atomically and readable only by the user.
fn save(target: &Path, img: &Image, uri: &str, mtime: &str) -> io::Result<()> {
    let dir = target
        .parent()
        .ok_or_else(|| io::Error::other("no parent"))?;
    create_private_dir(dir)?;
    let tmp = dir.join(format!(
        ".{}.mcfilin-{}.tmp",
        target.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    {
        let file = io::BufWriter::new(fs::File::create(&tmp)?);
        let mut enc = png::Encoder::new(file, img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let text = |k: &str, v: &str| (k.to_string(), v.to_string());
        for (k, v) in [
            text("Thumb::URI", uri),
            text("Thumb::MTime", mtime),
            text("Software", concat!("McFilin ", env!("CARGO_PKG_VERSION"))),
        ] {
            enc.add_text_chunk(k, v).map_err(io::Error::other)?;
        }
        let mut writer = enc.write_header().map_err(io::Error::other)?;
        let data: Vec<u8> = img
            .pixels
            .iter()
            .flat_map(|&p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8])
            .collect();
        writer.write_image_data(&data).map_err(io::Error::other)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, target)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)
}

/// A PNG's text chunks, as keyword/text pairs.
type TextChunks = Vec<(String, String)>;

/// Decodes a PNG into RGBA pixels plus its text chunks.
fn decode_png(file: &Path) -> Result<(Image, TextChunks), Box<dyn std::error::Error>> {
    let mut decoder = png::Decoder::new(io::BufReader::new(fs::File::open(file)?));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf)?;
    // Text chunks may come after the image data.
    let _ = reader.finish();
    let (w, h) = (frame.width, frame.height);
    let data = &buf[..frame.buffer_size()];
    let pixels: Vec<u32> = match frame.color_type {
        png::ColorType::Rgba => data
            .chunks_exact(4)
            .map(|c| argb(c[3], c[0], c[1], c[2]))
            .collect(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .map(|c| argb(255, c[0], c[1], c[2]))
            .collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .map(|c| argb(c[1], c[0], c[0], c[0]))
            .collect(),
        png::ColorType::Grayscale => data.iter().map(|&g| argb(255, g, g, g)).collect(),
        png::ColorType::Indexed => return Err("unexpanded palette".into()),
    };
    let info = reader.info();
    let mut text: TextChunks = info
        .uncompressed_latin1_text
        .iter()
        .map(|t| (t.keyword.clone(), t.text.clone()))
        .collect();
    for t in &info.compressed_latin1_text {
        if let Ok(v) = t.get_text() {
            text.push((t.keyword.clone(), v));
        }
    }
    for t in &info.utf8_text {
        if let Ok(v) = t.get_text() {
            text.push((t.keyword.clone(), v));
        }
    }
    Ok((
        Image {
            width: w,
            height: h,
            pixels,
        },
        text,
    ))
}

fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Shrinks `img` (never enlarges it) to fit a `max`×`max` square, averaging
/// the source pixels each target pixel covers.
fn scale_to_fit(img: Image, max: u32) -> Image {
    if img.width <= max && img.height <= max {
        return img;
    }
    let f = max as f64 / img.width.max(img.height) as f64;
    let w = ((img.width as f64 * f).round() as u32).max(1);
    let h = ((img.height as f64 * f).round() as u32).max(1);
    let mut pixels = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let y0 = (y as u64 * img.height as u64 / h as u64) as u32;
        let y1 = (((y + 1) as u64 * img.height as u64 / h as u64) as u32).max(y0 + 1);
        for x in 0..w {
            let x0 = (x as u64 * img.width as u64 / w as u64) as u32;
            let x1 = (((x + 1) as u64 * img.width as u64 / w as u64) as u32).max(x0 + 1);
            // Average with alpha weighting, so transparent pixels don't
            // darken the edges.
            let (mut a, mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = img.pixels[(sy * img.width + sx) as usize];
                    let pa = (p >> 24) as u64;
                    a += pa;
                    r += pa * ((p >> 16) & 0xff) as u64;
                    g += pa * ((p >> 8) & 0xff) as u64;
                    b += pa * (p & 0xff) as u64;
                    n += 1;
                }
            }
            pixels.push(if a == 0 {
                0
            } else {
                argb((a / n) as u8, (r / a) as u8, (g / a) as u8, (b / a) as u8)
            });
        }
    }
    Image {
        width: w,
        height: h,
        pixels,
    }
}

// ============================================================================
// Thumbnailers.
// ============================================================================

struct Thumbnailer {
    exec: Vec<String>,
    mime_types: Vec<String>,
}

/// The installed thumbnailers whose program is there, from the most
/// important data directory to the least.
fn thumbnailers() -> &'static [Thumbnailer] {
    static ALL: OnceLock<Vec<Thumbnailer>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for dir in mime::data_dirs() {
            let Ok(entries) = fs::read_dir(dir.join("thumbnailers")) else {
                continue;
            };
            let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            files.sort();
            for file in files {
                // A file in a more important directory hides one of the same
                // name in a less important one.
                if file.extension().is_none_or(|e| e != "thumbnailer")
                    || !seen.insert(file.file_name().map(|n| n.to_owned()))
                {
                    continue;
                }
                if let Some(t) = fs::read_to_string(&file)
                    .ok()
                    .and_then(|s| parse_thumbnailer(&s))
                {
                    out.push(t);
                }
            }
        }
        out
    })
}

fn parse_thumbnailer(text: &str) -> Option<Thumbnailer> {
    let mut in_section = false;
    let (mut exec, mut try_exec, mut mimes) = (None, None, None);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[Thumbnailer Entry]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Exec" => exec = Some(v.trim().to_string()),
                "TryExec" => try_exec = Some(v.trim().to_string()),
                "MimeType" => mimes = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    let exec = split_exec(&exec?);
    let program = try_exec.unwrap_or_else(|| exec.first().cloned().unwrap_or_default());
    if !on_path(&program) {
        return None;
    }
    Some(Thumbnailer {
        exec,
        mime_types: mimes?
            .split(';')
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

/// Splits an `Exec` line into arguments, honoring double quotes.
fn split_exec(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if has || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        args.push(cur);
    }
    args
}

fn on_path(program: &str) -> bool {
    let p = Path::new(program);
    if p.is_absolute() {
        return p.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// The thumbnailer for `mime`: one naming the type itself rather than a
/// wildcard, and the type itself rather than a parent type.
fn thumbnailer_for(mime: &str) -> Option<&'static Thumbnailer> {
    let all = thumbnailers();
    let ancestors = mime::db().ancestors(mime);
    // Never by the catch-all parents: no thumbnailer renders any old file.
    for m in ancestors
        .iter()
        .filter(|m| *m != "application/octet-stream" && *m != "text/plain")
    {
        if let Some(t) = all.iter().find(|t| t.mime_types.iter().any(|x| x == m)) {
            return Some(t);
        }
        if let Some(t) = all.iter().find(|t| {
            t.mime_types
                .iter()
                .any(|x| x.contains('*') && mime::glob_match(x, m))
        }) {
            return Some(t);
        }
    }
    None
}

fn run_thumbnailers(path: &Path, uri: &str, mime: &str, size: Size) -> Option<Image> {
    let t = thumbnailer_for(mime)?;
    let out = std::env::temp_dir().join(format!(
        "mcfilin-thumb-{}-{}.png",
        std::process::id(),
        md5_hex(uri)
    ));
    let args: Vec<String> = t
        .exec
        .iter()
        .map(|a| expand(a, path, uri, &out, size))
        .collect();
    let (program, rest) = args.split_first()?;
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed() < THUMBNAILER_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let img = ok
        .then(|| decode_png(&out).ok().map(|(img, _)| img))
        .flatten();
    let _ = fs::remove_file(&out);
    img
}

/// Fills in the `%` codes of a thumbnailer argument.
fn expand(arg: &str, path: &Path, uri: &str, out: &Path, size: Size) -> String {
    let mut s = String::new();
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            s.push(c);
            continue;
        }
        match chars.next() {
            Some('i') => s.push_str(&path.to_string_lossy()),
            Some('u') => s.push_str(uri),
            Some('o') => s.push_str(&out.to_string_lossy()),
            Some('s') => s.push_str(&size.pixels().to_string()),
            Some('%') => s.push('%'),
            Some(other) => {
                s.push('%');
                s.push(other);
            }
            None => s.push('%'),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_and_hashes_match_the_standard() {
        // The example from the Thumbnail Managing Standard.
        let uri = file_uri(Path::new("/home/jens/photos/me.png"));
        assert_eq!(uri, "file:///home/jens/photos/me.png");
        assert_eq!(md5_hex(&uri), "c6ee772d9e49320e97ec29a7eb5b1697");
        assert_eq!(
            file_uri(Path::new("/tmp/a b#c%d/ü")),
            "file:///tmp/a%20b%23c%25d/%C3%BC"
        );
    }

    #[test]
    fn exec_lines() {
        assert_eq!(
            split_exec(r#"foo -s %s "%u" %o"#),
            ["foo", "-s", "%s", "%u", "%o"]
        );
        let e = expand(
            "--size=%s",
            Path::new("/a"),
            "file:///a",
            Path::new("/o"),
            Size::Large,
        );
        assert_eq!(e, "--size=256");
    }

    #[test]
    fn saves_and_validates_thumbnails() {
        let dir = std::env::temp_dir().join(format!("mcfilin-thumbs-{}", std::process::id()));
        let file = dir.join("t.png");
        let img = Image {
            width: 300,
            height: 150,
            pixels: vec![0xff00ff00; 300 * 150],
        };
        let small = scale_to_fit(img, 128);
        assert_eq!((small.width, small.height), (128, 64));
        save(&file, &small, "file:///x", "42").unwrap();
        assert!(load_valid(&file, "file:///x", "42").is_some());
        assert!(load_valid(&file, "file:///x", "43").is_none());
        assert!(load_valid(&file, "file:///y", "42").is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn size_search_order() {
        assert_eq!(
            Size::Large.search_order(),
            [Size::Large, Size::XLarge, Size::XXLarge, Size::Normal]
        );
        assert_eq!(Size::for_pixels(300.0), Size::XLarge);
    }
}
