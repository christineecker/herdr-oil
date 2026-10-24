//! Application state and event loop: explorer buffers per directory, the
//! finder, overlays (confirm / help / quit prompt / report) and the bridge
//! to the editor.

use crate::apply;
use crate::buffer::Buffer;
use crate::config::{Config, OpenTarget};
use crate::entry::{Kind, Sort, Store};
use crate::finder::{self, Finder};
use crate::open::{self, Outcome};
use crate::plan::{self, Plan};
use crate::preview;
use crate::theme::Theme;
use crate::vim::{Action, FinderKind, Mode, Reg, Vim};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Explorer,
    Finder,
    Viewer,
}

/// Yanked/cut lines waiting for a destination picked in the finder.
pub struct Transfer {
    pub copy: bool,
    pub lines: Vec<crate::buffer::Line>,
    pub count: usize,
}

/// A file shown read-only inside the popup.
pub struct Viewer {
    pub path: PathBuf,
    pub line: Option<usize>,
    /// First visible line; None until the first draw places it.
    pub top: Option<usize>,
    pub back: View,
    pub height: usize,
    pub total: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Info,
    Ok,
    Warn,
    Err,
}

pub enum Overlay {
    None,
    Confirm { plan: Plan, scroll: usize, then_quit: bool },
    Help { scroll: usize },
    QuitPrompt,
    Report { title: String, lines: Vec<String>, error: bool, scroll: usize },
}

pub struct App {
    pub cfg: Config,
    pub th: Theme,
    pub store: Store,
    pub buffers: HashMap<PathBuf, Buffer>,
    pub cwd: PathBuf,
    pub start: PathBuf,
    pub vim: Vim,
    pub sort: Sort,
    pub hidden: bool,
    pub details: u8,
    pub preview_on: bool,
    pub finder_preview: bool,
    pub view: View,
    pub finder: Option<Finder>,
    /// Finder opened on its own (launch mode) — Esc quits instead of going back.
    pub finder_only: bool,
    pub overlay: Overlay,
    pub msg: Option<(String, MsgKind, Instant)>,
    pub origin: Option<String>,
    pub preview: preview::Cache,
    pub viewer: Option<Viewer>,
    pub transfer: Option<Transfer>,
    pub viewer_cache: preview::Cache,
    pub preview_scroll: usize,
    pub quit: bool,
    pub pick: bool,
    pub picked: Vec<PathBuf>,
    suspend: Option<Vec<String>>,
    pub list_area: Rect,
    last_click: Option<(Instant, u16)>,
    pub spinner: usize,
}

impl App {
    pub fn new(start: PathBuf, mode: &str, cfg: Config, pick: bool) -> App {
        let (dir, focus) = if start.is_file() {
            (start.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/")), start.file_name().map(|n| n.to_string_lossy().into_owned()))
        } else {
            (start.clone(), None)
        };
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        let mut app = App {
            th: Theme::load(),
            store: Store::default(),
            buffers: HashMap::new(),
            cwd: dir.clone(),
            start: dir.clone(),
            vim: Vim::default(),
            sort: Sort::Name,
            hidden: cfg.show_hidden,
            details: cfg.details,
            preview_on: cfg.preview,
            finder_preview: true,
            view: View::Explorer,
            finder: None,
            finder_only: false,
            overlay: Overlay::None,
            msg: None,
            origin: std::env::var("HERDR_OIL_ORIGIN").ok().filter(|s| crate::launch::flag_safe(s)),
            preview: preview::Cache::default(),
            viewer: None,
            transfer: None,
            viewer_cache: preview::Cache::default(),
            preview_scroll: 0,
            quit: false,
            pick,
            picked: vec![],
            suspend: None,
            list_area: Rect::default(),
            last_click: None,
            spinner: 0,
            cfg,
        };
        app.goto(&dir, focus.as_deref());
        match mode {
            "finder" | "files" => {
                app.open_finder(FinderKind::Files);
                app.finder_only = true;
            }
            "grep" => {
                app.open_finder(FinderKind::Grep);
                app.finder_only = true;
            }
            "recent" => {
                app.open_finder(FinderKind::Recent);
                app.finder_only = true;
            }
            _ => {}
        }
        app
    }

    pub fn buf(&self) -> &Buffer {
        &self.buffers[&self.cwd]
    }

    pub fn buf_mut(&mut self) -> &mut Buffer {
        self.buffers.get_mut(&self.cwd).unwrap()
    }

    pub fn flash(&mut self, s: impl Into<String>, k: MsgKind) {
        self.msg = Some((s.into(), k, Instant::now()));
    }

    pub fn modified_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.buffers.values().filter(|b| b.modified()).map(|b| b.dir.clone()).collect();
        v.sort();
        v
    }

    pub fn goto(&mut self, dir: &Path, focus: Option<&str>) {
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let (hidden, sort, git) = (self.hidden, self.sort, self.cfg.git_status);
        match self.buffers.get_mut(&dir) {
            Some(b) if b.modified() => {}
            Some(b) => b.reload(&mut self.store, hidden, sort, git),
            None => {
                let b = Buffer::load(&dir, &mut self.store, hidden, sort, git);
                self.buffers.insert(dir.clone(), b);
            }
        }
        // Drop unmodified buffers of other dirs to keep things fresh.
        let cwd = dir.clone();
        self.buffers.retain(|d, b| *d == cwd || b.modified());
        self.cwd = dir;
        if let Some(name) = focus {
            let b = self.buf_mut();
            if let Some(i) = b.lines.iter().position(|l| l.text.trim_end_matches('/') == name) {
                b.row = i;
            }
        }
        self.preview_scroll = 0;
        if let Some(e) = self.buf().error.clone() {
            self.flash(e, MsgKind::Err);
        }
    }

    fn reload_all(&mut self, focus: Option<String>) {
        self.store.clear();
        if matches!(self.vim.reg, Some(Reg::Lines(_))) {
            self.vim.reg = None;
        }
        self.buffers.clear();
        let mut cwd = self.cwd.clone();
        while !cwd.is_dir() {
            match cwd.parent() {
                Some(p) => cwd = p.to_path_buf(),
                None => break,
            }
        }
        self.goto(&cwd, focus.as_deref());
    }

    // ── saving ──────────────────────────────────────────────────────────────

    fn save(&mut self, then_quit: bool) {
        let plan = plan::build(self.buffers.values(), &self.store);
        if !plan.errors.is_empty() {
            self.overlay = Overlay::Report { title: "Cannot save".into(), lines: plan.errors, error: true, scroll: 0 };
            return;
        }
        if plan.ops.is_empty() {
            self.flash("no changes", MsgKind::Info);
            if then_quit {
                self.quit = true;
            }
            return;
        }
        if self.cfg.confirm || plan.deletes() > 0 {
            self.overlay = Overlay::Confirm { plan, scroll: 0, then_quit };
        } else {
            self.execute(plan, then_quit);
        }
    }

    fn execute(&mut self, plan: Plan, then_quit: bool) {
        let rep = apply::apply(&plan.ops, self.cfg.delete);
        let focus = rep.landed.iter().find(|p| p.parent() == Some(self.cwd.as_path())).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        let focus = focus.or_else(|| self.buf().cur().text.trim().trim_end_matches('/').split('/').next().map(str::to_string));
        self.reload_all(focus);
        if rep.errors.is_empty() {
            self.flash(format!("applied {} change(s)", plan.ops.len()), MsgKind::Ok);
            if then_quit {
                self.quit = true;
            }
        } else {
            self.overlay = Overlay::Report {
                title: format!("{} done, {} problem(s)", rep.done, rep.errors.iter().filter(|e| !e.starts_with("  ")).count()),
                lines: rep.errors,
                error: true,
                scroll: 0,
            };
        }
    }

    fn try_quit(&mut self, force: bool) {
        if force || self.modified_dirs().is_empty() {
            self.quit = true;
        } else {
            self.overlay = Overlay::QuitPrompt;
        }
    }

    // ── opening ─────────────────────────────────────────────────────────────

    fn open_files(&mut self, files: Vec<PathBuf>, line: Option<usize>, how: OpenTarget) {
        if self.pick {
            self.picked = files;
            self.quit = true;
            return;
        }
        finder::push_recent(&self.cfg.state_dir, &files);
        if how == OpenTarget::View {
            let back = if self.view == View::Viewer { View::Explorer } else { self.view };
            self.viewer = Some(Viewer { path: files[0].clone(), line, top: None, back, height: 20, total: 0 });
            self.view = View::Viewer;
            return;
        }
        match open::open(&files, line, how, &self.cfg, self.origin.as_deref()) {
            Ok(Outcome::Suspend(argv)) => self.suspend = Some(argv),
            Ok(Outcome::Handed(m)) => {
                if self.cfg.stay_open {
                    self.flash(m, MsgKind::Ok);
                } else if self.modified_dirs().is_empty() {
                    self.quit = true;
                } else {
                    self.flash(format!("{m} — unsaved edits keep the explorer open"), MsgKind::Warn);
                }
            }
            Err(e) => self.flash(e, MsgKind::Err),
        }
    }

    fn open_current(&mut self, how: Option<OpenTarget>) {
        let line = self.buf().cur().clone();
        let entry = self.buf().cur_entry(&self.store).cloned();
        let Some(e) = entry else {
            if line.significant() {
                // A typed path that already exists? Follow it.
                let p = plan::normalize(&self.cwd.join(line.text.trim()));
                if line.id.is_none() && p.is_dir() {
                    self.goto(&p, None);
                    return;
                }
                self.flash("unsaved line — :w first", MsgKind::Warn);
            }
            return;
        };
        if e.kind.is_dir() && how != Some(OpenTarget::System) {
            self.goto(&e.path, None);
            return;
        }
        if e.kind == Kind::Other {
            self.flash("not a regular file", MsgKind::Warn);
            return;
        }
        self.open_files(vec![e.path.clone()], None, how.unwrap_or(self.cfg.open));
    }

    /// `gm` / `gc` after a yank or cut: pick a destination folder with the finder.
    fn start_transfer(&mut self, copy: bool) {
        let lines: Vec<_> = match &self.vim.reg {
            Some(Reg::Lines(ls)) => ls.iter().filter(|l| l.significant()).cloned().collect(),
            _ => vec![],
        };
        if lines.is_empty() {
            self.flash("yank (yy) or cut (dd) a file or folder first", MsgKind::Warn);
            return;
        }
        // Search from the repository root when there is one: destinations are
        // usually elsewhere in the project. ^r cycles roots as usual.
        let root = finder::repo_root(&self.cwd).unwrap_or_else(|| self.cwd.clone());
        self.finder = Some(Finder::new(FinderKind::Dirs, root, &self.cfg));
        let count = lines.len();
        self.transfer = Some(Transfer { copy, lines, count });
        self.view = View::Finder;
        self.finder_only = false;
        self.preview_scroll = 0;
    }

    /// Stage the transfer as buffer edits (paste into the destination folder,
    /// and for a move drop the originals wherever they are listed), then show
    /// the usual review dialog. `sub` is a new sub-path typed with ^n.
    fn finish_transfer(&mut self, dest: PathBuf, sub: Option<String>) {
        let Some(t) = self.transfer.take() else { return };
        self.view = View::Explorer;
        let dest = std::fs::canonicalize(&dest).unwrap_or(dest);
        if !dest.is_dir() {
            self.flash(format!("not a folder: {}", finder::tilde(&dest)), MsgKind::Err);
            return;
        }
        let prefix = sub.map(|s| s.trim().trim_matches('/').to_string()).filter(|s| !s.is_empty());
        let all_here = t.lines.iter().all(|l| l.id.and_then(|id| self.store.get(id)).is_some_and(|e| e.path.parent() == Some(dest.as_path())));
        if all_here && prefix.is_none() {
            self.flash(if t.copy { "same folder — use p to duplicate here" } else { "already in this folder" }, MsgKind::Info);
            return;
        }
        if !t.copy {
            let ids: std::collections::HashSet<u64> = t.lines.iter().filter_map(|l| l.id).collect();
            for b in self.buffers.values_mut() {
                if b.lines.iter().any(|l| l.id.is_some_and(|id| ids.contains(&id))) {
                    b.snapshot();
                    b.lines.retain(|l| !l.id.is_some_and(|id| ids.contains(&id)));
                    b.ensure_nonempty();
                    b.clamp();
                }
            }
        }
        let (h, s, g) = (self.hidden, self.sort, self.cfg.git_status);
        let store = &mut self.store;
        let d = self.buffers.entry(dest.clone()).or_insert_with(|| crate::buffer::Buffer::load(&dest, store, h, s, g));
        d.snapshot();
        d.lines.retain(|l| l.significant());
        for mut l in t.lines {
            if let Some(p) = &prefix {
                l.text = format!("{p}/{}", l.text.trim());
            }
            d.lines.push(l);
        }
        d.ensure_nonempty();
        if !t.copy {
            // the entries moved; don't let a later p duplicate them
            self.vim.reg = None;
        }
        let verb = if t.copy { "copy" } else { "move" };
        let place = match &prefix {
            Some(p) => format!("{}/{p}/", finder::tilde(&dest)),
            None => format!("{}/", finder::tilde(&dest)),
        };
        self.flash(format!("{verb} {} → {place} staged", t.count), MsgKind::Info);
        self.save(false);
    }

    fn open_finder(&mut self, k: FinderKind) {
        match &mut self.finder {
            Some(f) if f.root == self.cwd => f.set_kind(k),
            _ => self.finder = Some(Finder::new(k, self.cwd.clone(), &self.cfg)),
        }
        self.view = View::Finder;
        self.preview_scroll = 0;
    }

    fn reveal(&mut self, p: &Path) {
        let dir = p.parent().unwrap_or(Path::new("/")).to_path_buf();
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
        self.goto(&dir, name.as_deref());
        self.view = View::Explorer;
        self.finder_only = false;
    }

    // ── actions from the explorer keymap ────────────────────────────────────

    fn act(&mut self, a: Action) {
        match a {
            Action::None => {}
            Action::Open(how) => self.open_current(how),
            Action::Parent => {
                let name = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
                if let Some(p) = self.cwd.parent().map(Path::to_path_buf) {
                    self.goto(&p, name.as_deref());
                }
            }
            Action::Home => {
                if let Ok(h) = std::env::var("HOME") {
                    self.goto(Path::new(&h), None);
                }
            }
            Action::StartDir => {
                let s = self.start.clone();
                self.goto(&s, None);
            }
            Action::TogglePreview => {
                self.preview_on = !self.preview_on;
                self.preview_scroll = 0;
            }
            Action::PreviewScroll(d) => {
                self.preview_scroll = (self.preview_scroll as i64 + d as i64).max(0) as usize;
            }
            Action::ToggleHidden => {
                if !self.modified_dirs().is_empty() {
                    self.flash("save (:w) or discard (:e!) edits before toggling hidden files", MsgKind::Warn);
                    return;
                }
                self.hidden = !self.hidden;
                let cwd = self.cwd.clone();
                let name = self.buf().cur().text.trim_end_matches('/').to_string();
                self.buffers.clear();
                self.goto(&cwd, Some(&name));
                self.flash(if self.hidden { "showing hidden files" } else { "hiding dotfiles" }, MsgKind::Info);
            }
            Action::CycleSort => {
                self.sort = self.sort.next();
                let s = self.sort;
                let store = &self.store;
                if let Some(b) = self.buffers.get_mut(&self.cwd) {
                    b.resort(store, s);
                }
                self.flash(format!("sort: {}", s.label()), MsgKind::Info);
            }
            Action::ToggleDetails => {
                self.details = (self.details + 1) % 3;
                self.flash(["details off", "sizes", "perms · size · age"][self.details as usize], MsgKind::Info);
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::Save => self.save(false),
            Action::SaveQuit => self.save(true),
            Action::Quit => self.try_quit(false),
            Action::ForceQuit => self.quit = true,
            Action::Refresh => {
                if self.buf().modified() {
                    self.flash("buffer has edits — :e! discards them", MsgKind::Warn);
                } else {
                    let (h, s, g) = (self.hidden, self.sort, self.cfg.git_status);
                    let store = &mut self.store;
                    if let Some(b) = self.buffers.get_mut(&self.cwd) {
                        b.reload(store, h, s, g);
                    }
                    self.flash("refreshed", MsgKind::Info);
                }
            }
            Action::Discard => {
                let (h, s, g) = (self.hidden, self.sort, self.cfg.git_status);
                let store = &mut self.store;
                if let Some(b) = self.buffers.get_mut(&self.cwd) {
                    b.reload(store, h, s, g);
                }
                self.flash("edits in this directory discarded", MsgKind::Info);
            }
            Action::Finder(k) => self.open_finder(k),
            Action::Transfer { copy } => self.start_transfer(copy),
            Action::YankPath => {
                let p = match self.buf().cur_entry(&self.store) {
                    Some(e) => e.path.clone(),
                    None => self.cwd.clone(),
                };
                let s = p.display().to_string();
                match open::clipboard(&s) {
                    Ok(()) => self.flash(format!("copied {}", finder::tilde(&p)), MsgKind::Ok),
                    Err(e) => self.flash(e, MsgKind::Err),
                }
            }
            Action::Command(c) => self.command(&c),
        }
    }

    fn command(&mut self, c: &str) {
        let (head, rest) = c.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((c, ""));
        match head {
            "cd" | "e" | "edit" => {
                let target = if rest.is_empty() {
                    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| self.cwd.clone())
                } else if let Some(r) = rest.strip_prefix('~') {
                    PathBuf::from(format!("{}{r}", std::env::var("HOME").unwrap_or_default()))
                } else {
                    plan::normalize(&self.cwd.join(rest))
                };
                if target.is_dir() {
                    self.goto(&target, None);
                } else if target.is_file() {
                    self.reveal(&target);
                } else {
                    self.flash(format!("no such directory: {rest}"), MsgKind::Err);
                }
            }
            "sort" => {
                self.sort = match rest {
                    "mtime" | "time" => Sort::Mtime,
                    "size" => Sort::Size,
                    "ext" | "type" => Sort::Ext,
                    _ => Sort::Name,
                };
                let s = self.sort;
                let store = &self.store;
                if let Some(b) = self.buffers.get_mut(&self.cwd) {
                    b.resort(store, s);
                }
            }
            "pwd" => {
                let s = finder::tilde(&self.cwd);
                self.flash(s, MsgKind::Info);
            }
            _ => self.flash(format!("unknown command: {c}"), MsgKind::Err),
        }
    }

    // ── keys ────────────────────────────────────────────────────────────────

    fn key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        if !matches!(self.overlay, Overlay::None) {
            self.overlay_key(k);
            return;
        }
        match self.view {
            View::Explorer => {
                self.vim.msg = None;
                let cwd = self.cwd.clone();
                let b = self.buffers.get_mut(&cwd).unwrap();
                let a = self.vim.handle(b, k);
                b.follow();
                if let Some(m) = self.vim.msg.take() {
                    self.flash(m, MsgKind::Info);
                }
                self.act(a);
            }
            View::Finder => self.finder_key(k),
            View::Viewer => self.viewer_key(k),
        }
    }

    fn overlay_key(&mut self, k: KeyEvent) {
        let ov = std::mem::replace(&mut self.overlay, Overlay::None);
        match ov {
            Overlay::Confirm { plan, mut scroll, then_quit } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.execute(plan, then_quit),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.flash("cancelled — edits kept", MsgKind::Info),
                KeyCode::Char('j') | KeyCode::Down => {
                    scroll += 1;
                    self.overlay = Overlay::Confirm { plan, scroll, then_quit };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Overlay::Confirm { plan, scroll, then_quit };
                }
                _ => self.overlay = Overlay::Confirm { plan, scroll, then_quit },
            },
            Overlay::QuitPrompt => match k.code {
                KeyCode::Char('s') | KeyCode::Char('w') | KeyCode::Char('y') => self.save(true),
                KeyCode::Char('d') | KeyCode::Char('!') => self.quit = true,
                KeyCode::Char('c') | KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.overlay = Overlay::QuitPrompt,
            },
            Overlay::Help { mut scroll } => match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    scroll += 1;
                    self.overlay = Overlay::Help { scroll };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Overlay::Help { scroll };
                }
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    scroll += 10;
                    self.overlay = Overlay::Help { scroll };
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.overlay = Overlay::Help { scroll };
                }
                _ => {}
            },
            Overlay::Report { title, lines, error, mut scroll } => match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    scroll += 1;
                    self.overlay = Overlay::Report { title, lines, error, scroll };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Overlay::Report { title, lines, error, scroll };
                }
                _ => {}
            },
            Overlay::None => {}
        }
    }

    fn finder_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let page = self.list_area.height.max(2) as isize - 1;
        if self.transfer.is_some() {
            let Some(f) = self.finder.as_mut() else { return };
            match (k.code, ctrl) {
                (KeyCode::Esc, _) | (KeyCode::Char('c'), true) => {
                    self.transfer = None;
                    self.view = View::Explorer;
                    self.flash("move/copy cancelled", MsgKind::Info);
                    return;
                }
                (KeyCode::Enter, _) => {
                    if let Some(it) = f.selected() {
                        // a file (Files/Grep/Recent) stands for its folder
                        let dest = if it.is_dir { it.abs } else { it.abs.parent().map(Path::to_path_buf).unwrap_or(it.abs) };
                        self.finish_transfer(dest, None);
                    }
                    return;
                }
                (KeyCode::Char('n'), true) => {
                    let (root, q) = (f.root.clone(), f.query.clone());
                    if q.trim().is_empty() {
                        self.flash("type the new folder path first, then ^n", MsgKind::Warn);
                    } else if q.contains("..") || q.trim().starts_with('/') || q.trim().starts_with('~') {
                        self.flash("new folder must be a path inside the search root", MsgKind::Warn);
                    } else {
                        self.finish_transfer(root, Some(q));
                    }
                    return;
                }
                (KeyCode::Char('v' | 'x' | 't' | 'e' | 'g' | 'o' | 'y' | ' '), true) => return,
                _ => {}
            }
        }
        let Some(f) = self.finder.as_mut() else { return };
        let mut open_how: Option<OpenTarget> = None;
        let mut edited = None;
        match (k.code, ctrl, alt) {
            (KeyCode::Esc, _, _) | (KeyCode::Char('c'), true, _) | (KeyCode::Char('q'), true, _) => {
                if self.finder_only || (k.code == KeyCode::Char('c') && ctrl) {
                    self.try_quit(false);
                } else {
                    self.view = View::Explorer;
                }
                return;
            }
            (KeyCode::Tab, _, _) | (KeyCode::BackTab, _, _) => {
                let order = [FinderKind::Files, FinderKind::Dirs, FinderKind::Grep, FinderKind::Recent];
                let i = order.iter().position(|x| *x == f.kind).unwrap_or(0);
                let n = if k.code == KeyCode::BackTab { (i + order.len() - 1) % order.len() } else { (i + 1) % order.len() };
                f.set_kind(order[n]);
                self.preview_scroll = 0;
                return;
            }
            (KeyCode::Enter, _, _) => open_how = Some(self.cfg.open),
            (KeyCode::Char('v'), true, _) => open_how = Some(OpenTarget::SplitRight),
            (KeyCode::Char('x'), true, _) => open_how = Some(OpenTarget::SplitDown),
            (KeyCode::Char('t'), true, _) => open_how = Some(OpenTarget::Tab),
            (KeyCode::Char('e'), true, _) => open_how = Some(OpenTarget::Popup),
            (KeyCode::Char('g'), true, _) => open_how = Some(OpenTarget::System),
            (KeyCode::Char('o'), true, _) => {
                if let Some(it) = f.selected() {
                    self.reveal(&it.abs);
                }
                return;
            }
            (KeyCode::Char('y'), true, _) => {
                if let Some(it) = f.selected() {
                    let s = it.abs.display().to_string();
                    match open::clipboard(&s) {
                        Ok(()) => self.flash(format!("copied {}", finder::tilde(&it.abs)), MsgKind::Ok),
                        Err(e) => self.flash(e, MsgKind::Err),
                    }
                }
                return;
            }
            (KeyCode::Char(' '), true, _) => {
                f.toggle_mark();
                f.move_sel(1);
            }
            (KeyCode::Char('r'), true, _) => {
                let repo = finder::repo_root(&self.cwd);
                let target = match repo {
                    Some(r) if r != f.root => r,
                    _ if f.root != self.cwd => self.cwd.clone(),
                    _ => f.root.parent().map(Path::to_path_buf).unwrap_or_else(|| f.root.clone()),
                };
                f.set_root(target);
            }
            (KeyCode::Char('h'), _, true) => f.toggle_hidden(),
            (KeyCode::Char('i'), _, true) => f.toggle_ignore(),
            (KeyCode::Char('p'), true, _) => f.move_sel(-1),
            (KeyCode::Char('n'), true, _) | (KeyCode::Char('j'), true, _) | (KeyCode::Down, _, _) => f.move_sel(1),
            (KeyCode::Char('k'), true, _) | (KeyCode::Up, _, _) => f.move_sel(-1),
            (KeyCode::PageDown, _, _) => f.move_sel(page),
            (KeyCode::PageUp, _, _) => f.move_sel(-page),
            (KeyCode::Char('f'), true, _) => self.preview_scroll += 10,
            (KeyCode::Char('b'), true, _) => self.preview_scroll = self.preview_scroll.saturating_sub(10),
            (KeyCode::Char('p'), _, true) | (KeyCode::Char('l'), true, _) => self.finder_preview = !self.finder_preview,
            (KeyCode::Char('u'), true, _) => {
                f.query.clear();
                f.qcur = 0;
                edited = Some(false);
            }
            (KeyCode::Char('w'), true, _) => {
                let chars: Vec<char> = f.query.chars().collect();
                let mut a = f.qcur;
                while a > 0 && chars[a - 1] == ' ' {
                    a -= 1;
                }
                while a > 0 && chars[a - 1] != ' ' {
                    a -= 1;
                }
                f.query = chars[..a].iter().chain(chars[f.qcur..].iter()).collect();
                f.qcur = a;
                edited = Some(false);
            }
            (KeyCode::Char('a'), true, _) | (KeyCode::Home, _, _) => f.qcur = 0,
            (KeyCode::Char('e'), _, true) | (KeyCode::End, _, _) => f.qcur = f.query.chars().count(),
            (KeyCode::Left, _, _) => f.qcur = f.qcur.saturating_sub(1),
            (KeyCode::Right, _, _) => f.qcur = (f.qcur + 1).min(f.query.chars().count()),
            (KeyCode::Backspace, _, _) => {
                if f.qcur > 0 {
                    let b = crate::buffer::byte_at(&f.query, f.qcur - 1);
                    f.query.remove(b);
                    f.qcur -= 1;
                    edited = Some(false);
                } else if f.query.is_empty() && !self.finder_only {
                    self.view = View::Explorer;
                    return;
                }
            }
            (KeyCode::Char(c), false, false) => {
                let at_end = f.qcur == f.query.chars().count();
                let b = crate::buffer::byte_at(&f.query, f.qcur);
                f.query.insert(b, c);
                f.qcur += 1;
                edited = Some(at_end);
            }
            _ => {}
        }
        if let Some(appended) = edited {
            f.query_changed(appended);
            self.preview_scroll = 0;
        }
        if let Some(how) = open_how {
            let marked = f.marked.clone();
            let Some(it) = f.selected() else { return };
            if it.is_dir {
                if how == OpenTarget::System {
                    let _ = open::system_open(&it.abs);
                    return;
                }
                let p = it.abs.clone();
                self.goto(&p, None);
                self.view = View::Explorer;
                self.finder_only = false;
                return;
            }
            let files = if marked.is_empty() { vec![it.abs.clone()] } else { marked };
            self.open_files(files, it.line, how);
        }
    }

    fn viewer_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(v) = self.viewer.as_mut() else {
            self.view = View::Explorer;
            return;
        };
        let h = v.height.max(1);
        let max_top = v.total.saturating_sub(h);
        let top = v.top.unwrap_or(0);
        let mut scroll = |d: isize| v.top = Some((top as isize + d).clamp(0, max_top as isize) as usize);
        let path = v.path.clone();
        let line = v.line;
        match (k.code, ctrl) {
            (KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter, false) => scroll(1),
            (KeyCode::Char('k') | KeyCode::Up, false) => scroll(-1),
            (KeyCode::Char('d'), true) => scroll(h as isize / 2),
            (KeyCode::Char('u'), true) => scroll(-(h as isize / 2)),
            (KeyCode::Char(' ') | KeyCode::PageDown, false) | (KeyCode::Char('f'), true) => scroll(h as isize - 1),
            (KeyCode::Char('b') | KeyCode::PageUp, false) | (KeyCode::Char('b'), true) => scroll(-(h as isize - 1)),
            (KeyCode::Char('g') | KeyCode::Home, false) => scroll(-(top as isize)),
            (KeyCode::Char('G') | KeyCode::End, false) => scroll(max_top as isize),
            (KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('-') | KeyCode::Backspace | KeyCode::Left, false) => {
                self.view = v.back;
                self.viewer = None;
            }
            (KeyCode::Char('c'), true) => self.try_quit(false),
            (KeyCode::Char('e'), _) => self.open_files(vec![path], line, OpenTarget::Popup),
            (KeyCode::Char('v'), true) => self.open_files(vec![path], line, OpenTarget::SplitRight),
            (KeyCode::Char('x'), true) => self.open_files(vec![path], line, OpenTarget::SplitDown),
            (KeyCode::Char('t'), true) => self.open_files(vec![path], line, OpenTarget::Tab),
            (KeyCode::Char('o'), false) => {
                if let Err(e) = open::system_open(&path) {
                    self.flash(e, MsgKind::Err);
                }
            }
            (KeyCode::Char('y'), false) => match open::clipboard(&path.display().to_string()) {
                Ok(()) => self.flash(format!("copied {}", finder::tilde(&path)), MsgKind::Ok),
                Err(e) => self.flash(e, MsgKind::Err),
            },
            _ => {}
        }
    }

    fn mouse(&mut self, m: event::MouseEvent) {
        let a = self.list_area;
        let inside = m.column >= a.x && m.column < a.x + a.width && m.row >= a.y && m.row < a.y + a.height;
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d: isize = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                match self.view {
                    View::Explorer => {
                        if !inside && self.preview_on {
                            self.preview_scroll = (self.preview_scroll as isize + d).max(0) as usize;
                            return;
                        }
                        let b = self.buf_mut();
                        b.row = (b.row as isize + d).clamp(0, b.lines.len() as isize - 1) as usize;
                        b.clamp();
                        b.follow();
                    }
                    View::Viewer => {
                        if let Some(v) = self.viewer.as_mut() {
                            let max_top = v.total.saturating_sub(v.height.max(1));
                            v.top = Some((v.top.unwrap_or(0) as isize + d).clamp(0, max_top as isize) as usize);
                        }
                    }
                    View::Finder => {
                        if !inside {
                            self.preview_scroll = (self.preview_scroll as isize + d).max(0) as usize;
                        } else if let Some(f) = self.finder.as_mut() {
                            f.move_sel(d);
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) if inside && matches!(self.overlay, Overlay::None) => {
                let rel = (m.row - a.y) as usize;
                let dbl = self.last_click.is_some_and(|(t, r)| r == m.row && t.elapsed() < Duration::from_millis(400));
                self.last_click = Some((Instant::now(), m.row));
                match self.view {
                    View::Explorer => {
                        if self.vim.mode != Mode::Normal {
                            return;
                        }
                        let b = self.buf_mut();
                        let r = b.scroll + rel;
                        if r < b.lines.len() {
                            b.row = r;
                            b.clamp();
                            if dbl {
                                self.open_current(None);
                            }
                        }
                    }
                    View::Viewer => {}
                    View::Finder => {
                        if let Some(f) = self.finder.as_mut() {
                            let r = f.scroll + rel;
                            if r < f.len() {
                                f.sel = r;
                                if dbl {
                                    self.finder_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn run_suspended(&mut self, terminal: &mut DefaultTerminal, argv: Vec<String>) {
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        let status = std::process::Command::new(&argv[0]).args(&argv[1..]).current_dir(&self.cwd).status();
        *terminal = ratatui::init();
        let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
        let _ = terminal.clear();
        match status {
            Ok(_) => {}
            Err(e) => self.flash(format!("{}: {e}", argv[0]), MsgKind::Err),
        }
        // The file may have changed; refresh listings that have no edits.
        let (h, s, g) = (self.hidden, self.sort, self.cfg.git_status);
        let store = &mut self.store;
        if let Some(b) = self.buffers.get_mut(&self.cwd) {
            if !b.modified() {
                b.reload(store, h, s, g);
            }
        }
        self.preview.key = None;
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        let mut last_tick = Instant::now();
        loop {
            if let Some(f) = self.finder.as_mut() {
                f.tick();
            }
            terminal.draw(|fr| crate::ui::draw(fr, self))?;
            if self.quit {
                return Ok(());
            }
            let busy = self.finder.as_ref().is_some_and(|f| f.walking()) && self.view == View::Finder;
            let timeout = if busy { Duration::from_millis(40) } else { Duration::from_millis(250) };
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(k) => self.key(k),
                    Event::Mouse(m) => self.mouse(m),
                    Event::Paste(s) => {
                        if self.view == View::Explorer && self.vim.mode == Mode::Insert {
                            let line: String = s.lines().next().unwrap_or("").to_string();
                            self.buf_mut().insert_str(&line);
                        } else if let (View::Finder, Some(f)) = (self.view, self.finder.as_mut()) {
                            let b = crate::buffer::byte_at(&f.query, f.qcur);
                            let s: String = s.lines().next().unwrap_or("").to_string();
                            f.query.insert_str(b, &s);
                            f.qcur += s.chars().count();
                            f.query_changed(false);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(argv) = self.suspend.take() {
                self.run_suspended(terminal, argv);
            }
            if last_tick.elapsed() > Duration::from_millis(90) {
                self.spinner = self.spinner.wrapping_add(1);
                last_tick = Instant::now();
            }
            if let Some((_, _, t)) = &self.msg {
                if t.elapsed() > Duration::from_secs(6) {
                    self.msg = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Op;

    fn app_in(tag: &str) -> (crate::plan::tests::Tmp, App) {
        let t = crate::plan::tests::tmpdir(tag);
        std::fs::create_dir_all(t.0.join("src/deep")).unwrap();
        std::fs::write(t.0.join("a.txt"), "a").unwrap();
        std::fs::write(t.0.join("b.txt"), "b").unwrap();
        let cfg = Config { git_status: false, state_dir: t.0.join(".state"), ..Config::default() };
        let app = App::new(t.0.clone(), "explorer", cfg, false);
        (t, app)
    }

    fn row_of(app: &App, name: &str) -> usize {
        app.buf().lines.iter().position(|l| l.text == name).unwrap()
    }

    fn yank(app: &mut App, from: usize, n: usize, cut: bool) {
        let lines: Vec<_> = app.buf().lines[from..from + n].to_vec();
        if cut {
            let b = app.buf_mut();
            b.lines.drain(from..from + n);
            b.ensure_nonempty();
        }
        app.vim.reg = Some(Reg::Lines(lines));
        app.vim.reg_cut = cut;
    }

    #[test]
    fn needs_a_yank_first() {
        let (_t, mut app) = app_in("xfer-none");
        app.act(Action::Transfer { copy: false });
        assert_eq!(app.view, View::Explorer);
        assert!(app.transfer.is_none());
    }

    #[test]
    fn move_yanked_to_picked_folder() {
        let (t, mut app) = app_in("xfer-move");
        let r = row_of(&app, "a.txt");
        yank(&mut app, r, 2, false);
        app.act(Action::Transfer { copy: false });
        assert_eq!(app.view, View::Finder);
        app.finish_transfer(t.0.join("src/deep"), None);
        let Overlay::Confirm { plan, .. } = &app.overlay else { panic!("no confirm dialog") };
        assert_eq!(
            plan.ops,
            vec![
                Op::Move { src: t.0.join("a.txt"), dst: t.0.join("src/deep/a.txt"), dir: false },
                Op::Move { src: t.0.join("b.txt"), dst: t.0.join("src/deep/b.txt"), dir: false },
            ]
        );
        assert!(app.vim.reg.is_none());
    }

    #[test]
    fn cut_then_move() {
        let (t, mut app) = app_in("xfer-cut");
        let r = row_of(&app, "a.txt");
        yank(&mut app, r, 1, true);
        app.act(Action::Transfer { copy: false });
        app.finish_transfer(t.0.join("src"), None);
        let Overlay::Confirm { plan, .. } = &app.overlay else { panic!("no confirm dialog") };
        assert_eq!(plan.ops, vec![Op::Move { src: t.0.join("a.txt"), dst: t.0.join("src/a.txt"), dir: false }]);
    }

    #[test]
    fn copy_yanked_into_new_subfolder() {
        let (t, mut app) = app_in("xfer-copy");
        let r = row_of(&app, "b.txt");
        yank(&mut app, r, 1, false);
        app.act(Action::Transfer { copy: true });
        app.finish_transfer(t.0.clone(), Some("archive/2026/".into()));
        let Overlay::Confirm { plan, .. } = &app.overlay else { panic!("no confirm dialog") };
        assert_eq!(plan.ops, vec![Op::Copy { src: t.0.join("b.txt"), dst: t.0.join("archive/2026/b.txt"), dir: false }]);
    }

    #[test]
    fn same_folder_is_a_noop() {
        let (t, mut app) = app_in("xfer-same");
        let r = row_of(&app, "a.txt");
        yank(&mut app, r, 1, false);
        app.act(Action::Transfer { copy: false });
        app.finish_transfer(t.0.clone(), None);
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app.modified_dirs().is_empty());
    }
}
