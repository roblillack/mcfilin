//! McFilin — a file viewer with Miller columns, after NeXTSTEP's File Viewer,
//! built on the Saudade GUI toolkit.

pub mod browser;
pub mod clipboard;
pub mod fsops;
pub mod info;
pub mod mime;
pub mod thumbs;
pub mod trash;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use saudade::{
    Accel, Column, Dialog, Event, EventCtx, Menu, MenuBar, MenuItem, NamedKey, Painter,
    PopupRequest, Rect, Theme, Widget, WindowConfig,
};

use browser::{Browser, MIN_COLUMN_WIDTH};

const WINDOW_W: i32 = 760;
const WINDOW_H: i32 = 480;
const MENU_BAR_H: i32 = 20;

/// The main window and its widget tree, opened at `start`.
pub fn window(start: PathBuf) -> (WindowConfig, impl Widget) {
    let dialog = Rc::new(RefCell::new(Dialog::new()));
    let browser = Rc::new_cyclic(|this| {
        RefCell::new(Browser::new(&start, dialog.clone(), this.clone()))
    });

    // Each menu item borrows the shared browser, does its thing, and repaints.
    // A name being edited is settled first, as a click elsewhere would.
    let act = |f: fn(&mut Browser)| {
        let browser = browser.clone();
        move |cx: &mut EventCtx| {
            let mut browser = browser.borrow_mut();
            browser.finish_rename();
            f(&mut browser);
            cx.request_paint();
        }
    };

    let menu_bar = MenuBar::new(Rect::new(0, 0, WINDOW_W, MENU_BAR_H))
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
            ],
        ))
        .add_menu(Menu::new(
            "&View",
            vec![
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
        .add_menu(Menu::new(
            "&Go",
            vec![
                MenuItem::action("&Enclosing Folder", act(Browser::go_parent))
                    .with_accel(Accel::primary(NamedKey::Up)),
                MenuItem::action("&Home", act(Browser::go_home))
                    .with_accel(Accel::primary('h').shift()),
                MenuItem::action("&Root", act(|b| b.go_to(std::path::Path::new("/")))),
            ],
        ));

    let root = Column::new()
        .add_fixed(menu_bar, MENU_BAR_H)
        .add_fill(Shared(browser))
        .add_overlay(Shared(dialog));

    let config = WindowConfig::new("McFilin", WINDOW_W, WINDOW_H)
        .resizable(true)
        .min_size(MIN_COLUMN_WIDTH * 2 + 40, 280);
    (config, root)
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
