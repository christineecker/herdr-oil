//! Application state and event loop: explorer buffers per directory, the
//! finder, overlays (confirm / help / quit prompt / report) and the bridge
//! to the editor.

use crate::apply;
use crate::archive::{create_archive, extract_archive};
use crate::buffer::Buffer;
use crate::config::{Config, OpenTarget};
use crate::entry::{Kind, Sort, Store};
use crate::finder::{self, Finder};
use crate::open::{self, Outcome};
use crate::plan::{self, Plan};
use crate::preview;
use crate::projects;
use crate::theme::Theme;
use crate::vim::{Action, FinderKind, Mode, PathKind, Reg, Vim};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// `/` prompt being typed, with its cursor.
    pub typing: Option<(String, usize)>,
    /// Last search and the (cache) line indices that match it.
    pub search: String,
    pub hits: Vec<usize>,
    /// Line of the current hit.
    pub at: Option<usize>,
    /// Show the structured diff instead of the file (`d` toggles).
    pub diff: bool,
    /// The diff view's state, kept while toggling back to the file.
    pub dv: Option<crate::diffview::DiffView>,
    /// `L`: show `git log` of the path instead.
    pub log: bool,
}

/// One applied `:w` batch, for `:history`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Batch {
    /// Seconds since the epoch.
    pub when: u64,
    pub lines: Vec<String>,
    /// Ops that roll it back (empty: nothing can be undone).
    pub undo: Vec<plan::Op>,
    pub permanent: usize,
}

/// A confirmed action waiting for `y` in [`Overlay::Ask`].
pub enum Deferred {
    /// Discard unstaged changes to these files.
    Restore(Vec<PathBuf>),
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
    /// `undo`: the plan rolls back the last apply (deletes always go to Trash).
    /// `sel`: the op under the cursor (`x` drops it, `o` jumps to it).
    Confirm { plan: Plan, scroll: usize, then_quit: bool, undo: bool, sel: usize },
    /// Yes / no before something that cannot be taken back.
    Ask { title: String, lines: Vec<String>, then: Deferred },
    /// `:history`: applied batches, newest first.
    History { sel: usize },
    /// `:w` refused because names clash: `c` numbers them, `o` jumps there.
    Conflicts { lines: Vec<String>, scroll: usize },
    /// `query` filters the rows (`/` starts typing it).
    Help { scroll: usize, query: String, typing: bool },
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
    /// The finder's ␣ menu (pick a source) is open.
    pub finder_menu: bool,
    /// `?` opened the full sheet (sources, row keys, view keys) instead.
    pub finder_sheet: bool,
    /// Finder footer: key / verb pairs for the selected row, set while drawing.
    pub finder_foot: Vec<(String, String)>,
    /// Last keypress, for the finder's hint on hesitation.
    pub last_key: Instant,
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
    /// Projects tab: `^k` was pressed once on this project.
    close_armed: Option<PathBuf>,
    pub pick: bool,
    pub picked: Vec<PathBuf>,
    suspend: Option<Vec<String>>,
    /// Listing handed to `$EDITOR` (`ge`): temp file, line-id table, directory.
    edit_file: Option<(PathBuf, Vec<u64>, PathBuf)>,
    /// Inverse of the last applied batch, for `U`.
    pub last_batch: Option<Vec<plan::Op>>,
    pub list_area: Rect,
    last_click: Option<(Instant, u16)>,
    pub spinner: usize,
    /// Visited directories for `H` / `L`.
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    hist_nav: bool,
    /// Directory bookmarks (`m{a-z}` / `'{a-z}`), persisted in the state dir.
    pub marks: BTreeMap<char, PathBuf>,
    /// Diff files marked viewed (`v`), by path and change fingerprint.
    pub viewed: HashSet<(PathBuf, u64)>,
    /// `gw`: parent-folder column in wide popups.
    pub show_parent: bool,
    /// Parent listing for that column: folder, then (name, is_dir).
    pub parent_cache: Option<(PathBuf, Vec<(String, bool)>)>,
    /// Clickable path segments in the explorer header: x range → folder.
    pub crumbs: Vec<(u16, u16, PathBuf)>,
    pub head_y: u16,
    /// `gG`: list only entries with a git status.
    pub git_only: bool,
    /// Background `git status` for listings (see `request_git`).
    git_tx: Option<std::sync::mpsc::Sender<PathBuf>>,
    git_rx: Option<std::sync::mpsc::Receiver<GitResult>>,
    git_pending: HashSet<PathBuf>,
    /// Applied batches, newest last (persisted, for `:history`).
    pub journal: Vec<Batch>,
    /// The pending plan, keyed by a hash of the edited buffers.
    plan_cache: Option<(u64, std::rc::Rc<Plan>)>,
    /// Viewer gutter: changed lines of the shown file (path, mtime → line → kind).
    pub gutter: Option<(PathBuf, Option<std::time::SystemTime>, HashMap<usize, char>)>,
}

type GitResult = (PathBuf, HashMap<String, crate::entry::Git>, Option<crate::entry::Branch>);

fn journal_file(cfg: &Config) -> PathBuf {
    cfg.state_dir.join("journal.json")
}

fn load_journal(cfg: &Config) -> Vec<Batch> {
    std::fs::read_to_string(journal_file(cfg)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn marks_file(cfg: &Config) -> PathBuf {
    cfg.state_dir.join("marks")
}

fn load_marks(cfg: &Config) -> BTreeMap<char, PathBuf> {
    let text = std::fs::read_to_string(marks_file(cfg)).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (k, p) = l.split_once('\t')?;
            let c = k.chars().next().filter(|c| c.is_ascii_alphabetic() && k.len() == 1)?;
            Some((c, PathBuf::from(p)))
        })
        .collect()
}

/// Paths as typed into the origin pane: relative to where the popup was
/// opened, `:line` when known, `@path` for agents, shell-quoted otherwise.
fn send_words(paths: &[(PathBuf, Option<usize>)], start: &Path, agent: bool) -> Vec<String> {
    paths
        .iter()
        .map(|(p, line)| {
            let rel = match p.strip_prefix(start) {
                Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
                _ => p.display().to_string(),
            };
            let rel = match line {
                Some(n) => format!("{rel}:{n}"),
                None => rel,
            };
            if agent { format!("@{rel}") } else { open::sh_quote(&rel) }
        })
        .collect()
}

/// Change permissions through a handle opened with O_NOFOLLOW, so a file
/// swapped for a symlink after listing is refused instead of followed.
fn chmod_nofollow(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(p) {
        Ok(f) => f,
        // unreadable (e.g. mode 000): stat and chmod relative to the parent
        // directory without following the last component
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return chmodat_nofollow(p, mode),
        Err(e) => return Err(e),
    };
    if !(f.metadata()?.is_file() || f.metadata()?.is_dir()) {
        return Err(std::io::Error::other("not a regular file or directory"));
    }
    f.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// `fstatat` + `fchmodat(AT_SYMLINK_NOFOLLOW)` against a directory handle,
/// so neither a swapped last component nor a swapped parent is followed.
fn chmodat_nofollow(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let not_ok = || std::io::Error::other("not a regular file or directory");
    let (Some(parent), Some(name)) = (p.parent(), p.file_name()) else { return Err(not_ok()) };
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    let dir = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(parent)?;
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| not_ok())?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let kind = st.st_mode & libc::S_IFMT;
    if kind != libc::S_IFREG && kind != libc::S_IFDIR {
        return Err(not_ok());
    }
    if unsafe { libc::fchmodat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `:chmod` spec on a mode: octal (`755`) or `[ugoa]*[+-=][rwx]+` (`+x`, `go-w`).
pub fn chmod_mode(mode: u32, spec: &str) -> Option<u32> {
    if !spec.is_empty() && spec.len() <= 4 && spec.chars().all(|c| ('0'..='7').contains(&c)) {
        return u32::from_str_radix(spec, 8).ok();
    }
    let op_at = spec.find(['+', '-', '='])?;
    let (who, rest) = spec.split_at(op_at);
    let (op, bits) = rest.split_at(1);
    if bits.is_empty() || !who.chars().all(|c| "ugoa".contains(c)) || !bits.chars().all(|c| "rwx".contains(c)) {
        return None;
    }
    let who = if who.is_empty() || who.contains('a') { "ugo" } else { who };
    let mut mask = 0;
    for w in who.chars() {
        let shift = match w {
            'u' => 6,
            'g' => 3,
            _ => 0,
        };
        for b in bits.chars() {
            mask |= match b {
                'r' => 4,
                'w' => 2,
                _ => 1,
            } << shift;
        }
    }
    Some(match op {
        "+" => mode | mask,
        "-" => mode & !mask,
        _ => {
            let mut clear = 0;
            for w in who.chars() {
                clear |= 7 << match w {
                    'u' => 6,
                    'g' => 3,
                    _ => 0,
                };
            }
            (mode & !clear) | mask
        }
    } & 0o7777)
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
            finder_menu: false,
            finder_sheet: false,
            finder_foot: vec![],
            last_key: Instant::now(),
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
            close_armed: None,
            picked: vec![],
            suspend: None,
            edit_file: None,
            last_batch: None,
            list_area: Rect::default(),
            last_click: None,
            spinner: 0,
            back: vec![],
            fwd: vec![],
            hist_nav: false,
            marks: load_marks(&cfg),
            viewed: HashSet::new(),
            show_parent: false,
            parent_cache: None,
            crumbs: vec![],
            head_y: 0,
            git_only: false,
            git_tx: None,
            git_rx: None,
            git_pending: HashSet::new(),
            journal: load_journal(&cfg),
            plan_cache: None,
            gutter: None,
            cfg,
        };
        app.vim.load_history(app.cfg.state_dir.join("history.txt"));
        app.vim.recipes = app.cfg.recipes.clone();
        app.goto(&dir, focus.as_deref());
        match mode {
            "finder" => {
                app.open_finder(FinderKind::All);
                app.finder_only = true;
            }
            "files" => {
                app.open_finder(FinderKind::Files);
                app.finder_only = true;
            }
            "dash" => {
                app.open_finder(FinderKind::Dash);
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
            "git" | "changes" => {
                app.open_finder(FinderKind::Git);
                app.finder_only = true;
            }
            "projects" | "hop" => {
                app.open_finder(FinderKind::Projects);
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
        let (hidden, sort) = (self.hidden, self.sort);
        self.plan_cache = None;
        match self.buffers.get_mut(&dir) {
            Some(b) if b.modified() => {}
            Some(b) => b.reload(&mut self.store, hidden, sort, false),
            None => {
                let mut b = Buffer::load(&dir, &mut self.store, hidden, sort, false);
                if self.git_only && self.cfg.git_status {
                    b.only_changed = true;
                    b.reload(&mut self.store, hidden, sort, false);
                }
                self.buffers.insert(dir.clone(), b);
            }
        }
        self.request_git(&dir);
        // Drop unmodified buffers of other dirs to keep things fresh.
        let cwd = dir.clone();
        self.buffers.retain(|d, b| *d == cwd || b.modified());
        if dir != self.cwd && !self.hist_nav {
            self.back.push(self.cwd.clone());
            if self.back.len() > 100 {
                self.back.remove(0);
            }
            self.fwd.clear();
        }
        self.cwd = dir;
        if let Some(name) = focus {
            let store = &self.store;
            let b = self.buffers.get_mut(&self.cwd).unwrap();
            if let Some(i) = b.lines.iter().position(|l| l.name(store) == name) {
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
            self.overlay = if plan.conflicts.is_empty() {
                Overlay::Report { title: "Cannot save".into(), lines: plan.errors, error: true, scroll: 0 }
            } else {
                Overlay::Conflicts { lines: plan.errors, scroll: 0 }
            };
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
            self.overlay = Overlay::Confirm { plan, scroll: 0, then_quit, undo: false, sel: 0 };
        } else {
            self.execute(plan, then_quit, false);
        }
    }

    fn execute(&mut self, plan: Plan, then_quit: bool, undo: bool) {
        let method = if undo { crate::config::DeleteMethod::Trash } else { self.cfg.delete };
        let rep = apply::apply(&plan.ops, method);
        // any landed change makes the previous journal stale, even when this
        // batch has nothing to undo (permanent deletes only)
        if rep.done > 0 {
            self.last_batch = (!rep.undo.is_empty()).then(|| rep.undo.clone());
            self.record(&plan, &rep);
        }
        let focus = rep.landed.iter().find(|p| p.parent() == Some(self.cwd.as_path())).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
        let focus = focus.or_else(|| self.buf().cur().name(&self.store).split('/').next().map(str::to_string));
        self.reload_all(focus);
        if rep.errors.is_empty() {
            let msg = match (undo, rep.permanent) {
                (true, _) => format!("undid {} change(s) — U redoes", plan.ops.len()),
                (false, _) if rep.undo.is_empty() => format!("applied {} change(s) — permanent, nothing to undo", plan.ops.len()),
                (false, 0) => format!("applied {} change(s) — U undoes", plan.ops.len()),
                (false, n) => format!("applied {} change(s) — U undoes all but {n} permanent delete(s)", plan.ops.len()),
            };
            self.flash(msg, MsgKind::Ok);
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
            // from the Git tab, or a deleted file: its diff is all there is to show
            let git_tab = self.view == View::Finder && self.finder.as_ref().is_some_and(|f| f.kind == FinderKind::Git);
            let gone = std::fs::symlink_metadata(&files[0]).is_err();
            if git_tab || gone {
                if let Ok((root, items)) = crate::diffview::items_under(&files[0]) {
                    if !items.is_empty() {
                        self.open_diff(root, items, 0);
                        return;
                    }
                }
            }
            self.open_viewer(files[0].clone(), line, false);
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
        let (h, s) = (self.hidden, self.sort);
        let store = &mut self.store;
        let d = self.buffers.entry(dest.clone()).or_insert_with(|| crate::buffer::Buffer::load(&dest, store, h, s, false));
        d.snapshot();
        d.lines.retain(|l| l.significant());
        for mut l in t.lines {
            if let Some(p) = &prefix {
                l.text = format!("{p}/{}", l.text.trim());
            }
            d.lines.push(l);
        }
        d.ensure_nonempty();
        self.request_git(&dest);
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

    /// Pending edits for the dashboard: (`+ ~ → ⧉ −`, path).
    fn pending_tags(&mut self) -> Vec<(char, PathBuf)> {
        let Some(p) = self.pending_plan() else { return vec![] };
        p.ops
            .iter()
            .map(|op| match op {
                plan::Op::Create { dst, .. } | plan::Op::Link { dst, .. } => ('+', dst.clone()),
                plan::Op::Move { src, dst, .. } if src.parent() == dst.parent() => ('~', dst.clone()),
                plan::Op::Move { dst, .. } => ('→', dst.clone()),
                plan::Op::Copy { dst, .. } => ('⧉', dst.clone()),
                plan::Op::Delete { src, .. } => ('−', src.clone()),
                plan::Op::Retarget { path, .. } => ('~', path.clone()),
            })
            .collect()
    }

    fn open_finder(&mut self, k: FinderKind) {
        self.finder_menu = false;
        self.finder_sheet = false;
        if matches!(k, FinderKind::Dash | FinderKind::All) {
            let pending = self.pending_tags();
            let f = self.finder.get_or_insert_with(|| Finder::new(FinderKind::Files, self.cwd.clone(), &self.cfg));
            f.pending = pending;
        }
        match &mut self.finder {
            Some(f) if f.root == self.cwd || f.pre_git.as_ref() == Some(&self.cwd) => f.set_kind(k),
            _ => self.finder = Some(Finder::new(k, self.cwd.clone(), &self.cfg)),
        }
        self.view = View::Finder;
        self.preview_scroll = 0;
    }

    /// Switch the open finder's source (␣ menu, ⌫ back to All); the query stays.
    fn finder_switch(&mut self, k: FinderKind) {
        let pending = matches!(k, FinderKind::Dash | FinderKind::All).then(|| self.pending_tags());
        if let Some(f) = self.finder.as_mut() {
            if let Some(p) = pending {
                f.pending = p;
            }
            f.set_kind(k);
        }
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
                let name = self.buf().cur().name(&self.store).to_string();
                self.buffers.clear();
                self.goto(&cwd, Some(&name));
                self.flash(if self.hidden { "showing hidden files" } else { "hiding dotfiles" }, MsgKind::Info);
            }
            Action::CycleSort | Action::ReverseSort => {
                if a == Action::CycleSort {
                    self.sort = self.sort.next();
                } else {
                    self.store.reverse = !self.store.reverse;
                }
                let s = self.sort;
                let store = &self.store;
                if let Some(b) = self.buffers.get_mut(&self.cwd) {
                    b.resort(store, s);
                }
                let rev = if self.store.reverse { ", reversed" } else { "" };
                self.flash(format!("sort: {}{rev}", s.label()), MsgKind::Info);
            }
            Action::HistBack | Action::HistFwd => {
                let fwd = a == Action::HistFwd;
                loop {
                    let next = if fwd { self.fwd.pop() } else { self.back.pop() };
                    let Some(d) = next else {
                        self.flash(if fwd { "no newer directory" } else { "no older directory" }, MsgKind::Info);
                        break;
                    };
                    if d.is_dir() && d != self.cwd {
                        let here = self.cwd.clone();
                        if fwd { self.back.push(here) } else { self.fwd.push(here) }
                        self.hist_nav = true;
                        self.goto(&d, None);
                        self.hist_nav = false;
                        break;
                    }
                }
            }
            Action::SetMark(c) => {
                self.marks.insert(c, self.cwd.clone());
                let text: String = self.marks.iter().map(|(k, p)| format!("{k}\t{}\n", p.display())).collect();
                let _ = std::fs::create_dir_all(&self.cfg.state_dir);
                match std::fs::write(marks_file(&self.cfg), text) {
                    Ok(()) => self.flash(format!("mark '{c} → {}", finder::tilde(&self.cwd)), MsgKind::Ok),
                    Err(e) => self.flash(format!("mark '{c} set, not saved: {e}"), MsgKind::Warn),
                }
            }
            Action::JumpMark(c) => match self.marks.get(&c).cloned() {
                Some(p) if p.is_dir() => self.goto(&p, None),
                Some(p) => self.flash(format!("mark '{c}: {} is gone", finder::tilde(&p)), MsgKind::Warn),
                None => self.flash(format!("no mark '{c} — m{c} sets it"), MsgKind::Info),
            },
            Action::Chmod { spec, ids } => self.chmod(&spec, &ids),
            Action::UndoApply => self.undo_apply(),
            Action::EditBuffer => self.edit_buffer(),
            Action::Send(ids) => {
                let paths: Vec<(PathBuf, Option<usize>)> = ids.iter().filter_map(|id| self.store.get(*id)).map(|e| (e.path.clone(), None)).collect();
                self.send_paths(paths);
            }
            Action::ToggleDetails => {
                self.details = (self.details + 1) % 4;
                self.flash(["details off", "sizes", "perms · size · age", "owner · perms · xattrs (@) · broken links"][self.details as usize], MsgKind::Info);
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0, query: String::new(), typing: false },
            Action::Save => self.save(false),
            Action::SaveQuit => self.save(true),
            Action::Quit => self.try_quit(false),
            Action::ForceQuit => self.quit = true,
            Action::Refresh => {
                if self.buf().modified() {
                    self.flash("buffer has edits — :e! discards them", MsgKind::Warn);
                } else {
                    self.reload_cwd();
                    self.flash("refreshed", MsgKind::Info);
                }
            }
            Action::Discard => {
                self.reload_cwd();
                self.flash("edits in this directory discarded", MsgKind::Info);
            }
            Action::Git { op, ids } => self.git_op(&op, &ids),
            Action::Extract(ids) => self.extract(&ids),
            Action::Archive { name, tar, ids } => self.archive(&name, tar, &ids),
            Action::DiffView => {
                let p = self.buf().cur_source_entry(&self.store).map(|e| e.path.clone()).unwrap_or_else(|| self.cwd.clone());
                self.open_diff_for(&p);
            }
            Action::Log => {
                let p = self.buf().cur_source_entry(&self.store).map(|e| e.path.clone()).unwrap_or_else(|| self.cwd.clone());
                self.open_viewer(p, None, false);
                if let Some(v) = self.viewer.as_mut() {
                    v.log = true;
                }
            }
            Action::GitFilter => {
                if !self.cfg.git_status {
                    self.flash("git status is off in the config", MsgKind::Warn);
                } else if self.buf().modified() {
                    self.flash("save (:w) or discard (:e!) edits before filtering", MsgKind::Warn);
                } else {
                    self.git_only = !self.git_only;
                    let on = self.git_only;
                    if let Some(b) = self.buffers.get_mut(&self.cwd) {
                        b.only_changed = on;
                    }
                    self.reload_cwd();
                    self.flash(if on { "only changed entries — gG shows all" } else { "showing all entries" }, MsgKind::Info);
                }
            }
            Action::ToggleParent => {
                self.show_parent = !self.show_parent;
                self.flash(if self.show_parent { "parent column (wide popups)" } else { "parent column off" }, MsgKind::Info);
            }
            Action::Finder(k) => self.open_finder(k),
            Action::Transfer { copy } => self.start_transfer(copy),
            Action::YankPath(kind) => {
                let p = match self.buf().cur_source_entry(&self.store) {
                    Some(e) => e.path.clone(),
                    None => self.cwd.clone(),
                };
                let s = match kind {
                    PathKind::Absolute => p.display().to_string(),
                    PathKind::Name => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()),
                    PathKind::Relative => {
                        let root = finder::repo_root(&self.cwd).unwrap_or_else(|| self.start.clone());
                        match p.strip_prefix(&root) {
                            Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
                            _ => p.display().to_string(),
                        }
                    }
                };
                match open::clipboard(&s) {
                    Ok(()) => self.flash(format!("copied {s}"), MsgKind::Ok),
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
            "history" => {
                if self.journal.is_empty() {
                    self.flash("nothing applied yet", MsgKind::Info);
                } else {
                    self.overlay = Overlay::History { sel: 0 };
                }
            }
            "plan-copy" | "plan" => {
                let Some(p) = self.pending_plan() else {
                    self.flash("no pending changes", MsgKind::Info);
                    return;
                };
                let mut text = plan::describe(&p.ops);
                text.extend(p.errors.iter().map(|e| format!("problem  {e}")));
                let body = text.join("\n") + "\n";
                match rest.strip_prefix('>').map(str::trim) {
                    Some(f) if !f.is_empty() => {
                        let dst = if let Some(r) = f.strip_prefix('~') { PathBuf::from(format!("{}{r}", std::env::var("HOME").unwrap_or_default())) } else { plan::normalize(&self.cwd.join(f)) };
                        if std::fs::symlink_metadata(&dst).is_ok() {
                            self.flash(format!("{} exists — not overwritten", finder::tilde(&dst)), MsgKind::Err);
                        } else {
                            match std::fs::write(&dst, body) {
                                Ok(()) => self.flash(format!("plan written to {}", finder::tilde(&dst)), MsgKind::Ok),
                                Err(e) => self.flash(format!("{}: {e}", finder::tilde(&dst)), MsgKind::Err),
                            }
                        }
                    }
                    _ if head == "plan" && !rest.is_empty() => self.flash("usage: :plan > file   or   :plan-copy", MsgKind::Warn),
                    _ => match open::clipboard(&body) {
                        Ok(()) => self.flash(format!("copied the plan ({} op{})", p.ops.len(), if p.ops.len() == 1 { "" } else { "s" }), MsgKind::Ok),
                        Err(e) => self.flash(e, MsgKind::Err),
                    },
                }
            }
            "marks" => {
                let lines: Vec<String> = if self.marks.is_empty() {
                    vec!["no marks yet — m{a-z} marks the current folder, '{a-z} jumps back".into()]
                } else {
                    self.marks.iter().map(|(k, p)| format!("'{k}   {}", finder::tilde(p))).collect()
                };
                self.overlay = Overlay::Report { title: "Marks".into(), lines, error: false, scroll: 0 };
            }
            _ => self.flash(format!("unknown command: {c}"), MsgKind::Err),
        }
    }

    /// `U`: review and run the inverse of the last applied batch. Running it
    /// records its own inverse, so `U` again redoes.
    fn undo_apply(&mut self) {
        if !self.modified_dirs().is_empty() {
            self.flash("save (:w) or discard (:e!) the current edits before undoing the last apply", MsgKind::Warn);
            return;
        }
        let Some(ops) = self.last_batch.clone() else {
            self.flash("nothing applied in this session yet", MsgKind::Info);
            return;
        };
        let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
        let (ops, gone): (Vec<_>, Vec<_>) = ops.into_iter().partition(|op| match op {
            plan::Op::Move { src, .. } | plan::Op::Delete { src, .. } => exists(src),
            plan::Op::Retarget { path, .. } => std::fs::read_link(path).is_ok(),
            _ => true,
        });
        if ops.is_empty() {
            self.flash("the last apply was already changed on disk — nothing to undo", MsgKind::Warn);
            return;
        }
        let errors = gone.iter().filter_map(|op| op.src().map(|p| format!("{}: no longer there, skipped", finder::tilde(p)))).collect::<Vec<_>>();
        if !errors.is_empty() {
            self.flash(errors.join(" · "), MsgKind::Warn);
        }
        self.overlay = Overlay::Confirm { plan: Plan { ops, ..Default::default() }, scroll: 0, then_quit: false, undo: true, sel: 0 };
    }

    /// `ge`: hand this listing to `$EDITOR` as `/001 name` lines (the number
    /// keeps the entry identity, like oil.nvim), read it back after.
    fn edit_buffer(&mut self) {
        // one line per entry: a name with a newline would come back as an
        // extra (possibly tagged) line and stage edits nobody typed
        if self.buf().lines.iter().any(|l| l.text.chars().any(char::is_control)) {
            self.flash("a name here contains control characters — ge is off for this folder", MsgKind::Err);
            return;
        }
        let mut table = Vec::new();
        let mut text = String::new();
        for l in &self.buf().lines {
            match l.id {
                Some(id) => {
                    table.push(id);
                    text.push_str(&format!("/{:03} {}\n", table.len(), l.text));
                }
                None if l.significant() => text.push_str(&format!("{}\n", l.text)),
                None => {}
            }
        }
        let _ = std::fs::create_dir_all(&self.cfg.state_dir);
        let path = self.cfg.state_dir.join(format!("listing-{}.oil", std::process::id()));
        if let Err(e) = std::fs::write(&path, text) {
            self.flash(format!("cannot write {}: {e}", finder::tilde(&path)), MsgKind::Err);
            return;
        }
        self.suspend = Some(open::editor_argv(&self.cfg.editor(), std::slice::from_ref(&path), None));
        self.edit_file = Some((path, table, self.cwd.clone()));
    }

    /// Read the listing back from the editor into its buffer (one undo step).
    fn finish_edit_buffer(&mut self) {
        let Some((path, table, dir)) = self.edit_file.take() else { return };
        let text = std::fs::read_to_string(&path);
        let _ = std::fs::remove_file(&path);
        let Ok(text) = text else {
            self.flash("editor left no listing behind — nothing changed", MsgKind::Warn);
            return;
        };
        let mut lines = Vec::new();
        let mut bad = 0;
        for raw in text.lines() {
            let tagged = raw.strip_prefix('/').and_then(|r| r.split_once(' ')).filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            match tagged {
                Some((n, rest)) => match n.parse::<usize>().ok().and_then(|n| table.get(n.wrapping_sub(1))) {
                    Some(&id) => lines.push(crate::buffer::Line { id: Some(id), text: rest.to_string() }),
                    None => {
                        bad += 1;
                        lines.push(crate::buffer::Line::new(rest));
                    }
                },
                None if raw.trim().is_empty() => {}
                None => lines.push(crate::buffer::Line::new(raw)),
            }
        }
        let Some(b) = self.buffers.get_mut(&dir) else { return };
        b.snapshot();
        b.lines = lines;
        b.ensure_nonempty();
        b.clamp();
        if b.squash_noop() {
            self.flash("no changes from the editor", MsgKind::Info);
        } else if bad > 0 {
            self.flash(format!("{bad} line(s) had an unknown /NNN tag and become new entries — :w to review"), MsgKind::Warn);
        } else {
            self.flash("listing updated from the editor — :w to review", MsgKind::Info);
        }
    }

    /// Type paths into the origin pane (`gi`, finder `^s`, viewer `p`): no
    /// Enter, so they land on the agent's or shell's input line.
    fn send_paths(&mut self, paths: Vec<(PathBuf, Option<usize>)>) {
        let Some(origin) = self.origin.clone() else {
            self.flash("no origin pane — open herdr-oil from herdr to send paths", MsgKind::Warn);
            return;
        };
        if paths.is_empty() {
            self.flash("no saved entry here to send", MsgKind::Warn);
            return;
        }
        let agent = crate::launch::panes().iter().any(|p| p.pane_id.as_deref() == Some(origin.as_str()) && p.agent.is_some());
        let words = send_words(&paths, &self.start, agent);
        if words.iter().any(|w| w.chars().any(char::is_control)) {
            self.flash("name contains control characters — not sent", MsgKind::Err);
            return;
        }
        let text = format!("{} ", words.join(" "));
        match crate::launch::herdr(&["pane", "send-text", &origin, &text]) {
            Ok(_) => {
                let m = format!("sent {} path(s) to the origin pane", words.len());
                if self.cfg.stay_open {
                    self.flash(m, MsgKind::Ok);
                } else if self.modified_dirs().is_empty() {
                    self.quit = true;
                } else {
                    self.flash(format!("{m} — unsaved edits keep the explorer open"), MsgKind::Warn);
                }
            }
            Err(e) => self.flash(format!("send failed: {e}"), MsgKind::Err),
        }
    }

    /// `:chmod` — permission changes are not part of the edit plan, they
    /// apply to the selected entries right away.
    fn chmod(&mut self, spec: &str, ids: &[u64]) {
        let (mut done, mut errs) = (0, Vec::new());
        for id in ids {
            let Some(e) = self.store.get(*id).cloned() else { continue };
            if matches!(e.kind, Kind::Link | Kind::LinkDir) {
                continue;
            }
            let Some(mode) = chmod_mode(e.mode & 0o7777, spec) else {
                self.flash(format!("chmod: bad mode {spec} (try +x, go-w, 644)"), MsgKind::Err);
                return;
            };
            match chmod_nofollow(&e.path, mode) {
                Ok(()) => {
                    done += 1;
                    if let Some(x) = self.store.entries.get_mut(id) {
                        x.mode = (x.mode & !0o7777) | mode;
                    }
                }
                Err(err) => errs.push(format!("{}: {err}", e.name)),
            }
        }
        match errs.first() {
            Some(e) => self.flash(format!("chmod {spec}: {done} ok, {e}"), MsgKind::Err),
            None if done == 0 => self.flash("chmod: no saved entries on these lines", MsgKind::Warn),
            None => self.flash(format!("chmod {spec}: {done} entr{}", if done == 1 { "y" } else { "ies" }), MsgKind::Ok),
        }
    }

    // ── viewer, diff view ───────────────────────────────────────────────────

    fn open_viewer(&mut self, path: PathBuf, line: Option<usize>, diff: bool) {
        let back = if self.view == View::Viewer { self.viewer.as_ref().map(|v| v.back).unwrap_or(View::Explorer) } else { self.view };
        let search = self.viewer.as_ref().map(|v| v.search.clone()).unwrap_or_default();
        self.viewer = Some(Viewer { path, line, top: None, back, height: 20, total: 0, typing: None, search, hits: vec![], at: None, diff, dv: None, log: false });
        self.view = View::Viewer;
    }

    /// The diff view over `items` (repository `root`), starting at `cur`.
    fn open_diff(&mut self, root: PathBuf, items: Vec<crate::diffview::FileItem>, cur: usize) {
        let path = items.get(cur).map(|i| i.path.clone()).unwrap_or_else(|| root.clone());
        self.open_viewer(path, None, true);
        if let Some(v) = self.viewer.as_mut() {
            v.dv = Some(crate::diffview::DiffView::new(root, items, cur, self.cfg.diff_layout));
        }
    }

    /// `gD`: a file's diff, or every change under a folder.
    fn open_diff_for(&mut self, p: &Path) {
        match crate::diffview::items_under(p) {
            Ok((_, items)) if items.is_empty() => self.flash(format!("no changes in {}", finder::tilde(p)), MsgKind::Info),
            Ok((root, items)) => self.open_diff(root, items, 0),
            Err(e) => self.flash(e, MsgKind::Warn),
        }
    }

    fn dv_out(&mut self, out: crate::diffview::Out) {
        use crate::diffview::Out;
        match out {
            Out::None => {}
            Out::Back => {
                if let Some(v) = self.viewer.take() {
                    self.view = v.back;
                }
            }
            Out::FileView => {
                if let Some(v) = self.viewer.as_mut() {
                    if let Some(p) = v.dv.as_ref().and_then(|d| d.current()).map(|c| c.path.clone()) {
                        v.path = p;
                    }
                    v.diff = false;
                    (v.top, v.at) = (None, None);
                }
                self.flash("showing the file — d for the diff", MsgKind::Info);
            }
            Out::Flash(m) => self.flash(m, MsgKind::Info),
            Out::Edit(p, line, how) => self.open_files(vec![p], line, how.unwrap_or(OpenTarget::Popup)),
            Out::Send(p, line) => self.send_paths(vec![(p, line)]),
            Out::SendText(t) => self.send_context(t),
            Out::Copy(s) => match open::clipboard(&s) {
                Ok(()) => self.flash(format!("copied {s}"), MsgKind::Ok),
                Err(e) => self.flash(e, MsgKind::Err),
            },
            Out::SystemOpen(p) => {
                if let Err(e) = open::system_open(&p) {
                    self.flash(e, MsgKind::Err);
                }
            }
            Out::Search => {
                if let Some(v) = self.viewer.as_mut() {
                    v.typing = Some((String::new(), 0));
                }
            }
        }
    }

    /// Hand a block of text (a hunk, grep lines) to the origin pane. Agents
    /// get it as a bracketed paste, so newlines do not submit; a shell gets
    /// nothing typed (newlines would run commands) — it goes to the clipboard.
    fn send_context(&mut self, text: String) {
        let text: String = text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
        let n = text.lines().count();
        let agent = self.origin.clone().filter(|o| crate::launch::panes().iter().any(|p| p.pane_id.as_deref() == Some(o.as_str()) && p.agent.is_some()));
        let Some(origin) = agent else {
            match open::clipboard(&text) {
                Ok(()) => self.flash(format!("{n} line(s) copied — the origin pane is not an agent, so nothing was typed"), MsgKind::Ok),
                Err(e) => self.flash(e, MsgKind::Err),
            }
            return;
        };
        match crate::launch::herdr(&["pane", "send-text", &origin, &format!("\x1b[200~{text}\n\x1b[201~")]) {
            Ok(_) => self.flash(format!("sent {n} line(s) to the agent"), MsgKind::Ok),
            Err(e) => self.flash(format!("send failed: {e}"), MsgKind::Err),
        }
    }

    // ── git, archives ───────────────────────────────────────────────────────

    fn reload_cwd(&mut self) {
        let (h, s) = (self.hidden, self.sort);
        let store = &mut self.store;
        if let Some(b) = self.buffers.get_mut(&self.cwd) {
            b.reload(store, h, s, false);
        }
        self.plan_cache = None;
        let cwd = self.cwd.clone();
        self.request_git(&cwd);
    }

    /// Ask the background worker for `dir`'s git status (badges, branch).
    pub fn request_git(&mut self, dir: &Path) {
        if !self.cfg.git_status {
            return;
        }
        if self.git_tx.is_none() {
            let (tx, req) = std::sync::mpsc::channel::<PathBuf>();
            let (done, rx) = std::sync::mpsc::channel::<GitResult>();
            std::thread::spawn(move || {
                while let Ok(d) = req.recv() {
                    let mut dirs = vec![d];
                    while let Ok(more) = req.try_recv() {
                        if !dirs.contains(&more) {
                            dirs.push(more);
                        }
                    }
                    for d in dirs {
                        let (m, b) = crate::entry::git_status_full(&d);
                        if done.send((d, m, b)).is_err() {
                            return;
                        }
                    }
                }
            });
            self.git_tx = Some(tx);
            self.git_rx = Some(rx);
        }
        if self.git_tx.as_ref().is_some_and(|tx| tx.send(dir.to_path_buf()).is_ok()) {
            self.git_pending.insert(dir.to_path_buf());
        }
    }

    /// Take finished git statuses; true when a listing got new badges.
    fn poll_git(&mut self) -> bool {
        let Some(rx) = &self.git_rx else { return false };
        let mut changed = false;
        while let Ok((d, m, b)) = rx.try_recv() {
            self.git_pending.remove(&d);
            if let Some(buf) = self.buffers.get_mut(&d) {
                buf.git = m;
                buf.branch = b;
                changed = true;
            }
        }
        changed
    }

    /// Background work the screen is waiting for.
    pub fn busy(&self) -> bool {
        !self.git_pending.is_empty() || self.preview.pending.is_some() || self.viewer_cache.pending.is_some()
    }

    /// The plan the edited buffers make, rebuilt only when their text changes.
    pub fn pending_plan(&mut self) -> Option<std::rc::Rc<Plan>> {
        use std::hash::{Hash, Hasher};
        let modified = self.modified_dirs();
        if modified.is_empty() {
            self.plan_cache = None;
            return None;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for d in &modified {
            d.hash(&mut h);
            for l in &self.buffers[d].lines {
                l.id.hash(&mut h);
                l.text.hash(&mut h);
            }
        }
        let key = h.finish();
        if let Some((k, p)) = &self.plan_cache {
            if *k == key {
                return Some(p.clone());
            }
        }
        let p = std::rc::Rc::new(plan::build(self.buffers.values(), &self.store));
        self.plan_cache = Some((key, p.clone()));
        Some(p)
    }

    fn git_op(&mut self, op: &str, ids: &[u64]) {
        let paths: Vec<PathBuf> = ids.iter().filter_map(|id| self.store.get(*id)).map(|e| e.path.clone()).collect();
        self.git_paths(op, paths);
    }

    /// `stage` / `unstage` right away; `restore` asks first.
    fn git_paths(&mut self, op: &str, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            self.flash("no saved entries here", MsgKind::Warn);
            return;
        }
        let root = match crate::git::guard(&paths[0]) {
            Ok(r) => r,
            Err(e) => {
                self.flash(e, MsgKind::Err);
                return;
            }
        };
        let n = paths.len();
        if op == "restore" {
            let lines = paths.iter().map(|p| finder::tilde(p)).collect();
            let title = format!("Discard unstaged changes in {n} file{}? This cannot be undone", if n == 1 { "" } else { "s" });
            self.overlay = Overlay::Ask { title, lines, then: Deferred::Restore(paths) };
            return;
        }
        let r = if op == "stage" { crate::git::stage(&root, &paths) } else { crate::git::unstage(&root, &paths) };
        match r {
            Ok(()) => self.flash(format!("{op}d {n} path{}", if n == 1 { "" } else { "s" }), MsgKind::Ok),
            Err(e) => self.flash(format!("{op}: {e}"), MsgKind::Err),
        }
        self.after_git();
    }

    fn run_deferred(&mut self, d: Deferred) {
        match d {
            Deferred::Restore(paths) => {
                let r = crate::git::guard(&paths[0]).and_then(|root| crate::git::restore(&root, &paths));
                match r {
                    Ok(()) => self.flash(format!("restored {} file(s) to their staged state", paths.len()), MsgKind::Ok),
                    Err(e) => self.flash(format!("restore: {e}"), MsgKind::Err),
                }
                self.after_git();
                if !self.buf().modified() {
                    self.reload_cwd();
                }
            }
        }
    }

    /// Statuses changed: refresh badges, the Git tab and open diffs.
    fn after_git(&mut self) {
        let dirs: Vec<PathBuf> = self.buffers.keys().cloned().collect();
        for d in dirs {
            self.request_git(&d);
        }
        if let Some(f) = self.finder.as_mut() {
            if matches!(f.kind, FinderKind::Git | FinderKind::Dash) {
                let sel = f.sel;
                f.restart();
                f.sel = sel;
            } else {
                // only the Dash rows; the walk stays
                f.refresh_zero();
            }
        }
        self.preview.invalidate();
        if let Some(dv) = self.viewer.as_mut().and_then(|v| v.dv.as_mut()) {
            dv.invalidate();
        }
    }

    fn extract(&mut self, ids: &[u64]) {
        let (mut done, mut errs) = (Vec::new(), Vec::new());
        for id in ids {
            let Some(e) = self.store.get(*id).cloned() else { continue };
            match extract_archive(&e.path) {
                Ok(d) => done.push(d),
                Err(err) => errs.push(format!("{}: {err}", e.name)),
            }
        }
        if !self.buf().modified() {
            self.reload_cwd();
            if let Some(name) = done.first().and_then(|d| d.file_name()).map(|n| format!("{}/", n.to_string_lossy())) {
                let b = self.buf_mut();
                if let Some(i) = b.lines.iter().position(|l| l.text == name) {
                    b.row = i;
                }
            }
        }
        match (done.len(), errs.first()) {
            (_, Some(e)) => self.flash(format!("extract: {} ok, {e}", done.len()), MsgKind::Err),
            (0, None) => self.flash("extract: no saved archive on these lines", MsgKind::Warn),
            (_, None) => self.flash(format!("extracted into {}", done.iter().map(|d| finder::tilde(d)).collect::<Vec<_>>().join(", ")), MsgKind::Ok),
        }
    }

    fn archive(&mut self, name: &str, tar: bool, ids: &[u64]) {
        let entries: Vec<PathBuf> = ids.iter().filter_map(|id| self.store.get(*id)).filter(|e| e.path.parent() == Some(self.cwd.as_path())).map(|e| e.path.clone()).collect();
        if entries.is_empty() {
            self.flash("no saved entries of this folder on these lines", MsgKind::Warn);
            return;
        }
        match create_archive(&self.cwd, name, tar, &entries) {
            Ok(p) => {
                if !self.buf().modified() {
                    self.reload_cwd();
                }
                self.flash(format!("packed {} entr{} into {}", entries.len(), if entries.len() == 1 { "y" } else { "ies" }, finder::tilde(&p)), MsgKind::Ok);
            }
            Err(e) => self.flash(e, MsgKind::Err),
        }
    }

    // ── history, conflicts ──────────────────────────────────────────────────

    /// Remember an applied batch for `:history` (kept across sessions).
    fn record(&mut self, plan: &Plan, rep: &apply::Report) {
        let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.journal.push(Batch { when, lines: plan::describe(&plan.ops), undo: rep.undo.clone(), permanent: rep.permanent });
        if self.journal.len() > 50 {
            self.journal.remove(0);
        }
        if let Ok(t) = serde_json::to_string(&self.journal) {
            let _ = std::fs::create_dir_all(&self.cfg.state_dir);
            let _ = std::fs::write(journal_file(&self.cfg), t);
        }
    }

    /// Review rolling back batch `i` of the journal (as `U` does for the last).
    fn undo_batch(&mut self, i: usize) {
        if !self.modified_dirs().is_empty() {
            self.flash("save (:w) or discard (:e!) the current edits first", MsgKind::Warn);
            return;
        }
        let Some(b) = self.journal.get(i) else { return };
        if b.undo.is_empty() {
            self.flash("that batch only deleted permanently — nothing to bring back", MsgKind::Warn);
            return;
        }
        let exists = |p: &Path| std::fs::symlink_metadata(p).is_ok();
        let ops: Vec<plan::Op> = b
            .undo
            .iter()
            .filter(|op| match op {
                plan::Op::Move { src, .. } | plan::Op::Delete { src, .. } => exists(src),
                plan::Op::Retarget { path, .. } => std::fs::read_link(path).is_ok(),
                _ => true,
            })
            .cloned()
            .collect();
        if ops.is_empty() {
            self.flash("everything in that batch was changed on disk since — nothing to undo", MsgKind::Warn);
            return;
        }
        let mut plan = Plan { ops, ..Default::default() };
        plan::revalidate(&mut plan);
        self.overlay = Overlay::Confirm { plan, scroll: 0, then_quit: false, undo: true, sel: 0 };
    }

    /// Buffer lines whose destination clashes (dir, row, key, reason).
    fn conflict_lines(&self) -> Vec<(PathBuf, usize, String, String)> {
        let plan = plan::build(self.buffers.values(), &self.store);
        let mut dirs: Vec<&PathBuf> = self.buffers.keys().collect();
        dirs.sort();
        let mut out = Vec::new();
        for d in dirs {
            let b = &self.buffers[d];
            for (i, l) in b.lines.iter().enumerate() {
                let entry = l.id.and_then(|id| self.store.get(id));
                let (name, _) = plan::line_parts(&l.text, entry);
                let Ok(Some((rel, _))) = plan::parse_name(name) else { continue };
                let dst = plan::normalize(&b.dir.join(rel));
                let k = plan::key(&dst);
                // an untouched line keeps its name; the edited ones move
                if entry.is_some_and(|e| plan::key(&e.path) == k) {
                    continue;
                }
                if let Some(why) = plan.conflicts.get(&k) {
                    out.push((d.clone(), i, k, why.clone()));
                }
            }
        }
        out
    }

    /// `c` in the conflict report: give clashing names a counter
    /// (`report 2.pdf`), skipping names that exist or are taken by the plan.
    fn number_conflicts(&mut self) {
        let lines = self.conflict_lines();
        if lines.is_empty() {
            return;
        }
        let plan = plan::build(self.buffers.values(), &self.store);
        let mut taken: HashSet<String> = plan.ops.iter().filter_map(|o| o.dst()).map(plan::key).collect();
        let mut first_kept: HashSet<String> = HashSet::new();
        let mut snapped: HashSet<PathBuf> = HashSet::new();
        let mut n = 0;
        for (dir, row, k, why) in lines {
            // of several new names for one path, the first keeps it
            if why.starts_with("two entries") && first_kept.insert(k.clone()) {
                continue;
            }
            let Some(b) = self.buffers.get_mut(&dir) else { continue };
            let text = b.lines[row].text.clone();
            let entry = b.lines[row].id.and_then(|id| self.store.get(id));
            let (name, link) = plan::line_parts(&text, entry);
            let link = link.map(|t| format!(" -> {t}")).unwrap_or_default();
            let slash = if name.trim_end().ends_with('/') { "/" } else { "" };
            let core = name.trim().trim_end_matches('/');
            let (lead, last) = match core.rfind('/') {
                Some(i) => (&core[..=i], &core[i + 1..]),
                None => ("", core),
            };
            let (stem, ext) = match last.rfind('.') {
                Some(i) if i > 0 && slash.is_empty() => (&last[..i], &last[i..]),
                _ => (last, ""),
            };
            for c in 2.. {
                let cand = format!("{lead}{stem} {c}{ext}");
                let dst = plan::normalize(&dir.join(&cand));
                let ck = plan::key(&dst);
                if !taken.contains(&ck) && std::fs::symlink_metadata(&dst).is_err() {
                    taken.insert(ck);
                    if snapped.insert(dir.clone()) {
                        b.snapshot();
                    }
                    b.lines[row].text = format!("{cand}{slash}{link}");
                    n += 1;
                    break;
                }
            }
        }
        self.plan_cache = None;
        self.flash(format!("numbered {n} clashing name{} — :w to review", if n == 1 { "" } else { "s" }), MsgKind::Info);
    }

    /// `o` in the conflict report: the first clashing line.
    fn jump_conflict(&mut self) {
        if let Some((dir, row, _, why)) = self.conflict_lines().into_iter().next() {
            self.goto(&dir, None);
            let b = self.buf_mut();
            b.row = row;
            b.clamp();
            self.flash(format!("✗ {why}"), MsgKind::Warn);
        }
    }

    // ── keys ────────────────────────────────────────────────────────────────

    fn key(&mut self, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }
        self.last_key = Instant::now();
        if !matches!(self.overlay, Overlay::None) {
            self.overlay_key(k);
            return;
        }
        match self.view {
            View::Explorer => {
                self.vim.msg = None;
                let cwd = self.cwd.clone();
                let b = self.buffers.get_mut(&cwd).unwrap();
                let was_cmd = matches!(self.vim.mode, Mode::Cmd { .. });
                let a = self.vim.handle(b, k);
                b.follow();
                if was_cmd || matches!(self.vim.mode, Mode::Cmd { .. }) {
                    // the command line shows completions / live :s counts on
                    // its right; each key replaces them
                    self.msg = None;
                }
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
            Overlay::Confirm { mut plan, scroll, then_quit, undo, mut sel } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter if plan.errors.is_empty() => self.execute(plan, then_quit, undo),
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.flash("fix the problems first — x drops the op under the cursor", MsgKind::Warn);
                    self.overlay = Overlay::Confirm { plan, scroll, then_quit, undo, sel };
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.flash("cancelled — edits kept", MsgKind::Info),
                KeyCode::Char('j') | KeyCode::Down => {
                    sel = (sel + 1).min(plan.ops.len().saturating_sub(1));
                    self.overlay = Overlay::Confirm { plan, scroll, then_quit, undo, sel };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    sel = sel.saturating_sub(1);
                    self.overlay = Overlay::Confirm { plan, scroll, then_quit, undo, sel };
                }
                KeyCode::Char('x') | KeyCode::Char('d') => {
                    if sel < plan.ops.len() {
                        let gone = plan::describe(&plan.ops[sel..=sel]).remove(0);
                        plan.ops.remove(sel);
                        self.flash(format!("dropped: {}", gone.split_whitespace().collect::<Vec<_>>().join(" ")), MsgKind::Info);
                    }
                    if plan.ops.is_empty() {
                        self.flash("nothing left to apply — edits kept", MsgKind::Info);
                    } else {
                        plan::revalidate(&mut plan);
                        sel = sel.min(plan.ops.len() - 1);
                        self.overlay = Overlay::Confirm { plan, scroll, then_quit, undo, sel };
                    }
                }
                KeyCode::Char('o') | KeyCode::Char('g') => {
                    // to the line that makes this op (a delete: its folder)
                    if let Some(op) = plan.ops.get(sel) {
                        let target = match op {
                            plan::Op::Retarget { path, .. } => Some(path.clone()),
                            op => op.dst().or(op.src()).map(Path::to_path_buf),
                        };
                        if let Some(t) = target {
                            let dir = t.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"));
                            let name = t.file_name().map(|n| n.to_string_lossy().into_owned());
                            if dir.is_dir() {
                                self.goto(&dir, name.as_deref());
                                self.view = View::Explorer;
                            }
                        }
                    }
                }
                _ => self.overlay = Overlay::Confirm { plan, scroll, then_quit, undo, sel },
            },
            Overlay::Ask { title, lines, then } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.run_deferred(then),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => self.flash("cancelled", MsgKind::Info),
                _ => self.overlay = Overlay::Ask { title, lines, then },
            },
            Overlay::History { mut sel } => match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    sel = (sel + 1).min(self.journal.len().saturating_sub(1));
                    self.overlay = Overlay::History { sel };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.overlay = Overlay::History { sel: sel.saturating_sub(1) };
                }
                KeyCode::Char('u') | KeyCode::Enter => {
                    let i = self.journal.len().saturating_sub(1 + sel);
                    self.undo_batch(i);
                }
                KeyCode::Char('y') => {
                    let i = self.journal.len().saturating_sub(1 + sel);
                    let text = self.journal.get(i).map(|b| b.lines.join("\n")).unwrap_or_default();
                    match open::clipboard(&text) {
                        Ok(()) => self.flash("copied the batch", MsgKind::Ok),
                        Err(e) => self.flash(e, MsgKind::Err),
                    }
                    self.overlay = Overlay::History { sel };
                }
                _ => {}
            },
            Overlay::Conflicts { lines, mut scroll } => match k.code {
                KeyCode::Char('c') => self.number_conflicts(),
                KeyCode::Char('o') => self.jump_conflict(),
                KeyCode::Char('j') | KeyCode::Down => {
                    scroll += 1;
                    self.overlay = Overlay::Conflicts { lines, scroll };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.overlay = Overlay::Conflicts { lines, scroll: scroll.saturating_sub(1) };
                }
                _ => {}
            },
            Overlay::QuitPrompt => match k.code {
                KeyCode::Char('s') | KeyCode::Char('w') | KeyCode::Char('y') => self.save(true),
                KeyCode::Char('d') | KeyCode::Char('!') => self.quit = true,
                KeyCode::Char('c') | KeyCode::Esc | KeyCode::Char('n') => {}
                _ => self.overlay = Overlay::QuitPrompt,
            },
            Overlay::Help { mut query, typing: true, .. } => {
                match k.code {
                    KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => query.push(c),
                    KeyCode::Backspace => {
                        query.pop();
                    }
                    KeyCode::Esc => query.clear(),
                    _ => {}
                }
                let typing = !matches!(k.code, KeyCode::Enter | KeyCode::Esc);
                self.overlay = Overlay::Help { scroll: 0, query, typing };
            }
            Overlay::Help { mut scroll, query, typing } => match k.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    scroll += 1;
                    self.overlay = Overlay::Help { scroll, query, typing };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    self.overlay = Overlay::Help { scroll, query, typing };
                }
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    scroll += 10;
                    self.overlay = Overlay::Help { scroll, query, typing };
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(10);
                    self.overlay = Overlay::Help { scroll, query, typing };
                }
                KeyCode::Char('/') => self.overlay = Overlay::Help { scroll: 0, query: String::new(), typing: true },
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
                (KeyCode::Tab | KeyCode::BackTab, _) => {
                    f.set_kind(if f.kind == FinderKind::Dirs { FinderKind::Files } else { FinderKind::Dirs });
                    self.preview_scroll = 0;
                    return;
                }
                (KeyCode::Char('v' | 'x' | 't' | 'e' | 'g' | 'o' | 'y' | 's' | ' '), true) => return,
                _ => {}
            }
        } else if self.finder_menu {
            // ␣ menu / ? sheet: a letter picks the source, anything else closes it
            self.finder_menu = false;
            self.finder_sheet = false;
            if let (KeyCode::Char(c), false, false) = (k.code, ctrl, alt) {
                if let Some(kind) = FinderKind::from_leader(c) {
                    self.finder_switch(kind);
                }
            }
            return;
        } else if matches!((k.code, ctrl, alt), (KeyCode::Char(' ' | '?'), false, false)) && self.finder.as_ref().is_some_and(|f| f.query.is_empty()) {
            self.finder_menu = true;
            self.finder_sheet = k.code == KeyCode::Char('?');
            return;
        }
        if self.transfer.is_none() && self.finder.as_ref().is_some_and(|f| f.kind == FinderKind::Projects) && self.project_key(k) {
            return;
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
                f.jump_group(if k.code == KeyCode::BackTab { -1 } else { 1 });
                self.preview_scroll = 0;
                return;
            }
            (KeyCode::Enter, _, _) if f.kind == FinderKind::Git && !self.pick => {
                // the diff view, with every listed change one ⇥ away
                let (n, sel) = (f.len(), f.sel);
                let items: Vec<crate::diffview::FileItem> = f
                    .items(0, n)
                    .into_iter()
                    .filter(|it| !it.is_dir)
                    .map(|it| crate::diffview::FileItem { path: it.abs, rel: it.display, git: it.git })
                    .collect();
                let root = f.root.clone();
                if items.is_empty() {
                    return;
                }
                self.open_diff(root, items, sel);
                return;
            }
            (KeyCode::Char('a'), true, _) if f.kind == FinderKind::Git => {
                // stage the selection (or marked files); staged ones get unstaged
                let picked: Vec<(PathBuf, bool)> = if f.marked.is_empty() {
                    f.selected().map(|it| (it.abs, it.git.is_some_and(|g| g.staged && !g.unstaged))).into_iter().collect()
                } else {
                    let n = f.len();
                    let all = f.items(0, n);
                    f.marked.iter().map(|m| (m.path.clone(), all.iter().find(|it| it.abs == m.path).and_then(|it| it.git).is_some_and(|g| g.staged && !g.unstaged))).collect()
                };
                f.marked.clear();
                let (unstage, stage): (Vec<_>, Vec<_>) = picked.into_iter().partition(|(_, staged)| *staged);
                if !stage.is_empty() {
                    self.git_paths("stage", stage.into_iter().map(|(p, _)| p).collect());
                }
                if !unstage.is_empty() {
                    self.git_paths("unstage", unstage.into_iter().map(|(p, _)| p).collect());
                }
                return;
            }
            (KeyCode::Char('d'), _, true) if f.kind == FinderKind::Git => {
                let paths: Vec<PathBuf> = if f.marked.is_empty() { f.selected().map(|it| it.abs).into_iter().collect() } else { f.marked.iter().map(|m| m.path.clone()).collect() };
                self.git_paths("restore", paths);
                return;
            }
            (KeyCode::Char('s'), _, true) => {
                // grep hits (marked, else the selection) as text for an agent
                let hits: Vec<finder::Item> = if f.marked.is_empty() {
                    f.selected().into_iter().collect()
                } else {
                    let n = f.len();
                    f.items(0, n).into_iter().filter(|it| f.marked.iter().any(|m| m.path == it.abs && m.line == it.line)).collect()
                };
                let start = self.start.clone();
                let text: Vec<String> = hits
                    .iter()
                    .map(|it| {
                        let rel = it.abs.strip_prefix(&start).map(|r| r.display().to_string()).unwrap_or_else(|_| it.abs.display().to_string());
                        match (&it.grep_text, it.line) {
                            (Some(t), Some(l)) => format!("{rel}:{l}: {}", t.trim()),
                            _ => rel,
                        }
                    })
                    .collect();
                if text.is_empty() {
                    return;
                }
                self.send_context(text.join("\n"));
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
            (KeyCode::Char('s'), true, _) => {
                let paths: Vec<(PathBuf, Option<usize>)> = if f.marked.is_empty() {
                    f.selected().map(|it| (it.abs, it.line)).into_iter().collect()
                } else {
                    f.marked.iter().map(|m| (m.path.clone(), m.line)).collect()
                };
                self.send_paths(paths);
                return;
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
            (KeyCode::Backspace, false, false) if f.query.is_empty() && f.kind != FinderKind::All => {
                self.finder_switch(FinderKind::All);
                return;
            }
            (KeyCode::Backspace, false, false) if f.query.is_empty() && !self.finder_only => {
                self.view = View::Explorer;
                return;
            }
            _ => {
                if let crate::input::Edit::Changed(appended) = crate::input::edit(&mut f.query, &mut f.qcur, k) {
                    edited = Some(appended);
                }
            }
        }
        if let Some(appended) = edited {
            if !f.take_sigil() {
                f.query_changed(appended);
            }
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
            if marked.is_empty() {
                self.open_files(vec![it.abs.clone()], it.line, how);
            } else {
                // each file once, in mark order; the first mark's line leads
                let mut files: Vec<PathBuf> = Vec::new();
                for m in &marked {
                    if !files.contains(&m.path) {
                        files.push(m.path.clone());
                    }
                }
                self.open_files(files, marked[0].line, how);
            }
        }
    }

    /// Projects tab keys; false when the generic finder keys should handle
    /// it (typing, moving, ^y ^g ^s, ⇥, esc).
    fn project_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(f) = self.finder.as_mut() else { return false };
        let settings = f.proj_settings.clone();
        let sel = f.selected().and_then(|it| it.proj);
        let query = f.query.trim().to_string();
        // a pending ^k confirmation lasts one key
        let armed = self.close_armed.take();
        if k.code == KeyCode::Char('k') && ctrl {
            self.close_armed = armed;
        }
        match (k.code, ctrl, alt) {
            (KeyCode::Enter, _, _) | (KeyCode::Char('t'), true, _) => {
                let Some(p) = sel else {
                    // nothing matches: ⏎ on a fresh name offers ^n
                    if !query.is_empty() {
                        self.flash(format!("no project matches — ^n creates {}", finder::tilde(&settings.default_root.join(query.replace(' ', "-")))), MsgKind::Info);
                    }
                    return true;
                };
                if self.pick {
                    self.picked = vec![p.path];
                    self.quit = true;
                    return true;
                }
                self.hop(&p, k.code != KeyCode::Enter);
            }
            (KeyCode::Char('o'), true, _) => {
                if let Some(p) = sel {
                    projects::bump_recent(&settings.recent_file, &p.path);
                    self.goto(&p.path, None);
                    self.view = View::Explorer;
                    self.finder_only = false;
                }
            }
            (KeyCode::Char('f'), true, _) | (KeyCode::Char('g'), _, true) => {
                if let Some(p) = sel {
                    projects::bump_recent(&settings.recent_file, &p.path);
                    let kind = if k.code == KeyCode::Char('f') { FinderKind::Files } else { FinderKind::Grep };
                    f.query.clear();
                    f.qcur = 0;
                    f.set_kind(kind);
                    f.set_root(p.path);
                    self.preview_scroll = 0;
                }
            }
            (KeyCode::Char('a'), true, _) => {
                f.cycle_scope();
                self.preview.invalidate();
            }
            (KeyCode::Char('x'), true, _) => {
                match sel.filter(|p| p.hopped.is_some()) {
                    Some(p) => {
                        projects::forget_recent(&settings.recent_file, &p.path);
                        f.restart();
                        self.flash(format!("forgot {} (still listed under all)", p.name), MsgKind::Info);
                    }
                    None => self.flash("not a recent project", MsgKind::Info),
                }
            }
            (KeyCode::Char('k'), true, _) => match sel.as_ref().and_then(|p| p.ws.as_ref().map(|w| (p, w))) {
                // closing kills its panes (agents too): ^k twice
                Some((p, ws)) if self.close_armed.as_ref() != Some(&p.path) => {
                    match projects::close_target(ws) {
                        Some((what, _)) => {
                            let agents = if ws.agents > 0 { format!(" and its {} agent(s)", ws.agents) } else { String::new() };
                            self.flash(format!("^k again closes the {what} of {}{agents}", p.name), MsgKind::Warn);
                            self.close_armed = Some(p.path.clone());
                        }
                        None => self.flash("other projects share its workspace and tab — close it in herdr", MsgKind::Info),
                    }
                    return true;
                }
                Some((p, ws)) => match projects::close(ws) {
                    Ok(what) => {
                        let s = f.sel;
                        f.restart();
                        f.sel = s;
                        self.preview.invalidate();
                        self.flash(format!("closed the {what} of {}", p.name), MsgKind::Ok);
                    }
                    Err(e) => self.flash(format!("close: {e}"), MsgKind::Err),
                },
                None => self.flash("no workspace open on this project", MsgKind::Info),
            },
            (KeyCode::Char('n'), true, _) => {
                let q = query;
                if q.is_empty() {
                    self.flash(format!("type the new project's name first, then ^n (created in {})", finder::tilde(&settings.default_root)), MsgKind::Warn);
                    return true;
                }
                match projects::create(&q, &settings) {
                    Ok(path) => {
                        let all = projects::index(&settings);
                        let row = projects::Proj {
                            name: projects::display_name(&path, &settings.roots),
                            label: projects::label(&path, &all),
                            hopped: None,
                            icon: projects::icon(&path),
                            head: projects::head(&path),
                            ws: None,
                            here: false,
                            dirty: None,
                            path,
                        };
                        if self.pick {
                            self.picked = vec![row.path];
                            self.quit = true;
                        } else {
                            self.hop(&row, false);
                        }
                    }
                    Err(e) => self.flash(e, MsgKind::Err),
                }
            }
            (KeyCode::Char('v' | 'e' | ' '), true, _) => {}
            _ => return false,
        }
        true
    }

    /// Hop to a project as a herdr workspace (or a tab) and close the popup.
    fn hop(&mut self, p: &projects::Proj, tab: bool) {
        if !crate::launch::in_herdr() {
            self.flash("not inside herdr — ^o explores the project here instead", MsgKind::Warn);
            return;
        }
        let settings = match self.finder.as_ref() {
            Some(f) => f.proj_settings.clone(),
            None => self.cfg.projects.clone(),
        };
        match projects::hop(&p.path, tab, p.ws.as_ref(), &p.label, &settings) {
            Ok(m) if self.modified_dirs().is_empty() => {
                self.flash(m, MsgKind::Ok);
                self.quit = true;
            }
            Ok(m) => self.flash(format!("{m} — unsaved edits keep the explorer open"), MsgKind::Warn),
            Err(e) => self.flash(format!("hop: {e}"), MsgKind::Err),
        }
    }

    /// Recompute viewer search hits and jump to the first one at/after `from`
    /// (or before, going backwards).
    fn viewer_find(&mut self, from: usize, forward: bool) {
        let Some(v) = self.viewer.as_mut() else { return };
        if v.search.is_empty() {
            return;
        }
        let smart = v.search.chars().any(|c| c.is_uppercase());
        let pat = if smart { v.search.clone() } else { v.search.to_lowercase() };
        v.hits = self
            .viewer_cache
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                if smart { t.contains(&pat) } else { t.to_lowercase().contains(&pat) }
            })
            .map(|(i, _)| i)
            .collect();
        let hit = if forward { v.hits.iter().find(|&&h| h >= from).or(v.hits.first()) } else { v.hits.iter().rev().find(|&&h| h <= from).or(v.hits.last()) };
        match hit.copied() {
            Some(h) => {
                let max_top = v.total.saturating_sub(v.height.max(1));
                v.top = Some(h.saturating_sub(v.height / 3).min(max_top));
                v.at = Some(h);
                let i = v.hits.iter().position(|&x| x == h).unwrap_or(0);
                let msg = format!("/{}  [{}/{}]", v.search, i + 1, v.hits.len());
                self.flash(msg, MsgKind::Info);
            }
            None => {
                v.at = None;
                let msg = format!("pattern not found: {}", v.search);
                self.flash(msg, MsgKind::Warn);
            }
        }
    }

    fn viewer_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(v) = self.viewer.as_mut() else {
            self.view = View::Explorer;
            return;
        };
        if let Some((t, cur)) = v.typing.as_mut() {
            match k.code {
                KeyCode::Esc => v.typing = None,
                KeyCode::Enter => {
                    let (t, _) = v.typing.take().unwrap_or_default();
                    if !t.is_empty() {
                        v.search = t;
                    }
                    if let (true, Some(dv)) = (v.diff, v.dv.as_mut()) {
                        dv.search = v.search.clone();
                        dv.find();
                        let out = dv.jump_hit(true);
                        self.dv_out(out);
                        return;
                    }
                    let from = v.top.unwrap_or(0) + v.height / 3;
                    self.viewer_find(from, true);
                }
                KeyCode::Backspace if t.is_empty() => v.typing = None,
                _ => {
                    crate::input::edit(t, cur, k);
                }
            }
            return;
        }
        if let (true, Some(dv)) = (v.diff, v.dv.as_mut()) {
            if (k.code, ctrl) == (KeyCode::Char('c'), true) {
                self.try_quit(false);
                return;
            }
            let out = dv.key(k, &mut self.viewed);
            self.dv_out(out);
            return;
        }
        let h = v.height.max(1);
        let max_top = v.total.saturating_sub(h);
        let top = v.top.unwrap_or(0);
        let mut scroll = |d: isize| v.top = Some((top as isize + d).clamp(0, max_top as isize) as usize);
        let path = v.path.clone();
        let line = v.line;
        // cache index → file line (the viewer renders from `start`)
        let start = line.map(|l| l.saturating_sub(crate::preview::VIEWER_LINES / 4).max(1)).unwrap_or(1);
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
            (KeyCode::Char('/'), false) => v.typing = Some((String::new(), 0)),
            (KeyCode::Char('L'), false) => {
                v.log = !v.log;
                (v.top, v.at) = (None, None);
                v.hits.clear();
                let msg = if v.log { "git log of this path — L for the file" } else { "showing the file" };
                self.flash(msg, MsgKind::Info);
            }
            (KeyCode::Char('d'), false) => {
                if v.dv.is_none() {
                    match crate::diffview::items_under(&path) {
                        Ok((root, items)) if !items.is_empty() => v.dv = Some(crate::diffview::DiffView::new(root, items, 0, self.cfg.diff_layout)),
                        Ok(_) => {
                            self.flash("no changes against HEAD", MsgKind::Info);
                            return;
                        }
                        Err(e) => {
                            self.flash(e, MsgKind::Warn);
                            return;
                        }
                    }
                }
                v.diff = true;
                v.log = false;
                self.flash("diff — d for the file, ]c [c hunks, s split", MsgKind::Info);
            }
            (KeyCode::Char('p'), false) => {
                // the current search hit, else the line it was opened at
                let line = v.at.map(|i| start + i).or(line);
                self.send_paths(vec![(path, line)]);
            }
            (KeyCode::Char('P'), false) => {
                // a few lines around the current hit (or the screen's focus)
                let at = v.at.unwrap_or(top + h / 3) + start;
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let (a, z) = (at.saturating_sub(5).max(1), at + 5);
                let body: Vec<&str> = text.lines().skip(a - 1).take(z + 1 - a).collect();
                if body.is_empty() {
                    self.flash("nothing to send here", MsgKind::Info);
                    return;
                }
                let rel = path.strip_prefix(&self.start).map(|r| r.display().to_string()).unwrap_or_else(|_| path.display().to_string());
                let z = a + body.len() - 1;
                self.send_context(format!("{rel}:{a}-{z}\n```\n{}\n```", body.join("\n")));
            }
            (KeyCode::Char('n') | KeyCode::Char('N'), false) => {
                if v.search.is_empty() {
                    self.flash("no search yet — / to search", MsgKind::Info);
                    return;
                }
                // next/previous hit after the current one, or from the
                // screen's focus line once it has been scrolled away
                let fwd = k.code == KeyCode::Char('n');
                let from = match v.at.filter(|&a| a >= top && a < top + h) {
                    Some(a) if fwd => a + 1,
                    Some(a) => a.wrapping_sub(1).min(v.total),
                    None => top + h / 3,
                };
                self.viewer_find(from, fwd);
            }
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
            MouseEventKind::Down(MouseButton::Left) if self.view == View::Explorer && m.row == self.head_y && matches!(self.overlay, Overlay::None) => {
                // a path segment in the header: jump to that folder
                if let Some((_, _, p)) = self.crumbs.iter().find(|(a, z, _)| m.column >= *a && m.column < *z).cloned() {
                    let name = self.cwd.strip_prefix(&p).ok().and_then(|r| r.components().next()).map(|c| c.as_os_str().to_string_lossy().into_owned());
                    self.goto(&p, name.as_deref());
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
        if !self.buf().modified() {
            self.reload_cwd();
        }
        self.preview.invalidate();
        self.viewer_cache.invalidate();
        self.gutter = None;
        if let Some(dv) = self.viewer.as_mut().and_then(|v| v.dv.as_mut()) {
            dv.invalidate();
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        let mut last_tick = Instant::now();
        loop {
            if let Some(f) = self.finder.as_mut() {
                f.tick();
            }
            self.poll_git();
            self.preview.poll();
            self.viewer_cache.poll();
            terminal.draw(|fr| crate::ui::draw(fr, self))?;
            if self.quit {
                return Ok(());
            }
            let busy = self.finder.as_ref().is_some_and(|f| f.walking()) && self.view == View::Finder;
            // previews and git badges land from worker threads: look again soon
            let timeout = if self.busy() {
                Duration::from_millis(16)
            } else if busy {
                Duration::from_millis(40)
            } else {
                Duration::from_millis(250)
            };
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
                self.finish_edit_buffer();
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

    #[test]
    fn finder_sigils_backspace_and_menu() {
        let (_t, mut app) = app_in("app-omnibar");
        let key = |app: &mut App, c: KeyCode| app.finder_key(KeyEvent::new(c, KeyModifiers::NONE));
        app.act(Action::Finder(FinderKind::All));
        assert_eq!(app.finder.as_ref().unwrap().kind, FinderKind::All);
        // a sigil typed first switches source and is not kept in the query
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Char('x'));
        let f = app.finder.as_ref().unwrap();
        assert_eq!((f.kind, f.query.as_str()), (FinderKind::Grep, "x"));
        // ⌫ edits, then on the empty query goes back to All, then to the explorer
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.finder.as_ref().unwrap().kind, FinderKind::All);
        assert_eq!(app.view, View::Finder);
        // ␣ on the empty query: the menu; a letter picks
        key(&mut app, KeyCode::Char(' '));
        assert!(app.finder_menu);
        key(&mut app, KeyCode::Char('p'));
        assert!(!app.finder_menu);
        assert_eq!(app.finder.as_ref().unwrap().kind, FinderKind::Projects);
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.view, View::Explorer);
    }

    fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|fr| crate::ui::draw(fr, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    }

    #[test]
    fn finder_quiet_prompt() {
        let (_t, mut app) = app_in("app-quiet");
        let key = |app: &mut App, c: KeyCode| app.finder_key(KeyEvent::new(c, KeyModifiers::NONE));
        app.act(Action::Finder(FinderKind::All));
        let rows = screen(&mut app, 100, 20);
        let text = rows.join("\n");
        // one header row: the prompt with its sigil and a noun, no key hints
        assert!(rows.iter().any(|r| r.contains("› files & folders")), "{text}");
        assert!(!text.contains("FIND") && !text.contains("⌫ all") && !text.contains("␣ sources"), "{text}");
        assert!(rows.last().unwrap().contains("? keys"), "{text}");
        // after two idle seconds the placeholder names the sigils
        app.last_key = Instant::now() - Duration::from_secs(3);
        assert!(screen(&mut app, 100, 20).join("\n").contains("or start with / grep"));
        key(&mut app, KeyCode::Char('@'));
        let rows = screen(&mut app, 100, 20);
        assert!(rows.iter().any(|r| r.contains("@ projects project or branch")), "{}", rows.join("\n"));
        // ? opens the full sheet; a letter still picks a source
        key(&mut app, KeyCode::Char('?'));
        assert!(app.finder_sheet);
        assert!(screen(&mut app, 100, 30).join("\n").contains("? keys"));
        key(&mut app, KeyCode::Char('g'));
        assert!(!app.finder_sheet && !app.finder_menu);
        assert_eq!(app.finder.as_ref().unwrap().kind, FinderKind::Grep);
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

    #[test]
    fn chmod_specs() {
        assert_eq!(chmod_mode(0o644, "+x"), Some(0o755));
        assert_eq!(chmod_mode(0o755, "go-x"), Some(0o744));
        assert_eq!(chmod_mode(0o777, "u=rw"), Some(0o677));
        assert_eq!(chmod_mode(0o644, "600"), Some(0o600));
        assert_eq!(chmod_mode(0o644, "+q"), None);
        assert_eq!(chmod_mode(0o644, "999"), None);
    }

    #[test]
    fn history_back_and_forward() {
        let (t, mut app) = app_in("hist");
        app.goto(&t.0.join("src"), None);
        app.goto(&t.0.join("src/deep"), None);
        app.act(Action::HistBack);
        assert_eq!(app.cwd, t.0.join("src"));
        app.act(Action::HistBack);
        assert_eq!(app.cwd, t.0);
        app.act(Action::HistFwd);
        assert_eq!(app.cwd, t.0.join("src"));
        app.goto(&t.0, None);
        app.act(Action::HistFwd);
        assert_eq!(app.cwd, t.0, "a new jump clears forward history");
    }

    #[test]
    fn marks_persist() {
        let (t, mut app) = app_in("marks");
        app.goto(&t.0.join("src"), None);
        app.act(Action::SetMark('w'));
        app.goto(&t.0, None);
        app.act(Action::JumpMark('w'));
        assert_eq!(app.cwd, t.0.join("src"));
        assert_eq!(load_marks(&app.cfg).get(&'w'), Some(&t.0.join("src")));
    }

    #[test]
    fn chmod_applies_to_entries() {
        use std::os::unix::fs::PermissionsExt;
        let (t, mut app) = app_in("chmod");
        let id = app.buf().lines[row_of(&app, "a.txt")].id.unwrap();
        app.act(Action::Chmod { spec: "+x".into(), ids: vec![id] });
        let mode = std::fs::metadata(t.0.join("a.txt")).unwrap().permissions().mode();
        assert_ne!(mode & 0o100, 0);
    }

    #[test]
    fn edit_listing_roundtrip() {
        let (t, mut app) = app_in("ebuf");
        app.act(Action::EditBuffer);
        let (path, _, _) = app.edit_file.clone().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("/001 ") && text.contains(" a.txt\n"), "{text}");
        // what an editor would do: rename a, drop b, add a line
        let edited: String = text
            .lines()
            .filter(|l| !l.ends_with(" b.txt"))
            .map(|l| format!("{}\n", l.replace(" a.txt", " alpha.txt")))
            .chain(std::iter::once("new.md\n".to_string()))
            .collect();
        std::fs::write(&path, edited).unwrap();
        app.suspend = None;
        app.finish_edit_buffer();
        assert!(!path.exists());
        let plan = plan::build(app.buffers.values(), &app.store);
        assert!(plan.errors.is_empty(), "{:?}", plan.errors);
        assert!(plan.ops.contains(&plan::Op::Move { src: t.0.join("a.txt"), dst: t.0.join("alpha.txt"), dir: false }));
        assert!(plan.ops.contains(&plan::Op::Create { dst: t.0.join("new.md"), dir: false }));
        assert_eq!(plan.deletes(), 1);
        // one undo step brings the listing back
        app.buf_mut().undo();
        assert!(app.modified_dirs().is_empty());
    }

    #[test]
    fn edit_listing_refuses_control_characters() {
        // a link target with a newline would come back as a forged tagged line
        let (t, mut app) = app_in("ebuf-ctl");
        std::os::unix::fs::symlink("x\n/001 evil", t.0.join("trap")).unwrap();
        app.act(Action::Refresh);
        assert!(app.buf().lines.iter().any(|l| l.text.contains('\n')));
        app.act(Action::EditBuffer);
        assert!(app.edit_file.is_none() && app.suspend.is_none());
    }

    #[test]
    fn undo_last_apply() {
        let (t, mut app) = app_in("undo-apply");
        app.cfg.confirm = false;
        let r = row_of(&app, "a.txt");
        app.buf_mut().lines[r].text = "z.txt".into();
        app.act(Action::Save);
        assert!(t.0.join("z.txt").exists());
        app.act(Action::UndoApply);
        let Overlay::Confirm { plan, undo: true, .. } = &app.overlay else { panic!("no undo dialog") };
        assert_eq!(plan.ops, vec![plan::Op::Move { src: t.0.join("z.txt"), dst: t.0.join("a.txt"), dir: false }]);
        app.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(t.0.join("a.txt").exists() && !t.0.join("z.txt").exists());
    }

    #[test]
    fn send_formats() {
        let start = Path::new("/r");
        let ps = vec![(PathBuf::from("/r/src/app.rs"), Some(42)), (PathBuf::from("/elsewhere/a b"), None)];
        assert_eq!(send_words(&ps, start, true), ["@src/app.rs:42", "@/elsewhere/a b"]);
        assert_eq!(send_words(&ps[1..], start, false), [open::sh_quote("/elsewhere/a b")]);
    }

    #[test]
    fn chmod_refuses_symlinks_and_handles_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let (t, _app) = app_in("chmod-nofollow");
        std::os::unix::fs::symlink(t.0.join("a.txt"), t.0.join("l")).unwrap();
        assert!(chmod_nofollow(&t.0.join("l"), 0o777).is_err());
        assert_ne!(std::fs::metadata(t.0.join("a.txt")).unwrap().permissions().mode() & 0o777, 0o777);
        std::fs::set_permissions(t.0.join("b.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
        chmod_nofollow(&t.0.join("b.txt"), 0o644).unwrap();
        assert_eq!(std::fs::metadata(t.0.join("b.txt")).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn apply_without_undo_clears_journal() {
        use crate::plan::Op;
        let (t, mut app) = app_in("journal");
        app.cfg.delete = crate::config::DeleteMethod::Permanent;
        let mv = Op::Move { src: t.0.join("a.txt"), dst: t.0.join("c.txt"), dir: false };
        app.execute(Plan { ops: vec![mv], ..Default::default() }, false, false);
        assert!(app.last_batch.is_some());
        let del = Op::Delete { src: t.0.join("b.txt"), dir: false };
        app.execute(Plan { ops: vec![del], ..Default::default() }, false, false);
        assert!(app.last_batch.is_none());
    }

    #[test]
    fn help_filter_narrows_rows() {
        let all: usize = crate::ui::help_rows("").iter().map(|(_, r)| r.len()).sum();
        let some = crate::ui::help_rows("chmod");
        let n: usize = some.iter().map(|(_, r)| r.len()).sum();
        assert!(n > 0 && n < all);
        assert!(some.iter().all(|(_, r)| r.iter().all(|(k, v)| (k.to_string() + v).to_lowercase().contains("chmod"))));
        assert!(crate::ui::help_rows("zzzz-nothing").is_empty());
    }

    #[test]
    fn deleted_file_opens_as_diff() {
        let (t, mut app) = app_in("viewer-diff");
        let g = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&t.0).output().unwrap();
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(t.0.join("gone.txt"), "bye\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "x"]);
        std::fs::remove_file(t.0.join("gone.txt")).unwrap();
        app.open_files(vec![t.0.join("gone.txt")], None, OpenTarget::View);
        let v = app.viewer.as_mut().unwrap();
        assert!(v.diff);
        let f = v.dv.as_mut().and_then(|d| d.file()).expect("diff loaded");
        assert_eq!((f.status, f.stat()), ('D', (0, 1)));
        app.open_files(vec![t.0.join("a.txt")], None, OpenTarget::View);
        assert!(app.viewer.as_ref().is_some_and(|v| !v.diff));
        // a.txt is committed and unchanged: d has nothing to show
        app.viewer_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.viewer.as_ref().is_some_and(|v| !v.diff));
        std::fs::write(t.0.join("a.txt"), "changed\n").unwrap();
        app.viewer_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.viewer.as_ref().is_some_and(|v| v.diff));
    }

    #[test]
    fn conflicts_get_numbered() {
        let (t, mut app) = app_in("conflict-fix");
        let r = row_of(&app, "a.txt");
        app.buf_mut().lines[r].text = "b.txt".into();
        app.buf_mut().lines.push(crate::buffer::Line::new("new.md"));
        app.buf_mut().lines.push(crate::buffer::Line::new("new.md"));
        app.act(Action::Save);
        assert!(matches!(app.overlay, Overlay::Conflicts { .. }));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        let texts: Vec<String> = app.buf().lines.iter().map(|l| l.text.clone()).collect();
        assert!(texts.contains(&"b 2.txt".to_string()) && texts.contains(&"new.md".to_string()) && texts.contains(&"new 2.md".to_string()), "{texts:?}");
        let plan = plan::build(app.buffers.values(), &app.store);
        assert!(plan.errors.is_empty(), "{:?}", plan.errors);
        assert!(plan.ops.contains(&plan::Op::Move { src: t.0.join("a.txt"), dst: t.0.join("b 2.txt"), dir: false }));
    }

    #[test]
    fn confirm_drops_ops_and_history_records() {
        let (t, mut app) = app_in("confirm-drop");
        let r = row_of(&app, "a.txt");
        app.buf_mut().lines[r].text = "z.txt".into();
        let r = row_of(&app, "b.txt");
        app.buf_mut().lines.remove(r);
        app.act(Action::Save);
        let key = |app: &mut App, c: char| app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        // ops: the rename, then the delete; drop the delete
        key(&mut app, 'j');
        key(&mut app, 'x');
        let Overlay::Confirm { plan, .. } = &app.overlay else { panic!("dialog closed") };
        assert_eq!(plan.ops.len(), 1);
        key(&mut app, 'y');
        assert!(t.0.join("z.txt").exists() && t.0.join("b.txt").exists());
        assert_eq!(app.journal.len(), 1);
        assert_eq!(load_journal(&app.cfg).len(), 1, "persisted");
        app.command("history");
        key(&mut app, 'u');
        let Overlay::Confirm { plan, undo: true, .. } = &app.overlay else { panic!("no undo review") };
        assert_eq!(plan.ops, vec![plan::Op::Move { src: t.0.join("z.txt"), dst: t.0.join("a.txt"), dir: false }]);
    }

    #[test]
    fn plan_export_writes_text() {
        let (t, mut app) = app_in("plan-export");
        let r = row_of(&app, "a.txt");
        app.buf_mut().lines[r].text = "c.txt".into();
        app.command("plan > plan.txt");
        let text = std::fs::read_to_string(t.0.join("plan.txt")).unwrap();
        assert!(text.contains("rename") && text.contains("c.txt"), "{text}");
        app.command("plan > plan.txt");
        assert!(app.msg.as_ref().is_some_and(|m| m.0.contains("not overwritten")));
    }
}
