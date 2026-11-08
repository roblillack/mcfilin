//! McFilin — a file viewer with Miller columns, after NeXTSTEP's File Viewer,
//! built on the Saudade GUI toolkit.

pub mod browser;
pub mod clipboard;
pub mod fsops;
pub mod info;
pub mod mime;
pub mod settings;
pub mod shelf;
pub mod thumbs;
pub mod trash;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use saudade::{
    Accel, Column, Dialog, Event, EventCtx, Menu, MenuBar, MenuItem, NamedKey, Painter,
    PopupRequest, Rect, SvgImage, Theme, Widget, WindowConfig, include_svg,
};

use browser::{Browser, MIN_COLUMN_WIDTH};
use settings::{Settings, WindowState};

const WINDOW_W: i32 = 760;
const WINDOW_H: i32 = 480;
const MENU_BAR_H: i32 = 20;
const MIN_W: i32 = MIN_COLUMN_WIDTH * 2 + 40;
const MIN_H: i32 = 280;

/// The program's icon, for the title bar, the taskbar and the dock.
const ICON: SvgImage = include_svg!("assets/icons/mcfilin.svg");

/// The main window and its widget tree, opened at `start` the way `settings`
/// and `state` have it — along with a function that tells the settings and
/// state the window has now, to be saved once it has closed.
pub fn window(
    start: PathBuf,
    settings: &Settings,
    state: &WindowState,
) -> (WindowConfig, impl Widget + use<>, impl Fn() -> (Settings, WindowState) + use<>) {
    let dialog = Rc::new(RefCell::new(Dialog::new()));
    let browser = Rc::new_cyclic(|this| {
        RefCell::new(Browser::new(&start, dialog.clone(), this.clone()))
    });
    browser.borrow_mut().set_shelf_view(settings.shelf);
    browser.borrow_mut().set_path_view(settings.path_view);

    let menu_bar = Menus::new(browser.clone());
    let root = Rc::new(RefCell::new(
        Column::new()
            .add_fixed(menu_bar, MENU_BAR_H)
            .add_fill(Shared(browser.clone()))
            .add_overlay(Shared(dialog)),
    ));

    let (w, h) = state.size.unwrap_or((WINDOW_W, WINDOW_H));
    let config = WindowConfig::new("McFilin", w.max(MIN_W), h.max(MIN_H))
        .icon(ICON)
        .resizable(true)
        .min_size(MIN_W, MIN_H);

    // The root is laid out at the window's size, so its bounds are that size.
    let current = {
        let root = root.clone();
        let saved = state.size;
        move || {
            let b = root.borrow().bounds();
            let browser = browser.borrow();
            let settings = Settings {
                shelf: browser.shelf_view(),
                path_view: browser.path_view(),
            };
            let size = if b.w > 0 && b.h > 0 { Some((b.w, b.h)) } else { saved };
            (settings, WindowState { size })
        }
    };
    (config, Shared(root), current)
}

/// The menu bar, built anew whenever the places on the shelf change so the Go
/// menu lists them.
struct Menus {
    bar: MenuBar,
    browser: Rc<RefCell<Browser>>,
    /// The labels of the bookmarks the Go menu lists now.
    bookmarks: Vec<String>,
}

impl Menus {
    fn new(browser: Rc<RefCell<Browser>>) -> Self {
        let bookmarks = browser.borrow_mut().bookmark_labels();
        let bar = menu_bar(&browser, &bookmarks);
        Self { bar, browser, bookmarks }
    }

    /// Rebuilds the bar if the shelf changed — but not while a menu is open.
    fn sync(&mut self) {
        if self.bar.captures_pointer() {
            return;
        }
        let bookmarks = self.browser.borrow_mut().bookmark_labels();
        if bookmarks != self.bookmarks {
            let bounds = self.bar.bounds();
            self.bar = menu_bar(&self.browser, &bookmarks);
            self.bar.layout(bounds);
            self.bookmarks = bookmarks;
        }
    }
}

impl Widget for Menus {
    fn bounds(&self) -> Rect {
        self.bar.bounds()
    }
    fn paint(&mut self, painter: &mut Painter, theme: &Theme) {
        self.sync();
        self.bar.paint(painter, theme);
    }
    fn paint_overlay(&mut self, painter: &mut Painter, theme: &Theme) {
        self.bar.paint_overlay(painter, theme);
    }
    fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
        if let Event::KeyDown { .. } | Event::PointerDown { .. } = event {
            self.sync();
        }
        self.bar.event(event, ctx);
    }
    fn on_cancel(&mut self, ctx: &mut EventCtx) {
        self.bar.on_cancel(ctx);
    }
    fn captures_pointer(&self) -> bool {
        self.bar.captures_pointer()
    }
    fn accepts_accelerators(&self) -> bool {
        self.bar.accepts_accelerators()
    }
    fn layout(&mut self, bounds: Rect) {
        self.bar.layout(bounds);
    }
    fn popup_request(&self) -> Option<PopupRequest> {
        self.bar.popup_request()
    }
}

/// A menu item's callback: borrows the shared browser, does its thing, and
/// repaints. A name being edited is settled first, as a click elsewhere would.
fn on_browser(
    browser: &Rc<RefCell<Browser>>,
    f: impl Fn(&mut Browser) + 'static,
) -> impl FnMut(&mut EventCtx) + 'static {
    let browser = browser.clone();
    move |cx| {
        let mut browser = browser.borrow_mut();
        browser.finish_rename();
        f(&mut browser);
        cx.request_paint();
    }
}

/// The menu bar. The Go menu lists the places on the shelf — home, then the
/// `bookmarks` by their labels — the first ten by Ctrl+1 … Ctrl+0.
fn menu_bar(browser: &Rc<RefCell<Browser>>, bookmarks: &[String]) -> MenuBar {
    let act = |f: fn(&mut Browser)| on_browser(browser, f);

    let mut go = vec![
        MenuItem::action("&Enclosing Folder", act(Browser::go_parent))
            .with_accel(Accel::primary(NamedKey::Up)),
        MenuItem::action("&Root", act(|b| b.go_to(std::path::Path::new("/")))),
        MenuItem::separator(),
    ];
    let escaped = bookmarks.iter().map(|b| b.replace('&', "&&"));
    for (place, label) in std::iter::once("&Home".to_string()).chain(escaped).enumerate() {
        let mut item = MenuItem::action(label, on_browser(browser, move |b| b.go_to_place(place)));
        if place < 10 {
            let digit = char::from(b'0' + ((place + 1) % 10) as u8);
            item = item.with_accel(Accel::primary(digit));
        }
        go.push(item);
    }
    go.extend([
        MenuItem::separator(),
        MenuItem::action("&Add to Shelf", act(Browser::add_to_shelf)),
        MenuItem::action("Re&move from Shelf", act(Browser::remove_from_shelf)),
    ]);

    MenuBar::new(Rect::new(0, 0, WINDOW_W, MENU_BAR_H))
        .add_menu(Menu::new(
            "&File",
            vec![
                MenuItem::action("&Open", act(Browser::open_selection))
                    .with_accel(Accel::primary('o')),
                MenuItem::action("&New Folder", act(Browser::new_folder))
                    .with_accel(Accel::primary('n').shift()),
                MenuItem::action("Re&name", act(Browser::start_rename))
                    .with_accel(Accel::new(NamedKey::F2)),
                MenuItem::action("&Duplicate", act(Browser::duplicate))
                    .with_accel(Accel::primary('d')),
                MenuItem::separator(),
                MenuItem::action("Move to &Trash", act(Browser::move_to_trash))
                    .with_accel(Accel::new(NamedKey::Delete)),
                MenuItem::action("&Delete Immediately…", act(Browser::delete_immediately))
                    .with_accel(Accel::new(NamedKey::Delete).shift()),
                MenuItem::separator(),
                MenuItem::action("&Quit", |cx| cx.close()).with_accel(Accel::primary('q')),
            ],
        ))
        .add_menu(Menu::new(
            "&Edit",
            vec![
                MenuItem::action("Cu&t", act(Browser::cut)).with_accel(Accel::primary('x')),
                MenuItem::action("&Copy", act(Browser::copy)).with_accel(Accel::primary('c')),
                MenuItem::action("&Paste", act(Browser::paste)).with_accel(Accel::primary('v')),
                MenuItem::separator(),
                MenuItem::action("Select &All", act(Browser::select_all))
                    .with_accel(Accel::primary('a')),
                MenuItem::action("&Deselect All", act(Browser::deselect_all))
                    .with_accel(Accel::primary('a').shift()),
            ],
        ))
        .add_menu(Menu::new(
            "&View",
            vec![
                MenuItem::action("Show &Shelf", act(Browser::toggle_shelf_view)).with_checked({
                    let browser = browser.clone();
                    move || browser.borrow().shelf_view()
                }),
                MenuItem::action("Show &Path", act(Browser::toggle_path_view)).with_checked({
                    let browser = browser.clone();
                    move || browser.borrow().path_view()
                }),
                MenuItem::action("Show &Hidden Files", act(Browser::toggle_hidden))
                    .with_accel(Accel::primary('h'))
                    .with_checked({
                        let browser = browser.clone();
                        move || browser.borrow().show_hidden()
                    }),
                MenuItem::action("&Reload", act(Browser::reload))
                    .with_accel(Accel::primary('r')),
            ],
        ))
        .add_menu(Menu::new("&Go", go))
}

/// A widget shared with the menu callbacks: the column owns this handle, the
/// callbacks hold clones of the `Rc`.
struct Shared<W: Widget>(Rc<RefCell<W>>);

impl<W: Widget> Widget for Shared<W> {
    fn bounds(&self) -> Rect {
        self.0.borrow().bounds()
    }
    fn paint(&mut self, painter: &mut Painter, theme: &Theme) {
        self.0.borrow_mut().paint(painter, theme);
    }
    fn paint_overlay(&mut self, painter: &mut Painter, theme: &Theme) {
        self.0.borrow_mut().paint_overlay(painter, theme);
    }
    fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
        self.0.borrow_mut().event(event, ctx);
    }
    fn on_cancel(&mut self, ctx: &mut EventCtx) {
        self.0.borrow_mut().on_cancel(ctx);
    }
    fn captures_pointer(&self) -> bool {
        self.0.borrow().captures_pointer()
    }
    fn focusable(&self) -> bool {
        self.0.borrow().focusable()
    }
    fn focus_first(&mut self) -> bool {
        self.0.borrow_mut().focus_first()
    }
    fn set_focused(&mut self, focused: bool) {
        self.0.borrow_mut().set_focused(focused);
    }
    fn accepts_accelerators(&self) -> bool {
        self.0.borrow().accepts_accelerators()
    }
    fn layout(&mut self, bounds: Rect) {
        self.0.borrow_mut().layout(bounds);
    }
    fn popup_request(&self) -> Option<PopupRequest> {
        self.0.borrow().popup_request()
    }
    fn collect_popups(&self, out: &mut Vec<PopupRequest>) {
        self.0.borrow().collect_popups(out);
    }
    fn wants_ticks(&self) -> bool {
        self.0.borrow().wants_ticks()
    }
}
