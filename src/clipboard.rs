//! Cut, Copy and Paste of files, through the desktop's clipboard where there
//! is one.
//!
//! Copied files go on the system clipboard as a list of file URIs, so they
//! can be pasted into other file managers, and files copied there can be
//! pasted here. Whether they were cut or copied isn't part of that list, so
//! McFilin remembers it: pasting the very files it cut moves them, anything
//! else is copied. Without a system clipboard, the files are kept here only.

use std::path::PathBuf;

use crate::fsops::Op;

/// What Cut or Copy last put on the clipboard.
struct Clip {
    paths: Vec<PathBuf>,
    op: Op,
    /// Whether they made it onto the system clipboard too, which then has the
    /// final word on what is pasted.
    on_system: bool,
}

#[derive(Default)]
pub struct Clipboard {
    /// Connected on first use, and kept: on X11 it is what serves the
    /// clipboard to other applications.
    system: Option<arboard::Clipboard>,
    tried: bool,
    own: Option<Clip>,
}

impl Clipboard {
    pub fn new() -> Self {
        Self::default()
    }

    fn system(&mut self) -> Option<&mut arboard::Clipboard> {
        if !self.tried {
            self.tried = true;
            self.system = arboard::Clipboard::new().ok();
        }
        self.system.as_mut()
    }

    /// Puts `paths` on the clipboard, to be moved (`Op::Move`, for Cut) or
    /// copied by the next paste.
    pub fn put(&mut self, paths: Vec<PathBuf>, op: Op) {
        let on_system = self
            .system()
            .is_some_and(|cb| cb.set().file_list(&paths).is_ok());
        self.own = Some(Clip {
            paths,
            op,
            on_system,
        });
    }

    /// The files a paste brings in, and whether they are moved or copied.
    pub fn contents(&mut self) -> Option<(Vec<PathBuf>, Op)> {
        if let Some(own) = &self.own
            && !own.on_system
        {
            return Some((own.paths.clone(), own.op));
        }
        let list = self
            .system()
            .and_then(|cb| cb.get().file_list().ok())
            .filter(|l| !l.is_empty());
        let Some(list) = list else {
            // Something other than files was copied since.
            self.own = None;
            return None;
        };
        match &self.own {
            Some(own) if own.paths == list => Some((list, own.op)),
            _ => {
                // Files copied in another application.
                self.own = None;
                Some((list, Op::Copy))
            }
        }
    }

    /// After cut files were pasted: they are where they belong, and pasting
    /// them again would find them gone.
    pub fn clear_cut(&mut self) {
        if let Some(own) = self.own.take_if(|c| c.op == Op::Move)
            && own.on_system
            && let Some(cb) = self.system()
        {
            let _ = cb.clear();
        }
    }

    /// Whether `path` was cut and waits to be pasted somewhere.
    pub fn is_cut(&self, path: &std::path::Path) -> bool {
        self.own
            .as_ref()
            .is_some_and(|c| c.op == Op::Move && c.paths.iter().any(|p| p == path))
    }
}
