//! The file viewer itself: NeXTSTEP-style Miller columns.
//!
//! From top to bottom the widget shows
//!
//! - the **shelf** (see [`crate::shelf`]), the home folder and the user's
//!   bookmarked folders, unless it is turned off in the View menu,
//! - the **path view**, one large icon per visible column naming the folder
//!   that column lists (and, past the last folder, the selected file), unless
//!   it is turned off in the View menu,
//! - a horizontal **scroller** that pans the columns along the path, shown
//!   and hidden along with the path view,
//! - the **columns**, as many as fit the window at [`MIN_COLUMN_WIDTH`], each
//!   with its scroll bar on the right, and
//! - a **status line**.
//!
//! When the last column has files selected, the column past it shows what
//! there is to know about them: a thumbnail (see [`crate::thumbs`]), the file
//! type, size, dates, permissions, and owner.
//!
//! Selecting a folder in a column opens the next column for it; the view keeps
//! the end of the path in sight. Items are dragged with the mouse onto a folder
//! row, a column (its folder), or a folder icon in the path view: a plain drop
//! moves, holding Ctrl or Alt copies, and Ctrl+Shift makes symbolic links to
//! them. The pointer shows which, or a no-drop sign where a drop can't land.
//! Escape calls a drag off, and the release then does nothing. On Wayland a
//! drag is the desktop's from the start, as in other programs, so it can be
//! dropped onto other applications as well, which may move the files too. The
//! desktop then draws the pointer, showing a link — or a drop on the shelf —
//! as a question, and calls the drag off at Escape. Drops from other
//! applications are copied in.
//!
//! A click on the shelf goes to that folder, as does its item in the Go menu,
//! where the first ten places have Ctrl+1 … Ctrl+0. Folders dropped on the
//! shelf between its icons are put there; dropped on an icon, files go into that
//! folder as anywhere else. The icons after the home folder's are dragged to
//! put them in another order, or off the shelf to take them off it. On
//! Wayland they can be dragged onto other applications as well, which only
//! ever get to copy the folder.
//!
//! The keyboard works in one column at a time: the arrow keys, Page Up/Down,
//! Home and End move the selection, and with Shift held extend it from where
//! it started, as Shift+click does. Select All and Deselect All act on that
//! column too.
//!
//! The file operations act on the selection of the column the keyboard is in:
//! Delete moves it to the trash, Shift+Delete deletes it for good once
//! confirmed, and F2 edits the name of a single selected item in place —
//! Enter, or a click elsewhere, renames it; Escape leaves it as it was. A new
//! folder starts out with its name being edited. Duplicate copies the
//! selection next to itself as "name copy". Cut and Copy put the
//! selection on the clipboard (see [`crate::clipboard`]); Paste brings it into
//! the current folder — the last column's — and selects what arrived.

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use saudade::{
    Accel, Color, Cursor, Dialog, DragData, DropAction, Event, EventCtx, FontFamily, FontStyle,
    Key, Merged, ModifierScheme, Modifiers, MouseButton, NamedKey, Painter, Point, Rect,
    SCROLLBAR_THICKNESS, ScrollBar, Size, SvgImage, TextInput, Theme, Widget, include_svg,
};

use crate::clipboard::Clipboard;
use crate::fsops::{self, Entry, Op};
use crate::info::{self, FileInfo};
use crate::shelf::{self, Shelf};
use crate::thumbs::{self, Thumb, Thumbnails};

const FOLDER_ICON: SvgImage = include_svg!("assets/icons/folder.svg");
const FILE_ICON: SvgImage = include_svg!("assets/icons/file.svg");
const FOLDER_LARGE: SvgImage = include_svg!("assets/icons/folder-large.svg");
const FILE_LARGE: SvgImage = include_svg!("assets/icons/file-large.svg");
const FILES_LARGE: SvgImage = include_svg!("assets/icons/files-large.svg");
const HOME_LARGE: SvgImage = include_svg!("assets/icons/home-large.svg");

/// The narrowest a column gets before the window shows one column fewer.
pub const MIN_COLUMN_WIDTH: i32 = 170;
const COLUMN_GAP: i32 = 6;
const MARGIN: i32 = 6;
const ROW_H: i32 = 16;
const ROW_PAD_Y: i32 = 2;
const SMALL_ICON: i32 = 16;
const LARGE_ICON: i32 = 32;
const PATH_H: i32 = 64;
/// The width of a place on the shelf, which narrows down to the least one
/// to keep them all in a row, and grows a row when they don't fit even so.
const SHELF_CELL_W: i32 = 84;
const SHELF_CELL_MIN_W: i32 = 64;
const SHELF_ROW_H: i32 = 62;
const STATUS_H: i32 = 20;
/// How far the pointer must travel from a press before it becomes a drag.
const DRAG_THRESHOLD: i32 = 5;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// How long typed letters keep adding up to one type-ahead search.
const TYPE_AHEAD: Duration = Duration::from_millis(1000);
/// The largest a thumbnail is shown in the info column, in logical pixels.
const PREVIEW_MAX: i32 = 192;
const INFO_PAD: i32 = 8;

// ============================================================================
// One column: the listing of a folder plus its selection and scroll position.
// ============================================================================

struct BrowserColumn {
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Why the folder couldn't be listed, shown in place of its entries.
    error: Option<String>,
    /// Selected rows, ascending.
    selected: Vec<usize>,
    /// Where a Shift+click range starts.
    anchor: Option<usize>,
    /// The row the keyboard moves from: the one most recently clicked.
    lead: Option<usize>,
    /// Vertical scroll bar; its value is the top visible row.
    bar: ScrollBar,
}

impl BrowserColumn {
    fn open(dir: PathBuf, show_hidden: bool) -> Self {
        let mut col = Self {
            dir,
            entries: Vec::new(),
            error: None,
            selected: Vec::new(),
            anchor: None,
            lead: None,
            bar: ScrollBar::vertical(Rect::new(0, 0, 0, 0)),
        };
        col.read(show_hidden);
        col
    }

    fn read(&mut self, show_hidden: bool) {
        match fsops::read_entries(&self.dir, show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e.kind().to_string());
            }
        }
    }

    /// Re-lists the folder, keeping the selection, lead, and anchor on the same
    /// names wherever they still exist.
    fn reread(&mut self, show_hidden: bool) {
        let name = |i: Option<usize>, entries: &[Entry]| i.map(|i| entries[i].name.clone());
        let selected: Vec<OsString> = self
            .selected
            .iter()
            .map(|&i| self.entries[i].name.clone())
            .collect();
        let lead = name(self.lead, &self.entries);
        let anchor = name(self.anchor, &self.entries);
        self.read(show_hidden);
        self.selected = selected.iter().filter_map(|n| self.index_of(n)).collect();
        self.selected.sort_unstable();
        self.lead = lead.and_then(|n| self.index_of(&n));
        self.anchor = anchor.and_then(|n| self.index_of(&n));
    }

    fn index_of(&self, name: &OsStr) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    fn path_of(&self, row: usize) -> PathBuf {
        self.dir.join(&self.entries[row].name)
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected.iter().map(|&i| self.path_of(i)).collect()
    }

    /// The folder the next column lists: the selection, if it is exactly one
    /// folder.
    fn single_dir(&self) -> Option<PathBuf> {
        match self.selected[..] {
            [i] if self.entries[i].is_dir => Some(self.path_of(i)),
            _ => None,
        }
    }

    fn select_only(&mut self, row: usize) {
        self.selected = vec![row];
        self.anchor = Some(row);
        self.lead = Some(row);
    }

    fn clear_selection(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.lead = None;
    }

    /// Scrolls so `row` is on screen, given how many rows fit.
    fn scroll_to(&mut self, row: usize, rows: usize) {
        let top = self.bar.value().max(0) as usize;
        let rows = rows.max(1);
        let new_top = if row < top {
            row
        } else if row >= top + rows {
            row + 1 - rows
        } else {
            top
        };
        // The bar clamps to its range, which may not be set yet; keep the
        // wanted value in the range we know the list will have.
        self.bar.set_range(rows as i32, self.entries.len().saturating_sub(rows) as i32);
        self.bar.set_value(new_top as i32);
    }
}

// ============================================================================
// Interaction state.
// ============================================================================

/// Where something dragged would land, and what lights up to say so.
#[derive(Clone, Debug, PartialEq)]
enum Highlight {
    /// A folder row: `(slot, row)`.
    Row(usize, usize),
    /// A column as a whole — its own folder.
    Column(usize),
    /// A folder icon in the path view.
    PathIcon(usize),
    /// An icon on the shelf, by its place: 0 is the home folder, `i + 1`
    /// the `i`-th bookmark.
    ShelfIcon(usize),
    /// The gap on the shelf before the `i`-th bookmark.
    ShelfGap(usize),
}

#[derive(Clone, Debug)]
enum Dest {
    /// Move or copy into this folder.
    Folder(PathBuf),
    /// Put the folders on the shelf, before the `i`-th bookmark.
    Shelf(usize),
}

#[derive(Clone, Debug)]
struct Target {
    dest: Dest,
    highlight: Highlight,
}

/// A drag handed to the desktop, followed for as long as it lasts.
enum OsDrag {
    /// Files, moved, copied or linked when dropped back in the window, as by
    /// a drag inside it.
    Files(Vec<PathBuf>),
    /// The `i`-th bookmark: dropped back in the window, it goes to a gap on
    /// the shelf, or off the shelf.
    Bookmark(usize),
}

/// What a drag over the window would do, dropped where it is now.
enum Hover {
    /// Nothing: it can't land here.
    Nowhere,
    /// `op` on `target`.
    Drop { target: Target, op: Op },
    /// Move the bookmark dragged to the shelf's `gap`, or with none, off the
    /// shelf.
    Bookmark { gap: Option<usize> },
}

impl Hover {
    /// What the drop does, as the desktop shows it in the pointer. It knows
    /// no links, nor the shelf, which keeps a reference as a link does: for
    /// those it shows a question.
    fn action(&self) -> Option<DropAction> {
        match self {
            Hover::Nowhere => None,
            Hover::Bookmark { .. } | Hover::Drop { op: Op::Link, .. } => Some(DropAction::Ask),
            Hover::Drop { target, .. } if matches!(target.dest, Dest::Shelf(_)) => {
                Some(DropAction::Ask)
            }
            Hover::Drop { op: Op::Move, .. } => Some(DropAction::Move),
            Hover::Drop { op: Op::Copy, .. } => Some(DropAction::Copy),
        }
    }
}

/// What a press on the left button started, followed until its release.
enum Gesture {
    Idle,
    /// Operating the horizontal scroller.
    HScroll,
    /// Operating the scroll bar of the column in this slot.
    ColumnBar(usize),
    /// Pressed, not yet moved far enough to drag. `paths` is what a drag would
    /// carry.
    Press {
        origin: Point,
        paths: Vec<PathBuf>,
        click: Click,
    },
    /// Dragging `paths` around inside the window.
    Drag {
        paths: Vec<PathBuf>,
        pos: Point,
        /// What a drop would do, by the modifiers held.
        op: Op,
        /// The drop target under the pointer, for `op`.
        target: Option<Target>,
    },
    /// Pressed on the shelf's icon in `place`, not yet moved far enough to
    /// drag it.
    ShelfPress { origin: Point, place: usize },
    /// Dragging the `index`-th bookmark: to the gap before the `gap`-th one,
    /// or, off the shelf, away.
    ShelfDrag {
        index: usize,
        pos: Point,
        gap: Option<usize>,
    },
}

/// What a press turns into if it is released without dragging.
enum Click {
    Nothing,
    /// A press on a row of a multi-selection keeps the whole group selected
    /// (so it can be dragged) and narrows it to this row on release.
    Collapse { slot: usize, row: usize },
    /// A press on an icon of the path view goes there on release.
    PathIcon(usize),
}

/// What the info column shows for the selected files.
enum Preview {
    One(FileInfo),
    Many(Vec<(&'static str, String)>),
}

/// A name being edited in place.
struct Rename {
    /// The folder the item is in, and its name there.
    dir: PathBuf,
    name: OsString,
    /// The name as it was shown, to tell whether it was changed.
    label: String,
    input: TextInput,
}

/// What sits in one slot of the path view.
struct SlotInfo {
    icon: SvgImage,
    label: String,
    /// What dragging the icon carries.
    paths: Vec<PathBuf>,
}

// ============================================================================
// The browser widget.
// ============================================================================

pub struct Browser {
    bounds: Rect,
    /// Column `i` lists `columns[i].dir`; the selection in column `i` decides
    /// column `i + 1`. Column 0 is the filesystem root.
    columns: Vec<BrowserColumn>,
    /// The first slot on screen. A slot is a column, plus — when the last
    /// column has files selected — one empty slot past it for the files'
    /// icon, as NeXT's browser shows it.
    first: usize,
    /// How many columns fit the window.
    visible: usize,
    /// The column the keyboard works in.
    active: usize,
    focused: bool,
    show_hidden: bool,
    shelf_view: bool,
    path_view: bool,
    shelf: Shelf,
    /// The first place on the shelf: the home folder, named for the user.
    home: PathBuf,
    home_label: String,
    hscroll: ScrollBar,
    /// Scroll bars for the empty columns past the end of the path.
    blank_bars: Vec<ScrollBar>,
    gesture: Gesture,
    /// The last row clicked, for double-clicks: `(slot, row, when)`.
    last_click: Option<(usize, usize, Instant)>,
    typed: String,
    typed_at: Instant,
    /// The drag handed to the desktop, so dropping it back into the window
    /// does what a drag inside it would.
    os_drag: Option<OsDrag>,
    /// What a drag over the window would do; `None` while none is over it.
    os_hover: Option<Hover>,
    /// Lead rows still to be scrolled into view once the columns have a size.
    reveal_pending: bool,
    /// The selection the info column describes, and what it says about it.
    preview_key: Vec<PathBuf>,
    preview: Option<Preview>,
    /// How far the info column is scrolled down, and how tall its content
    /// came out on the last paint.
    info_scroll: i32,
    info_height: i32,
    thumbs: Thumbnails,
    /// The display scale of the last paint, to ask for thumbnails sharp enough.
    scale: f32,
    dialog: Rc<RefCell<Dialog>>,
    /// Errors still to be shown, `(title, message)`, waiting for the dialog
    /// to be free.
    errors: Vec<(String, String)>,
    /// This browser, for dialogs that act on it once confirmed.
    this: Weak<RefCell<Browser>>,
    rename: Option<Rename>,
    clipboard: Clipboard,
}

impl Browser {
    pub fn new(start: &Path, dialog: Rc<RefCell<Dialog>>, this: Weak<RefCell<Browser>>) -> Self {
        let mut me = Self {
            bounds: Rect::new(0, 0, 0, 0),
            columns: Vec::new(),
            first: 0,
            visible: 1,
            active: 0,
            focused: false,
            show_hidden: false,
            shelf_view: true,
            path_view: true,
            shelf: Shelf::load(),
            home: std::env::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            home_label: shelf::user_name(),
            hscroll: ScrollBar::horizontal(Rect::new(0, 0, 0, 0)),
            blank_bars: Vec::new(),
            gesture: Gesture::Idle,
            last_click: None,
            typed: String::new(),
            typed_at: Instant::now(),
            os_drag: None,
            os_hover: None,
            reveal_pending: false,
            preview_key: Vec::new(),
            preview: None,
            info_scroll: 0,
            info_height: 0,
            thumbs: Thumbnails::new(),
            scale: 1.0,
            dialog,
            errors: Vec::new(),
            this,
            rename: None,
            clipboard: Clipboard::new(),
        };
        me.go_to(start);
        me
    }

    // ------------------------------------------------------------------
    // Navigation.
    // ------------------------------------------------------------------

    /// Opens the columns from the root down to `target`, selecting each step,
    /// and `target` itself in the last column if it is a file.
    pub fn go_to(&mut self, target: &Path) {
        let target = target.canonicalize().unwrap_or_else(|_| target.to_path_buf());
        let mut steps: Vec<&Path> = target.ancestors().collect();
        steps.reverse();
        let Some((root, rest)) = steps.split_first() else {
            return;
        };
        self.columns = vec![BrowserColumn::open(root.to_path_buf(), self.show_hidden)];
        for step in rest {
            let Some(name) = step.file_name() else { break };
            if !self.show_hidden && name.to_string_lossy().starts_with('.') {
                // Going somewhere hidden shows hidden files, or the path
                // couldn't be shown at all.
                self.show_hidden = true;
                for col in &mut self.columns {
                    col.reread(true);
                }
            }
            let col = self.columns.last_mut().expect("the root column");
            let Some(row) = col.index_of(name) else { break };
            col.select_only(row);
            if !col.entries[row].is_dir {
                break;
            }
            self.columns
                .push(BrowserColumn::open(step.to_path_buf(), self.show_hidden));
        }
        self.active = self.columns.len() - 1;
        self.reveal_pending = true;
        self.reveal_end();
    }

    /// The folder new things are made in: the one the last column lists.
    pub fn current_dir(&self) -> &Path {
        &self.columns.last().expect("at least the root column").dir
    }

    pub fn go_home(&mut self) {
        if let Some(home) = std::env::home_dir() {
            self.go_to(&home);
        }
    }

    /// Moves the keyboard to the column to the left, clearing the selection in
    /// the column it leaves.
    pub fn go_parent(&mut self) {
        let slot = self.active.min(self.columns.len() - 1);
        if slot == 0 {
            return;
        }
        self.columns.truncate(slot + 1);
        self.columns[slot].clear_selection();
        self.active = slot - 1;
        self.reveal_end();
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.reload();
    }

    pub fn path_view(&self) -> bool {
        self.path_view
    }

    pub fn set_path_view(&mut self, shown: bool) {
        self.path_view = shown;
        self.keep_leads_in_sight();
    }

    pub fn toggle_path_view(&mut self) {
        self.set_path_view(!self.path_view);
    }

    pub fn shelf_view(&self) -> bool {
        self.shelf_view
    }

    pub fn set_shelf_view(&mut self, shown: bool) {
        self.shelf_view = shown;
        self.keep_leads_in_sight();
    }

    pub fn toggle_shelf_view(&mut self) {
        self.set_shelf_view(!self.shelf_view);
    }

    /// After the columns got shorter or taller, scrolls them to keep their
    /// selections in sight. (Before the first layout, `reveal_pending` sees
    /// to that.)
    fn keep_leads_in_sight(&mut self) {
        if self.columns_rect().h == 0 {
            return;
        }
        let rows = self.rows_per_column();
        for col in &mut self.columns {
            if let Some(lead) = col.lead {
                col.scroll_to(lead, rows);
            }
        }
        self.sync();
    }

    // ------------------------------------------------------------------
    // The shelf.
    // ------------------------------------------------------------------

    /// The folder in a place on the shelf.
    fn shelf_path(&self, place: usize) -> Option<PathBuf> {
        match place {
            0 => Some(self.home.clone()),
            p => self.shelf.get(p - 1).map(|b| b.path.clone()),
        }
    }

    fn shelf_label(&self, place: usize) -> String {
        match place {
            0 => self.home_label.clone(),
            p => self.shelf.get(p - 1).map(|b| b.label()).unwrap_or_default(),
        }
    }

    /// What Add to Shelf and Remove from Shelf act on: the folders selected
    /// in the active column, or, with nothing selected there, its own folder.
    fn shelf_subject(&self) -> Vec<PathBuf> {
        let col = &self.columns[self.active_slot()];
        if col.selected.is_empty() {
            return vec![col.dir.clone()];
        }
        col.selected
            .iter()
            .filter(|&&i| col.entries[i].is_dir)
            .map(|&i| col.path_of(i))
            .collect()
    }

    pub fn add_to_shelf(&mut self) {
        let paths = self.shelf_subject();
        self.shelve(&paths, self.shelf.len());
    }

    pub fn remove_from_shelf(&mut self) {
        let mut changed = false;
        for path in self.shelf_subject() {
            if let Some(i) = self.shelf.position(&path) {
                changed |= self.shelf.remove(i);
            }
        }
        self.shelf_changed(changed);
    }

    /// Whether dropping `paths` between the shelf's icons would put anything
    /// there: they must all be folders, and not all there already.
    fn can_shelve(&self, paths: &[PathBuf]) -> bool {
        !paths.is_empty()
            && paths.iter().all(|p| p.is_dir())
            && paths.iter().any(|p| *p != self.home && !self.shelf.contains(p))
    }

    /// Puts the folders among `paths` on the shelf, before the `at`-th
    /// bookmark.
    fn shelve(&mut self, paths: &[PathBuf], at: usize) {
        let folders: Vec<PathBuf> = paths
            .iter()
            .filter(|p| p.is_dir() && **p != self.home)
            .cloned()
            .collect();
        let added = self.shelf.insert(at, &folders) > 0;
        self.shelf_changed(added);
    }

    /// Saves the shelf if it `changed`; the columns may have a new height.
    fn shelf_changed(&mut self, changed: bool) {
        if !changed {
            return;
        }
        if let Err(e) = self.shelf.save() {
            self.show_errors("Couldn't save the shelf", &[e.to_string()]);
        }
        self.keep_leads_in_sight();
    }

    /// The labels of the bookmarks on the shelf, after home — re-reading them
    /// if another program changed them.
    pub fn bookmark_labels(&mut self) -> Vec<String> {
        self.refresh_shelf();
        self.shelf.bookmarks().map(|b| b.label()).collect()
    }

    /// Picks up changes another program made to the bookmarks.
    fn refresh_shelf(&mut self) {
        if self.shelf.refresh() && self.shelf_view {
            self.keep_leads_in_sight();
        }
    }

    /// Whether the viewer is in the folder of a place on the shelf.
    pub fn is_current_place(&self, place: usize) -> bool {
        self.shelf_path(place).as_deref() == Some(self.current_dir())
    }

    /// A click on the shelf, or its item in the Go menu: show that folder.
    pub fn go_to_place(&mut self, place: usize) {
        let Some(path) = self.shelf_path(place) else { return };
        if path.is_dir() {
            self.go_to(&path);
        } else {
            let name = self.shelf_label(place);
            self.show_errors("Couldn't open", &[format!("“{name}” isn't there any more")]);
        }
    }

    /// Re-reads every column from disk, keeping selections by name and closing
    /// columns whose folder is gone.
    pub fn reload(&mut self) {
        // The selected files may have changed too.
        self.preview_key.clear();
        let mut i = 0;
        while i < self.columns.len() {
            if i > 0 && !self.columns[i].dir.is_dir() {
                self.columns.truncate(i);
                break;
            }
            self.columns[i].reread(self.show_hidden);
            let want = self.columns[i].single_dir();
            let next_ok = matches!((&want, self.columns.get(i + 1)), (Some(w), Some(next)) if next.dir == *w);
            if !next_ok {
                self.columns.truncate(i + 1);
                if let Some(dir) = want {
                    self.columns.push(BrowserColumn::open(dir, self.show_hidden));
                }
            }
            i += 1;
        }
        self.active = self.active.min(self.columns.len() - 1);
        self.sync();
    }

    /// Sets the selection of the column in `slot`, closes the columns past it
    /// and opens one for the selection if it is a single folder.
    fn set_selection(&mut self, slot: usize, selected: Vec<usize>) {
        let col = &mut self.columns[slot];
        let unchanged = col.selected == selected;
        col.selected = selected;
        let want = col.single_dir();
        // Re-picking the folder that's already open keeps its column as it is.
        let keep = unchanged
            && want.is_some()
            && self.columns.get(slot + 1).map(|c| &c.dir) == want.as_ref();
        if !keep {
            self.columns.truncate(slot + 1);
            if let Some(dir) = want {
                self.columns.push(BrowserColumn::open(dir, self.show_hidden));
            }
        }
        self.active = slot;
        self.reveal_end();
    }

    fn select_row(&mut self, slot: usize, row: usize) {
        self.columns[slot].select_only(row);
        let rows = self.rows_per_column();
        self.columns[slot].scroll_to(row, rows);
        self.set_selection(slot, vec![row]);
    }

    /// Selects everything in the active column.
    pub fn select_all(&mut self) {
        let slot = self.active_slot();
        let len = self.columns[slot].entries.len();
        if len == 0 {
            return;
        }
        // As if Shift+clicked from the first row to the last: Shift and the
        // arrow keys narrow it from the end.
        let col = &mut self.columns[slot];
        col.anchor = Some(0);
        col.lead = Some(len - 1);
        self.set_selection(slot, (0..len).collect());
    }

    /// Clears the selection of the active column.
    pub fn deselect_all(&mut self) {
        let slot = self.active_slot();
        self.columns[slot].clear_selection();
        self.set_selection(slot, vec![]);
    }

    /// Selects the rows from the anchor — where the selection started — to
    /// `row`, which becomes the lead the keyboard moves on from.
    fn extend_selection(&mut self, slot: usize, row: usize) {
        let col = &mut self.columns[slot];
        let anchor = *col.anchor.get_or_insert(col.lead.unwrap_or(row));
        col.lead = Some(row);
        let rows = self.rows_per_column();
        self.columns[slot].scroll_to(row, rows);
        self.set_selection(slot, (anchor.min(row)..=anchor.max(row)).collect());
    }

    /// Scrolls the path so its end — the deepest column, or the selected
    /// files' slot — is the rightmost one on screen.
    fn reveal_end(&mut self) {
        self.first = self.slots().saturating_sub(self.visible);
        self.sync();
    }

    /// Opens the selection of the active column: folders by moving into them,
    /// files with the application the desktop has for them.
    pub fn open_selection(&mut self) {
        let slot = self.active.min(self.columns.len() - 1);
        if self.columns[slot].single_dir().is_some() {
            self.enter_next_column();
            return;
        }
        let mut errors = Vec::new();
        for path in self.columns[slot].selected_paths() {
            if let Err(e) = fsops::open_with_system(&path) {
                errors.push(format!("{}: {e}", fsops::display_name(&path)));
            }
        }
        self.show_errors("Couldn't open", &errors);
    }

    fn enter_next_column(&mut self) {
        let slot = self.active.min(self.columns.len() - 1);
        if self.columns[slot].single_dir().is_some()
            && self.columns.get(slot + 1).is_some_and(|c| !c.entries.is_empty())
        {
            let row = self.columns[slot + 1].lead.unwrap_or(0);
            self.select_row(slot + 1, row);
        }
    }

    /// Makes a folder in the current folder and starts editing its name.
    pub fn new_folder(&mut self) {
        let dir = self.current_dir().to_path_buf();
        match fsops::new_folder(&dir) {
            Ok(path) => {
                self.reload();
                if self.select_path(&path) {
                    self.start_rename();
                }
            }
            Err(e) => self.show_errors("Couldn't create a folder", &[e.to_string()]),
        }
    }

    /// Selects `path` in the column listing its folder, if one does.
    fn select_path(&mut self, path: &Path) -> bool {
        self.select_paths(std::slice::from_ref(&path.to_path_buf()))
    }

    /// Selects `paths`, all in one folder, in the column listing it, if one
    /// does.
    fn select_paths(&mut self, paths: &[PathBuf]) -> bool {
        let Some(dir) = paths.first().and_then(|p| p.parent()) else {
            return false;
        };
        let Some(slot) = self.columns.iter().position(|c| c.dir == dir) else {
            return false;
        };
        let col = &self.columns[slot];
        let mut rows: Vec<usize> = paths
            .iter()
            .filter(|p| p.parent() == Some(dir))
            .filter_map(|p| col.index_of(p.file_name()?))
            .collect();
        rows.sort_unstable();
        rows.dedup();
        let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else {
            return false;
        };
        if rows.len() == 1 {
            self.select_row(slot, first);
            return true;
        }
        let col = &mut self.columns[slot];
        col.anchor = Some(first);
        col.lead = Some(last);
        let n = self.rows_per_column();
        self.columns[slot].scroll_to(last, n);
        self.set_selection(slot, rows);
        true
    }

    // ------------------------------------------------------------------
    // Renaming in place.
    // ------------------------------------------------------------------

    /// Starts editing the name of the selected item of the active column, if
    /// exactly one is selected.
    pub fn start_rename(&mut self) {
        self.finish_rename();
        let slot = self.active_slot();
        let col = &self.columns[slot];
        let [row] = col.selected[..] else { return };
        let entry = &col.entries[row];
        let mut input = TextInput::new(Rect::new(0, 0, 0, 0)).with_text(&entry.label);
        input.select_all();
        input.set_focused(self.focused);
        self.rename = Some(Rename {
            dir: col.dir.clone(),
            name: entry.name.clone(),
            label: entry.label.clone(),
            input,
        });
        // Bring the row on screen to edit it there.
        let rows = self.rows_per_column();
        self.columns[slot].scroll_to(row, rows);
        if slot < self.first {
            self.first = slot;
        }
        self.sync();
    }

    /// Where the name being edited is: its column's slot and row.
    fn rename_target(&self) -> Option<(usize, usize)> {
        let r = self.rename.as_ref()?;
        let slot = self.columns.iter().position(|c| c.dir == r.dir)?;
        Some((slot, self.columns[slot].index_of(&r.name)?))
    }

    /// The text field over the row being renamed, while that row is on screen.
    fn rename_rect(&self) -> Option<Rect> {
        let (slot, row) = self.rename_target()?;
        let v = slot.checked_sub(self.first).filter(|&v| v < self.visible)?;
        let top = self.columns[slot].bar.value().max(0) as usize;
        let shown = row.checked_sub(top).filter(|&r| r < self.rows_per_column())?;
        let field = Self::field_rect(self.cell(v, self.columns_rect()));
        let y = field.y + ROW_PAD_Y + shown as i32 * ROW_H;
        // The text lands where the label was: the field pads it by 4.
        let x = field.x + 4 + SMALL_ICON + 4 - 4;
        Some(Rect::new(x, y - 2, field.right() - 2 - x, ROW_H + 4))
    }

    /// Renames the item to what was typed, if it was changed, and stops
    /// editing.
    pub fn finish_rename(&mut self) {
        let Some(r) = self.rename.take() else { return };
        let new_name = r.input.text();
        if new_name == r.label {
            return;
        }
        match fsops::rename(&r.dir.join(&r.name), &new_name) {
            Ok(path) => {
                self.reload();
                self.select_path(&path);
            }
            Err(e) => self.show_errors("Couldn't rename", &[format!("{}: {e}", r.label)]),
        }
    }

    fn cancel_rename(&mut self) {
        self.rename = None;
    }

    /// Hands an event to the name being edited. Returns whether it was
    /// handled there; otherwise it goes on to the browser, and a click
    /// elsewhere has first finished the renaming.
    fn rename_event(&mut self, event: &Event, ctx: &mut EventCtx) -> bool {
        if self.rename.is_none() {
            return false;
        }
        if self.rename_target().is_none() {
            // The item is gone from under the field.
            self.cancel_rename();
            return false;
        }
        // Off screen, scrolled out of view, the field takes no clicks.
        let rect = self.rename_rect().unwrap_or(Rect::new(0, 0, 0, 0));
        let r = self.rename.as_mut().expect("checked above");
        r.input.rect = rect;
        match event {
            Event::KeyDown {
                key: Key::Named(NamedKey::Enter),
                ..
            } => self.finish_rename(),
            Event::KeyDown {
                key: Key::Named(NamedKey::Escape),
                ..
            } => self.cancel_rename(),
            Event::KeyDown { .. } | Event::KeyUp { .. } | Event::Char { .. } => {
                r.input.event(event, ctx);
            }
            Event::PointerDown { pos, .. } => {
                if !rect.contains(*pos) {
                    self.finish_rename();
                    return false;
                }
                r.input.event(event, ctx);
            }
            Event::PointerMove { .. } | Event::PointerUp { .. } if r.input.captures_pointer() => {
                r.input.event(event, ctx);
            }
            Event::Tick => {
                r.input.event(event, ctx);
                return false;
            }
            _ => return false,
        }
        ctx.consume_event();
        ctx.request_paint();
        true
    }

    /// Moves or copies `paths` into `dest` and returns where they ended up.
    fn transfer(&mut self, paths: &[PathBuf], dest: &Path, op: Op) -> Vec<PathBuf> {
        let (done, errors) = fsops::transfer(paths, dest, op);
        self.reload();
        let title = match op {
            Op::Move => "Couldn't move",
            Op::Copy => "Couldn't copy",
            Op::Link => "Couldn't link",
        };
        self.show_errors(title, &errors);
        done
    }

    /// Copies the selection of the active column next to itself, as
    /// "name copy", and selects the copies.
    pub fn duplicate(&mut self) {
        let slot = self.active_slot();
        let paths = self.columns[slot].selected_paths();
        if paths.is_empty() {
            return;
        }
        let dir = self.columns[slot].dir.clone();
        let done = self.transfer(&paths, &dir, Op::Copy);
        self.select_paths(&done);
    }

    pub fn cut(&mut self) {
        self.put_on_clipboard(Op::Move);
    }

    pub fn copy(&mut self) {
        self.put_on_clipboard(Op::Copy);
    }

    fn put_on_clipboard(&mut self, op: Op) {
        let paths = self.columns[self.active_slot()].selected_paths();
        if !paths.is_empty() {
            self.clipboard.put(paths, op);
        }
    }

    /// Brings what was cut or copied into the current folder.
    pub fn paste(&mut self) {
        let Some((paths, op)) = self.clipboard.contents() else {
            return;
        };
        let dest = self.current_dir().to_path_buf();
        if !fsops::can_drop(&paths, &dest, op) {
            if op == Op::Move && paths.iter().all(|p| p.parent() == Some(&dest)) {
                return; // cut and pasted back where they are
            }
            self.show_errors(
                "Couldn't paste",
                &[format!("{} can't go into “{}”", describe(&paths), fsops::display_name(&dest))],
            );
            return;
        }
        let done = self.transfer(&paths, &dest, op);
        if op == Op::Move && !done.is_empty() {
            self.clipboard.clear_cut();
        }
        self.select_paths(&done);
    }

    /// The column the keyboard is in, whose selection the file operations
    /// act on.
    fn active_slot(&self) -> usize {
        self.active.min(self.columns.len() - 1)
    }

    pub fn move_to_trash(&mut self) {
        let slot = self.active_slot();
        let paths = self.columns[slot].selected_paths();
        if paths.is_empty() {
            return;
        }
        let errors = fsops::trash(&paths);
        self.after_removal(slot);
        self.show_errors("Couldn't move to the Trash", &errors);
    }

    /// Asks before deleting the selection of the active column for good.
    pub fn delete_immediately(&mut self) {
        let slot = self.active_slot();
        let paths = self.columns[slot].selected_paths();
        if paths.is_empty() {
            return;
        }
        let message = format!(
            "Delete {} immediately? This can't be undone.",
            describe(&paths)
        );
        let this = self.this.clone();
        self.dialog
            .borrow_mut()
            .show_confirm("Delete Immediately", message, "Delete", move |cx| {
                if let Some(browser) = this.upgrade() {
                    let mut browser = browser.borrow_mut();
                    let errors = fsops::delete(&paths);
                    // The selection may have moved on while the box was up.
                    let slot = browser
                        .columns
                        .iter()
                        .position(|c| paths.first().and_then(|p| p.parent()) == Some(&c.dir))
                        .unwrap_or_else(|| browser.active_slot());
                    browser.after_removal(slot);
                    browser.show_errors("Couldn't delete", &errors);
                }
                cx.request_paint();
            });
    }

    /// Re-reads the folders after the selection of the column in `slot` was
    /// trashed or deleted, and selects the entry that took the place of the
    /// first one gone, so the keyboard can carry on from there.
    fn after_removal(&mut self, slot: usize) {
        let first = self.columns[slot].selected.first().copied();
        self.reload();
        if slot >= self.columns.len() {
            return;
        }
        let col = &self.columns[slot];
        if let Some(row) = first
            && col.selected.is_empty()
            && !col.entries.is_empty()
        {
            self.select_row(slot, row.min(col.entries.len() - 1));
        } else {
            self.active = slot;
            self.reveal_end();
        }
    }

    fn show_errors(&mut self, title: &str, errors: &[String]) {
        if !errors.is_empty() {
            self.errors.push((title.to_string(), errors.join("\n")));
        }
        self.flush_errors();
    }

    /// Shows the next waiting error, unless the dialog is busy — open, or in
    /// the middle of running the confirmed action that failed.
    fn flush_errors(&mut self) {
        if self.errors.is_empty() {
            return;
        }
        if let Ok(mut dialog) = self.dialog.try_borrow_mut()
            && !dialog.is_open()
        {
            let (title, message) = self.errors.remove(0);
            dialog.show_error(title, message);
        }
    }

    // ------------------------------------------------------------------
    // Geometry.
    // ------------------------------------------------------------------

    /// How many slots the path has: the columns, plus one for the selected
    /// files of the last column.
    fn slots(&self) -> usize {
        let files = self
            .columns
            .last()
            .is_some_and(|c| !c.selected.is_empty());
        self.columns.len() + files as usize
    }

    fn shelf_cell_w(&self) -> i32 {
        let room = self.bounds.w - 2 * MARGIN - 2;
        (room / (1 + self.shelf.len()) as i32).clamp(SHELF_CELL_MIN_W, SHELF_CELL_W)
    }

    /// How many places the shelf has room for in a row.
    fn shelf_per_row(&self) -> usize {
        ((self.bounds.w - 2 * MARGIN - 2) / self.shelf_cell_w()).max(1) as usize
    }

    /// The shelf's area, a row of places high or more as they need: no
    /// height at all while it is turned off.
    fn shelf_rect(&self) -> Rect {
        let b = self.bounds;
        let rows = (1 + self.shelf.len()).div_ceil(self.shelf_per_row()) as i32;
        let h = if self.shelf_view { rows * SHELF_ROW_H + 2 } else { 0 };
        Rect::new(b.x + MARGIN, b.y + MARGIN, (b.w - 2 * MARGIN).max(0), h)
    }

    /// The `place`-th place on the shelf, inside its border.
    fn shelf_cell(&self, place: usize) -> Rect {
        let area = self.shelf_rect();
        let per_row = self.shelf_per_row();
        let (row, col) = ((place / per_row) as i32, (place % per_row) as i32);
        let w = self.shelf_cell_w();
        Rect::new(area.x + 1 + col * w, area.y + 1 + row * SHELF_ROW_H, w, SHELF_ROW_H)
    }

    /// Where the icon of a place on the shelf is drawn.
    fn shelf_icon_rect(cell: Rect) -> Rect {
        Rect::new(cell.x + (cell.w - LARGE_ICON) / 2, cell.y + 6, LARGE_ICON, LARGE_ICON)
    }

    /// The place on the shelf under `pos`.
    fn shelf_place_at(&self, pos: Point) -> Option<usize> {
        if !self.shelf_rect().contains(pos) {
            return None;
        }
        (0..=self.shelf.len()).find(|&p| self.shelf_cell(p).contains(pos))
    }

    /// The gap on the shelf nearest `pos` — the one before the `i`-th
    /// bookmark — for something dropped there. The home folder stays first.
    fn shelf_gap_at(&self, pos: Point) -> Option<usize> {
        let area = self.shelf_rect();
        if !area.contains(pos) {
            return None;
        }
        let n = self.shelf.len();
        let per_row = self.shelf_per_row();
        let row = ((pos.y - area.y - 1) / SHELF_ROW_H).max(0) as usize;
        let cell_w = self.shelf_cell_w();
        let dx = (pos.x - area.x - 1).max(0);
        let col = (dx / cell_w) as usize;
        let place = row * per_row + col.min(per_row - 1);
        if place > n {
            return Some(n); // past the last icon
        }
        // Over the right half of a place, or past the end of its row, the
        // gap after it; the gap after place `p` is the one before bookmark `p`.
        let after = col >= per_row || dx % cell_w >= cell_w / 2;
        Some(if after { place } else { place.saturating_sub(1) })
    }

    /// The path view's area: no height at all while it is turned off.
    fn path_rect(&self) -> Rect {
        let b = self.bounds;
        let top = if self.shelf_view {
            self.shelf_rect().bottom() + MARGIN
        } else {
            b.y + MARGIN
        };
        let h = if self.path_view { PATH_H } else { 0 };
        Rect::new(b.x + MARGIN, top, (b.w - 2 * MARGIN).max(0), h)
    }

    /// The scroller's area, sharing its top edge with the path view's bottom
    /// border. It goes with the path view: no height while that is off.
    fn hscroll_rect(&self) -> Rect {
        let p = self.path_rect();
        let h = if self.path_view { SCROLLBAR_THICKNESS } else { 0 };
        Rect::new(p.x, p.bottom() - 1, p.w, h)
    }

    fn columns_rect(&self) -> Rect {
        let b = self.bounds;
        let top = if self.path_view {
            self.hscroll_rect().bottom() + MARGIN
        } else {
            self.path_rect().y
        };
        let bottom = b.bottom() - STATUS_H;
        Rect::new(b.x + MARGIN, top, (b.w - 2 * MARGIN).max(0), (bottom - top).max(0))
    }

    fn status_rect(&self) -> Rect {
        let b = self.bounds;
        Rect::new(b.x + MARGIN, b.bottom() - STATUS_H, (b.w - 2 * MARGIN).max(0), STATUS_H)
    }

    /// The `v`-th of the visible cells across `area`, which share its width
    /// evenly with a gap between each two.
    fn cell(&self, v: usize, area: Rect) -> Rect {
        let n = self.visible as i32;
        let v = v as i32;
        let total = (area.w - COLUMN_GAP * (n - 1)).max(0);
        let base = total / n;
        let extra = total % n;
        let x = area.x + v * (base + COLUMN_GAP) + v.min(extra);
        Rect::new(x, area.y, base + (v < extra) as i32, area.h)
    }

    fn bar_rect(cell: Rect) -> Rect {
        Rect::new(cell.right() - SCROLLBAR_THICKNESS, cell.y, SCROLLBAR_THICKNESS, cell.h)
    }

    /// The white list area of a column cell, sharing its right border with
    /// the scroll bar's left one.
    fn field_rect(cell: Rect) -> Rect {
        let d = SCROLLBAR_THICKNESS - 1;
        Rect::new(cell.x, cell.y, (cell.w - d).max(0), cell.h)
    }

    fn rows_per_column(&self) -> usize {
        ((self.columns_rect().h - 2 * ROW_PAD_Y) / ROW_H).max(1) as usize
    }

    /// The visible index of the cell under `x`, if any.
    fn visible_at(&self, x: i32, area: Rect) -> Option<usize> {
        (0..self.visible).find(|&v| {
            let c = self.cell(v, area);
            x >= c.x && x < c.right()
        })
    }

    /// The column slot under `pos` in the columns area.
    fn column_slot_at(&self, pos: Point) -> Option<usize> {
        let area = self.columns_rect();
        if !area.contains(pos) {
            return None;
        }
        self.visible_at(pos.x, area).map(|v| self.first + v)
    }

    /// The path-view slot under `pos`, if one is shown there.
    fn path_slot_at(&self, pos: Point) -> Option<usize> {
        let area = self.path_rect();
        if !area.contains(pos) {
            return None;
        }
        let slot = self.first + self.visible_at(pos.x, area)?;
        (slot < self.slots()).then_some(slot)
    }

    /// The row of the column in `slot` under `pos`.
    fn row_at(&self, slot: usize, pos: Point) -> Option<usize> {
        let col = self.columns.get(slot)?;
        let v = slot.checked_sub(self.first)?;
        let field = Self::field_rect(self.cell(v, self.columns_rect()));
        if !field.contains(pos) || pos.y < field.y + ROW_PAD_Y {
            return None;
        }
        let row = col.bar.value().max(0) as usize + ((pos.y - field.y - ROW_PAD_Y) / ROW_H) as usize;
        (row < col.entries.len()).then_some(row)
    }

    /// Brings every derived bit of geometry up to date with the bounds and the
    /// columns: how many columns fit, where every scroll bar sits and how far
    /// it scrolls.
    fn sync(&mut self) {
        let area = self.columns_rect();
        let n = ((area.w + COLUMN_GAP) / (MIN_COLUMN_WIDTH + COLUMN_GAP)).max(1) as usize;
        if n != self.visible {
            // Resizing keeps the end of the path where it was: on the right.
            self.visible = n;
            self.first = self.slots().saturating_sub(n);
        }
        let max_first = self.slots().saturating_sub(n);
        self.first = self.first.min(max_first);
        self.active = self.active.min(self.columns.len() - 1);

        self.hscroll.set_rect(self.hscroll_rect());
        self.hscroll.set_range(n as i32, max_first as i32);
        self.hscroll.set_value(self.first as i32);

        let rows = self.rows_per_column();
        for col in &mut self.columns {
            col.bar.set_rect(Rect::new(0, 0, 0, 0));
        }
        self.blank_bars.resize_with(n, || ScrollBar::vertical(Rect::new(0, 0, 0, 0)));
        for v in 0..n {
            let cell = self.cell(v, area);
            let slot = self.first + v;
            if let Some(col) = self.columns.get_mut(slot) {
                col.bar.set_rect(Self::bar_rect(cell));
                col.bar
                    .set_range(rows as i32, col.entries.len().saturating_sub(rows) as i32);
                self.blank_bars[v].set_rect(Rect::new(0, 0, 0, 0));
            } else {
                self.blank_bars[v].set_rect(Self::bar_rect(cell));
            }
        }

        if self.reveal_pending && area.h > 0 {
            self.reveal_pending = false;
            for col in &mut self.columns {
                if let Some(lead) = col.lead {
                    // Put the lead row a third of the way down, not on an edge.
                    col.bar.set_value(lead as i32 - rows as i32 / 3);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Drag and drop.
    // ------------------------------------------------------------------

    /// What a slot of the path view shows.
    fn slot_info(&self, slot: usize) -> Option<SlotInfo> {
        if let Some(col) = self.columns.get(slot) {
            let is_root = col.dir.parent().is_none();
            return Some(SlotInfo {
                icon: FOLDER_LARGE,
                label: fsops::display_name(&col.dir),
                paths: if is_root { vec![] } else { vec![col.dir.clone()] },
            });
        }
        let last = self.columns.last()?;
        if slot != self.columns.len() || last.selected.is_empty() {
            return None;
        }
        let paths = last.selected_paths();
        Some(match &last.selected[..] {
            [i] => SlotInfo {
                icon: if last.entries[*i].is_dir { FOLDER_LARGE } else { FILE_LARGE },
                label: last.entries[*i].label.clone(),
                paths,
            },
            many => SlotInfo {
                icon: FILES_LARGE,
                label: format!("{} items", many.len()),
                paths,
            },
        })
    }

    /// What under `pos` would take a drop, whatever is dragged: a folder, or
    /// a gap on the shelf.
    fn raw_target_at(&self, pos: Point) -> Option<Target> {
        let folder = |dir: PathBuf, highlight| Some(Target { dest: Dest::Folder(dir), highlight });
        if self.shelf_rect().contains(pos) {
            // The middle of a place is its icon; around it, the gaps.
            if let Some(place) = self.shelf_place_at(pos) {
                let cell = self.shelf_cell(place);
                let icon = Rect::new(cell.x + cell.w / 4, cell.y, cell.w / 2, cell.h);
                if icon.contains(pos) {
                    return folder(self.shelf_path(place)?, Highlight::ShelfIcon(place));
                }
            }
            let gap = self.shelf_gap_at(pos)?;
            return Some(Target {
                dest: Dest::Shelf(gap),
                highlight: Highlight::ShelfGap(gap),
            });
        }
        if self.path_rect().contains(pos) {
            let slot = self.path_slot_at(pos)?;
            let col = self.columns.get(slot)?;
            return folder(col.dir.clone(), Highlight::PathIcon(slot));
        }
        let slot = self.column_slot_at(pos)?;
        let col = self.columns.get(slot)?;
        if let Some(row) = self.row_at(slot, pos)
            && col.entries[row].is_dir
        {
            return folder(col.path_of(row), Highlight::Row(slot, row));
        }
        folder(col.dir.clone(), Highlight::Column(slot))
    }

    /// What a drag over the window would do, dropped at `pos` with `mods`
    /// held: one of our own handed to the desktop as it would inside the
    /// window, another application's as a copy.
    fn hover_at(&self, pos: Point, mods: Modifiers) -> Hover {
        let target = match &self.os_drag {
            Some(OsDrag::Bookmark(_)) => return Hover::Bookmark { gap: self.shelf_gap_at(pos) },
            Some(OsDrag::Files(paths)) => {
                let op = drop_op(mods);
                self.target_at(pos, paths, op).map(|target| (target, op))
            }
            None => self.raw_target_at(pos).map(|target| (target, Op::Copy)),
        };
        match target {
            Some((target, op)) => Hover::Drop { target, op },
            None => Hover::Nowhere,
        }
    }

    fn target_at(&self, pos: Point, paths: &[PathBuf], op: Op) -> Option<Target> {
        self.raw_target_at(pos).filter(|t| match &t.dest {
            Dest::Folder(dir) => fsops::can_drop(paths, dir, op),
            Dest::Shelf(_) => self.can_shelve(paths),
        })
    }

    /// Drops `paths` on `target`.
    fn drop_on(&mut self, target: Target, paths: &[PathBuf], op: Op) {
        match target.dest {
            Dest::Folder(dir) => {
                self.transfer(paths, &dir, op);
            }
            Dest::Shelf(at) => self.shelve(paths, at),
        }
    }

    fn highlight(&self) -> Option<Highlight> {
        match (&self.gesture, &self.os_hover) {
            (Gesture::Drag { target, .. }, _) => target.as_ref().map(|t| t.highlight.clone()),
            (_, Some(Hover::Drop { target, .. })) => Some(target.highlight.clone()),
            (_, Some(Hover::Bookmark { gap: Some(gap) })) => Some(Highlight::ShelfGap(*gap)),
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Pointer handling.
    // ------------------------------------------------------------------

    fn pointer_down(&mut self, event: &Event, pos: Point, mods: Modifiers, ctx: &mut EventCtx) {
        self.os_drag = None;
        if self.hscroll.rect().contains(pos) {
            self.hscroll.event(event, ctx);
            self.gesture = Gesture::HScroll;
            self.follow_hscroll();
            return;
        }
        if self.shelf_rect().contains(pos) {
            if let Some(place) = self.shelf_place_at(pos) {
                self.gesture = Gesture::ShelfPress { origin: pos, place };
            }
            return;
        }
        if let Some(slot) = self.path_slot_at(pos) {
            let paths = self.slot_info(slot).map(|s| s.paths).unwrap_or_default();
            self.gesture = Gesture::Press {
                origin: pos,
                paths,
                click: Click::PathIcon(slot),
            };
            return;
        }
        let Some(slot) = self.column_slot_at(pos) else {
            return;
        };
        if slot >= self.columns.len() {
            return; // an empty column past the end of the path
        }
        if self.columns[slot].bar.rect().contains(pos) {
            self.columns[slot].bar.event(event, ctx);
            self.gesture = Gesture::ColumnBar(slot);
            return;
        }
        let Some(row) = self.row_at(slot, pos) else {
            // Below the last row: deselect, showing this column's folder.
            self.columns[slot].clear_selection();
            self.set_selection(slot, vec![]);
            return;
        };

        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(s, r, t)| s == slot && r == row && now - t < DOUBLE_CLICK);
        self.last_click = if double { None } else { Some((slot, row, now)) };

        let mut click = Click::Nothing;
        let col = &mut self.columns[slot];
        let toggle = mods.control || mods.logo;
        if mods.shift && let Some(anchor) = col.anchor {
            let (a, b) = (anchor.min(row), anchor.max(row));
            col.lead = Some(row);
            self.set_selection(slot, (a..=b).collect());
        } else if toggle {
            let mut sel = col.selected.clone();
            match sel.binary_search(&row) {
                Ok(i) => {
                    sel.remove(i);
                }
                Err(i) => sel.insert(i, row),
            }
            col.anchor = Some(row);
            col.lead = Some(row);
            self.set_selection(slot, sel);
        } else if col.selected.len() > 1 && col.selected.contains(&row) {
            col.lead = Some(row);
            col.anchor = Some(row);
            self.active = slot;
            click = Click::Collapse { slot, row };
        } else {
            col.select_only(row);
            self.set_selection(slot, vec![row]);
        }

        if double {
            self.open_selection();
            return;
        }
        self.gesture = Gesture::Press {
            origin: pos,
            paths: self.columns[slot].selected_paths(),
            click,
        };
    }

    fn pointer_move(&mut self, event: &Event, pos: Point, ctx: &mut EventCtx) {
        match &mut self.gesture {
            Gesture::Idle => {}
            Gesture::HScroll => {
                self.hscroll.event(event, ctx);
                self.follow_hscroll();
            }
            Gesture::ColumnBar(slot) => {
                let slot = *slot;
                if let Some(col) = self.columns.get_mut(slot) {
                    col.bar.event(event, ctx);
                }
            }
            Gesture::Press { origin, paths, .. } => {
                let (dx, dy) = (pos.x - origin.x, pos.y - origin.y);
                if dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD && !paths.is_empty() {
                    let paths = std::mem::take(paths);
                    if wayland_session() {
                        // The desktop's drag from the start, as in other
                        // programs: over this window it comes back as drag
                        // events.
                        let actions = [DropAction::Move, DropAction::Ask];
                        ctx.start_drag_offering(DragData::from_paths(paths.clone()), &actions);
                        self.os_drag = Some(OsDrag::Files(paths));
                        self.gesture = Gesture::Idle;
                    } else {
                        let op = drop_op(ctx.modifiers());
                        let target = self.target_at(pos, &paths, op);
                        self.gesture = Gesture::Drag { paths, pos, op, target };
                    }
                }
            }
            Gesture::ShelfPress { origin, place } => {
                let (dx, dy) = (pos.x - origin.x, pos.y - origin.y);
                if dx * dx + dy * dy >= DRAG_THRESHOLD * DRAG_THRESHOLD {
                    let place = *place;
                    self.gesture = Gesture::Idle;
                    if place == 0 {
                        // The home folder stays where it is; nor is it a click.
                    } else if wayland_session()
                        && let Some(path) = self.shelf_path(place)
                    {
                        // Copied, never moved: a bookmark dragged onto another
                        // file manager mustn't take its folder along.
                        ctx.start_drag_offering(DragData::from_paths([path]), &[DropAction::Ask]);
                        self.os_drag = Some(OsDrag::Bookmark(place - 1));
                    } else {
                        self.gesture = Gesture::ShelfDrag {
                            index: place - 1,
                            pos,
                            gap: self.shelf_gap_at(pos),
                        };
                    }
                }
            }
            Gesture::ShelfDrag { .. } => {
                let new_gap = self.shelf_gap_at(pos);
                if let Gesture::ShelfDrag { pos: p, gap, .. } = &mut self.gesture {
                    *p = pos;
                    *gap = new_gap;
                }
            }
            Gesture::Drag { .. } => self.aim_drag(pos, drop_op(ctx.modifiers())),
        }
        if let Some(cursor) = self.drag_cursor() {
            ctx.set_cursor(cursor);
        }
        ctx.request_paint();
    }

    /// Points the drag at `pos`, for a drop doing `op`.
    fn aim_drag(&mut self, pos: Point, op: Op) {
        let new_target = match &self.gesture {
            Gesture::Drag { paths, .. } => self.target_at(pos, paths, op),
            _ => return,
        };
        if let Gesture::Drag { pos: p, op: o, target, .. } = &mut self.gesture {
            *p = pos;
            *o = op;
            *target = new_target;
        }
    }

    /// The pointer for the drag going on: what a drop would do there, or that
    /// it can't land there at all.
    fn drag_cursor(&self) -> Option<Cursor> {
        match &self.gesture {
            Gesture::Drag { target: None, .. } => Some(Cursor::NoDrop),
            Gesture::Drag { target: Some(target), op, .. } => Some(match (&target.dest, op) {
                // The shelf keeps a reference to the folder, as a link does.
                (Dest::Shelf(_), _) | (Dest::Folder(_), Op::Link) => Cursor::Alias,
                (Dest::Folder(_), Op::Copy) => Cursor::Copy,
                (Dest::Folder(_), Op::Move) => Cursor::Grabbing,
            }),
            Gesture::ShelfDrag { .. } => Some(Cursor::Grabbing),
            _ => None,
        }
    }

    fn pointer_up(&mut self, event: &Event, pos: Point, mods: Modifiers, ctx: &mut EventCtx) {
        match std::mem::replace(&mut self.gesture, Gesture::Idle) {
            Gesture::Idle => {}
            Gesture::HScroll => {
                self.hscroll.event(event, ctx);
                self.follow_hscroll();
            }
            Gesture::ColumnBar(slot) => {
                if let Some(col) = self.columns.get_mut(slot) {
                    col.bar.event(event, ctx);
                }
            }
            Gesture::Press { click, .. } => match click {
                Click::Nothing => {}
                Click::Collapse { slot, row } => {
                    if slot < self.columns.len() {
                        self.columns[slot].select_only(row);
                        self.set_selection(slot, vec![row]);
                    }
                }
                Click::PathIcon(slot) => self.go_to_slot(slot),
            },
            Gesture::Drag { paths, .. } => {
                let op = drop_op(mods);
                if let Some(target) = self.target_at(pos, &paths, op) {
                    self.drop_on(target, &paths, op);
                }
            }
            Gesture::ShelfPress { place, .. } => self.go_to_place(place),
            Gesture::ShelfDrag { index, gap, .. } => {
                let changed = match gap {
                    Some(gap) => self.shelf.move_to(index, gap),
                    None => self.shelf.remove(index),
                };
                self.shelf_changed(changed);
            }
        }
        ctx.request_paint();
    }

    /// A click on a path-view icon: show that folder as the end of the path.
    fn go_to_slot(&mut self, slot: usize) {
        if slot + 1 >= self.slots() {
            return; // already the end
        }
        self.columns.truncate(slot + 1);
        self.columns[slot].clear_selection();
        self.active = slot;
        self.reveal_end();
    }

    fn follow_hscroll(&mut self) {
        let value = self.hscroll.value().max(0) as usize;
        if value != self.first {
            self.first = value;
            self.sync();
        }
    }

    fn scroll(&mut self, pos: Point, delta_x: f32, delta_y: f32, ctx: &mut EventCtx) {
        if self.path_rect().contains(pos) || self.hscroll.rect().contains(pos) {
            let ev = Event::Scroll {
                pos,
                delta_x: delta_x + delta_y,
                delta_y: 0.0,
            };
            self.hscroll.event(&ev, ctx);
            self.follow_hscroll();
        } else if self.column_slot_at(pos) == Some(self.columns.len()) && self.preview.is_some() {
            // The info column scrolls by the pixel.
            self.info_scroll = (self.info_scroll + (delta_y * ROW_H as f32) as i32).max(0);
        } else if let Some(slot) = self.column_slot_at(pos)
            && let Some(col) = self.columns.get_mut(slot)
        {
            col.bar.event(
                &Event::Scroll {
                    pos,
                    delta_x: 0.0,
                    delta_y,
                },
                ctx,
            );
        } else if delta_x != 0.0 {
            let ev = Event::Scroll {
                pos,
                delta_x,
                delta_y: 0.0,
            };
            self.hscroll.event(&ev, ctx);
            self.follow_hscroll();
        }
        // While dragging, what's under the pointer has just changed.
        if let Gesture::Drag { pos, .. } = self.gesture {
            self.aim_drag(pos, drop_op(ctx.modifiers()));
        }
        ctx.request_paint();
    }

    // ------------------------------------------------------------------
    // Keyboard handling.
    // ------------------------------------------------------------------

    fn key_down(&mut self, key: &Key, mods: Modifiers, ctx: &mut EventCtx) {
        // Escape calls off a drag, or a press that would become one. The button
        // is still held: its release finds the gesture idle and does nothing.
        if matches!(key, Key::Named(NamedKey::Escape))
            && matches!(
                self.gesture,
                Gesture::Press { .. }
                    | Gesture::Drag { .. }
                    | Gesture::ShelfPress { .. }
                    | Gesture::ShelfDrag { .. }
            )
        {
            self.gesture = Gesture::Idle;
            ctx.request_paint();
            return;
        }
        if mods.control || mods.alt || mods.logo {
            let home = Accel::primary('h').shift();
            match key {
                Key::Named(NamedKey::Up) => self.go_parent(),
                // Home's chord from before the Go menu listed the shelf.
                _ if home.matches(*key, mods, ModifierScheme::native()) => self.go_home(),
                _ => return, // the menu bar's accelerators
            }
            ctx.request_paint();
            return;
        }
        let slot = self.active.min(self.columns.len() - 1);
        let len = self.columns[slot].entries.len();
        let page = self.rows_per_column().saturating_sub(1).max(1);
        let lead = self.columns[slot].lead;
        let target = match key {
            Key::Named(NamedKey::Up) => Some(lead.map_or(0, |l| l.saturating_sub(1))),
            Key::Named(NamedKey::Down) => Some(lead.map_or(0, |l| l + 1)),
            Key::Named(NamedKey::PageUp) => Some(lead.map_or(0, |l| l.saturating_sub(page))),
            Key::Named(NamedKey::PageDown) => Some(lead.map_or(0, |l| l + page)),
            Key::Named(NamedKey::Home) => Some(0),
            Key::Named(NamedKey::End) => Some(len.saturating_sub(1)),
            Key::Named(NamedKey::Left | NamedKey::Backspace) => {
                self.go_parent();
                None
            }
            Key::Named(NamedKey::Right) => {
                self.enter_next_column();
                None
            }
            Key::Named(NamedKey::Enter) => {
                self.open_selection();
                None
            }
            _ => return,
        };
        if let Some(row) = target
            && len > 0
        {
            if mods.shift {
                self.extend_selection(slot, row.min(len - 1));
            } else {
                self.select_row(slot, row.min(len - 1));
            }
        }
        ctx.consume_event();
        ctx.request_paint();
    }

    /// Type-ahead: jump to the first entry of the active column starting with
    /// what was typed in the last second.
    fn type_ahead(&mut self, ch: char, ctx: &mut EventCtx) {
        if ch.is_control() {
            return;
        }
        if self.typed_at.elapsed() > TYPE_AHEAD {
            self.typed.clear();
        }
        self.typed_at = Instant::now();
        self.typed.extend(ch.to_lowercase());
        let slot = self.active.min(self.columns.len() - 1);
        let hit = self.columns[slot]
            .entries
            .iter()
            .position(|e| e.label.to_lowercase().starts_with(&self.typed));
        if let Some(row) = hit {
            self.select_row(slot, row);
            ctx.request_paint();
        }
    }

    // ------------------------------------------------------------------
    // Painting.
    // ------------------------------------------------------------------

    fn paint_shelf(&mut self, p: &mut Painter, theme: &Theme) {
        // A sunken frame, but no fill: the window's background shows through.
        let area = self.shelf_rect();
        p.sunken_bevel(area, theme.highlight, theme.shadow);
        let saved = p.push_clip_frame(area, 1, Merged::NONE);
        let highlight = self.highlight();
        let pressed = match self.gesture {
            Gesture::ShelfPress { place, .. } => Some(place),
            _ => None,
        };
        for place in 0..=self.shelf.len() {
            let cell = self.shelf_cell(place);
            let icon = Self::shelf_icon_rect(cell);
            if highlight == Some(Highlight::ShelfIcon(place)) {
                fill_rounded_rect(p, icon.inset(-3), 4, theme.highlight_bg);
            }
            let (image, there) = match place {
                0 => (HOME_LARGE, true),
                _ => (FOLDER_LARGE, self.shelf_path(place).is_some_and(|p| p.is_dir())),
            };
            // The place being clicked lights up as a selected row does; the
            // one the viewer is in, as a selected row in a column without the
            // keyboard.
            let lit = if pressed == Some(place) {
                Some((theme.highlight_bg, theme.highlight_text))
            } else if self.is_current_place(place) {
                Some((theme.face, theme.text))
            } else {
                None
            };
            if let Some((bg, _)) = lit {
                fill_rounded_rect(p, icon.inset(-3), 4, bg);
            }
            p.draw_svg(&image, icon);
            let label = fit_text(p, &self.shelf_label(place), theme.font_size, cell.w - 6);
            let size = p.measure_text(&label, theme.font_size);
            let (x, y) = (cell.x + (cell.w - size.w) / 2, icon.bottom() + 5);
            // A folder that's gone — deleted, or on a disk not mounted —
            // stays on the shelf, greyed out.
            let color = match lit {
                Some((bg, fg)) => {
                    p.fill_rect(Rect::new(x - 2, y, size.w + 4, size.h), bg);
                    fg
                }
                None if there => theme.text,
                None => theme.disabled_text,
            };
            p.text(x, y, &label, theme.font_size, color);
        }
        // Where something dropped would go: a bar after the place before it.
        let gap = match (&self.gesture, highlight) {
            (Gesture::ShelfDrag { gap, .. }, _) => *gap,
            (_, Some(Highlight::ShelfGap(gap))) => Some(gap),
            _ => None,
        };
        if let Some(gap) = gap {
            let cell = self.shelf_cell(gap);
            let x = (cell.right() - 1).min(area.right() - 3);
            p.fill_rect(Rect::new(x, cell.y + 4, 2, cell.h - 8), theme.highlight_bg);
        }
        p.restore_clip(saved);
    }

    fn paint_path_view(&mut self, p: &mut Painter, theme: &Theme) {
        let area = self.path_rect();
        p.fill_rect(area, theme.background);
        p.sunken_bevel(area, theme.highlight, theme.shadow);
        p.stroke_rect(area, theme.border);
        let saved = p.push_clip_frame(area, 1, Merged::NONE);
        let slots = self.slots();
        let highlight = self.highlight();
        for v in 0..self.visible {
            let slot = self.first + v;
            let Some(info) = self.slot_info(slot) else { break };
            let cell = self.cell(v, area);
            let icon = Rect::new(
                cell.x + (cell.w - LARGE_ICON) / 2,
                area.y + 7,
                LARGE_ICON,
                LARGE_ICON,
            );
            if highlight == Some(Highlight::PathIcon(slot)) {
                p.fill_rect(icon.inset(-3), theme.highlight_bg);
            }
            p.draw_svg(&info.icon, icon);

            let label = fit_text(p, &info.label, theme.font_size, cell.w - 8);
            let w = p.measure_text(&label, theme.font_size).w;
            let ly = icon.bottom() + 5;
            p.text(cell.x + (cell.w - w) / 2, ly, &label, theme.font_size, theme.text);

            if slot + 1 < slots && v + 1 < self.visible {
                // The little arrow leading on to the next step of the path.
                let x = cell.right() + COLUMN_GAP / 2 - 2;
                draw_arrow(p, x, icon.y + LARGE_ICON / 2, theme.text);
            }
        }
        p.restore_clip(saved);
    }

    fn paint_column(&mut self, p: &mut Painter, theme: &Theme, v: usize) {
        let cell = self.cell(v, self.columns_rect());
        let field = Self::field_rect(cell);
        let slot = self.first + v;
        p.fill_rect(field, theme.background);
        p.sunken_bevel_merged(field, Merged::RIGHT, theme.highlight, theme.shadow);
        p.stroke_rect_merged(field, Merged::RIGHT, theme.border);

        let highlight = self.highlight();
        let active = self.focused && slot == self.active;
        let renaming = self.rename_target().filter(|&(s, _)| s == slot).map(|(_, row)| row);
        if let Some(col) = self.columns.get(slot) {
            let saved = p.push_clip_frame(field, 1, Merged::RIGHT);
            let top = col.bar.value().max(0) as usize;
            let rows = self.rows_per_column();
            for (i, entry) in col.entries.iter().enumerate().skip(top).take(rows + 1) {
                let y = field.y + ROW_PAD_Y + (i - top) as i32 * ROW_H;
                let row_rect = Rect::new(field.x, y, field.w, ROW_H);
                let selected = col.selected.binary_search(&i).is_ok();
                let target = highlight == Some(Highlight::Row(slot, i));
                let (fg, bg) = if target || (selected && active) {
                    (theme.highlight_text, Some(theme.highlight_bg))
                } else if selected {
                    (theme.text, Some(theme.face))
                } else if self.clipboard.is_cut(&col.path_of(i)) {
                    // Cut, waiting to be pasted elsewhere.
                    (theme.disabled_text, None)
                } else {
                    (theme.text, None)
                };
                if let Some(bg) = bg {
                    p.fill_rect(row_rect, bg);
                }
                let icon_x = field.x + 4;
                let icon = if entry.is_dir { FOLDER_ICON } else { FILE_ICON };
                p.draw_svg(
                    &icon,
                    Rect::new(icon_x, y + (ROW_H - SMALL_ICON) / 2, SMALL_ICON, SMALL_ICON),
                );
                if renaming == Some(i) {
                    continue; // the text field goes there
                }
                let label_x = icon_x + SMALL_ICON + 4;
                let arrow_w = if entry.is_dir { 14 } else { 4 };
                let label = fit_text(p, &entry.label, theme.font_size, field.right() - label_x - arrow_w);
                let label_y = y + (ROW_H - theme.font_size as i32) / 2 - 1;
                p.text(label_x, label_y, &label, theme.font_size, fg);
                if entry.is_dir {
                    draw_arrow(p, field.right() - 10, y + ROW_H / 2, fg);
                }
                if active && col.lead == Some(i) && col.selected.len() > 1 {
                    p.focus_rect(row_rect.inset(1), fg);
                }
            }
            if let Some(err) = &col.error {
                let msg = fit_text(p, err, theme.font_size, field.w - 12);
                p.text(field.x + 6, field.y + 6, &msg, theme.font_size, theme.disabled_text);
            }
            p.restore_clip(saved);
            if highlight == Some(Highlight::Column(slot)) {
                let r = field.inset(2);
                p.stroke_rect(r, theme.highlight_bg);
                p.stroke_rect(r.inset(1), theme.highlight_bg);
            }
            self.columns[slot].bar.paint(p, theme);
        } else if slot == self.columns.len() && self.preview.is_some() {
            self.paint_info(p, theme, cell);
        } else {
            self.blank_bars[v].paint(p, theme);
        }
    }

    /// Brings the info column up to date with the selection of the last
    /// column, and asks for the thumbnail it shows.
    fn refresh_preview(&mut self) {
        let key = self
            .columns
            .last()
            .map(BrowserColumn::selected_paths)
            .unwrap_or_default();
        if key != self.preview_key {
            self.preview = match &key[..] {
                [] => None,
                [one] => FileInfo::gather(one).map(Preview::One),
                many => Some(Preview::Many(info::summarize(many))),
            };
            self.preview_key = key;
            self.info_scroll = 0;
        }
        if let Some(Preview::One(info)) = &self.preview
            && !info.is_dir
        {
            let size = thumbs::Size::for_pixels(PREVIEW_MAX as f32 * self.scale);
            self.thumbs.get(&info.path, &info.mime, info.mtime, size);
        }
    }

    /// The info column: thumbnail or icon, name, and a table of facts.
    fn paint_info(&mut self, p: &mut Painter, theme: &Theme, field: Rect) {
        p.fill_rect(field, theme.background);
        p.sunken_bevel(field, theme.highlight, theme.shadow);
        p.stroke_rect(field, theme.border);
        let Some(preview) = &self.preview else {
            return;
        };
        let saved = p.push_clip_frame(field, 1, Merged::NONE);
        let x = field.x + INFO_PAD;
        let inner_w = (field.w - 2 * INFO_PAD).max(0);
        self.info_scroll = self.info_scroll.min(self.info_height - field.h).max(0);
        let top = field.y + INFO_PAD + 4 - self.info_scroll;
        let mut y = top;
        let font = theme.font_size;
        let line_h = (font * 1.45).round() as i32;

        // The picture: a thumbnail, or the type's icon while there is none.
        let (thumb, icon, name, rows) = match preview {
            Preview::One(info) => {
                let thumb = if info.is_dir {
                    None
                } else {
                    let size = thumbs::Size::for_pixels(PREVIEW_MAX as f32 * self.scale);
                    match self.thumbs.get(&info.path, &info.mime, info.mtime, size) {
                        Thumb::Ready(img) => Some(img.clone()),
                        _ => None,
                    }
                };
                let icon = if info.is_dir { FOLDER_LARGE } else { FILE_LARGE };
                (thumb, icon, info.name.clone(), &info.rows)
            }
            Preview::Many(rows) => (
                None,
                FILES_LARGE,
                format!("{} items", self.preview_key.len()),
                rows,
            ),
        };
        match thumb {
            Some(img) => {
                let side = inner_w.min(PREVIEW_MAX).min((field.h * 2 / 5).max(48));
                let (w, h) = (img.width as i32, img.height as i32);
                // Fit the box, but never past the thumbnail's own size.
                let f = (side as f32 / w.max(h) as f32).min(1.0);
                let (dw, dh) = (
                    ((w as f32 * f).round() as i32).max(1),
                    ((h as f32 * f).round() as i32).max(1),
                );
                let rect = Rect::new(field.x + (field.w - dw) / 2, y, dw, dh);
                let flat = flatten(&img.pixels, theme.background);
                p.draw_resampled(rect, Size::new(w, h), 1.0, theme.background, |q| {
                    q.blit_argb(0, 0, img.width, img.height, &flat)
                });
                p.stroke_rect(rect.inset(-1), theme.shadow);
                y += dh + 10;
            }
            None => {
                let size = 48;
                p.draw_svg(&icon, Rect::new(field.x + (field.w - size) / 2, y, size, size));
                y += size + 8;
            }
        }

        // The name, bold and centered, wrapped as needed.
        let bold = |p: &Painter, s: &str| {
            p.measure_text_styled(s, font, FontFamily::Sans, FontStyle::Bold).w
        };
        let lines: Vec<String> = p
            .wrap_text_styled(&name, font, inner_w, FontFamily::Sans, FontStyle::Bold)
            .into_iter()
            .map(str::to_string)
            .collect();
        for line in lines {
            let w = bold(p, &line);
            p.text_styled(
                field.x + (field.w - w) / 2,
                y,
                &line,
                font,
                theme.text,
                FontFamily::Sans,
                FontStyle::Bold,
            );
            y += line_h;
        }
        y += 8;

        // The facts: labels right-aligned against their values — or, where
        // the column is too narrow for a date beside its label, each label
        // above its value.
        let label_w = rows
            .iter()
            .map(|(k, _)| p.measure_text(&format!("{k}:"), font).w)
            .max()
            .unwrap_or(0);
        let beside = inner_w - label_w - 6 >= p.measure_text("0000-00-00 00:00", font).w;
        let (value_x, gap) = if beside {
            (x + label_w + 6, 2)
        } else {
            (x + 8, 6)
        };
        let value_w = (field.right() - INFO_PAD - value_x).max(1);
        for (label, value) in rows {
            let label = format!("{label}:");
            if beside {
                let lw = p.measure_text(&label, font).w;
                p.text(x + label_w - lw, y, &label, font, theme.disabled_text);
            } else {
                p.text(x, y, &label, font, theme.disabled_text);
                y += line_h;
            }
            let lines: Vec<String> = p
                .wrap_text(value, font, value_w)
                .into_iter()
                .map(str::to_string)
                .collect();
            for line in &lines {
                p.text(value_x, y, line, font, theme.text);
                y += line_h;
            }
            if lines.is_empty() {
                y += line_h;
            }
            y += gap;
        }
        self.info_height = y - top + 2 * INFO_PAD;
        p.restore_clip(saved);
    }

    fn status_text(&self) -> String {
        if let Gesture::Drag { paths, op, target, .. } = &self.gesture {
            return drag_status(paths, *op, target.as_ref());
        }
        if let Gesture::ShelfDrag { index, gap, .. } = &self.gesture {
            return self.shelf_drag_status(*index, *gap);
        }
        // A drag of our own handed to the desktop, while it's over the window.
        match (&self.os_drag, &self.os_hover) {
            (Some(OsDrag::Files(paths)), Some(Hover::Drop { target, op })) => {
                return drag_status(paths, *op, Some(target));
            }
            (Some(OsDrag::Files(paths)), Some(_)) => return drag_status(paths, Op::Move, None),
            (Some(OsDrag::Bookmark(index)), Some(Hover::Bookmark { gap })) => {
                return self.shelf_drag_status(*index, *gap);
            }
            _ => {}
        }
        let last = self.columns.last().expect("the root column");
        match &last.selected[..] {
            [] => {
                if let Some(err) = &last.error {
                    format!("{} — {err}", last.dir.display())
                } else {
                    let n = last.entries.len();
                    let items = if n == 1 { "item" } else { "items" };
                    format!("{} — {n} {items}", last.dir.display())
                }
            }
            [i] => {
                let path = last.path_of(*i);
                match std::fs::metadata(&path) {
                    Ok(m) => format!("{} — {}", path.display(), fsops::human_size(m.len())),
                    Err(_) => path.display().to_string(),
                }
            }
            many => format!("{} items selected in {}", many.len(), last.dir.display()),
        }
    }

    /// The status line while the `index`-th bookmark is dragged, to the
    /// shelf's `gap` or off the shelf.
    fn shelf_drag_status(&self, index: usize, gap: Option<usize>) -> String {
        let name = self.shelf_label(index + 1);
        match gap {
            Some(_) => format!("Move “{name}” along the shelf — drag it off to take it off"),
            None => format!("Take “{name}” off the shelf"),
        }
    }

    fn paint_drag_image(&self, p: &mut Painter, theme: &Theme) {
        let (icon, label, pos) = match &self.gesture {
            Gesture::Drag { paths, pos, .. } => {
                let icon = match &paths[..] {
                    [one] if one.is_dir() => FOLDER_LARGE,
                    [_] => FILE_LARGE,
                    _ => FILES_LARGE,
                };
                let label = match &paths[..] {
                    [one] => fsops::display_name(one),
                    many => format!("{} items", many.len()),
                };
                (icon, label, pos)
            }
            Gesture::ShelfDrag { index, pos, .. } => {
                (FOLDER_LARGE, self.shelf_label(index + 1), pos)
            }
            _ => return,
        };
        let icon_rect = Rect::new(pos.x + 8, pos.y + 8, LARGE_ICON, LARGE_ICON);
        p.draw_svg(&icon, icon_rect);
        let label = fit_text(p, &label, theme.font_size, 200);
        let size = p.measure_text(&label, theme.font_size);
        let box_rect = Rect::new(
            icon_rect.x + LARGE_ICON / 2 - size.w / 2 - 3,
            icon_rect.bottom() + 2,
            size.w + 6,
            ROW_H,
        );
        p.fill_rect(box_rect, theme.highlight_bg);
        p.text(
            box_rect.x + 3,
            box_rect.y + (ROW_H - theme.font_size as i32) / 2 - 1,
            &label,
            theme.font_size,
            theme.highlight_text,
        );
    }
}

impl Widget for Browser {
    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.sync();
    }

    fn paint(&mut self, p: &mut Painter, theme: &Theme) {
        self.flush_errors();
        self.refresh_shelf();
        self.sync();
        self.scale = p.scale();
        self.refresh_preview();
        if self.shelf_view {
            self.paint_shelf(p, theme);
        }
        if self.path_view {
            self.paint_path_view(p, theme);
            self.hscroll.paint(p, theme);
        }
        for v in 0..self.visible {
            self.paint_column(p, theme, v);
        }
        if let Some(rect) = self.rename_rect()
            && let Some(r) = &mut self.rename
        {
            r.input.rect = rect;
            r.input.paint(p, theme);
        }
        let status = self.status_rect();
        let text = fit_text(p, &self.status_text(), theme.font_size, status.w);
        p.text(
            status.x,
            status.y + (STATUS_H - theme.font_size as i32) / 2 - 1,
            &text,
            theme.font_size,
            theme.text,
        );
    }

    fn paint_overlay(&mut self, p: &mut Painter, theme: &Theme) {
        self.paint_drag_image(p, theme);
    }

    fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
        self.handle(event, ctx);
        // Ask for the thumbnail now, not at paint time, so the runtime sees
        // `wants_ticks` and keeps polling until it arrives.
        self.refresh_preview();
    }

    fn wants_ticks(&self) -> bool {
        // While a name is edited, for the caret's blink.
        self.thumbs.pending() || self.rename.is_some()
    }

    fn captures_pointer(&self) -> bool {
        // Editing a name keeps the keyboard from the menu's accelerators, so
        // Delete, Ctrl+C, and the like edit the text.
        !matches!(self.gesture, Gesture::Idle) || self.rename.is_some()
    }

    fn focusable(&self) -> bool {
        true
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(r) = &mut self.rename {
            r.input.set_focused(focused);
        }
    }
}

impl Browser {
    fn handle(&mut self, event: &Event, ctx: &mut EventCtx) {
        if self.rename_event(event, ctx) {
            return;
        }
        match event {
            Event::PointerDown {
                pos,
                button: MouseButton::Left,
                modifiers,
            } => {
                self.pointer_down(event, *pos, *modifiers, ctx);
                ctx.request_paint();
            }
            Event::PointerMove { pos } => self.pointer_move(event, *pos, ctx),
            Event::PointerUp {
                pos,
                button: MouseButton::Left,
                modifiers,
            } => self.pointer_up(event, *pos, *modifiers, ctx),
            Event::Scroll {
                pos,
                delta_x,
                delta_y,
            } => self.scroll(*pos, *delta_x, *delta_y, ctx),
            Event::Tick => {
                self.thumbs.poll();
                self.hscroll.event(event, ctx);
                self.follow_hscroll();
                for col in &mut self.columns {
                    col.bar.event(event, ctx);
                }
                ctx.request_paint();
            }
            Event::KeyDown { key, modifiers } => self.key_down(key, *modifiers, ctx),
            Event::Char { ch, modifiers } if !modifiers.has_command() => {
                self.type_ahead(*ch, ctx)
            }
            Event::DragEnter { pos } | Event::DragMove { pos } => {
                if !ctx.is_dragging_out() {
                    self.os_drag = None; // over, and this one is another's
                }
                let hover = self.hover_at(*pos, ctx.modifiers());
                if let Some(action) = hover.action() {
                    ctx.accept_drop_as(action);
                }
                self.os_hover = Some(hover);
                ctx.request_paint();
            }
            Event::DragLeave => {
                self.os_hover = None;
                ctx.request_paint();
            }
            Event::Drop { pos, data } => {
                let hover = self.os_hover.take();
                match (self.os_drag.take(), hover) {
                    // Our own drag, come back: what it showed it would do.
                    (Some(OsDrag::Files(paths)), Some(Hover::Drop { target, op })) => {
                        self.drop_on(target, &paths, op);
                    }
                    (Some(OsDrag::Bookmark(index)), Some(Hover::Bookmark { gap })) => {
                        let changed = match gap {
                            Some(gap) => self.shelf.move_to(index, gap),
                            None => self.shelf.remove(index),
                        };
                        self.shelf_changed(changed);
                    }
                    // Another application's is copied in.
                    (None, _) if !data.paths.is_empty() => {
                        if let Some(target) = self.target_at(*pos, &data.paths, Op::Copy) {
                            self.drop_on(target, &data.paths, Op::Copy);
                        }
                    }
                    _ => {}
                }
                ctx.request_paint();
            }
            _ => {}
        }
    }
}

// ============================================================================
// Helpers.
// ============================================================================

/// `pixels` (straight-alpha ARGB) composited over `bg`, opaque.
fn flatten(pixels: &[u32], bg: Color) -> Vec<u32> {
    let blend = |s: u32, d: u32, a: u32| (s * a + d * (255 - a) + 127) / 255;
    pixels
        .iter()
        .map(|&px| {
            let a = px >> 24;
            let ch = |shift: u32| blend((px >> shift) & 0xff, (bg.0 >> shift) & 0xff, a) << shift;
            0xff00_0000 | ch(16) | ch(8) | ch(0)
        })
        .collect()
}

/// What a drop does with the modifiers `mods` held: Ctrl or Alt (or the logo
/// key, Command on a Mac) copies, Ctrl+Shift links, and nothing moves.
fn drop_op(mods: Modifiers) -> Op {
    if mods.control && mods.shift {
        Op::Link
    } else if mods.control || mods.alt || mods.logo {
        Op::Copy
    } else {
        Op::Move
    }
}

/// "notes.txt", or "3 items".
/// The status line while `paths` are dragged, to do `op` on `target`.
fn drag_status(paths: &[PathBuf], op: Op, target: Option<&Target>) -> String {
    let what = describe(paths);
    match target.map(|t| &t.dest) {
        Some(Dest::Folder(dir)) => {
            let dir = fsops::display_name(dir);
            match op {
                Op::Move => format!("Move {what} to “{dir}” — hold Ctrl to copy, Ctrl+Shift to link"),
                Op::Copy => format!("Copy {what} to “{dir}”"),
                Op::Link => format!("Link to {what} from “{dir}”"),
            }
        }
        Some(Dest::Shelf(_)) => format!("Put {what} on the shelf"),
        None => format!("Drag {what} onto a folder"),
    }
}

fn describe(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("“{}”", fsops::display_name(one)),
        many => format!("{} items", many.len()),
    }
}

/// `text`, cut short with an ellipsis if it is wider than `max_w`.
fn fit_text(p: &Painter, text: &str, size: f32, max_w: i32) -> String {
    if max_w <= 0 {
        return String::new();
    }
    if p.measure_text(text, size).w <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().chain(['…'].iter()).collect();
        if p.measure_text(&candidate, size).w <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().chain(['…'].iter()).collect()
}

/// Fills `rect` with its corners rounded off by `radius`, crisp in device
/// pixels.
fn fill_rounded_rect(p: &mut Painter, rect: Rect, radius: i32, color: Color) {
    let r = (radius as f32 * p.scale()).round() as i32;
    p.physical(rect, |p, rect| {
        let r = r.min(rect.w / 2).min(rect.h / 2);
        for dy in 0..r {
            // How far in the curve is on this row, measured at its middle.
            let off = (r - dy) as f32 - 0.5;
            let inset = r - ((r * r) as f32 - off * off).max(0.0).sqrt().round() as i32;
            let w = rect.w - 2 * inset;
            p.fill_rect(Rect::new(rect.x + inset, rect.y + dy, w, 1), color);
            p.fill_rect(Rect::new(rect.x + inset, rect.bottom() - 1 - dy, w, 1), color);
        }
        p.fill_rect(Rect::new(rect.x, rect.y + r, rect.w, rect.h - 2 * r), color);
    });
}

/// A small right-pointing triangle, 4 px wide and 7 tall, its point at
/// `(x + 3, cy)` — NeXT's marker for "there's more to the right".
fn draw_arrow(p: &mut Painter, x: i32, cy: i32, color: Color) {
    for i in 0..4 {
        p.v_line(x + i, cy - 3 + i, 7 - 2 * i, color);
    }
}

fn wayland_session() -> bool {
    cfg!(all(unix, not(target_os = "macos")))
        && std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
}
