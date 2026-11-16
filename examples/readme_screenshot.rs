//! Renders the README's screenshot, `docs/screenshot.png`, from the real
//! McFilin window.
//!
//! It makes up a home folder under the system's temp folder — a few folders,
//! documents and pictures, a shelf of bookmarks, all with fixed modification
//! times — points `$HOME` and the XDG folders at it, and opens the window on a
//! picture in it. The window is drawn offscreen through Saudade's
//! `MockBackend`, so no display is needed, wrapped in window chrome (title
//! bar, frame and drop shadow) and captured at 2× for hi-DPI displays. Text is
//! set in the system's sans-serif font.
//!
//! Run it from anywhere to refresh the image:
//!
//! ```sh
//! cargo run --example readme_screenshot
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use saudade::mock::MockBackend;
use saudade::{Event, Font, Widget, WindowChrome};

const WINDOW_W: i32 = 800;
const WINDOW_H: i32 = 440;
const SCALE: f32 = 2.0;

/// Every file's modification time: 2026-10-12 09:41 UTC, plus a few minutes
/// per file so that they don't all look the same.
const MTIME: u64 = 1_791_884_460;

fn main() {
    let root = std::env::temp_dir().join("mcfilin-demo");
    if root.exists() {
        fs::remove_dir_all(&root).expect("clear the old fixture");
    }
    let home = root.join("home").join(mcfilin::shelf::user_name());
    build_home(&home).expect("build the fixture home folder");

    // SAFETY: nothing else runs yet, so nothing reads the environment while
    // it changes.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
        std::env::set_var("XDG_CACHE_HOME", home.join(".cache"));
        std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
        std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
    }

    let start = home.join("Pictures/Holiday/Harbour at Dusk.png");
    let (_config, mut window, _) = mcfilin::window(start, &Default::default(), &Default::default());
    let mut backend = MockBackend::new(WINDOW_W, WINDOW_H).with_scale(SCALE);
    if let Some(font) = Font::load_sans() {
        backend = backend.with_sans_font(font);
    }
    backend.render(&mut window); // lays the tree out
    window.focus_first(); // as the runtime does after the first layout

    // Let the thumbnail finish, as the live app's ticks would.
    let deadline = Instant::now() + Duration::from_secs(20);
    backend.render(&mut window);
    while window.wants_ticks() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        backend.dispatch(&mut window, &Event::Tick);
    }

    let png = backend
        .render_framed(&mut window, &WindowChrome::resizable("McFilin"))
        .to_png();
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/screenshot.png");
    fs::create_dir_all(out.parent().unwrap()).expect("create docs/");
    fs::write(&out, png).expect("write the screenshot");
    println!("wrote {}", out.display());

    let _ = fs::remove_dir_all(&root);
}

/// The made-up home folder: what's in it, and the shelf's bookmarks.
fn build_home(home: &Path) -> io::Result<()> {
    let mut files = Files::default();
    for dir in ["Desktop", "Downloads", "Music", "Videos"] {
        files.dir(&home.join(dir))?;
    }
    let docs = home.join("Documents");
    files.text(
        &docs.join("Letters/To the Landlord.txt"),
        "Dear Mr. Hale,\n",
    )?;
    files.text(
        &docs.join("Letters/Thank You, Aunt Vera.txt"),
        "Dear Vera,\n",
    )?;
    files.text(&docs.join("Recipes/Plum Cake.md"), "# Plum Cake\n")?;
    files.text(&docs.join("Recipes/Lentil Soup.md"), "# Lentil Soup\n")?;
    files.text(
        &docs.join("Garden Plan.txt"),
        "Tomatoes by the south wall.\n",
    )?;
    files.text(&docs.join("Reading List.md"), "- The Rings of Saturn\n")?;
    files.text(&docs.join("Budget 2026.csv"), "month,rent,food\n")?;
    files.text(&home.join("Music/Playlist.m3u"), "#EXTM3U\n")?;
    files.text(&home.join("Downloads/manual.txt"), "Read me first.\n")?;

    let pics = home.join("Pictures");
    files.dir(&pics.join("Family"))?;
    files.dir(&pics.join("Scans"))?;
    let holiday = pics.join("Holiday");
    files.picture(&holiday.join("Beach at Noon.png"), Scene::NOON)?;
    files.picture(&holiday.join("Harbour at Dusk.png"), Scene::DUSK)?;
    files.picture(&holiday.join("Lighthouse.png"), Scene::GREY)?;
    files.picture(&holiday.join("Morning Swim.png"), Scene::MORNING)?;
    files.picture(&holiday.join("Night Ferry.png"), Scene::NIGHT)?;
    files.picture(&pics.join("Wallpaper.png"), Scene::MORNING)?;

    let bookmarks: String = ["Documents", "Pictures", "Music", "Downloads"]
        .iter()
        .map(|d| format!("{}\n", mcfilin::thumbs::file_uri(&home.join(d))))
        .collect();
    fs::create_dir_all(home.join(".config/gtk-3.0"))?;
    fs::write(home.join(".config/gtk-3.0/bookmarks"), bookmarks)?;

    // Folders last, as filling them in changed their times.
    for dir in std::mem::take(&mut files.dirs).iter().rev() {
        files.stamp(dir)?;
    }
    Ok(())
}

/// Makes files and folders, each with its own fixed modification time.
#[derive(Default)]
struct Files {
    count: u64,
    dirs: Vec<PathBuf>,
}

impl Files {
    fn dir(&mut self, path: &Path) -> io::Result<()> {
        for dir in path.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if !dir.exists() {
                fs::create_dir(dir)?;
                self.dirs.push(dir.to_path_buf());
            }
        }
        Ok(())
    }

    fn text(&mut self, path: &Path, text: &str) -> io::Result<()> {
        self.dir(path.parent().unwrap())?;
        fs::write(path, text)?;
        self.stamp(path)
    }

    fn picture(&mut self, path: &Path, scene: Scene) -> io::Result<()> {
        self.dir(path.parent().unwrap())?;
        let (w, h) = (320, 240);
        let file = fs::File::create(path)?;
        let mut encoder = png::Encoder::new(io::BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(io::Error::other)?;
        writer
            .write_image_data(&scene.paint(w, h))
            .map_err(io::Error::other)?;
        writer.finish().map_err(io::Error::other)?;
        self.stamp(path)
    }

    fn stamp(&mut self, path: &Path) -> io::Result<()> {
        self.count += 1;
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(MTIME + self.count * 7 * 60);
        fs::File::open(path)?.set_modified(time)
    }
}

/// A seaside picture: a sky fading from `top` to `horizon`, a sun, and the
/// sea below.
#[derive(Clone, Copy)]
struct Scene {
    top: [u8; 3],
    horizon: [u8; 3],
    sun: [u8; 3],
    sea: [u8; 3],
    /// Where the sun stands, as fractions of the width and the sky's height.
    sun_at: (f32, f32),
}

impl Scene {
    const NOON: Scene = Scene {
        top: [40, 110, 200],
        horizon: [170, 215, 240],
        sun: [255, 250, 220],
        sea: [20, 90, 140],
        sun_at: (0.7, 0.25),
    };
    const DUSK: Scene = Scene {
        top: [45, 40, 100],
        horizon: [250, 150, 80],
        sun: [255, 220, 140],
        sea: [40, 50, 90],
        sun_at: (0.4, 0.85),
    };
    const GREY: Scene = Scene {
        top: [120, 130, 140],
        horizon: [200, 205, 210],
        sun: [235, 235, 230],
        sea: [70, 85, 95],
        sun_at: (0.25, 0.4),
    };
    const MORNING: Scene = Scene {
        top: [110, 160, 220],
        horizon: [250, 210, 180],
        sun: [255, 240, 200],
        sea: [50, 110, 150],
        sun_at: (0.8, 0.7),
    };
    const NIGHT: Scene = Scene {
        top: [5, 10, 30],
        horizon: [30, 45, 90],
        sun: [230, 230, 210],
        sea: [10, 20, 40],
        sun_at: (0.3, 0.3),
    };

    /// The picture's pixels, as RGB rows.
    fn paint(self, w: u32, h: u32) -> Vec<u8> {
        let sky_h = h as f32 * 0.62;
        let (sx, sy) = (self.sun_at.0 * w as f32, self.sun_at.1 * sky_h);
        let radius = h as f32 * 0.09;
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let rgb = if fy < sky_h {
                    let sky = mix(self.top, self.horizon, fy / sky_h);
                    // The sun, with a soft edge one pixel wide.
                    let d = ((fx - sx).powi(2) + (fy - sy).powi(2)).sqrt();
                    mix(sky, self.sun, (radius + 0.5 - d).clamp(0.0, 1.0))
                } else {
                    // The sea darkens toward the bottom and catches the sun's
                    // light in a few streaks below it.
                    let depth = (fy - sky_h) / (h as f32 - sky_h);
                    let sea = mix(self.horizon, self.sea, (depth * 4.0).min(1.0));
                    let glint = (1.0 - ((fx - sx).abs() / (radius * (1.0 + depth * 2.0)))).max(0.0)
                        * if (y / 3) % 3 == 0 { 0.6 } else { 0.0 }
                        * (1.0 - depth);
                    mix(sea, self.sun, glint)
                };
                data.extend_from_slice(&rgb);
            }
        }
        data
    }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}
